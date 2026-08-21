//! Private composition of the pinned tools-only MCP codec and one bounded
//! serial child owner. This module is deliberately unmounted from product and
//! inventory routes.

#![allow(dead_code)]

use serde_json::Value;

use super::authority::PreparedChildLaunchV1;
use super::serial::{
    SerialChildCauseV1, SerialChildTransportEffectsV1, SerialChildTransportErrorV1,
    SerialChildTransportFailureV1, SerialChildTransportStateV1, SerialChildTransportV1,
};
use crate::mcp_codec::{
    encode_line, ClientError, ClientEvent, CodecError, LineDecoder, OutboundMessage,
    ToolsOnlyClient,
};

const MINIMUM_MCP_FRAMES_V1: u16 = 4;
const MCP_CLIENT_NAME_V1: &str = "kitsu-tools";
const MCP_CLIENT_VERSION_V1: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, PartialEq, Eq)]
pub(super) enum McpSerialChildConstructionErrorV1 {
    InvalidPayloadCapacity,
    InsufficientFrameCapacity { required: u16, authorized: u16 },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum McpSerialChildFailureV1 {
    Codec(CodecError),
    Client(ClientError),
    TransportCause(SerialChildCauseV1),
    Transport(SerialChildTransportFailureV1),
    TransportOperation(SerialChildTransportErrorV1),
    Cleanup {
        primary: Box<McpSerialChildFailureV1>,
        transport: SerialChildTransportFailureV1,
    },
    Invariant(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum McpSerialChildStateV1 {
    Ready,
    Starting,
    InitializeInFlight,
    InitializedNotificationInFlight,
    ReadyForToolsList,
    ToolsListInFlight,
    ReadyForToolsCall,
    ToolsCallInFlight,
    EventReady,
    Exhausted,
    Cancelling,
    ShuttingDown,
    Cleaning { failure: McpSerialChildFailureV1 },
    Stopped,
    Failed { failure: McpSerialChildFailureV1 },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum McpSerialChildErrorV1 {
    StartAlreadyAttempted,
    ToolsListRequired,
    OperationInFlight,
    EventNotConsumed,
    NoEventReady,
    FrameCapacityExhausted,
    NoActiveOperation,
    Client(ClientError),
    Transport(SerialChildTransportErrorV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingResponseKindV1 {
    Initialize,
    ToolsList,
    ToolsCall,
}

pub(super) struct McpSerialChildOwnerV1<'effects, E: SerialChildTransportEffectsV1> {
    transport: SerialChildTransportV1<'effects, E>,
    client: ToolsOnlyClient,
    decoder: LineDecoder,
    payload_limit: usize,
    state: McpSerialChildStateV1,
    event: Option<ClientEvent>,
}

impl<'effects, E> McpSerialChildOwnerV1<'effects, E>
where
    E: SerialChildTransportEffectsV1,
{
    pub(super) fn new(
        prepared: PreparedChildLaunchV1,
        effects: &'effects mut E,
    ) -> Result<Self, McpSerialChildConstructionErrorV1> {
        let transport = SerialChildTransportV1::new(prepared, effects);
        let payload_limit = transport.line_payload_limit();
        let authorized_frames = transport.maximum_frames();
        if payload_limit == 0 {
            return Err(McpSerialChildConstructionErrorV1::InvalidPayloadCapacity);
        }
        if authorized_frames < MINIMUM_MCP_FRAMES_V1 {
            return Err(
                McpSerialChildConstructionErrorV1::InsufficientFrameCapacity {
                    required: MINIMUM_MCP_FRAMES_V1,
                    authorized: authorized_frames,
                },
            );
        }
        let decoder = LineDecoder::new(payload_limit)
            .map_err(|_| McpSerialChildConstructionErrorV1::InvalidPayloadCapacity)?;
        Ok(Self {
            transport,
            client: ToolsOnlyClient::new(),
            decoder,
            payload_limit,
            state: McpSerialChildStateV1::Ready,
            event: None,
        })
    }

    pub(super) fn state(&self) -> &McpSerialChildStateV1 {
        &self.state
    }

    pub(super) fn start(&mut self) -> Result<&McpSerialChildStateV1, McpSerialChildErrorV1> {
        if self.state != McpSerialChildStateV1::Ready {
            return Err(McpSerialChildErrorV1::StartAlreadyAttempted);
        }
        self.transport
            .start()
            .map_err(McpSerialChildErrorV1::Transport)?;
        self.state = McpSerialChildStateV1::Starting;
        self.apply_transport_truth();
        Ok(&self.state)
    }

    pub(super) fn poll(&mut self) -> Result<&McpSerialChildStateV1, McpSerialChildErrorV1> {
        match self.transport.state() {
            SerialChildTransportStateV1::Starting { .. }
            | SerialChildTransportStateV1::FrameInFlight { .. }
            | SerialChildTransportStateV1::NotificationInFlight { .. }
            | SerialChildTransportStateV1::Draining { .. }
            | SerialChildTransportStateV1::Killing { .. }
            | SerialChildTransportStateV1::Reaping { .. } => {
                if let Err(error) = self.transport.poll() {
                    self.fail_protocol(McpSerialChildFailureV1::TransportOperation(error));
                    return Ok(&self.state);
                }
            }
            SerialChildTransportStateV1::Idle { .. }
            | SerialChildTransportStateV1::ResponseReady { .. }
            | SerialChildTransportStateV1::Exhausted { .. }
            | SerialChildTransportStateV1::StoppedBeforeSpawn(_)
            | SerialChildTransportStateV1::Stopped { .. }
            | SerialChildTransportStateV1::Failed(_) => {}
            SerialChildTransportStateV1::Ready => {
                return Err(McpSerialChildErrorV1::NoActiveOperation)
            }
        }
        self.apply_transport_truth();
        self.advance_protocol();
        Ok(&self.state)
    }

    pub(super) fn begin_tools_list(
        &mut self,
        cursor: Option<String>,
    ) -> Result<&McpSerialChildStateV1, McpSerialChildErrorV1> {
        match self.state {
            McpSerialChildStateV1::ReadyForToolsList => {}
            McpSerialChildStateV1::EventReady => {
                return Err(McpSerialChildErrorV1::EventNotConsumed)
            }
            McpSerialChildStateV1::Starting
            | McpSerialChildStateV1::InitializeInFlight
            | McpSerialChildStateV1::InitializedNotificationInFlight
            | McpSerialChildStateV1::ToolsListInFlight
            | McpSerialChildStateV1::ToolsCallInFlight => {
                return Err(McpSerialChildErrorV1::OperationInFlight)
            }
            McpSerialChildStateV1::Exhausted => {
                return Err(McpSerialChildErrorV1::FrameCapacityExhausted)
            }
            _ => return Err(McpSerialChildErrorV1::NoActiveOperation),
        }
        let outbound = match self.client.start_tools_list(cursor) {
            Ok(outbound) => outbound,
            Err(error) => {
                self.fail_protocol(McpSerialChildFailureV1::Client(error));
                return Ok(&self.state);
            }
        };
        self.begin_request(outbound, McpSerialChildStateV1::ToolsListInFlight);
        Ok(&self.state)
    }

    pub(super) fn begin_tools_call(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<&McpSerialChildStateV1, McpSerialChildErrorV1> {
        match self.state {
            McpSerialChildStateV1::ReadyForToolsCall => {}
            McpSerialChildStateV1::Ready | McpSerialChildStateV1::ReadyForToolsList => {
                return Err(McpSerialChildErrorV1::ToolsListRequired)
            }
            McpSerialChildStateV1::EventReady => {
                return Err(McpSerialChildErrorV1::EventNotConsumed)
            }
            McpSerialChildStateV1::Starting
            | McpSerialChildStateV1::InitializeInFlight
            | McpSerialChildStateV1::InitializedNotificationInFlight
            | McpSerialChildStateV1::ToolsListInFlight
            | McpSerialChildStateV1::ToolsCallInFlight => {
                return Err(McpSerialChildErrorV1::OperationInFlight)
            }
            McpSerialChildStateV1::Exhausted => {
                return Err(McpSerialChildErrorV1::FrameCapacityExhausted)
            }
            _ => return Err(McpSerialChildErrorV1::NoActiveOperation),
        }
        let outbound = match self.client.start_tools_call(name, arguments) {
            Ok(outbound) => outbound,
            Err(error) => {
                self.fail_protocol(McpSerialChildFailureV1::Client(error));
                return Ok(&self.state);
            }
        };
        self.begin_request(outbound, McpSerialChildStateV1::ToolsCallInFlight);
        Ok(&self.state)
    }

    pub(super) fn take_event(&mut self) -> Result<ClientEvent, McpSerialChildErrorV1> {
        if self.state != McpSerialChildStateV1::EventReady {
            return Err(McpSerialChildErrorV1::NoEventReady);
        }
        let event = self
            .event
            .take()
            .expect("event-ready MCP owner owns exactly one codec event");
        self.state = if matches!(
            self.transport.state(),
            SerialChildTransportStateV1::Exhausted { .. }
        ) {
            McpSerialChildStateV1::Exhausted
        } else {
            McpSerialChildStateV1::ReadyForToolsCall
        };
        Ok(event)
    }

    pub(super) fn cancel(&mut self) -> Result<&McpSerialChildStateV1, McpSerialChildErrorV1> {
        self.request_stop(false)
    }

    pub(super) fn shutdown(&mut self) -> Result<&McpSerialChildStateV1, McpSerialChildErrorV1> {
        self.request_stop(true)
    }

    fn request_stop(
        &mut self,
        shutdown: bool,
    ) -> Result<&McpSerialChildStateV1, McpSerialChildErrorV1> {
        if matches!(
            self.state,
            McpSerialChildStateV1::Stopped | McpSerialChildStateV1::Failed { .. }
        ) {
            return Ok(&self.state);
        }
        let preserve_failure = matches!(self.state, McpSerialChildStateV1::Cleaning { .. });
        let result = if shutdown {
            self.transport.shutdown()
        } else {
            self.transport.cancel()
        };
        result.map_err(McpSerialChildErrorV1::Transport)?;
        self.event.take();
        if !preserve_failure {
            self.state = if matches!(
                self.transport.state(),
                SerialChildTransportStateV1::StoppedBeforeSpawn(_)
            ) {
                McpSerialChildStateV1::Stopped
            } else if shutdown {
                McpSerialChildStateV1::ShuttingDown
            } else {
                McpSerialChildStateV1::Cancelling
            };
        }
        self.apply_transport_truth();
        Ok(&self.state)
    }

    fn advance_protocol(&mut self) {
        if matches!(
            self.state,
            McpSerialChildStateV1::Cleaning { .. }
                | McpSerialChildStateV1::Cancelling
                | McpSerialChildStateV1::ShuttingDown
                | McpSerialChildStateV1::Stopped
                | McpSerialChildStateV1::Failed { .. }
        ) {
            return;
        }

        if self.state == McpSerialChildStateV1::Starting
            && matches!(
                self.transport.state(),
                SerialChildTransportStateV1::Idle { .. }
            )
        {
            let outbound = match self
                .client
                .start_initialize(MCP_CLIENT_NAME_V1, MCP_CLIENT_VERSION_V1)
            {
                Ok(outbound) => outbound,
                Err(error) => {
                    self.fail_protocol(McpSerialChildFailureV1::Client(error));
                    return;
                }
            };
            self.begin_request(outbound, McpSerialChildStateV1::InitializeInFlight);
            return;
        }

        if self.state == McpSerialChildStateV1::InitializeInFlight
            && matches!(
                self.transport.state(),
                SerialChildTransportStateV1::ResponseReady { .. }
            )
        {
            self.receive_response(PendingResponseKindV1::Initialize);
        } else if self.state == McpSerialChildStateV1::InitializedNotificationInFlight
            && matches!(
                self.transport.state(),
                SerialChildTransportStateV1::Idle { .. }
            )
        {
            self.state = McpSerialChildStateV1::ReadyForToolsList;
        } else if self.state == McpSerialChildStateV1::ToolsListInFlight
            && matches!(
                self.transport.state(),
                SerialChildTransportStateV1::ResponseReady { .. }
            )
        {
            self.receive_response(PendingResponseKindV1::ToolsList);
        } else if self.state == McpSerialChildStateV1::ToolsCallInFlight
            && matches!(
                self.transport.state(),
                SerialChildTransportStateV1::ResponseReady { .. }
            )
        {
            self.receive_response(PendingResponseKindV1::ToolsCall);
        }
    }

    fn begin_request(&mut self, outbound: OutboundMessage, next: McpSerialChildStateV1) {
        if outbound.id().is_none() {
            self.fail_protocol(McpSerialChildFailureV1::Invariant(
                "MCP request did not own a request ID",
            ));
            return;
        }
        let frame = match encode_line(outbound.message(), self.payload_limit) {
            Ok(frame) => frame,
            Err(error) => {
                self.fail_protocol(McpSerialChildFailureV1::Codec(error));
                return;
            }
        };
        if let Err(error) = self.transport.begin_frame(frame) {
            self.fail_protocol(McpSerialChildFailureV1::TransportOperation(error));
            return;
        }
        self.apply_transport_truth();
        if matches!(
            self.transport.state(),
            SerialChildTransportStateV1::FrameInFlight { .. }
        ) {
            self.state = next;
        } else if !matches!(
            self.state,
            McpSerialChildStateV1::Cleaning { .. } | McpSerialChildStateV1::Failed { .. }
        ) {
            self.fail_protocol(McpSerialChildFailureV1::Invariant(
                "serial transport did not retain the MCP request frame",
            ));
        }
    }

    fn begin_initialized_notification(&mut self) {
        let outbound = match self.client.finish_initialize() {
            Ok(outbound) => outbound,
            Err(error) => {
                self.fail_protocol(McpSerialChildFailureV1::Client(error));
                return;
            }
        };
        if outbound.id().is_some() {
            self.fail_protocol(McpSerialChildFailureV1::Invariant(
                "MCP initialized notification unexpectedly owned an ID",
            ));
            return;
        }
        let frame = match encode_line(outbound.message(), self.payload_limit) {
            Ok(frame) => frame,
            Err(error) => {
                self.fail_protocol(McpSerialChildFailureV1::Codec(error));
                return;
            }
        };
        if let Err(error) = self.transport.begin_notification_frame(frame) {
            self.fail_protocol(McpSerialChildFailureV1::TransportOperation(error));
            return;
        }
        self.apply_transport_truth();
        if matches!(
            self.transport.state(),
            SerialChildTransportStateV1::NotificationInFlight { .. }
        ) {
            self.state = McpSerialChildStateV1::InitializedNotificationInFlight;
        } else if !matches!(
            self.state,
            McpSerialChildStateV1::Cleaning { .. } | McpSerialChildStateV1::Failed { .. }
        ) {
            self.fail_protocol(McpSerialChildFailureV1::Invariant(
                "serial transport did not retain the MCP notification frame",
            ));
        }
    }

    fn receive_response(&mut self, expected: PendingResponseKindV1) {
        let response = match self.transport.take_response() {
            Ok(response) => response,
            Err(error) => {
                self.fail_protocol(McpSerialChildFailureV1::TransportOperation(error));
                return;
            }
        };
        let mut messages = match self.decoder.push(&response) {
            Ok(messages) => messages,
            Err(error) => {
                self.fail_protocol(McpSerialChildFailureV1::Codec(error));
                return;
            }
        };
        if messages.len() != 1 {
            self.fail_protocol(McpSerialChildFailureV1::Invariant(
                "one serial response must decode to exactly one MCP message",
            ));
            return;
        }
        let event = match self.client.receive(messages.remove(0)) {
            Ok(event) => event,
            Err(error) => {
                self.fail_protocol(McpSerialChildFailureV1::Client(error));
                return;
            }
        };
        let event_matches = matches!(
            (expected, &event),
            (
                PendingResponseKindV1::Initialize,
                ClientEvent::Initialized(_)
            ) | (
                PendingResponseKindV1::ToolsList,
                ClientEvent::ToolsListed(_)
            ) | (PendingResponseKindV1::ToolsCall, ClientEvent::ToolCalled(_))
        );
        if !event_matches {
            self.fail_protocol(McpSerialChildFailureV1::Invariant(
                "MCP codec emitted an event for the wrong pending operation",
            ));
            return;
        }
        if expected == PendingResponseKindV1::Initialize {
            self.begin_initialized_notification();
        } else {
            self.event = Some(event);
            self.state = McpSerialChildStateV1::EventReady;
        }
    }

    fn fail_protocol(&mut self, failure: McpSerialChildFailureV1) {
        self.event.take();
        let _ = self.transport.abort_protocol();
        self.state = McpSerialChildStateV1::Cleaning { failure };
        self.apply_transport_truth();
    }

    fn apply_transport_truth(&mut self) {
        let transport_state = self.transport.state().clone();
        match transport_state {
            SerialChildTransportStateV1::Draining { cause, .. }
            | SerialChildTransportStateV1::Killing { cause, .. }
            | SerialChildTransportStateV1::Reaping { cause, .. } => {
                if !matches!(
                    self.state,
                    McpSerialChildStateV1::Cleaning { .. }
                        | McpSerialChildStateV1::Cancelling
                        | McpSerialChildStateV1::ShuttingDown
                ) {
                    self.event.take();
                    self.state = McpSerialChildStateV1::Cleaning {
                        failure: McpSerialChildFailureV1::TransportCause(cause),
                    };
                }
            }
            SerialChildTransportStateV1::Failed(transport) => {
                self.event.take();
                let previous = std::mem::replace(&mut self.state, McpSerialChildStateV1::Stopped);
                let failure = match previous {
                    McpSerialChildStateV1::Cleaning { failure } => {
                        McpSerialChildFailureV1::Cleanup {
                            primary: Box::new(failure),
                            transport,
                        }
                    }
                    McpSerialChildStateV1::Failed { failure } => failure,
                    _ => McpSerialChildFailureV1::Transport(transport),
                };
                self.state = McpSerialChildStateV1::Failed { failure };
            }
            SerialChildTransportStateV1::StoppedBeforeSpawn(_)
            | SerialChildTransportStateV1::Stopped { .. } => {
                self.event.take();
                let previous = std::mem::replace(&mut self.state, McpSerialChildStateV1::Stopped);
                self.state = match previous {
                    McpSerialChildStateV1::Cleaning { failure } => {
                        McpSerialChildStateV1::Failed { failure }
                    }
                    McpSerialChildStateV1::Cancelling
                    | McpSerialChildStateV1::ShuttingDown
                    | McpSerialChildStateV1::Stopped => McpSerialChildStateV1::Stopped,
                    McpSerialChildStateV1::Failed { failure } => {
                        McpSerialChildStateV1::Failed { failure }
                    }
                    _ => McpSerialChildStateV1::Failed {
                        failure: McpSerialChildFailureV1::Invariant(
                            "child stopped without an MCP cleanup request",
                        ),
                    },
                };
            }
            _ => {}
        }
    }

    #[cfg(test)]
    fn effects_for_test(&mut self) -> &mut E {
        self.transport.effects_for_test()
    }

    #[cfg(test)]
    fn payload_limit_for_test(&self) -> usize {
        self.payload_limit
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};

    use serde_json::{json, Value};

    use super::*;
    use crate::external::authority::{
        prepare_child_launch_request_v1, AdmittedChildLaunchV1, AuthorityProfileV1,
        ChildEnvironmentPolicyV1, ChildFilesystemAccessV1, ChildFilesystemGrantV1,
        ChildFilesystemPolicyV1, ChildIoLimitsV1, ChildLaunchAuthorityDecisionV1,
        ChildLaunchModeV1, ChildLaunchPreparationV1, ChildLaunchRequestV1, ChildNetworkPolicyV1,
        ChildPhaseBudgetsV1, ChildProcessGroupPolicyV1, ChildRequestPolicyV1, ChildRuntimePolicyV1,
        ChildTimeoutDispositionV1, PreparedChildContainmentTokenV1, PreparedChildLaunchV1,
    };
    use crate::external::lifecycle::ChildExitTruthV1;
    use crate::external::serial::{
        SerialChildCauseV1, SerialChildOperationV1, SerialChildTransportEffectsV1,
        SerialFrameObservationV1, SerialOwnedChildResidueV1, SerialOwnedResidueReasonV1,
        SerialPollAllowanceV1,
    };
    use crate::external::supervisor::{ChildReapPollV1, OwnedChildTokenV1};
    use crate::mcp_codec::{ClientError, ClientEvent, MCP_PROTOCOL_VERSION};

    const EXECUTABLE: &str = "/opt/kitsu-fixtures/mcp-peer";
    const WORKING_DIRECTORY: &str = "/tmp/kitsu-external";
    const DIGEST: &str = "abababababababababababababababababababababababababababababababab";
    const FRAME_BYTES: usize = 512;

    fn runtime_policy(max_frames: u16, frame_bytes: usize) -> ChildRuntimePolicyV1 {
        ChildRuntimePolicyV1::new(
            ChildPhaseBudgetsV1::new(5, 50, 9, 11, 13),
            ChildIoLimitsV1::new(4 * 1024, 8 * 1024, 2 * 1024, frame_bytes),
            ChildProcessGroupPolicyV1::Owned,
            ChildEnvironmentPolicyV1::new(true, Vec::new()),
            ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
                WORKING_DIRECTORY,
                ChildFilesystemAccessV1::ReadWrite,
            )]),
            ChildNetworkPolicyV1::Deny,
            ChildTimeoutDispositionV1::TerminateProcessGroupAndReap,
        )
        .with_request_policy(ChildRequestPolicyV1::serial(max_frames, 20))
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

    fn prepared_launch(max_frames: u16, frame_bytes: usize) -> PreparedChildLaunchV1 {
        let policy = runtime_policy(max_frames, frame_bytes);
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

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum EffectEvent {
        Now,
        Spawn,
        StartupReady,
        BeginFrame(Vec<u8>),
        PollFrame,
        BeginDrain(SerialChildCauseV1),
        DrainComplete,
        TerminateProcessGroup,
        ProcessGroupTerminated,
        Reap,
        RetainPrepared,
        RetainResidue,
    }

    struct McpFixture {
        events: Vec<EffectEvent>,
        times: VecDeque<Result<u64, String>>,
        startup_ready: VecDeque<Result<bool, String>>,
        frame_polls: VecDeque<Result<SerialFrameObservationV1, String>>,
        drain_complete: VecDeque<Result<bool, String>>,
        process_group_terminated: VecDeque<Result<bool, String>>,
        reap: VecDeque<ChildExitTruthV1>,
        retained_prepared: Vec<PreparedChildLaunchV1>,
        retained_residue: Vec<SerialOwnedChildResidueV1>,
        spawn_count: usize,
    }

    impl McpFixture {
        fn new() -> Self {
            Self {
                events: Vec::new(),
                times: (0..256).map(Ok).collect(),
                startup_ready: VecDeque::new(),
                frame_polls: VecDeque::new(),
                drain_complete: VecDeque::new(),
                process_group_terminated: VecDeque::new(),
                reap: VecDeque::new(),
                retained_prepared: Vec::new(),
                retained_residue: Vec::new(),
                spawn_count: 0,
            }
        }

        fn begin_frames(&self) -> Vec<&[u8]> {
            self.events
                .iter()
                .filter_map(|event| match event {
                    EffectEvent::BeginFrame(frame) => Some(frame.as_slice()),
                    _ => None,
                })
                .collect()
        }
    }

    impl SerialChildTransportEffectsV1 for McpFixture {
        fn monotonic_millis(&mut self) -> Result<u64, String> {
            self.events.push(EffectEvent::Now);
            self.times.pop_front().expect("fixture time")
        }

        fn spawn_serial(&mut self, _: PreparedChildLaunchV1) -> Result<OwnedChildTokenV1, String> {
            self.events.push(EffectEvent::Spawn);
            self.spawn_count += 1;
            Ok(OwnedChildTokenV1::new(37))
        }

        fn startup_ready(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(EffectEvent::StartupReady);
            self.startup_ready.pop_front().unwrap_or(Ok(false))
        }

        fn begin_frame(&mut self, _: &OwnedChildTokenV1, frame: &[u8]) -> Result<(), String> {
            self.events.push(EffectEvent::BeginFrame(frame.to_vec()));
            Ok(())
        }

        fn poll_frame(
            &mut self,
            _: &OwnedChildTokenV1,
            _: SerialPollAllowanceV1,
        ) -> Result<SerialFrameObservationV1, String> {
            self.events.push(EffectEvent::PollFrame);
            self.frame_polls
                .pop_front()
                .unwrap_or_else(|| Ok(SerialFrameObservationV1::pending()))
        }

        fn begin_drain(
            &mut self,
            _: &OwnedChildTokenV1,
            cause: SerialChildCauseV1,
        ) -> Result<(), String> {
            self.events.push(EffectEvent::BeginDrain(cause));
            Ok(())
        }

        fn drain_complete(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(EffectEvent::DrainComplete);
            self.drain_complete.pop_front().unwrap_or(Ok(false))
        }

        fn terminate_process_group(&mut self, _: &OwnedChildTokenV1) -> Result<(), String> {
            self.events.push(EffectEvent::TerminateProcessGroup);
            Ok(())
        }

        fn process_group_terminated(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(EffectEvent::ProcessGroupTerminated);
            self.process_group_terminated
                .pop_front()
                .unwrap_or(Ok(false))
        }

        fn poll_reap(&mut self, child: OwnedChildTokenV1) -> ChildReapPollV1 {
            self.events.push(EffectEvent::Reap);
            self.reap
                .pop_front()
                .map_or(ChildReapPollV1::Pending(child), ChildReapPollV1::Reaped)
        }

        fn retain_unstarted_launch(&mut self, launch: PreparedChildLaunchV1) {
            self.events.push(EffectEvent::RetainPrepared);
            self.retained_prepared.push(launch);
        }

        fn retain_owned_residue(&mut self, residue: SerialOwnedChildResidueV1) {
            self.events.push(EffectEvent::RetainResidue);
            self.retained_residue.push(residue);
        }
    }

    fn frame_json(frame: &[u8]) -> Value {
        assert_eq!(frame.last(), Some(&b'\n'));
        serde_json::from_slice(&frame[..frame.len() - 1]).unwrap()
    }

    fn initialize_result(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fixture", "version": "1.0" }
            }
        })
    }

    fn queue_response(owner: &mut McpSerialChildOwnerV1<'_, McpFixture>, response: Value) {
        let request_len = owner
            .effects_for_test()
            .begin_frames()
            .last()
            .expect("one outbound frame")
            .len();
        let mut response = serde_json::to_vec(&response).unwrap();
        response.push(b'\n');
        let response_len = response.len();
        owner
            .effects_for_test()
            .frame_polls
            .push_back(Ok(SerialFrameObservationV1::response(
                response,
                request_len,
                response_len,
                0,
            )));
    }

    fn queue_raw_response(owner: &mut McpSerialChildOwnerV1<'_, McpFixture>, response: Vec<u8>) {
        let request_len = owner
            .effects_for_test()
            .begin_frames()
            .last()
            .expect("one outbound frame")
            .len();
        let response_len = response.len();
        owner
            .effects_for_test()
            .frame_polls
            .push_back(Ok(SerialFrameObservationV1::response(
                response,
                request_len,
                response_len,
                0,
            )));
    }

    fn start_to_initialize(owner: &mut McpSerialChildOwnerV1<'_, McpFixture>) -> Value {
        owner.effects_for_test().startup_ready.push_back(Ok(true));
        owner.start().unwrap();
        owner.poll().unwrap();
        assert_eq!(owner.state(), &McpSerialChildStateV1::InitializeInFlight);
        frame_json(owner.effects_for_test().begin_frames().last().unwrap())
    }

    fn complete_initialize(owner: &mut McpSerialChildOwnerV1<'_, McpFixture>) {
        queue_response(owner, initialize_result(1));
        owner.poll().unwrap();
        assert_eq!(
            owner.state(),
            &McpSerialChildStateV1::InitializedNotificationInFlight
        );
        let notification = frame_json(owner.effects_for_test().begin_frames().last().unwrap());
        assert_eq!(notification["method"], "notifications/initialized");
        assert!(notification.get("id").is_none());
        let notification_len = owner
            .effects_for_test()
            .begin_frames()
            .last()
            .unwrap()
            .len();
        owner
            .effects_for_test()
            .frame_polls
            .push_back(Ok(SerialFrameObservationV1::progress(
                notification_len,
                0,
                0,
            )));
        owner.poll().unwrap();
        assert_eq!(owner.state(), &McpSerialChildStateV1::ReadyForToolsList);
    }

    #[test]
    fn exact_initialize_notification_list_call_order_reuses_one_owner_and_ids() {
        let mut effects = McpFixture::new();
        let mut owner =
            McpSerialChildOwnerV1::new(prepared_launch(6, FRAME_BYTES), &mut effects).unwrap();
        let initialize = start_to_initialize(&mut owner);
        assert_eq!(initialize["id"], 1);
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(
            initialize["params"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
        assert_eq!(initialize["params"]["capabilities"], json!({}));
        complete_initialize(&mut owner);

        owner.begin_tools_list(None).unwrap();
        let list = frame_json(owner.effects_for_test().begin_frames().last().unwrap());
        assert_eq!(list["id"], 2);
        assert_eq!(list["method"], "tools/list");
        queue_response(
            &mut owner,
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}}),
        );
        owner.poll().unwrap();
        assert!(matches!(
            owner.take_event().unwrap(),
            ClientEvent::ToolsListed(_)
        ));

        owner
            .begin_tools_call("fixture.echo", json!({"value": 7}))
            .unwrap();
        let call = frame_json(owner.effects_for_test().begin_frames().last().unwrap());
        assert_eq!(call["id"], 3);
        assert_eq!(call["method"], "tools/call");
        queue_response(
            &mut owner,
            json!({"jsonrpc":"2.0","id":3,"result":{"content":[]}}),
        );
        owner.poll().unwrap();
        assert!(matches!(
            owner.take_event().unwrap(),
            ClientEvent::ToolCalled(_)
        ));
        assert_eq!(owner.effects_for_test().spawn_count, 1);
        assert_eq!(owner.effects_for_test().begin_frames().len(), 4);
    }

    #[test]
    fn ordering_overlap_and_unconsumed_event_rejections_emit_no_frame() {
        let mut effects = McpFixture::new();
        let mut owner =
            McpSerialChildOwnerV1::new(prepared_launch(6, FRAME_BYTES), &mut effects).unwrap();
        assert_eq!(
            owner.begin_tools_call("too-early", json!({})),
            Err(McpSerialChildErrorV1::ToolsListRequired)
        );
        start_to_initialize(&mut owner);
        complete_initialize(&mut owner);
        assert_eq!(
            owner.begin_tools_call("still-too-early", json!({})),
            Err(McpSerialChildErrorV1::ToolsListRequired)
        );
        owner.begin_tools_list(None).unwrap();
        let frames = owner.effects_for_test().begin_frames().len();
        assert_eq!(
            owner.begin_tools_list(None),
            Err(McpSerialChildErrorV1::OperationInFlight)
        );
        assert_eq!(
            owner.begin_tools_call("overlap", json!({})),
            Err(McpSerialChildErrorV1::OperationInFlight)
        );
        assert_eq!(owner.effects_for_test().begin_frames().len(), frames);
        queue_response(
            &mut owner,
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}}),
        );
        owner.poll().unwrap();
        assert_eq!(
            owner.begin_tools_call("event-not-consumed", json!({})),
            Err(McpSerialChildErrorV1::EventNotConsumed)
        );
    }

    #[test]
    fn exact_payload_limit_and_four_frame_minimum_come_only_from_prepared_proof() {
        let mut too_few_effects = McpFixture::new();
        assert!(matches!(
            McpSerialChildOwnerV1::new(prepared_launch(3, FRAME_BYTES), &mut too_few_effects),
            Err(
                McpSerialChildConstructionErrorV1::InsufficientFrameCapacity {
                    required: 4,
                    authorized: 3,
                }
            )
        ));
        assert_eq!(too_few_effects.spawn_count, 0);
        assert_eq!(too_few_effects.retained_prepared.len(), 1);

        let mut effects = McpFixture::new();
        let owner =
            McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut effects).unwrap();
        assert_eq!(owner.payload_limit_for_test(), FRAME_BYTES - 1);
        drop(owner);
        assert_eq!(effects.retained_prepared.len(), 1);
    }

    #[test]
    fn wrong_response_id_aborts_with_typed_client_truth_and_never_emits_event() {
        let mut effects = McpFixture::new();
        let mut owner =
            McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut effects).unwrap();
        start_to_initialize(&mut owner);
        queue_response(&mut owner, initialize_result(99));
        owner.poll().unwrap();
        assert!(matches!(
            owner.state(),
            McpSerialChildStateV1::Cleaning {
                failure: McpSerialChildFailureV1::Client(ClientError::UnexpectedResponseId {
                    expected: Some(1),
                    actual: 99,
                })
            }
        ));
        assert_eq!(owner.take_event(), Err(McpSerialChildErrorV1::NoEventReady));
        assert_eq!(
            owner
                .effects_for_test()
                .events
                .iter()
                .filter(|event| {
                    **event == EffectEvent::BeginDrain(SerialChildCauseV1::ProtocolViolation)
                })
                .count(),
            1
        );
    }

    #[test]
    fn encoder_overflow_aborts_before_transport_frame_effect_and_retains_failure_truth() {
        let mut effects = McpFixture::new();
        let mut owner = McpSerialChildOwnerV1::new(prepared_launch(4, 16), &mut effects).unwrap();
        owner.effects_for_test().startup_ready.push_back(Ok(true));
        owner.start().unwrap();
        owner.poll().unwrap();
        assert!(matches!(
            owner.state(),
            McpSerialChildStateV1::Cleaning {
                failure: McpSerialChildFailureV1::Codec(_),
            }
        ));
        assert!(owner.effects_for_test().begin_frames().is_empty());
        assert_eq!(
            owner
                .effects_for_test()
                .events
                .iter()
                .filter(|event| {
                    **event == EffectEvent::BeginDrain(SerialChildCauseV1::ProtocolViolation)
                })
                .count(),
            1
        );
    }

    #[test]
    fn cancel_shutdown_and_transport_faults_never_become_protocol_success() {
        for shutdown in [false, true] {
            let mut effects = McpFixture::new();
            let mut owner =
                McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut effects).unwrap();
            start_to_initialize(&mut owner);
            if shutdown {
                owner.shutdown().unwrap();
            } else {
                owner.cancel().unwrap();
            }
            let drains = owner
                .effects_for_test()
                .events
                .iter()
                .filter(|event| matches!(event, EffectEvent::BeginDrain(_)))
                .count();
            if shutdown {
                owner.shutdown().unwrap();
            } else {
                owner.cancel().unwrap();
            }
            assert_eq!(
                owner
                    .effects_for_test()
                    .events
                    .iter()
                    .filter(|event| matches!(event, EffectEvent::BeginDrain(_)))
                    .count(),
                drains
            );
            assert!(!matches!(owner.state(), McpSerialChildStateV1::EventReady));
        }
    }

    #[test]
    fn malformed_json_and_multiple_peer_frames_fail_before_any_codec_event() {
        let mut malformed_effects = McpFixture::new();
        let mut malformed =
            McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut malformed_effects)
                .unwrap();
        start_to_initialize(&mut malformed);
        queue_raw_response(&mut malformed, b"{]\n".to_vec());
        malformed.poll().unwrap();
        assert!(matches!(
            malformed.state(),
            McpSerialChildStateV1::Cleaning {
                failure: McpSerialChildFailureV1::Codec(CodecError::InvalidJson { .. })
            }
        ));
        assert_eq!(
            malformed.take_event(),
            Err(McpSerialChildErrorV1::NoEventReady)
        );
        assert_eq!(
            malformed
                .effects_for_test()
                .events
                .iter()
                .filter(|event| {
                    **event == EffectEvent::BeginDrain(SerialChildCauseV1::ProtocolViolation)
                })
                .count(),
            1
        );

        let mut multiple_effects = McpFixture::new();
        let mut multiple =
            McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut multiple_effects)
                .unwrap();
        start_to_initialize(&mut multiple);
        let mut first = serde_json::to_vec(&initialize_result(1)).unwrap();
        first.push(b'\n');
        let request_len = multiple
            .effects_for_test()
            .begin_frames()
            .last()
            .unwrap()
            .len();
        let bytes_read = first.len() + 3;
        multiple.effects_for_test().frame_polls.push_back(Ok(
            SerialFrameObservationV1::response_with_extra_carry(
                first,
                request_len,
                bytes_read,
                0,
                3,
            ),
        ));
        multiple.poll().unwrap();
        assert!(matches!(
            multiple.state(),
            McpSerialChildStateV1::Cleaning {
                failure: McpSerialChildFailureV1::TransportCause(
                    SerialChildCauseV1::ProtocolViolation
                )
            }
        ));
        assert_eq!(
            multiple.take_event(),
            Err(McpSerialChildErrorV1::NoEventReady)
        );
    }

    #[test]
    fn unsupported_capability_and_server_method_abort_the_same_owned_child() {
        let cases = [
            json!({
                "jsonrpc":"2.0", "id":1,
                "result": {
                    "protocolVersion":MCP_PROTOCOL_VERSION,
                    "capabilities":{"tools":{}, "resources":{}},
                    "serverInfo":{"name":"fixture", "version":"1"}
                }
            }),
            json!({"jsonrpc":"2.0", "id":7, "method":"resources/list"}),
        ];
        for (case, response) in cases.into_iter().enumerate() {
            let mut effects = McpFixture::new();
            let mut owner =
                McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut effects).unwrap();
            start_to_initialize(&mut owner);
            queue_response(&mut owner, response);
            owner.poll().unwrap();
            assert!(
                matches!(
                    owner.state(),
                    McpSerialChildStateV1::Cleaning {
                        failure: McpSerialChildFailureV1::Client(
                            ClientError::UnsupportedServerCapability { .. }
                                | ClientError::UnsupportedServerMethod { .. }
                        )
                    }
                ),
                "case {case}: {:?}",
                owner.state()
            );
            assert_eq!(owner.effects_for_test().spawn_count, 1, "case {case}");
            assert_eq!(
                owner
                    .effects_for_test()
                    .events
                    .iter()
                    .filter(|event| {
                        **event == EffectEvent::BeginDrain(SerialChildCauseV1::ProtocolViolation)
                    })
                    .count(),
                1,
                "case {case}"
            );
            assert_eq!(
                owner.take_event(),
                Err(McpSerialChildErrorV1::NoEventReady),
                "case {case}"
            );
        }
    }

    #[test]
    fn local_client_validation_failure_aborts_once_before_a_call_frame() {
        let mut effects = McpFixture::new();
        let mut owner =
            McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut effects).unwrap();
        start_to_initialize(&mut owner);
        complete_initialize(&mut owner);
        owner.begin_tools_list(None).unwrap();
        queue_response(
            &mut owner,
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}}),
        );
        owner.poll().unwrap();
        assert!(matches!(
            owner.take_event().unwrap(),
            ClientEvent::ToolsListed(_)
        ));
        let frames_before = owner.effects_for_test().begin_frames().len();
        owner.begin_tools_call("", json!({})).unwrap();
        assert!(matches!(
            owner.state(),
            McpSerialChildStateV1::Cleaning {
                failure: McpSerialChildFailureV1::Client(ClientError::InvalidToolName)
            }
        ));
        assert_eq!(owner.effects_for_test().begin_frames().len(), frames_before);
        assert_eq!(
            owner
                .effects_for_test()
                .events
                .iter()
                .filter(|event| {
                    **event == EffectEvent::BeginDrain(SerialChildCauseV1::ProtocolViolation)
                })
                .count(),
            1
        );
        assert_eq!(owner.take_event(), Err(McpSerialChildErrorV1::NoEventReady));
    }

    #[test]
    fn notification_stdout_stderr_or_carry_aborts_without_notification_success() {
        let observations = [
            SerialFrameObservationV1::progress(0, 1, 0),
            SerialFrameObservationV1::progress(0, 0, 1),
            SerialFrameObservationV1::response_with_extra_carry(b"{}\n".to_vec(), 0, 6, 0, 3),
        ];
        for (case, observation) in observations.into_iter().enumerate() {
            let mut effects = McpFixture::new();
            let mut owner =
                McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut effects).unwrap();
            start_to_initialize(&mut owner);
            queue_response(&mut owner, initialize_result(1));
            owner.poll().unwrap();
            assert_eq!(
                owner.state(),
                &McpSerialChildStateV1::InitializedNotificationInFlight
            );
            owner
                .effects_for_test()
                .frame_polls
                .push_back(Ok(observation));
            owner.poll().unwrap();
            assert!(
                matches!(
                    owner.state(),
                    McpSerialChildStateV1::Cleaning {
                        failure: McpSerialChildFailureV1::TransportCause(
                            SerialChildCauseV1::ProtocolViolation
                        )
                    }
                ),
                "case {case}: {:?}",
                owner.state()
            );
            assert_eq!(
                owner.take_event(),
                Err(McpSerialChildErrorV1::NoEventReady),
                "case {case}"
            );
        }
    }

    #[test]
    fn eof_crash_carry_and_line_timeout_remain_typed_transport_failures() {
        let observations = [
            (SerialFrameObservationV1::eof(), SerialChildCauseV1::Eof),
            (
                SerialFrameObservationV1::exit(ChildExitTruthV1::Exited(17)),
                SerialChildCauseV1::Crash,
            ),
            (
                SerialFrameObservationV1::response_with_extra_carry(b"{}\n".to_vec(), 0, 6, 0, 3),
                SerialChildCauseV1::ProtocolViolation,
            ),
        ];
        for (case, (observation, expected)) in observations.into_iter().enumerate() {
            let mut effects = McpFixture::new();
            let mut owner =
                McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut effects).unwrap();
            start_to_initialize(&mut owner);
            owner
                .effects_for_test()
                .frame_polls
                .push_back(Ok(observation));
            owner.poll().unwrap();
            assert!(
                matches!(
                    owner.state(),
                    McpSerialChildStateV1::Cleaning {
                        failure: McpSerialChildFailureV1::TransportCause(cause)
                    } if *cause == expected
                ),
                "case {case}: {:?}",
                owner.state()
            );
        }

        let mut timeout_effects = McpFixture::new();
        let mut timeout =
            McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut timeout_effects)
                .unwrap();
        start_to_initialize(&mut timeout);
        timeout.effects_for_test().times.clear();
        timeout.effects_for_test().times.push_back(Ok(100));
        timeout.poll().unwrap();
        assert!(matches!(
            timeout.state(),
            McpSerialChildStateV1::Cleaning {
                failure: McpSerialChildFailureV1::TransportCause(SerialChildCauseV1::LineTimeout)
            }
        ));
    }

    #[test]
    fn clock_and_effect_faults_transfer_exactly_one_retryable_owned_residue() {
        for effect_fault in [false, true] {
            let mut effects = McpFixture::new();
            let mut owner =
                McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut effects).unwrap();
            start_to_initialize(&mut owner);
            if effect_fault {
                owner
                    .effects_for_test()
                    .frame_polls
                    .push_back(Err("poll failed".to_string()));
            } else {
                owner.effects_for_test().times.clear();
                owner
                    .effects_for_test()
                    .times
                    .push_back(Err("clock failed".to_string()));
            }
            owner.poll().unwrap();
            assert!(
                matches!(
                    owner.state(),
                    McpSerialChildStateV1::Failed {
                        failure: McpSerialChildFailureV1::Transport(
                            SerialChildTransportFailureV1::ClockRead(_)
                                | SerialChildTransportFailureV1::Effect {
                                    operation: SerialChildOperationV1::PollFrame,
                                    ..
                                }
                        )
                    }
                ),
                "effect_fault={effect_fault}: {:?}",
                owner.state()
            );
            assert_eq!(owner.effects_for_test().retained_residue.len(), 1);
            let expected_reason = if effect_fault {
                SerialOwnedResidueReasonV1::Effect(SerialChildOperationV1::PollFrame)
            } else {
                SerialOwnedResidueReasonV1::ClockRead
            };
            assert_eq!(
                owner.effects_for_test().retained_residue[0].reason_for_test(),
                expected_reason
            );
            assert_eq!(
                owner
                    .effects_for_test()
                    .events
                    .iter()
                    .filter(|event| matches!(event, EffectEvent::RetainResidue))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn protocol_cleanup_reap_deadline_preserves_primary_failure_and_owned_residue() {
        let mut effects = McpFixture::new();
        let mut owner =
            McpSerialChildOwnerV1::new(prepared_launch(4, FRAME_BYTES), &mut effects).unwrap();
        start_to_initialize(&mut owner);
        queue_response(&mut owner, initialize_result(99));
        owner.poll().unwrap();
        owner.effects_for_test().drain_complete.push_back(Ok(true));
        owner.poll().unwrap();
        owner
            .effects_for_test()
            .process_group_terminated
            .push_back(Ok(true));
        owner.poll().unwrap();
        owner.effects_for_test().times.clear();
        owner.effects_for_test().times.push_back(Ok(21));
        owner.poll().unwrap();
        assert!(matches!(
            owner.state(),
            McpSerialChildStateV1::Failed {
                failure: McpSerialChildFailureV1::Cleanup {
                    primary,
                    transport: SerialChildTransportFailureV1::ReapDeadline {
                        cause: SerialChildCauseV1::ProtocolViolation
                    }
                }
            } if matches!(
                primary.as_ref(),
                McpSerialChildFailureV1::Client(ClientError::UnexpectedResponseId {
                    expected: Some(1),
                    actual: 99,
                })
            )
        ));
        assert_eq!(owner.effects_for_test().retained_residue.len(), 1);
        assert_eq!(
            owner.effects_for_test().retained_residue[0].reason_for_test(),
            SerialOwnedResidueReasonV1::ReapDeadline {
                cause: SerialChildCauseV1::ProtocolViolation
            }
        );
        assert_eq!(owner.take_event(), Err(McpSerialChildErrorV1::NoEventReady));
    }
}
