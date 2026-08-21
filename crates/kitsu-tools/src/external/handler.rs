//! Private exact-identity composition for one sealed tools-only MCP source.
//!
//! This module is deliberately unmounted from product configuration, the
//! public registry, and provider request inventories.
//! Production code intentionally has no constructor for either exact-tool
//! grant: caller/subagent authority derivation remains a later mount gate.
//! This owner provides no discovery, pagination, retry, restart, concurrent
//! call, resources, prompts, sampling, or general MCP-handler claim.

#![allow(dead_code)]

use kitsu_providers::ToolDescriptor;
use serde_json::{json, Map, Value};

use super::authority::PreparedChildLaunchV1;
use super::identity::{
    validate_external_inventory_v1, DescriptorFingerprintV1, ExternalIdentityErrorV1,
    ExternalInventoryCandidateV1, ExternalToolCandidateV1, ExternalToolIdentityV1,
    ValidatedExternalInventoryV1, EXTERNAL_IDENTITY_CONTRACT_VERSION_V1,
};
use super::mcp::{
    McpSerialChildConstructionErrorV1, McpSerialChildErrorV1, McpSerialChildFailureV1,
    McpSerialChildOwnerV1, McpSerialChildStateV1,
};
use super::publication::{
    ExternalInventoryFenceV1, ExternalInventoryPublicationErrorV1, ExternalInventoryPublicationV1,
};
use super::serial::SerialChildTransportEffectsV1;
use crate::mcp_codec::ClientEvent;

const FIXTURE_ECHO_WIRE_NAME_V1: &str = "fixture.echo";
const FIXTURE_ECHO_DESCRIPTION_V1: &str = "Returns one integer as text.";

fn fixture_echo_descriptor_v1() -> ToolDescriptor {
    ToolDescriptor {
        name: FIXTURE_ECHO_WIRE_NAME_V1.to_string(),
        description: FIXTURE_ECHO_DESCRIPTION_V1.to_string(),
        parameters: json!({
            "additionalProperties": false,
            "properties": {"value": {"type": "integer"}},
            "required": ["value"],
            "type": "object"
        }),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExternalMcpEchoConstructionErrorV1 {
    Mcp(McpSerialChildConstructionErrorV1),
    Identity(ExternalIdentityErrorV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExternalCallDispositionV1 {
    NotSent,
    MayHaveExecuted,
    ResponseValidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FixtureEchoRequestV1 {
    value: i64,
}

impl FixtureEchoRequestV1 {
    pub(super) fn new(value: i64) -> Self {
        Self { value }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FixtureEchoResultV1 {
    value: i64,
}

impl FixtureEchoResultV1 {
    pub(super) fn value(self) -> i64 {
        self.value
    }
}

/// One exact published binding. The wire name is not an identity: callers
/// must also match the full host identity and canonical descriptor digest.
#[derive(Debug, PartialEq)]
pub(super) struct ExternalEchoToolBindingV1 {
    identity: ExternalToolIdentityV1,
    descriptor_fingerprint: DescriptorFingerprintV1,
    publication_fence: ExternalInventoryFenceV1,
    wire_name: String,
    descriptor: ToolDescriptor,
}

impl ExternalEchoToolBindingV1 {
    pub(super) fn wire_name(&self) -> &str {
        &self.wire_name
    }

    pub(super) fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    pub(super) fn identity(&self) -> &ExternalToolIdentityV1 {
        &self.identity
    }

    pub(super) fn descriptor_fingerprint(&self) -> DescriptorFingerprintV1 {
        self.descriptor_fingerprint
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct HostExternalToolGrantV1 {
    identity: ExternalToolIdentityV1,
    descriptor_fingerprint: DescriptorFingerprintV1,
}

impl HostExternalToolGrantV1 {
    #[cfg(test)]
    pub(super) fn from_binding(binding: &ExternalEchoToolBindingV1) -> Self {
        Self::from_exact(binding.identity.clone(), binding.descriptor_fingerprint)
    }

    #[cfg(test)]
    pub(super) fn from_exact(
        identity: ExternalToolIdentityV1,
        descriptor_fingerprint: DescriptorFingerprintV1,
    ) -> Self {
        Self {
            identity,
            descriptor_fingerprint,
        }
    }

    fn matches(&self, binding: &ExternalEchoToolBindingV1) -> bool {
        self.identity == binding.identity
            && self.descriptor_fingerprint == binding.descriptor_fingerprint
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct CallerExternalToolGrantV1 {
    identity: ExternalToolIdentityV1,
    descriptor_fingerprint: DescriptorFingerprintV1,
}

impl CallerExternalToolGrantV1 {
    #[cfg(test)]
    pub(super) fn from_binding(binding: &ExternalEchoToolBindingV1) -> Self {
        Self::from_exact(binding.identity.clone(), binding.descriptor_fingerprint)
    }

    #[cfg(test)]
    pub(super) fn from_exact(
        identity: ExternalToolIdentityV1,
        descriptor_fingerprint: DescriptorFingerprintV1,
    ) -> Self {
        Self {
            identity,
            descriptor_fingerprint,
        }
    }

    fn matches(&self, binding: &ExternalEchoToolBindingV1) -> bool {
        self.identity == binding.identity
            && self.descriptor_fingerprint == binding.descriptor_fingerprint
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ExternalCallAttemptV1 {
    sequence: u64,
    request: FixtureEchoRequestV1,
    identity: ExternalToolIdentityV1,
    descriptor_fingerprint: DescriptorFingerprintV1,
    disposition: ExternalCallDispositionV1,
}

impl ExternalCallAttemptV1 {
    pub(super) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) fn disposition(&self) -> ExternalCallDispositionV1 {
        self.disposition
    }

    pub(super) fn request(&self) -> FixtureEchoRequestV1 {
        self.request
    }

    pub(super) fn identity(&self) -> &ExternalToolIdentityV1 {
        &self.identity
    }

    pub(super) fn descriptor_fingerprint(&self) -> DescriptorFingerprintV1 {
        self.descriptor_fingerprint
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExternalMcpEchoFailureKindV1 {
    Inventory,
    Publication,
    Result,
    Mcp,
    Cleanup,
    Invariant,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExternalInventoryObservationErrorV1 {
    ResultNotObject,
    PaginationUnsupported,
    UnexpectedResultFields,
    ToolCount { observed: usize },
    ToolNotObject,
    UnexpectedToolFields,
    InvalidToolName,
    InvalidToolDescription,
    InvalidToolSchema,
    DescriptorMismatch,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExternalEchoResultErrorV1 {
    ResultNotObject,
    UnexpectedResultFields,
    RemoteReportedError,
    ContentCount { observed: usize },
    ContentNotObject,
    UnexpectedContentFields,
    InvalidContentType,
    InvalidContentText,
    ValueMismatch,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExternalMcpEchoFailureV1 {
    Inventory(ExternalInventoryObservationErrorV1),
    Publication(ExternalInventoryPublicationErrorV1),
    Result(ExternalEchoResultErrorV1),
    Mcp(McpSerialChildFailureV1),
    McpOperation(McpSerialChildErrorV1),
    Cleanup {
        primary: Box<ExternalMcpEchoFailureV1>,
        mcp: Box<McpSerialChildFailureV1>,
    },
    CleanupOperation {
        primary: Box<ExternalMcpEchoFailureV1>,
        mcp: Box<McpSerialChildErrorV1>,
    },
    Withdrawal {
        primary: Box<ExternalMcpEchoFailureV1>,
        publication: ExternalInventoryPublicationErrorV1,
    },
    Invariant(&'static str),
}

impl ExternalMcpEchoFailureV1 {
    fn kind(&self) -> ExternalMcpEchoFailureKindV1 {
        match self {
            Self::Inventory(_) => ExternalMcpEchoFailureKindV1::Inventory,
            Self::Publication(_) => ExternalMcpEchoFailureKindV1::Publication,
            Self::Result(_) => ExternalMcpEchoFailureKindV1::Result,
            Self::Mcp(_) | Self::McpOperation(_) => ExternalMcpEchoFailureKindV1::Mcp,
            Self::Cleanup { .. } | Self::CleanupOperation { .. } => {
                ExternalMcpEchoFailureKindV1::Cleanup
            }
            Self::Withdrawal { .. } => ExternalMcpEchoFailureKindV1::Publication,
            Self::Invariant(_) => ExternalMcpEchoFailureKindV1::Invariant,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExternalMcpEchoCompletionV1 {
    EchoValidated,
    Cancelled,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExternalMcpEchoSourceStateV1 {
    PreparedUnpublished,
    Starting,
    Listing,
    PublishedReady,
    CallInFlight,
    ResultStaged,
    Cancelling,
    ShuttingDown,
    Cleaning,
    Completed,
    Failed { kind: ExternalMcpEchoFailureKindV1 },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExternalMcpEchoSourceErrorV1 {
    StartAlreadyAttempted,
    NotPublished,
    CallAlreadyAttempted,
    HostGrantMismatch,
    CallerGrantMismatch,
    AttemptSpaceExhausted,
    ResultUnavailable,
    ResultAlreadyTaken,
    Mcp(McpSerialChildErrorV1),
}

pub(super) struct ExternalMcpEchoSourceOwnerV1<'effects, E: SerialChildTransportEffectsV1> {
    mcp: McpSerialChildOwnerV1<'effects, E>,
    publication: ExternalInventoryPublicationV1,
    expected_inventory: Option<ValidatedExternalInventoryV1>,
    binding: Option<ExternalEchoToolBindingV1>,
    state: ExternalMcpEchoSourceStateV1,
    failure: Option<ExternalMcpEchoFailureV1>,
    attempt: Option<ExternalCallAttemptV1>,
    next_attempt: u64,
    staged_result: Option<FixtureEchoResultV1>,
    result_taken: bool,
    requested_completion: Option<ExternalMcpEchoCompletionV1>,
    completion: Option<ExternalMcpEchoCompletionV1>,
}

impl<'effects, E> ExternalMcpEchoSourceOwnerV1<'effects, E>
where
    E: SerialChildTransportEffectsV1,
{
    pub(super) fn new(
        prepared: PreparedChildLaunchV1,
        effects: &'effects mut E,
        source_id: &str,
        source_incarnation: &str,
        inventory_revision: u64,
        tool_id: &str,
    ) -> Result<Self, ExternalMcpEchoConstructionErrorV1> {
        let mcp = McpSerialChildOwnerV1::new(prepared, effects)
            .map_err(ExternalMcpEchoConstructionErrorV1::Mcp)?;
        let expected_inventory = validate_external_inventory_v1(ExternalInventoryCandidateV1::new(
            EXTERNAL_IDENTITY_CONTRACT_VERSION_V1,
            source_id,
            source_incarnation,
            inventory_revision,
            vec![ExternalToolCandidateV1::new(
                tool_id,
                fixture_echo_descriptor_v1(),
            )],
        ))
        .map_err(ExternalMcpEchoConstructionErrorV1::Identity)?;
        Ok(Self {
            mcp,
            publication: ExternalInventoryPublicationV1::new(),
            expected_inventory: Some(expected_inventory),
            binding: None,
            state: ExternalMcpEchoSourceStateV1::PreparedUnpublished,
            failure: None,
            attempt: None,
            next_attempt: 1,
            staged_result: None,
            result_taken: false,
            requested_completion: None,
            completion: None,
        })
    }

    pub(super) fn state(&self) -> &ExternalMcpEchoSourceStateV1 {
        &self.state
    }

    pub(super) fn failure(&self) -> Option<&ExternalMcpEchoFailureV1> {
        self.failure.as_ref()
    }

    pub(super) fn publication(&self) -> Option<&ValidatedExternalInventoryV1> {
        self.publication.published()
    }

    pub(super) fn binding(&self) -> Option<&ExternalEchoToolBindingV1> {
        self.binding.as_ref()
    }

    pub(super) fn attempt(&self) -> Option<&ExternalCallAttemptV1> {
        self.attempt.as_ref()
    }

    pub(super) fn completion(&self) -> Option<ExternalMcpEchoCompletionV1> {
        self.completion
    }

    pub(super) fn staged_result_evidence(&self) -> Option<&FixtureEchoResultV1> {
        self.staged_result.as_ref()
    }

    pub(super) fn start(
        &mut self,
    ) -> Result<&ExternalMcpEchoSourceStateV1, ExternalMcpEchoSourceErrorV1> {
        if self.state != ExternalMcpEchoSourceStateV1::PreparedUnpublished {
            return Err(ExternalMcpEchoSourceErrorV1::StartAlreadyAttempted);
        }
        self.mcp
            .start()
            .map_err(ExternalMcpEchoSourceErrorV1::Mcp)?;
        self.state = ExternalMcpEchoSourceStateV1::Starting;
        self.synchronize_mcp();
        Ok(&self.state)
    }

    pub(super) fn poll(
        &mut self,
    ) -> Result<&ExternalMcpEchoSourceStateV1, ExternalMcpEchoSourceErrorV1> {
        if !matches!(
            self.mcp.state(),
            McpSerialChildStateV1::ReadyForToolsList
                | McpSerialChildStateV1::EventReady
                | McpSerialChildStateV1::ReadyForToolsCall
                | McpSerialChildStateV1::Exhausted
                | McpSerialChildStateV1::Stopped
                | McpSerialChildStateV1::Failed { .. }
        ) {
            if let Err(error) = self.mcp.poll() {
                self.fail_primary(ExternalMcpEchoFailureV1::McpOperation(error));
                return Ok(&self.state);
            }
        }
        self.synchronize_mcp();
        self.advance_source();
        Ok(&self.state)
    }

    pub(super) fn begin_echo(
        &mut self,
        request: FixtureEchoRequestV1,
        host_grant: &HostExternalToolGrantV1,
        caller_grant: &CallerExternalToolGrantV1,
    ) -> Result<&ExternalMcpEchoSourceStateV1, ExternalMcpEchoSourceErrorV1> {
        if self.state != ExternalMcpEchoSourceStateV1::PublishedReady {
            return Err(if self.attempt.is_some() {
                ExternalMcpEchoSourceErrorV1::CallAlreadyAttempted
            } else {
                ExternalMcpEchoSourceErrorV1::NotPublished
            });
        }
        let binding = self
            .binding
            .as_ref()
            .ok_or(ExternalMcpEchoSourceErrorV1::NotPublished)?;
        if !host_grant.matches(binding) {
            return Err(ExternalMcpEchoSourceErrorV1::HostGrantMismatch);
        }
        if !caller_grant.matches(binding) {
            return Err(ExternalMcpEchoSourceErrorV1::CallerGrantMismatch);
        }
        let sequence = self.next_attempt;
        self.next_attempt = self
            .next_attempt
            .checked_add(1)
            .ok_or(ExternalMcpEchoSourceErrorV1::AttemptSpaceExhausted)?;
        self.attempt = Some(ExternalCallAttemptV1 {
            sequence,
            request,
            identity: binding.identity.clone(),
            descriptor_fingerprint: binding.descriptor_fingerprint,
            disposition: ExternalCallDispositionV1::NotSent,
        });

        let outcome = self
            .mcp
            .begin_tools_call(binding.wire_name(), json!({"value": request.value}));
        match outcome {
            Ok(_) => {
                if let Some(attempt) = self.attempt.as_mut() {
                    if matches!(
                        self.mcp.state(),
                        McpSerialChildStateV1::ToolsCallInFlight
                            | McpSerialChildStateV1::Cleaning { .. }
                            | McpSerialChildStateV1::Failed { .. }
                    ) {
                        attempt.disposition = ExternalCallDispositionV1::MayHaveExecuted;
                    }
                }
                self.state = ExternalMcpEchoSourceStateV1::CallInFlight;
                self.synchronize_mcp();
                Ok(&self.state)
            }
            Err(error) => {
                self.fail_primary(ExternalMcpEchoFailureV1::McpOperation(error));
                Ok(&self.state)
            }
        }
    }

    pub(super) fn cancel(
        &mut self,
    ) -> Result<&ExternalMcpEchoSourceStateV1, ExternalMcpEchoSourceErrorV1> {
        self.stop(false)
    }

    pub(super) fn shutdown(
        &mut self,
    ) -> Result<&ExternalMcpEchoSourceStateV1, ExternalMcpEchoSourceErrorV1> {
        self.stop(true)
    }

    pub(super) fn take_result(
        &mut self,
    ) -> Result<FixtureEchoResultV1, ExternalMcpEchoSourceErrorV1> {
        if self.state != ExternalMcpEchoSourceStateV1::Completed {
            return Err(ExternalMcpEchoSourceErrorV1::ResultUnavailable);
        }
        if self.result_taken {
            return Err(ExternalMcpEchoSourceErrorV1::ResultAlreadyTaken);
        }
        let result = self
            .staged_result
            .take()
            .ok_or(ExternalMcpEchoSourceErrorV1::ResultUnavailable)?;
        self.result_taken = true;
        Ok(result)
    }

    fn advance_source(&mut self) {
        if self.state == ExternalMcpEchoSourceStateV1::Starting
            && matches!(self.mcp.state(), McpSerialChildStateV1::ReadyForToolsList)
        {
            match self.mcp.begin_tools_list(None) {
                Ok(_) => self.state = ExternalMcpEchoSourceStateV1::Listing,
                Err(error) => self.fail_primary(ExternalMcpEchoFailureV1::McpOperation(error)),
            }
            self.synchronize_mcp();
            return;
        }

        if self.state == ExternalMcpEchoSourceStateV1::Listing
            && matches!(self.mcp.state(), McpSerialChildStateV1::EventReady)
        {
            match self.mcp.take_event() {
                Ok(ClientEvent::ToolsListed(result)) => self.accept_inventory(result),
                Ok(_) => self.fail_primary(ExternalMcpEchoFailureV1::Invariant(
                    "tools/list completed with the wrong event kind",
                )),
                Err(error) => self.fail_primary(ExternalMcpEchoFailureV1::McpOperation(error)),
            }
            return;
        }

        if self.state == ExternalMcpEchoSourceStateV1::CallInFlight
            && matches!(self.mcp.state(), McpSerialChildStateV1::EventReady)
        {
            match self.mcp.take_event() {
                Ok(ClientEvent::ToolCalled(result)) => self.accept_call_result(result),
                Ok(_) => self.fail_primary(ExternalMcpEchoFailureV1::Invariant(
                    "tools/call completed with the wrong event kind",
                )),
                Err(error) => self.fail_primary(ExternalMcpEchoFailureV1::McpOperation(error)),
            }
        }
    }

    fn accept_inventory(&mut self, result: Value) {
        let descriptor = match parse_exact_inventory_v1(&result) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                self.fail_primary(ExternalMcpEchoFailureV1::Inventory(error));
                return;
            }
        };
        let Some(expected) = self.expected_inventory.as_ref() else {
            self.fail_primary(ExternalMcpEchoFailureV1::Invariant(
                "unpublished source lost its host-authored inventory",
            ));
            return;
        };
        let Some(expected_tool) = expected.tools().first() else {
            self.fail_primary(ExternalMcpEchoFailureV1::Invariant(
                "host-authored fixture inventory lost its exact tool",
            ));
            return;
        };
        if expected_tool.descriptor() != &descriptor {
            self.fail_primary(ExternalMcpEchoFailureV1::Inventory(
                ExternalInventoryObservationErrorV1::DescriptorMismatch,
            ));
            return;
        }
        let Some(inventory) = self.expected_inventory.take() else {
            self.fail_primary(ExternalMcpEchoFailureV1::Invariant(
                "validated inventory was already consumed",
            ));
            return;
        };
        let fence = ExternalInventoryFenceV1::from_inventory(&inventory);
        let Some(tool) = inventory.tools().first() else {
            self.fail_primary(ExternalMcpEchoFailureV1::Invariant(
                "validated fixture inventory has no exact tool",
            ));
            return;
        };
        let binding = ExternalEchoToolBindingV1 {
            identity: tool.identity().clone(),
            descriptor_fingerprint: tool.descriptor_fingerprint(),
            publication_fence: fence,
            wire_name: tool.descriptor().name.clone(),
            descriptor: tool.descriptor().clone(),
        };
        if let Err(error) = self.publication.publish_initial(inventory) {
            self.fail_primary(ExternalMcpEchoFailureV1::Publication(error));
            return;
        }
        self.binding = Some(binding);
        self.state = ExternalMcpEchoSourceStateV1::PublishedReady;
    }

    fn accept_call_result(&mut self, result: Value) {
        let request = match self.attempt.as_ref() {
            Some(attempt) => attempt.request,
            None => {
                self.fail_primary(ExternalMcpEchoFailureV1::Invariant(
                    "tools/call response has no owned logical attempt",
                ));
                return;
            }
        };
        let validated = match parse_exact_echo_result_v1(&result, request) {
            Ok(result) => result,
            Err(error) => {
                self.fail_primary(ExternalMcpEchoFailureV1::Result(error));
                return;
            }
        };
        if let Some(attempt) = self.attempt.as_mut() {
            attempt.disposition = ExternalCallDispositionV1::ResponseValidated;
        }
        self.staged_result = Some(validated);
        if let Err(failure) = self.withdraw() {
            self.fail_primary(failure);
            return;
        }
        self.state = ExternalMcpEchoSourceStateV1::ResultStaged;
        match self.mcp.shutdown() {
            Ok(_) => {
                self.state = ExternalMcpEchoSourceStateV1::ShuttingDown;
                self.synchronize_mcp();
            }
            Err(error) => self.fail_primary(ExternalMcpEchoFailureV1::McpOperation(error)),
        }
    }

    fn stop(
        &mut self,
        shutdown: bool,
    ) -> Result<&ExternalMcpEchoSourceStateV1, ExternalMcpEchoSourceErrorV1> {
        if matches!(
            self.state,
            ExternalMcpEchoSourceStateV1::Completed | ExternalMcpEchoSourceStateV1::Failed { .. }
        ) {
            return Ok(&self.state);
        }
        if let Err(failure) = self.withdraw() {
            self.fail_primary(failure);
            return Ok(&self.state);
        }
        let result = if shutdown {
            self.mcp.shutdown()
        } else {
            self.mcp.cancel()
        };
        result.map_err(ExternalMcpEchoSourceErrorV1::Mcp)?;
        if self.staged_result.is_none() && self.requested_completion.is_none() {
            self.requested_completion = Some(if shutdown {
                ExternalMcpEchoCompletionV1::Shutdown
            } else {
                ExternalMcpEchoCompletionV1::Cancelled
            });
        }
        self.state = if shutdown {
            ExternalMcpEchoSourceStateV1::ShuttingDown
        } else {
            ExternalMcpEchoSourceStateV1::Cancelling
        };
        self.synchronize_mcp();
        Ok(&self.state)
    }

    fn fail_primary(&mut self, failure: ExternalMcpEchoFailureV1) {
        if self.failure.is_none() {
            self.failure = Some(failure);
        }
        if let Err(secondary) = self.withdraw() {
            self.record_withdrawal_failure(secondary);
        }
        self.state = ExternalMcpEchoSourceStateV1::Cleaning;
        if let Err(error) = self.mcp.shutdown() {
            let primary = self
                .failure
                .take()
                .unwrap_or(ExternalMcpEchoFailureV1::Invariant(
                    "cleanup operation failed without a primary failure",
                ));
            self.failure = Some(ExternalMcpEchoFailureV1::CleanupOperation {
                primary: Box::new(primary),
                mcp: Box::new(error),
            });
        }
        self.synchronize_mcp();
    }

    fn withdraw(&mut self) -> Result<(), ExternalMcpEchoFailureV1> {
        let Some(binding) = self.binding.as_ref() else {
            return Ok(());
        };
        self.publication
            .withdraw(&binding.publication_fence)
            .map_err(ExternalMcpEchoFailureV1::Publication)?;
        self.binding.take();
        Ok(())
    }

    fn record_withdrawal_failure(&mut self, secondary: ExternalMcpEchoFailureV1) {
        let ExternalMcpEchoFailureV1::Publication(publication) = secondary else {
            self.failure.get_or_insert(secondary);
            return;
        };
        let primary = self
            .failure
            .take()
            .unwrap_or(ExternalMcpEchoFailureV1::Invariant(
                "withdrawal failed without a primary transition",
            ));
        self.failure = Some(ExternalMcpEchoFailureV1::Withdrawal {
            primary: Box::new(primary),
            publication,
        });
    }

    fn synchronize_mcp(&mut self) {
        match self.mcp.state() {
            McpSerialChildStateV1::Cleaning { .. } => {
                if self.publication.published().is_some() {
                    if let Err(failure) = self.withdraw() {
                        self.record_withdrawal_failure(failure);
                    }
                }
                self.state = ExternalMcpEchoSourceStateV1::Cleaning;
            }
            McpSerialChildStateV1::Failed { .. } => {
                if self.publication.published().is_some() {
                    if let Err(failure) = self.withdraw() {
                        self.record_withdrawal_failure(failure);
                    }
                }
                let Some(mcp) = self.mcp.take_terminal_failure() else {
                    let failure = ExternalMcpEchoFailureV1::Invariant(
                        "failed MCP state lost its terminal failure",
                    );
                    let kind = failure.kind();
                    self.failure = Some(failure);
                    self.state = ExternalMcpEchoSourceStateV1::Failed { kind };
                    return;
                };
                let failure = match self.failure.take() {
                    Some(primary) => ExternalMcpEchoFailureV1::Cleanup {
                        primary: Box::new(primary),
                        mcp: Box::new(mcp),
                    },
                    None => ExternalMcpEchoFailureV1::Mcp(mcp),
                };
                let kind = failure.kind();
                self.failure = Some(failure);
                self.state = ExternalMcpEchoSourceStateV1::Failed { kind };
            }
            McpSerialChildStateV1::Stopped => {
                if self.publication.published().is_some() {
                    if let Err(failure) = self.withdraw() {
                        self.record_withdrawal_failure(failure);
                    }
                }
                if let Some(failure) = self.failure.as_ref() {
                    self.state = ExternalMcpEchoSourceStateV1::Failed {
                        kind: failure.kind(),
                    };
                } else if self.staged_result.is_some() {
                    self.completion = Some(ExternalMcpEchoCompletionV1::EchoValidated);
                    self.state = ExternalMcpEchoSourceStateV1::Completed;
                } else if let Some(completion) = self.requested_completion {
                    self.completion = Some(completion);
                    self.state = ExternalMcpEchoSourceStateV1::Completed;
                } else {
                    let failure = ExternalMcpEchoFailureV1::Invariant(
                        "MCP child stopped without result or host stop request",
                    );
                    let kind = failure.kind();
                    self.failure = Some(failure);
                    self.state = ExternalMcpEchoSourceStateV1::Failed { kind };
                }
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub(super) fn effects_for_test(&mut self) -> &mut E {
        self.mcp.effects_for_test()
    }

    #[cfg(test)]
    pub(super) fn response_frames_for_test(&self) -> &[Vec<u8>] {
        self.mcp.response_frames_for_test()
    }
}

impl<E> Drop for ExternalMcpEchoSourceOwnerV1<'_, E>
where
    E: SerialChildTransportEffectsV1,
{
    fn drop(&mut self) {
        if let Err(failure) = self.withdraw() {
            self.record_withdrawal_failure(failure);
        }
    }
}

fn parse_exact_inventory_v1(
    result: &Value,
) -> Result<ToolDescriptor, ExternalInventoryObservationErrorV1> {
    let object = result
        .as_object()
        .ok_or(ExternalInventoryObservationErrorV1::ResultNotObject)?;
    if object.contains_key("nextCursor") {
        return Err(ExternalInventoryObservationErrorV1::PaginationUnsupported);
    }
    if object.len() != 1 || !object.contains_key("tools") {
        return Err(ExternalInventoryObservationErrorV1::UnexpectedResultFields);
    }
    let tools = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or(ExternalInventoryObservationErrorV1::UnexpectedResultFields)?;
    if tools.len() != 1 {
        return Err(ExternalInventoryObservationErrorV1::ToolCount {
            observed: tools.len(),
        });
    }
    let tool = tools[0]
        .as_object()
        .ok_or(ExternalInventoryObservationErrorV1::ToolNotObject)?;
    if !has_exact_keys(tool, &["description", "inputSchema", "name"]) {
        return Err(ExternalInventoryObservationErrorV1::UnexpectedToolFields);
    }
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .ok_or(ExternalInventoryObservationErrorV1::InvalidToolName)?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .ok_or(ExternalInventoryObservationErrorV1::InvalidToolDescription)?;
    let parameters = tool
        .get("inputSchema")
        .filter(|value| value.is_object())
        .ok_or(ExternalInventoryObservationErrorV1::InvalidToolSchema)?;
    Ok(ToolDescriptor {
        name: name.to_string(),
        description: description.to_string(),
        parameters: parameters.clone(),
    })
}

fn parse_exact_echo_result_v1(
    result: &Value,
    request: FixtureEchoRequestV1,
) -> Result<FixtureEchoResultV1, ExternalEchoResultErrorV1> {
    let object = result
        .as_object()
        .ok_or(ExternalEchoResultErrorV1::ResultNotObject)?;
    if !has_exact_keys(object, &["content", "isError"]) {
        return Err(ExternalEchoResultErrorV1::UnexpectedResultFields);
    }
    if object.get("isError").and_then(Value::as_bool) != Some(false) {
        return Err(ExternalEchoResultErrorV1::RemoteReportedError);
    }
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .ok_or(ExternalEchoResultErrorV1::UnexpectedResultFields)?;
    if content.len() != 1 {
        return Err(ExternalEchoResultErrorV1::ContentCount {
            observed: content.len(),
        });
    }
    let item = content[0]
        .as_object()
        .ok_or(ExternalEchoResultErrorV1::ContentNotObject)?;
    if !has_exact_keys(item, &["text", "type"]) {
        return Err(ExternalEchoResultErrorV1::UnexpectedContentFields);
    }
    if item.get("type").and_then(Value::as_str) != Some("text") {
        return Err(ExternalEchoResultErrorV1::InvalidContentType);
    }
    let text = item
        .get("text")
        .and_then(Value::as_str)
        .ok_or(ExternalEchoResultErrorV1::InvalidContentText)?;
    if text != request.value.to_string() {
        return Err(ExternalEchoResultErrorV1::ValueMismatch);
    }
    Ok(FixtureEchoResultV1 {
        value: request.value,
    })
}

fn has_exact_keys(object: &Map<String, Value>, expected: &[&str]) -> bool {
    object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};

    use kitsu_providers::ToolDescriptor;
    use serde_json::{json, Value};

    use super::{
        CallerExternalToolGrantV1, ExternalCallDispositionV1, ExternalEchoResultErrorV1,
        ExternalMcpEchoCompletionV1, ExternalMcpEchoFailureV1, ExternalMcpEchoSourceOwnerV1,
        ExternalMcpEchoSourceStateV1, FixtureEchoRequestV1, HostExternalToolGrantV1,
        McpSerialChildFailureV1,
    };
    use crate::external::authority::{
        prepare_child_launch_request_v1, AdmittedChildLaunchV1, AuthorityProfileV1,
        ChildEnvironmentPolicyV1, ChildFilesystemAccessV1, ChildFilesystemGrantV1,
        ChildFilesystemPolicyV1, ChildIoLimitsV1, ChildLaunchAuthorityDecisionV1,
        ChildLaunchModeV1, ChildLaunchPreparationV1, ChildLaunchRequestV1, ChildNetworkPolicyV1,
        ChildPhaseBudgetsV1, ChildProcessGroupPolicyV1, ChildRequestPolicyV1, ChildRuntimePolicyV1,
        ChildTimeoutDispositionV1, PreparedChildContainmentTokenV1, PreparedChildLaunchV1,
    };
    use crate::external::identity::{
        validate_external_inventory_v1, ExternalInventoryCandidateV1, ExternalToolCandidateV1,
        EXTERNAL_IDENTITY_CONTRACT_VERSION_V1,
    };
    use crate::external::lifecycle::ChildExitTruthV1;
    use crate::external::serial::{
        SerialChildCauseV1, SerialChildTransportEffectsV1, SerialFrameObservationV1,
        SerialOwnedChildResidueV1, SerialPollAllowanceV1,
    };
    use crate::external::supervisor::{ChildReapPollV1, OwnedChildTokenV1};

    const EXECUTABLE: &str = "/opt/kitsu-fixtures/mcp-peer";
    const WORKING_DIRECTORY: &str = "/tmp/kitsu-external";
    const DIGEST: &str = "abababababababababababababababababababababababababababababababab";

    fn echo_descriptor() -> ToolDescriptor {
        ToolDescriptor {
            name: "fixture.echo".to_string(),
            description: "Returns one integer as text.".to_string(),
            parameters: json!({
                "additionalProperties": false,
                "properties": {"value": {"type": "integer"}},
                "required": ["value"],
                "type": "object"
            }),
        }
    }

    fn runtime_policy() -> ChildRuntimePolicyV1 {
        ChildRuntimePolicyV1::new(
            ChildPhaseBudgetsV1::new(5, 50, 9, 11, 13),
            ChildIoLimitsV1::new(2 * 1024, 2 * 1024, 256, 2 * 1024),
            ChildProcessGroupPolicyV1::Owned,
            ChildEnvironmentPolicyV1::new(true, Vec::new()),
            ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
                WORKING_DIRECTORY,
                ChildFilesystemAccessV1::ReadWrite,
            )]),
            ChildNetworkPolicyV1::Deny,
            ChildTimeoutDispositionV1::TerminateProcessGroupAndReap,
        )
        .with_request_policy(ChildRequestPolicyV1::serial(4, 20))
    }

    #[derive(Default)]
    struct PreparationFixture;

    impl ChildLaunchPreparationV1 for PreparationFixture {
        fn canonicalize_executable(&mut self, executable: &Path) -> Result<PathBuf, String> {
            Ok(executable.to_path_buf())
        }

        fn executable_sha256(&mut self, _: &Path) -> Result<[u8; 32], String> {
            Ok([0xab; 32])
        }

        fn containment_preflight(
            &mut self,
            _: &AdmittedChildLaunchV1,
        ) -> Result<PreparedChildContainmentTokenV1, String> {
            Ok(PreparedChildContainmentTokenV1::new(91))
        }
    }

    fn prepared_launch() -> PreparedChildLaunchV1 {
        let policy = runtime_policy();
        let request = ChildLaunchRequestV1::new(
            1,
            ChildLaunchModeV1::Enabled,
            EXECUTABLE,
            DIGEST,
            vec!["--stdio".to_string()],
            WORKING_DIRECTORY,
            policy.clone(),
        );
        let authority = AuthorityProfileV1::exact(
            EXECUTABLE,
            DIGEST,
            vec!["--stdio".to_string()],
            WORKING_DIRECTORY,
            policy,
        )
        .unwrap();
        prepare_child_launch_request_v1(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&authority),
            &mut PreparationFixture,
        )
        .unwrap()
    }

    fn initialize_response() -> Value {
        json!({
            "id": 1,
            "jsonrpc": "2.0",
            "result": {
                "protocolVersion": crate::mcp_codec::MCP_PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "fixture", "version": "1"}
            }
        })
    }

    fn list_response(result: Value) -> Value {
        json!({"id": 2, "jsonrpc": "2.0", "result": result})
    }

    fn exact_list_result() -> Value {
        json!({
            "tools": [{
                "description": "Returns one integer as text.",
                "inputSchema": {
                    "additionalProperties": false,
                    "properties": {"value": {"type": "integer"}},
                    "required": ["value"],
                    "type": "object"
                },
                "name": "fixture.echo"
            }]
        })
    }

    fn call_response(value: Value) -> Value {
        json!({"id": 3, "jsonrpc": "2.0", "result": value})
    }

    fn line(value: Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        bytes
    }

    struct EffectFixture {
        now: u64,
        responses: VecDeque<Vec<u8>>,
        current_frame_bytes: usize,
        current_notification: bool,
        frames: Vec<Vec<u8>>,
        drains: Vec<SerialChildCauseV1>,
        spawns: usize,
        retained_launches: Vec<PreparedChildLaunchV1>,
        retained_residue: Vec<SerialOwnedChildResidueV1>,
        reap_pending: bool,
    }

    impl EffectFixture {
        fn exact(call_result: Value) -> Self {
            Self {
                now: 0,
                responses: VecDeque::from([
                    line(initialize_response()),
                    line(list_response(exact_list_result())),
                    line(call_response(call_result)),
                ]),
                current_frame_bytes: 0,
                current_notification: false,
                frames: Vec::new(),
                drains: Vec::new(),
                spawns: 0,
                retained_launches: Vec::new(),
                retained_residue: Vec::new(),
                reap_pending: false,
            }
        }

        fn with_list(list: Value) -> Self {
            let mut fixture = Self::exact(json!({
                "content": [{"text": "7", "type": "text"}],
                "isError": false
            }));
            fixture.responses[1] = line(list_response(list));
            fixture
        }

        fn call_frames(&self) -> usize {
            self.frames
                .iter()
                .filter(|frame| {
                    serde_json::from_slice::<Value>(&frame[..frame.len() - 1])
                        .ok()
                        .and_then(|value| value.get("method").cloned())
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .as_deref()
                        == Some("tools/call")
                })
                .count()
        }
    }

    impl SerialChildTransportEffectsV1 for EffectFixture {
        fn monotonic_millis(&mut self) -> Result<u64, String> {
            let now = self.now;
            self.now += 1;
            Ok(now)
        }

        fn spawn_serial(&mut self, _: PreparedChildLaunchV1) -> Result<OwnedChildTokenV1, String> {
            self.spawns += 1;
            Ok(OwnedChildTokenV1::new(37))
        }

        fn startup_ready(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            Ok(true)
        }

        fn begin_frame(&mut self, _: &OwnedChildTokenV1, frame: &[u8]) -> Result<(), String> {
            self.current_frame_bytes = frame.len();
            self.current_notification = serde_json::from_slice::<Value>(&frame[..frame.len() - 1])
                .ok()
                .and_then(|value| value.get("method").cloned())
                .and_then(|value| value.as_str().map(str::to_owned))
                .as_deref()
                == Some("notifications/initialized");
            self.frames.push(frame.to_vec());
            Ok(())
        }

        fn poll_frame(
            &mut self,
            _: &OwnedChildTokenV1,
            _: SerialPollAllowanceV1,
        ) -> Result<SerialFrameObservationV1, String> {
            if self.current_notification {
                self.current_notification = false;
                return Ok(SerialFrameObservationV1::progress(
                    self.current_frame_bytes,
                    0,
                    0,
                ));
            }
            let response = self.responses.pop_front().expect("one fixture response");
            Ok(SerialFrameObservationV1::response(
                response.clone(),
                self.current_frame_bytes,
                response.len(),
                0,
            ))
        }

        fn begin_drain(
            &mut self,
            _: &OwnedChildTokenV1,
            cause: SerialChildCauseV1,
        ) -> Result<(), String> {
            self.drains.push(cause);
            Ok(())
        }

        fn drain_complete(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            Ok(true)
        }

        fn terminate_process_group(&mut self, _: &OwnedChildTokenV1) -> Result<(), String> {
            Ok(())
        }

        fn process_group_terminated(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            Ok(true)
        }

        fn poll_reap(&mut self, child: OwnedChildTokenV1) -> ChildReapPollV1 {
            if self.reap_pending {
                ChildReapPollV1::Pending(child)
            } else {
                ChildReapPollV1::Reaped(ChildExitTruthV1::Exited(0))
            }
        }

        fn retain_unstarted_launch(&mut self, launch: PreparedChildLaunchV1) {
            self.retained_launches.push(launch);
        }

        fn retain_owned_residue(&mut self, residue: SerialOwnedChildResidueV1) {
            self.retained_residue.push(residue);
        }
    }

    fn drive_until(
        owner: &mut ExternalMcpEchoSourceOwnerV1<'_, EffectFixture>,
        expected: fn(&ExternalMcpEchoSourceStateV1) -> bool,
    ) {
        for _ in 0..128 {
            if expected(owner.state()) {
                return;
            }
            owner.poll().unwrap();
        }
        panic!(
            "composite did not reach expected state: {:?}",
            owner.state()
        );
    }

    fn start_published(
        fixture: &mut EffectFixture,
    ) -> ExternalMcpEchoSourceOwnerV1<'_, EffectFixture> {
        let mut owner = ExternalMcpEchoSourceOwnerV1::new(
            prepared_launch(),
            fixture,
            "fixture-source",
            "boot-1",
            1,
            "echo",
        )
        .unwrap();
        owner.start().unwrap();
        drive_until(&mut owner, |state| {
            matches!(state, ExternalMcpEchoSourceStateV1::PublishedReady)
        });
        owner
    }

    #[test]
    fn exact_inventory_identity_grants_call_and_withdrawal_form_one_linear_success() {
        let mut fixture = EffectFixture::exact(json!({
            "content": [{"text": "7", "type": "text"}],
            "isError": false
        }));
        let mut owner = start_published(&mut fixture);
        assert_eq!(owner.publication().unwrap().tools().len(), 1);
        assert_eq!(owner.binding().unwrap().wire_name(), "fixture.echo");
        assert_eq!(owner.binding().unwrap().descriptor(), &echo_descriptor());
        let expected_identity = owner.binding().unwrap().identity().clone();
        let expected_fingerprint = owner.binding().unwrap().descriptor_fingerprint();
        let host = HostExternalToolGrantV1::from_binding(owner.binding().unwrap());
        let caller = CallerExternalToolGrantV1::from_binding(owner.binding().unwrap());

        owner
            .begin_echo(FixtureEchoRequestV1::new(7), &host, &caller)
            .unwrap();
        assert_eq!(
            owner.attempt().unwrap().disposition(),
            ExternalCallDispositionV1::MayHaveExecuted
        );
        assert_eq!(owner.attempt().unwrap().sequence(), 1);
        assert_eq!(
            owner.attempt().unwrap().request(),
            FixtureEchoRequestV1::new(7)
        );
        assert_eq!(owner.attempt().unwrap().identity(), &expected_identity);
        assert_eq!(
            owner.attempt().unwrap().descriptor_fingerprint(),
            expected_fingerprint
        );
        drive_until(&mut owner, |state| {
            matches!(state, ExternalMcpEchoSourceStateV1::Completed)
        });

        assert!(owner.publication().is_none());
        assert!(owner.binding().is_none());
        assert_eq!(
            owner.completion(),
            Some(ExternalMcpEchoCompletionV1::EchoValidated)
        );
        assert_eq!(owner.take_result().unwrap().value(), 7);
        assert_eq!(owner.effects_for_test().spawns, 1);
        assert_eq!(owner.effects_for_test().frames.len(), 4);
        assert_eq!(owner.effects_for_test().call_frames(), 1);
        assert_eq!(
            owner.effects_for_test().drains,
            vec![SerialChildCauseV1::Shutdown]
        );
        assert!(owner.effects_for_test().retained_residue.is_empty());
    }

    #[test]
    fn invalid_host_identity_retains_the_unstarted_prepared_launch() {
        let mut fixture = EffectFixture::exact(json!({
            "content": [{"text": "7", "type": "text"}],
            "isError": false
        }));
        {
            let result = ExternalMcpEchoSourceOwnerV1::new(
                prepared_launch(),
                &mut fixture,
                "INVALID SOURCE",
                "boot-1",
                1,
                "echo",
            );
            match result {
                Err(super::ExternalMcpEchoConstructionErrorV1::Identity(_)) => {}
                Err(_) => panic!("invalid host identity returned the wrong construction error"),
                Ok(owner) => {
                    drop(owner);
                    panic!("invalid host identity unexpectedly constructed an owner");
                }
            }
        }
        assert_eq!(fixture.retained_launches.len(), 1);
        assert_eq!(fixture.spawns, 0);
        assert!(fixture.frames.is_empty());
    }

    #[test]
    fn invalid_or_non_exact_list_truth_never_partially_publishes_or_calls() {
        let invalid = [
            json!({"tools": [], "nextCursor": "later"}),
            json!({"tools": []}),
            json!({"tools": [
                {"name": "fixture.echo", "description": "Returns one integer as text.", "inputSchema": echo_descriptor().parameters},
                {"name": "extra", "description": "extra", "inputSchema": {"type": "object"}}
            ]}),
            json!({"tools": [{
                "name": "fixture.echo",
                "description": "changed",
                "inputSchema": echo_descriptor().parameters
            }]}),
            json!({"tools": [{
                "name": "fixture.echo",
                "description": "Returns one integer as text.",
                "inputSchema": echo_descriptor().parameters,
                "extra": true
            }]}),
        ];

        for list in invalid {
            let mut fixture = EffectFixture::with_list(list);
            let mut owner = ExternalMcpEchoSourceOwnerV1::new(
                prepared_launch(),
                &mut fixture,
                "fixture-source",
                "boot-1",
                1,
                "echo",
            )
            .unwrap();
            owner.start().unwrap();
            drive_until(&mut owner, |state| {
                matches!(state, ExternalMcpEchoSourceStateV1::Failed { .. })
            });
            assert!(owner.publication().is_none());
            assert!(owner.binding().is_none());
            assert!(owner.take_result().is_err());
            assert_eq!(owner.effects_for_test().call_frames(), 0);
        }
    }

    fn grant_pair(
        source: &str,
        incarnation: &str,
        revision: u64,
        descriptor: ToolDescriptor,
    ) -> (HostExternalToolGrantV1, CallerExternalToolGrantV1) {
        let inventory = validate_external_inventory_v1(ExternalInventoryCandidateV1::new(
            EXTERNAL_IDENTITY_CONTRACT_VERSION_V1,
            source,
            incarnation,
            revision,
            vec![ExternalToolCandidateV1::new("echo", descriptor)],
        ))
        .unwrap();
        let tool = &inventory.tools()[0];
        (
            HostExternalToolGrantV1::from_exact(
                tool.identity().clone(),
                tool.descriptor_fingerprint(),
            ),
            CallerExternalToolGrantV1::from_exact(
                tool.identity().clone(),
                tool.descriptor_fingerprint(),
            ),
        )
    }

    #[test]
    fn name_only_stale_or_denied_grants_refuse_before_call_frame() {
        let mut fixture = EffectFixture::exact(json!({
            "content": [{"text": "7", "type": "text"}],
            "isError": false
        }));
        let mut owner = start_published(&mut fixture);
        let before = owner.effects_for_test().frames.len();
        let mut changed = echo_descriptor();
        changed.description = "same name, changed descriptor".to_string();
        for (wrong_host, wrong_caller) in [
            grant_pair("fixture-source", "stale-boot", 1, echo_descriptor()),
            grant_pair("fixture-source", "boot-1", 2, echo_descriptor()),
            grant_pair("fixture-source", "boot-1", 1, changed),
        ] {
            let exact_host = HostExternalToolGrantV1::from_binding(owner.binding().unwrap());
            let exact_caller = CallerExternalToolGrantV1::from_binding(owner.binding().unwrap());
            assert!(owner
                .begin_echo(FixtureEchoRequestV1::new(7), &wrong_host, &exact_caller)
                .is_err());
            assert!(owner
                .begin_echo(FixtureEchoRequestV1::new(7), &exact_host, &wrong_caller)
                .is_err());
        }
        assert_eq!(owner.effects_for_test().frames.len(), before);
        assert!(matches!(
            owner.state(),
            ExternalMcpEchoSourceStateV1::PublishedReady
        ));
    }

    #[test]
    fn mismatched_echo_result_withdraws_unknown_attempt_and_never_replays() {
        let mut fixture = EffectFixture::exact(json!({
            "content": [{"text": "8", "type": "text"}],
            "isError": false
        }));
        let mut owner = start_published(&mut fixture);
        let host = HostExternalToolGrantV1::from_binding(owner.binding().unwrap());
        let caller = CallerExternalToolGrantV1::from_binding(owner.binding().unwrap());
        owner
            .begin_echo(FixtureEchoRequestV1::new(7), &host, &caller)
            .unwrap();
        drive_until(&mut owner, |state| {
            matches!(state, ExternalMcpEchoSourceStateV1::Failed { .. })
        });

        assert!(owner.publication().is_none());
        assert!(owner.binding().is_none());
        assert_eq!(
            owner.attempt().unwrap().disposition(),
            ExternalCallDispositionV1::MayHaveExecuted
        );
        assert!(owner.take_result().is_err());
        assert_eq!(owner.effects_for_test().call_frames(), 1);
        assert!(owner
            .begin_echo(FixtureEchoRequestV1::new(7), &host, &caller)
            .is_err());
        assert_eq!(owner.effects_for_test().call_frames(), 1);
    }

    #[test]
    fn peer_wrong_id_withdraws_publication_and_preserves_exact_mcp_failure() {
        let mut fixture = EffectFixture::exact(json!({
            "content": [{"text": "7", "type": "text"}],
            "isError": false
        }));
        fixture.responses[2] = line(json!({
            "id": 99,
            "jsonrpc": "2.0",
            "result": {
                "content": [{"text": "7", "type": "text"}],
                "isError": false
            }
        }));
        let mut owner = start_published(&mut fixture);
        let host = HostExternalToolGrantV1::from_binding(owner.binding().unwrap());
        let caller = CallerExternalToolGrantV1::from_binding(owner.binding().unwrap());
        owner
            .begin_echo(FixtureEchoRequestV1::new(7), &host, &caller)
            .unwrap();
        drive_until(&mut owner, |state| {
            matches!(state, ExternalMcpEchoSourceStateV1::Failed { .. })
        });

        assert!(owner.publication().is_none());
        assert!(owner.binding().is_none());
        assert!(
            matches!(
                owner.failure(),
                Some(ExternalMcpEchoFailureV1::Mcp(
                    McpSerialChildFailureV1::Cleanup {
                        primary,
                        transport: crate::external::serial::SerialChildTransportFailureV1::Transport {
                            cause: SerialChildCauseV1::ProtocolViolation,
                            exit: ChildExitTruthV1::Exited(0)
                        }
                    }
                )) if matches!(
                    primary.as_ref(),
                    McpSerialChildFailureV1::Client(
                        crate::mcp_codec::ClientError::UnexpectedResponseId {
                            expected: Some(3),
                            actual: 99
                        }
                    )
                )
            ),
            "unexpected preserved failure: {:?}",
            owner.failure()
        );
        assert_eq!(owner.effects_for_test().call_frames(), 1);
        assert!(owner.take_result().is_err());
    }

    #[test]
    fn cancel_after_publication_withdraws_once_and_never_exposes_a_result() {
        let mut fixture = EffectFixture::exact(json!({
            "content": [{"text": "7", "type": "text"}],
            "isError": false
        }));
        let mut owner = start_published(&mut fixture);
        owner.cancel().unwrap();
        drive_until(&mut owner, |state| {
            matches!(state, ExternalMcpEchoSourceStateV1::Completed)
        });

        assert!(owner.publication().is_none());
        assert!(owner.binding().is_none());
        assert_eq!(
            owner.completion(),
            Some(ExternalMcpEchoCompletionV1::Cancelled)
        );
        assert!(owner.take_result().is_err());
        assert_eq!(owner.effects_for_test().call_frames(), 0);
        assert_eq!(
            owner.effects_for_test().drains,
            vec![SerialChildCauseV1::Cancel]
        );
    }

    #[test]
    fn cleanup_deadline_keeps_primary_result_failure_and_one_owned_residue() {
        let mut fixture = EffectFixture::exact(json!({
            "content": [{"text": "8", "type": "text"}],
            "isError": false
        }));
        fixture.reap_pending = true;
        let mut owner = start_published(&mut fixture);
        let host = HostExternalToolGrantV1::from_binding(owner.binding().unwrap());
        let caller = CallerExternalToolGrantV1::from_binding(owner.binding().unwrap());
        owner
            .begin_echo(FixtureEchoRequestV1::new(7), &host, &caller)
            .unwrap();
        drive_until(&mut owner, |state| {
            matches!(state, ExternalMcpEchoSourceStateV1::Failed { .. })
        });

        assert!(owner.publication().is_none());
        assert!(owner.binding().is_none());
        assert!(matches!(
            owner.failure(),
            Some(ExternalMcpEchoFailureV1::Cleanup {
                primary,
                mcp
            }) if matches!(primary.as_ref(), ExternalMcpEchoFailureV1::Result(
                ExternalEchoResultErrorV1::ValueMismatch
            )) && matches!(mcp.as_ref(), McpSerialChildFailureV1::Transport(
                crate::external::serial::SerialChildTransportFailureV1::ReapDeadline {
                    cause: SerialChildCauseV1::Shutdown
                }
            ))
        ));
        assert_eq!(owner.effects_for_test().retained_residue.len(), 1);
        assert!(owner.take_result().is_err());
    }

    #[test]
    fn validated_response_stays_evidence_when_reap_cleanup_cannot_finish() {
        let mut fixture = EffectFixture::exact(json!({
            "content": [{"text": "7", "type": "text"}],
            "isError": false
        }));
        fixture.reap_pending = true;
        let mut owner = start_published(&mut fixture);
        let host = HostExternalToolGrantV1::from_binding(owner.binding().unwrap());
        let caller = CallerExternalToolGrantV1::from_binding(owner.binding().unwrap());
        owner
            .begin_echo(FixtureEchoRequestV1::new(7), &host, &caller)
            .unwrap();
        drive_until(&mut owner, |state| {
            matches!(state, ExternalMcpEchoSourceStateV1::Failed { .. })
        });

        assert!(owner.publication().is_none());
        assert!(owner.binding().is_none());
        assert_eq!(
            owner.attempt().unwrap().disposition(),
            ExternalCallDispositionV1::ResponseValidated
        );
        assert_eq!(owner.staged_result_evidence().unwrap().value(), 7);
        assert!(owner.take_result().is_err());
        assert_eq!(owner.effects_for_test().retained_residue.len(), 1);
        assert!(matches!(
            owner.failure(),
            Some(ExternalMcpEchoFailureV1::Mcp(
                McpSerialChildFailureV1::Transport(
                    crate::external::serial::SerialChildTransportFailureV1::ReapDeadline {
                        cause: SerialChildCauseV1::Shutdown
                    }
                )
            ))
        ));
    }
}
