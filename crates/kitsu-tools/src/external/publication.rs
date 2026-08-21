//! Atomic publication truth for validated external-tool inventories.

// This provider-free owner is deliberately staged before any product or MCP
// composition path consumes it.
#![allow(dead_code)]

use thiserror::Error;

use super::identity::{
    ExternalInventoryRevisionV1, ExternalSourceIdentityV1, ValidatedExternalInventoryV1,
};

/// The exact source lifetime and revision a publication transition expects.
///
/// A fence owns only closed identity values. It cannot retain descriptors or
/// become a second inventory view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalInventoryFenceV1 {
    source: ExternalSourceIdentityV1,
    revision: ExternalInventoryRevisionV1,
}

impl ExternalInventoryFenceV1 {
    pub(crate) fn from_inventory(inventory: &ValidatedExternalInventoryV1) -> Self {
        Self {
            source: inventory.source().clone(),
            revision: inventory.revision(),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum ExternalInventoryPublicationErrorV1 {
    #[error("an external inventory is already published")]
    AlreadyPublished,
    #[error("no external inventory is published")]
    NothingPublished,
    #[error("external inventory transition fence does not match the current publication")]
    PublicationFenceMismatch,
    #[error("successor inventory changed source identity")]
    SuccessorSourceMismatch,
    #[error("successor inventory revision is not the exact next revision")]
    SuccessorRevisionNotImmediate,
    #[error("replacement inventory retained the current source identity")]
    ReplacementSourceUnchanged,
}

/// Sole mutable owner of external inventory publication truth.
///
/// This type is deliberately neither `Clone` nor internally mutable. Readers
/// borrow one complete validated snapshot, and Rust requires those borrows to
/// end before an exclusive transition can replace or withdraw it.
#[derive(Debug, Default)]
pub(crate) struct ExternalInventoryPublicationV1 {
    published: Option<Box<ValidatedExternalInventoryV1>>,
}

impl ExternalInventoryPublicationV1 {
    pub(crate) fn new() -> Self {
        Self { published: None }
    }

    pub(crate) fn published(&self) -> Option<&ValidatedExternalInventoryV1> {
        self.published.as_deref()
    }

    pub(crate) fn publish_initial(
        &mut self,
        inventory: ValidatedExternalInventoryV1,
    ) -> Result<(), ExternalInventoryPublicationErrorV1> {
        if self.published.is_some() {
            return Err(ExternalInventoryPublicationErrorV1::AlreadyPublished);
        }
        self.published = Some(Box::new(inventory));
        Ok(())
    }

    pub(crate) fn publish_successor(
        &mut self,
        expected_current: &ExternalInventoryFenceV1,
        successor: ValidatedExternalInventoryV1,
    ) -> Result<(), ExternalInventoryPublicationErrorV1> {
        let current = self.require_exact_publication(expected_current)?;
        if successor.source() != current.source() {
            return Err(ExternalInventoryPublicationErrorV1::SuccessorSourceMismatch);
        }
        if !successor
            .revision()
            .is_immediate_successor_of(current.revision())
        {
            return Err(ExternalInventoryPublicationErrorV1::SuccessorRevisionNotImmediate);
        }

        self.published = Some(Box::new(successor));
        Ok(())
    }

    pub(crate) fn publish_replacement(
        &mut self,
        expected_current: &ExternalInventoryFenceV1,
        replacement: ValidatedExternalInventoryV1,
    ) -> Result<(), ExternalInventoryPublicationErrorV1> {
        let current = self.require_exact_publication(expected_current)?;
        if replacement.source() == current.source() {
            return Err(ExternalInventoryPublicationErrorV1::ReplacementSourceUnchanged);
        }

        self.published = Some(Box::new(replacement));
        Ok(())
    }

    pub(crate) fn withdraw(
        &mut self,
        expected_current: &ExternalInventoryFenceV1,
    ) -> Result<(), ExternalInventoryPublicationErrorV1> {
        self.require_exact_publication(expected_current)?;
        self.published = None;
        Ok(())
    }

    fn require_exact_publication(
        &self,
        expected_current: &ExternalInventoryFenceV1,
    ) -> Result<&ValidatedExternalInventoryV1, ExternalInventoryPublicationErrorV1> {
        let current = self
            .published
            .as_deref()
            .ok_or(ExternalInventoryPublicationErrorV1::NothingPublished)?;
        if ExternalInventoryFenceV1::from_inventory(current) != *expected_current {
            return Err(ExternalInventoryPublicationErrorV1::PublicationFenceMismatch);
        }
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::identity::{
        validate_external_inventory_v1, ExternalInventoryCandidateV1, ExternalToolCandidateV1,
        ExternalToolIdentityV1, ValidatedExternalInventoryV1,
        EXTERNAL_IDENTITY_CONTRACT_VERSION_V1,
    };
    use kitsu_providers::ToolDescriptor;

    fn descriptor(name: &str, description: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn inventory(
        source: &str,
        incarnation: &str,
        revision: u64,
        tools: &[(&str, &str, &str)],
    ) -> ValidatedExternalInventoryV1 {
        validate_external_inventory_v1(ExternalInventoryCandidateV1::new(
            EXTERNAL_IDENTITY_CONTRACT_VERSION_V1,
            source,
            incarnation,
            revision,
            tools
                .iter()
                .map(|(tool_id, name, description)| {
                    ExternalToolCandidateV1::new(*tool_id, descriptor(name, description))
                })
                .collect(),
        ))
        .expect("fixture is valid")
    }

    fn fence(inventory: &ValidatedExternalInventoryV1) -> ExternalInventoryFenceV1 {
        ExternalInventoryFenceV1::from_inventory(inventory)
    }

    fn assert_snapshot_unchanged(
        owner: &ExternalInventoryPublicationV1,
        snapshot_address: *const ValidatedExternalInventoryV1,
        descriptor_address: *const ToolDescriptor,
        identity_address: *const ExternalToolIdentityV1,
        expected_fence: &ExternalInventoryFenceV1,
        expected_descriptor: &ToolDescriptor,
        expected_identity: &ExternalToolIdentityV1,
    ) {
        let current = owner.published().expect("prior publication remains");
        assert_eq!(current as *const _, snapshot_address);
        assert_eq!(
            current.tools()[0].descriptor() as *const _,
            descriptor_address
        );
        assert_eq!(current.tools()[0].identity() as *const _, identity_address);
        assert_eq!(&fence(current), expected_fence);
        assert_eq!(current.tools()[0].descriptor(), expected_descriptor);
        assert_eq!(current.tools()[0].identity(), expected_identity);
    }

    #[test]
    fn initial_publication_is_explicit_and_refuses_overwrite() {
        let first = inventory("local.echo", "boot-a", 7, &[("read-a", "read_a", "first")]);
        let mut owner = ExternalInventoryPublicationV1::new();
        assert!(owner.published().is_none());

        owner.publish_initial(first).unwrap();
        let before = owner.published().unwrap();
        let snapshot_address = before as *const _;
        let descriptor_address = before.tools()[0].descriptor() as *const _;
        let identity_address = before.tools()[0].identity() as *const _;
        let prior_fence = fence(before);
        let prior_descriptor = before.tools()[0].descriptor().clone();
        let prior_identity = before.tools()[0].identity().clone();

        assert_eq!(
            owner.publish_initial(inventory(
                "local.other",
                "boot-b",
                1,
                &[("read-b", "read_b", "replacement")],
            )),
            Err(ExternalInventoryPublicationErrorV1::AlreadyPublished)
        );
        assert_snapshot_unchanged(
            &owner,
            snapshot_address,
            descriptor_address,
            identity_address,
            &prior_fence,
            &prior_descriptor,
            &prior_identity,
        );
    }

    #[test]
    fn successor_requires_exact_fence_and_immediate_same_incarnation_revision() {
        let first = inventory("local.echo", "boot-a", 7, &[("read-a", "read_a", "first")]);
        let current_fence = fence(&first);
        let mut owner = ExternalInventoryPublicationV1::new();
        owner.publish_initial(first).unwrap();

        let successor = inventory(
            "local.echo",
            "boot-a",
            8,
            &[("read-b", "read_b", "successor")],
        );
        let successor_fence = fence(&successor);
        owner.publish_successor(&current_fence, successor).unwrap();

        let published = owner.published().unwrap();
        assert_eq!(published.tools()[0].descriptor().name, "read_b");
        assert_eq!(fence(published), successor_fence);
    }

    #[test]
    fn maximum_revision_has_no_wrapping_successor() {
        let first = inventory(
            "local.echo",
            "boot-a",
            u64::MAX,
            &[("read-a", "read_a", "last revision")],
        );
        let exact_fence = fence(&first);
        let mut owner = ExternalInventoryPublicationV1::new();
        owner.publish_initial(first).unwrap();

        assert_eq!(
            owner.publish_successor(
                &exact_fence,
                inventory(
                    "local.echo",
                    "boot-a",
                    1,
                    &[("read-b", "read_b", "wrapped")]
                ),
            ),
            Err(ExternalInventoryPublicationErrorV1::SuccessorRevisionNotImmediate)
        );
        assert_eq!(fence(owner.published().unwrap()), exact_fence);
    }

    #[test]
    fn stale_gap_replay_and_wrong_identity_successors_preserve_exact_snapshot() {
        let first = inventory("local.echo", "boot-a", 7, &[("read-a", "read_a", "first")]);
        let exact_fence = fence(&first);
        let stale_fence = fence(&inventory("local.echo", "boot-a", 6, &[]));
        let mut owner = ExternalInventoryPublicationV1::new();
        owner.publish_initial(first).unwrap();

        let before = owner.published().unwrap();
        let snapshot_address = before as *const _;
        let descriptor_address = before.tools()[0].descriptor() as *const _;
        let identity_address = before.tools()[0].identity() as *const _;
        let prior_descriptor = before.tools()[0].descriptor().clone();
        let prior_identity = before.tools()[0].identity().clone();

        assert_eq!(
            owner.publish_successor(&exact_fence, inventory("local.echo", "boot-a", 7, &[]),),
            Err(ExternalInventoryPublicationErrorV1::SuccessorRevisionNotImmediate)
        );
        assert_snapshot_unchanged(
            &owner,
            snapshot_address,
            descriptor_address,
            identity_address,
            &exact_fence,
            &prior_descriptor,
            &prior_identity,
        );
        assert_eq!(
            owner.publish_successor(&exact_fence, inventory("local.echo", "boot-a", 9, &[]),),
            Err(ExternalInventoryPublicationErrorV1::SuccessorRevisionNotImmediate)
        );
        assert_snapshot_unchanged(
            &owner,
            snapshot_address,
            descriptor_address,
            identity_address,
            &exact_fence,
            &prior_descriptor,
            &prior_identity,
        );
        assert_eq!(
            owner.publish_successor(&stale_fence, inventory("local.echo", "boot-a", 8, &[]),),
            Err(ExternalInventoryPublicationErrorV1::PublicationFenceMismatch)
        );
        assert_snapshot_unchanged(
            &owner,
            snapshot_address,
            descriptor_address,
            identity_address,
            &exact_fence,
            &prior_descriptor,
            &prior_identity,
        );
        assert_eq!(
            owner.publish_successor(&exact_fence, inventory("local.other", "boot-a", 8, &[]),),
            Err(ExternalInventoryPublicationErrorV1::SuccessorSourceMismatch)
        );
        assert_snapshot_unchanged(
            &owner,
            snapshot_address,
            descriptor_address,
            identity_address,
            &exact_fence,
            &prior_descriptor,
            &prior_identity,
        );
        assert_eq!(
            owner.publish_successor(&exact_fence, inventory("local.echo", "boot-b", 8, &[]),),
            Err(ExternalInventoryPublicationErrorV1::SuccessorSourceMismatch)
        );
        assert_snapshot_unchanged(
            &owner,
            snapshot_address,
            descriptor_address,
            identity_address,
            &exact_fence,
            &prior_descriptor,
            &prior_identity,
        );
    }

    #[test]
    fn restart_or_replacement_is_explicit_and_fenced_by_previous_publication() {
        let first = inventory("local.echo", "boot-a", 7, &[("read-a", "read_a", "first")]);
        let exact_fence = fence(&first);
        let stale_fence = fence(&inventory("local.echo", "boot-a", 6, &[]));
        let mut owner = ExternalInventoryPublicationV1::new();
        owner.publish_initial(first).unwrap();

        let before = owner.published().unwrap();
        let snapshot_address = before as *const _;
        let descriptor_address = before.tools()[0].descriptor() as *const _;
        let identity_address = before.tools()[0].identity() as *const _;
        let prior_descriptor = before.tools()[0].descriptor().clone();
        let prior_identity = before.tools()[0].identity().clone();

        assert_eq!(
            owner.publish_replacement(&stale_fence, inventory("local.echo", "boot-b", 1, &[]),),
            Err(ExternalInventoryPublicationErrorV1::PublicationFenceMismatch)
        );
        assert_snapshot_unchanged(
            &owner,
            snapshot_address,
            descriptor_address,
            identity_address,
            &exact_fence,
            &prior_descriptor,
            &prior_identity,
        );
        assert_eq!(
            owner.publish_replacement(&exact_fence, inventory("local.echo", "boot-a", 8, &[]),),
            Err(ExternalInventoryPublicationErrorV1::ReplacementSourceUnchanged)
        );
        assert_snapshot_unchanged(
            &owner,
            snapshot_address,
            descriptor_address,
            identity_address,
            &exact_fence,
            &prior_descriptor,
            &prior_identity,
        );

        let restart = inventory(
            "local.echo",
            "boot-b",
            1,
            &[("write-b", "write_b", "new lifetime")],
        );
        let restart_fence = fence(&restart);
        owner.publish_replacement(&exact_fence, restart).unwrap();
        let published = owner.published().unwrap();
        assert_eq!(published.tools()[0].descriptor().name, "write_b");

        let replacement = inventory(
            "local.other",
            "boot-z",
            19,
            &[("list-z", "list_z", "replacement source")],
        );
        let replacement_fence = fence(&replacement);
        owner
            .publish_replacement(&restart_fence, replacement)
            .unwrap();
        assert_eq!(fence(owner.published().unwrap()), replacement_fence);
    }

    #[test]
    fn stale_withdrawal_preserves_snapshot_and_exact_withdrawal_removes_everything() {
        let first = inventory(
            "local.echo",
            "boot-a",
            7,
            &[
                ("read-a", "read_a", "first"),
                ("write-a", "write_a", "second"),
            ],
        );
        let exact_fence = fence(&first);
        let stale_fence = fence(&inventory("local.echo", "boot-a", 6, &[]));
        let mut owner = ExternalInventoryPublicationV1::new();
        owner.publish_initial(first).unwrap();

        let before = owner.published().unwrap();
        let snapshot_address = before as *const _;
        let descriptor_address = before.tools()[0].descriptor() as *const _;
        let identity_address = before.tools()[0].identity() as *const _;
        let prior_descriptor = before.tools()[0].descriptor().clone();
        let prior_identity = before.tools()[0].identity().clone();
        assert!(owner.withdraw(&stale_fence).is_err());
        assert_snapshot_unchanged(
            &owner,
            snapshot_address,
            descriptor_address,
            identity_address,
            &exact_fence,
            &prior_descriptor,
            &prior_identity,
        );

        owner.withdraw(&exact_fence).unwrap();
        assert!(owner.published().is_none());
        assert_eq!(
            owner.withdraw(&exact_fence),
            Err(ExternalInventoryPublicationErrorV1::NothingPublished)
        );
    }

    #[test]
    fn borrowed_readers_observe_one_whole_old_or_new_snapshot() {
        let first = inventory("local.echo", "boot-a", 7, &[("read-a", "read_a", "old")]);
        let first_fence = fence(&first);
        let mut owner = ExternalInventoryPublicationV1::new();
        owner.publish_initial(first).unwrap();

        {
            let borrowed = owner.published().unwrap();
            assert_eq!(borrowed.tools()[0].descriptor().description, "old");
            assert_eq!(fence(borrowed), first_fence);
        }

        let successor = inventory(
            "local.echo",
            "boot-a",
            8,
            &[
                ("read-a", "read_a", "new"),
                ("write-a", "write_a", "new second"),
            ],
        );
        let expected_identities: Vec<_> = successor
            .tools()
            .iter()
            .map(|tool| tool.identity().clone())
            .collect();
        owner.publish_successor(&first_fence, successor).unwrap();
        let borrowed = owner.published().unwrap();
        assert_eq!(borrowed.tools().len(), 2);
        assert!(borrowed
            .tools()
            .iter()
            .zip(expected_identities.iter())
            .all(|(tool, expected)| tool.identity() == expected));
        assert_eq!(borrowed.tools()[0].descriptor().description, "new");
    }
}
