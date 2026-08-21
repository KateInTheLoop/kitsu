//! Private, effect-free admission for one compiled external echo source.
//!
//! A declaration is inert by default and cannot itself grant authority. An
//! enabled declaration must match one exact host profile before it becomes an
//! admitted source. A separate exact caller profile must then match before a
//! paired host/caller tool grant can be issued. This module performs no child
//! preparation, spawn, registry mutation, provider call, filesystem access, or
//! request-inventory mount.

#![allow(dead_code)]

use super::handler::{fixture_echo_descriptor_v1, ExternalEchoToolBindingV1};
use super::identity::{
    validate_external_inventory_v1, DescriptorFingerprintV1, ExternalIdentityErrorV1,
    ExternalInventoryCandidateV1, ExternalToolCandidateV1, ExternalToolIdentityV1,
    EXTERNAL_IDENTITY_CONTRACT_VERSION_V1,
};

pub(super) const EXTERNAL_ECHO_SOURCE_DECLARATION_VERSION_V1: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ExternalSourceActivationV1 {
    #[default]
    Disabled,
    Enabled,
}

/// Raw declaration facts. They intentionally have no `Debug`, `Clone`, or
/// serialization implementation: validation and exact authority comparison
/// precede any projection.
pub(super) struct ExternalEchoSourceDeclarationV1 {
    version: u32,
    activation: ExternalSourceActivationV1,
    source_id: String,
    source_incarnation: String,
    inventory_revision: u64,
    tool_id: String,
}

impl ExternalEchoSourceDeclarationV1 {
    pub(super) fn new(
        version: u32,
        activation: ExternalSourceActivationV1,
        source_id: impl Into<String>,
        source_incarnation: impl Into<String>,
        inventory_revision: u64,
        tool_id: impl Into<String>,
    ) -> Self {
        Self {
            version,
            activation,
            source_id: source_id.into(),
            source_incarnation: source_incarnation.into(),
            inventory_revision,
            tool_id: tool_id.into(),
        }
    }
}

impl Default for ExternalEchoSourceDeclarationV1 {
    fn default() -> Self {
        Self {
            version: EXTERNAL_ECHO_SOURCE_DECLARATION_VERSION_V1,
            activation: ExternalSourceActivationV1::Disabled,
            source_id: String::new(),
            source_incarnation: String::new(),
            inventory_revision: 0,
            tool_id: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalEchoExactToolV1 {
    identity: ExternalToolIdentityV1,
    descriptor_fingerprint: DescriptorFingerprintV1,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExternalEchoExactProfileErrorV1 {
    Identity(ExternalIdentityErrorV1),
    Invariant,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ExternalEchoSourceAuthorityProfileV1 {
    exact: ExternalEchoExactToolV1,
}

impl ExternalEchoSourceAuthorityProfileV1 {
    pub(super) fn exact(
        source_id: impl Into<String>,
        source_incarnation: impl Into<String>,
        inventory_revision: u64,
        tool_id: impl Into<String>,
    ) -> Result<Self, ExternalEchoExactProfileErrorV1> {
        Ok(Self {
            exact: validate_exact_echo_tool_v1(
                source_id.into(),
                source_incarnation.into(),
                inventory_revision,
                tool_id.into(),
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExternalEchoSourceAuthorityDecisionV1<'authority> {
    Deny,
    Admit(&'authority ExternalEchoSourceAuthorityProfileV1),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExternalEchoSourceAdmissionErrorV1 {
    Disabled,
    AuthorityDenied,
    UnsupportedVersion { observed: u32 },
    Identity(ExternalIdentityErrorV1),
    AuthorityMismatch,
    Invariant,
}

/// Exact admitted host facts. This value is deliberately non-cloneable.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct AdmittedExternalEchoSourceV1 {
    exact: ExternalEchoExactToolV1,
}

impl AdmittedExternalEchoSourceV1 {
    pub(super) fn identity(&self) -> &ExternalToolIdentityV1 {
        &self.exact.identity
    }

    pub(super) fn descriptor_fingerprint(&self) -> DescriptorFingerprintV1 {
        self.exact.descriptor_fingerprint
    }
}

pub(super) fn admit_external_echo_source_v1(
    declaration: ExternalEchoSourceDeclarationV1,
    decision: ExternalEchoSourceAuthorityDecisionV1<'_>,
) -> Result<AdmittedExternalEchoSourceV1, ExternalEchoSourceAdmissionErrorV1> {
    // An inert entry cannot trigger even validation or future-version parsing.
    if declaration.activation == ExternalSourceActivationV1::Disabled {
        return Err(ExternalEchoSourceAdmissionErrorV1::Disabled);
    }
    let authority = match decision {
        ExternalEchoSourceAuthorityDecisionV1::Deny => {
            return Err(ExternalEchoSourceAdmissionErrorV1::AuthorityDenied)
        }
        ExternalEchoSourceAuthorityDecisionV1::Admit(authority) => authority,
    };
    if declaration.version != EXTERNAL_ECHO_SOURCE_DECLARATION_VERSION_V1 {
        return Err(ExternalEchoSourceAdmissionErrorV1::UnsupportedVersion {
            observed: declaration.version,
        });
    }
    let requested = validate_exact_echo_tool_v1(
        declaration.source_id,
        declaration.source_incarnation,
        declaration.inventory_revision,
        declaration.tool_id,
    )
    .map_err(|error| match error {
        ExternalEchoExactProfileErrorV1::Identity(error) => {
            ExternalEchoSourceAdmissionErrorV1::Identity(error)
        }
        ExternalEchoExactProfileErrorV1::Invariant => ExternalEchoSourceAdmissionErrorV1::Invariant,
    })?;
    if requested != authority.exact {
        return Err(ExternalEchoSourceAdmissionErrorV1::AuthorityMismatch);
    }
    Ok(AdmittedExternalEchoSourceV1 { exact: requested })
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ExternalEchoCallerAuthorityProfileV1 {
    exact: ExternalEchoExactToolV1,
}

impl ExternalEchoCallerAuthorityProfileV1 {
    pub(super) fn exact(
        source_id: impl Into<String>,
        source_incarnation: impl Into<String>,
        inventory_revision: u64,
        tool_id: impl Into<String>,
    ) -> Result<Self, ExternalEchoExactProfileErrorV1> {
        Ok(Self {
            exact: validate_exact_echo_tool_v1(
                source_id.into(),
                source_incarnation.into(),
                inventory_revision,
                tool_id.into(),
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExternalEchoCallerAuthorityDecisionV1<'authority> {
    Deny,
    Admit(&'authority ExternalEchoCallerAuthorityProfileV1),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExternalEchoCallerAdmissionErrorV1 {
    AuthorityDenied,
    AuthorityMismatch,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct HostExternalToolGrantV1 {
    exact: ExternalEchoExactToolV1,
}

impl HostExternalToolGrantV1 {
    pub(super) fn identity(&self) -> &ExternalToolIdentityV1 {
        &self.exact.identity
    }

    pub(super) fn descriptor_fingerprint(&self) -> DescriptorFingerprintV1 {
        self.exact.descriptor_fingerprint
    }

    pub(super) fn matches(&self, binding: &ExternalEchoToolBindingV1) -> bool {
        self.exact.identity == *binding.identity()
            && self.exact.descriptor_fingerprint == binding.descriptor_fingerprint()
    }

    #[cfg(test)]
    pub(super) fn from_binding(binding: &ExternalEchoToolBindingV1) -> Self {
        Self::from_exact(binding.identity().clone(), binding.descriptor_fingerprint())
    }

    #[cfg(test)]
    pub(super) fn from_exact(
        identity: ExternalToolIdentityV1,
        descriptor_fingerprint: DescriptorFingerprintV1,
    ) -> Self {
        Self {
            exact: ExternalEchoExactToolV1 {
                identity,
                descriptor_fingerprint,
            },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct CallerExternalToolGrantV1 {
    exact: ExternalEchoExactToolV1,
}

impl CallerExternalToolGrantV1 {
    pub(super) fn identity(&self) -> &ExternalToolIdentityV1 {
        &self.exact.identity
    }

    pub(super) fn descriptor_fingerprint(&self) -> DescriptorFingerprintV1 {
        self.exact.descriptor_fingerprint
    }

    pub(super) fn matches(&self, binding: &ExternalEchoToolBindingV1) -> bool {
        self.exact.identity == *binding.identity()
            && self.exact.descriptor_fingerprint == binding.descriptor_fingerprint()
    }

    #[cfg(test)]
    pub(super) fn from_binding(binding: &ExternalEchoToolBindingV1) -> Self {
        Self::from_exact(binding.identity().clone(), binding.descriptor_fingerprint())
    }

    #[cfg(test)]
    pub(super) fn from_exact(
        identity: ExternalToolIdentityV1,
        descriptor_fingerprint: DescriptorFingerprintV1,
    ) -> Self {
        Self {
            exact: ExternalEchoExactToolV1 {
                identity,
                descriptor_fingerprint,
            },
        }
    }
}

/// One paired authorization for the one-call Cycle 133 owner. Neither grant
/// is independently constructible in production.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ExternalEchoDualGrantV1 {
    host: HostExternalToolGrantV1,
    caller: CallerExternalToolGrantV1,
}

impl ExternalEchoDualGrantV1 {
    pub(super) fn host(&self) -> &HostExternalToolGrantV1 {
        &self.host
    }

    pub(super) fn caller(&self) -> &CallerExternalToolGrantV1 {
        &self.caller
    }
}

pub(super) fn admit_external_echo_caller_v1(
    admitted: &AdmittedExternalEchoSourceV1,
    decision: ExternalEchoCallerAuthorityDecisionV1<'_>,
) -> Result<ExternalEchoDualGrantV1, ExternalEchoCallerAdmissionErrorV1> {
    let authority = match decision {
        ExternalEchoCallerAuthorityDecisionV1::Deny => {
            return Err(ExternalEchoCallerAdmissionErrorV1::AuthorityDenied)
        }
        ExternalEchoCallerAuthorityDecisionV1::Admit(authority) => authority,
    };
    if admitted.exact != authority.exact {
        return Err(ExternalEchoCallerAdmissionErrorV1::AuthorityMismatch);
    }
    Ok(ExternalEchoDualGrantV1 {
        host: HostExternalToolGrantV1 {
            exact: admitted.exact.clone(),
        },
        caller: CallerExternalToolGrantV1 {
            exact: admitted.exact.clone(),
        },
    })
}

fn validate_exact_echo_tool_v1(
    source_id: String,
    source_incarnation: String,
    inventory_revision: u64,
    tool_id: String,
) -> Result<ExternalEchoExactToolV1, ExternalEchoExactProfileErrorV1> {
    let inventory = validate_external_inventory_v1(ExternalInventoryCandidateV1::new(
        EXTERNAL_IDENTITY_CONTRACT_VERSION_V1,
        source_id,
        source_incarnation,
        inventory_revision,
        vec![ExternalToolCandidateV1::new(
            tool_id,
            fixture_echo_descriptor_v1(),
        )],
    ))
    .map_err(ExternalEchoExactProfileErrorV1::Identity)?;
    let Some(tool) = inventory.tools().first() else {
        return Err(ExternalEchoExactProfileErrorV1::Invariant);
    };
    if inventory.tools().len() != 1 {
        return Err(ExternalEchoExactProfileErrorV1::Invariant);
    }
    Ok(ExternalEchoExactToolV1 {
        identity: tool.identity().clone(),
        descriptor_fingerprint: tool.descriptor_fingerprint(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::identity::ExternalIdentityErrorV1;

    const SOURCE: &str = "fixture-source";
    const INCARNATION: &str = "boot-1";
    const REVISION: u64 = 1;
    const TOOL: &str = "echo";

    fn declaration(
        source: &str,
        incarnation: &str,
        revision: u64,
        tool: &str,
    ) -> ExternalEchoSourceDeclarationV1 {
        ExternalEchoSourceDeclarationV1::new(
            EXTERNAL_ECHO_SOURCE_DECLARATION_VERSION_V1,
            ExternalSourceActivationV1::Enabled,
            source,
            incarnation,
            revision,
            tool,
        )
    }

    fn host_profile() -> ExternalEchoSourceAuthorityProfileV1 {
        ExternalEchoSourceAuthorityProfileV1::exact(SOURCE, INCARNATION, REVISION, TOOL).unwrap()
    }

    fn caller_profile() -> ExternalEchoCallerAuthorityProfileV1 {
        ExternalEchoCallerAuthorityProfileV1::exact(SOURCE, INCARNATION, REVISION, TOOL).unwrap()
    }

    #[test]
    fn default_and_explicitly_disabled_sources_are_inert_before_all_other_checks() {
        assert_eq!(
            admit_external_echo_source_v1(
                ExternalEchoSourceDeclarationV1::default(),
                ExternalEchoSourceAuthorityDecisionV1::Deny,
            ),
            Err(ExternalEchoSourceAdmissionErrorV1::Disabled)
        );

        let malformed_disabled = ExternalEchoSourceDeclarationV1::new(
            u32::MAX,
            ExternalSourceActivationV1::Disabled,
            "INVALID SOURCE",
            "",
            0,
            "bad/tool",
        );
        assert_eq!(
            admit_external_echo_source_v1(
                malformed_disabled,
                ExternalEchoSourceAuthorityDecisionV1::Admit(&host_profile()),
            ),
            Err(ExternalEchoSourceAdmissionErrorV1::Disabled)
        );
    }

    #[test]
    fn authority_denial_precedes_enabled_declaration_syntax_and_identity_checks() {
        let malformed_enabled = ExternalEchoSourceDeclarationV1::new(
            u32::MAX,
            ExternalSourceActivationV1::Enabled,
            "INVALID SOURCE",
            "",
            0,
            "bad/tool",
        );
        assert_eq!(
            admit_external_echo_source_v1(
                malformed_enabled,
                ExternalEchoSourceAuthorityDecisionV1::Deny,
            ),
            Err(ExternalEchoSourceAdmissionErrorV1::AuthorityDenied)
        );
    }

    #[test]
    fn enabled_source_requires_supported_version_and_closed_identity_fields() {
        assert_eq!(
            admit_external_echo_source_v1(
                ExternalEchoSourceDeclarationV1::new(
                    EXTERNAL_ECHO_SOURCE_DECLARATION_VERSION_V1 + 1,
                    ExternalSourceActivationV1::Enabled,
                    SOURCE,
                    INCARNATION,
                    REVISION,
                    TOOL,
                ),
                ExternalEchoSourceAuthorityDecisionV1::Admit(&host_profile()),
            ),
            Err(ExternalEchoSourceAdmissionErrorV1::UnsupportedVersion {
                observed: EXTERNAL_ECHO_SOURCE_DECLARATION_VERSION_V1 + 1,
            })
        );

        assert_eq!(
            admit_external_echo_source_v1(
                declaration("INVALID SOURCE", INCARNATION, REVISION, TOOL),
                ExternalEchoSourceAuthorityDecisionV1::Admit(&host_profile()),
            ),
            Err(ExternalEchoSourceAdmissionErrorV1::Identity(
                ExternalIdentityErrorV1::InvalidSourceId,
            ))
        );
        assert_eq!(
            admit_external_echo_source_v1(
                declaration(SOURCE, INCARNATION, 0, TOOL),
                ExternalEchoSourceAuthorityDecisionV1::Admit(&host_profile()),
            ),
            Err(ExternalEchoSourceAdmissionErrorV1::Identity(
                ExternalIdentityErrorV1::InvalidInventoryRevision,
            ))
        );
    }

    #[test]
    fn exact_host_profile_is_required_for_every_identity_dimension() {
        for mismatched_profile in [
            ExternalEchoSourceAuthorityProfileV1::exact(
                "other-source",
                INCARNATION,
                REVISION,
                TOOL,
            )
            .unwrap(),
            ExternalEchoSourceAuthorityProfileV1::exact(SOURCE, "boot-2", REVISION, TOOL).unwrap(),
            ExternalEchoSourceAuthorityProfileV1::exact(SOURCE, INCARNATION, 2, TOOL).unwrap(),
            ExternalEchoSourceAuthorityProfileV1::exact(
                SOURCE,
                INCARNATION,
                REVISION,
                "other-tool",
            )
            .unwrap(),
        ] {
            assert_eq!(
                admit_external_echo_source_v1(
                    declaration(SOURCE, INCARNATION, REVISION, TOOL),
                    ExternalEchoSourceAuthorityDecisionV1::Admit(&mismatched_profile),
                ),
                Err(ExternalEchoSourceAdmissionErrorV1::AuthorityMismatch)
            );
        }
    }

    #[test]
    fn caller_denial_or_stale_exact_profile_issues_no_dual_grant() {
        let admitted = admit_external_echo_source_v1(
            declaration(SOURCE, INCARNATION, REVISION, TOOL),
            ExternalEchoSourceAuthorityDecisionV1::Admit(&host_profile()),
        )
        .unwrap();
        assert_eq!(
            admit_external_echo_caller_v1(&admitted, ExternalEchoCallerAuthorityDecisionV1::Deny,),
            Err(ExternalEchoCallerAdmissionErrorV1::AuthorityDenied)
        );

        for stale in [
            ExternalEchoCallerAuthorityProfileV1::exact(
                "other-source",
                INCARNATION,
                REVISION,
                TOOL,
            )
            .unwrap(),
            ExternalEchoCallerAuthorityProfileV1::exact(SOURCE, "boot-2", REVISION, TOOL).unwrap(),
            ExternalEchoCallerAuthorityProfileV1::exact(SOURCE, INCARNATION, REVISION + 1, TOOL)
                .unwrap(),
            ExternalEchoCallerAuthorityProfileV1::exact(
                SOURCE,
                INCARNATION,
                REVISION,
                "other-tool",
            )
            .unwrap(),
        ] {
            assert_eq!(
                admit_external_echo_caller_v1(
                    &admitted,
                    ExternalEchoCallerAuthorityDecisionV1::Admit(&stale),
                ),
                Err(ExternalEchoCallerAdmissionErrorV1::AuthorityMismatch)
            );
        }
    }

    #[test]
    fn exact_separate_host_and_caller_authority_yields_one_paired_grant() {
        let admitted = admit_external_echo_source_v1(
            declaration(SOURCE, INCARNATION, REVISION, TOOL),
            ExternalEchoSourceAuthorityDecisionV1::Admit(&host_profile()),
        )
        .unwrap();
        let grants = admit_external_echo_caller_v1(
            &admitted,
            ExternalEchoCallerAuthorityDecisionV1::Admit(&caller_profile()),
        )
        .unwrap();

        assert_eq!(grants.host().identity(), admitted.identity());
        assert_eq!(grants.caller().identity(), admitted.identity());
        assert_eq!(
            grants.host().descriptor_fingerprint(),
            admitted.descriptor_fingerprint()
        );
        assert_eq!(
            grants.caller().descriptor_fingerprint(),
            admitted.descriptor_fingerprint()
        );
    }
}
