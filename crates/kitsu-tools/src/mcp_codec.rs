//! Provider-free framing and protocol state for Kitsu's bounded tools-only MCP slice.

use serde_json::{json, Map, Value};
use thiserror::Error;

/// Last stable MCP revision whose lifecycle includes `initialize` followed by
/// `notifications/initialized`. Supporting another revision requires another
/// compiled codec rather than silently accepting a different wire contract.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Upper bound for a tool name accepted into an outbound call.
pub const MAX_TOOL_NAME_BYTES: usize = 256;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("frame limit must be greater than zero")]
    InvalidLimit,
    #[error("JSON-RPC frame exceeds the {limit}-byte limit")]
    FrameTooLarge { limit: usize },
    #[error("empty JSON-RPC frame")]
    EmptyFrame,
    #[error("invalid JSON-RPC JSON: {detail}")]
    InvalidJson { detail: String },
    #[error("stream ended with a {bytes}-byte partial JSON-RPC frame")]
    TruncatedFrame { bytes: usize },
    #[error("decoder is poisoned after an earlier framing failure")]
    Poisoned,
}

/// A fail-stop newline-delimited JSON decoder with an explicit per-frame cap.
///
/// Once framing or parsing fails, the decoder cannot safely infer the peer's
/// message boundary and remains poisoned. The lifecycle owner must discard it.
#[derive(Debug)]
pub struct LineDecoder {
    limit: usize,
    pending: Vec<u8>,
    poisoned: bool,
}

impl LineDecoder {
    pub fn new(limit: usize) -> Result<Self, CodecError> {
        if limit == 0 {
            return Err(CodecError::InvalidLimit);
        }
        Ok(Self {
            limit,
            pending: Vec::new(),
            poisoned: false,
        })
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, CodecError> {
        if self.poisoned {
            return Err(CodecError::Poisoned);
        }

        let mut messages = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                let mut frame = std::mem::take(&mut self.pending);
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                if frame.is_empty() {
                    return self.poison(CodecError::EmptyFrame);
                }
                let value = serde_json::from_slice(&frame).map_err(|error| {
                    self.poisoned = true;
                    CodecError::InvalidJson {
                        detail: error.to_string(),
                    }
                })?;
                messages.push(value);
            } else {
                if self.pending.len() == self.limit {
                    self.pending.clear();
                    return self.poison(CodecError::FrameTooLarge { limit: self.limit });
                }
                self.pending.push(*byte);
            }
        }
        Ok(messages)
    }

    pub fn finish(&mut self) -> Result<(), CodecError> {
        if self.poisoned {
            return Err(CodecError::Poisoned);
        }
        if self.pending.is_empty() {
            return Ok(());
        }
        let bytes = self.pending.len();
        self.pending.clear();
        self.poison(CodecError::TruncatedFrame { bytes })
    }

    fn poison<T>(&mut self, error: CodecError) -> Result<T, CodecError> {
        self.poisoned = true;
        Err(error)
    }
}

/// Serialize one compact JSON value and terminate it with exactly one newline.
pub fn encode_line(message: &Value, limit: usize) -> Result<Vec<u8>, CodecError> {
    if limit == 0 {
        return Err(CodecError::InvalidLimit);
    }
    let mut encoded = serde_json::to_vec(message).map_err(|error| CodecError::InvalidJson {
        detail: error.to_string(),
    })?;
    if encoded.len() > limit {
        return Err(CodecError::FrameTooLarge { limit });
    }
    encoded.push(b'\n');
    Ok(encoded)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestId(u64);

impl RequestId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutboundMessage {
    id: Option<RequestId>,
    message: Value,
}

impl OutboundMessage {
    pub fn id(&self) -> Option<RequestId> {
        self.id
    }

    pub fn message(&self) -> &Value {
        &self.message
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    New,
    InitializePending,
    InitializeAcknowledged,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    Initialized(ServerIdentity),
    ToolsListed(Value),
    ToolCalled(Value),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClientError {
    #[error("cannot {operation} while the MCP client is {state:?}")]
    InvalidState {
        operation: &'static str,
        state: ClientState,
    },
    #[error("request {id} is already in flight")]
    RequestInFlight { id: u64 },
    #[error("request ID space is exhausted")]
    RequestIdExhausted,
    #[error("client name and version must be non-empty")]
    InvalidClientInfo,
    #[error("tool name is empty or exceeds the {MAX_TOOL_NAME_BYTES}-byte limit")]
    InvalidToolName,
    #[error("tool arguments must be a JSON object")]
    InvalidArguments,
    #[error("message must be a JSON object")]
    InvalidMessage,
    #[error("message is not JSON-RPC 2.0")]
    InvalidJsonRpcVersion,
    #[error("response ID must be a positive unsigned integer")]
    InvalidId,
    #[error("unexpected response ID {actual}; expected {expected:?}")]
    UnexpectedResponseId { expected: Option<u64>, actual: u64 },
    #[error("server method is unsupported by the tools-only client: {method}")]
    UnsupportedServerMethod { method: String },
    #[error("server capability is unsupported by the tools-only client: {capability}")]
    UnsupportedServerCapability { capability: String },
    #[error("server did not declare the required tools capability")]
    MissingToolsCapability,
    #[error("tools capability is unsupported: {capability}")]
    UnsupportedToolsCapability { capability: String },
    #[error("server chose MCP {actual}; this codec requires {expected}")]
    UnsupportedProtocolVersion {
        expected: &'static str,
        actual: String,
    },
    #[error("malformed response: {detail}")]
    MalformedResponse { detail: &'static str },
    #[error("remote request {id} failed with {code}: {message}")]
    RemoteError { id: u64, code: i64, message: String },
}

#[derive(Debug, Clone, Copy)]
enum PendingKind {
    Initialize,
    ToolsList,
    ToolsCall,
}

#[derive(Debug, Clone, Copy)]
struct PendingRequest {
    id: RequestId,
    kind: PendingKind,
}

/// Strict client-side state for the bounded initialize/list/call vocabulary.
///
/// This type owns no process, transport, configuration, or tool inventory.
#[derive(Debug)]
pub struct ToolsOnlyClient {
    state: ClientState,
    next_id: u64,
    pending: Option<PendingRequest>,
}

impl Default for ToolsOnlyClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolsOnlyClient {
    pub fn new() -> Self {
        Self {
            state: ClientState::New,
            next_id: 1,
            pending: None,
        }
    }

    pub fn state(&self) -> ClientState {
        self.state
    }

    pub fn start_initialize(
        &mut self,
        client_name: &str,
        client_version: &str,
    ) -> Result<OutboundMessage, ClientError> {
        self.require_state("initialize", ClientState::New)?;
        if client_name.is_empty() || client_version.is_empty() {
            return Err(ClientError::InvalidClientInfo);
        }
        let id = self.reserve(PendingKind::Initialize)?;
        self.state = ClientState::InitializePending;
        Ok(request(
            id,
            "initialize",
            Some(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": client_name, "version": client_version }
            })),
        ))
    }

    pub fn finish_initialize(&mut self) -> Result<OutboundMessage, ClientError> {
        self.require_state(
            "send initialized notification",
            ClientState::InitializeAcknowledged,
        )?;
        self.state = ClientState::Ready;
        Ok(OutboundMessage {
            id: None,
            message: json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        })
    }

    pub fn start_tools_list(
        &mut self,
        cursor: Option<String>,
    ) -> Result<OutboundMessage, ClientError> {
        self.require_state("list tools", ClientState::Ready)?;
        let id = self.reserve(PendingKind::ToolsList)?;
        let params = cursor.map(|cursor| json!({ "cursor": cursor }));
        Ok(request(id, "tools/list", params))
    }

    pub fn start_tools_call(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<OutboundMessage, ClientError> {
        self.require_state("call tool", ClientState::Ready)?;
        if name.is_empty() || name.len() > MAX_TOOL_NAME_BYTES {
            return Err(ClientError::InvalidToolName);
        }
        if !arguments.is_object() {
            return Err(ClientError::InvalidArguments);
        }
        let id = self.reserve(PendingKind::ToolsCall)?;
        Ok(request(
            id,
            "tools/call",
            Some(json!({ "name": name, "arguments": arguments })),
        ))
    }

    pub fn receive(&mut self, message: Value) -> Result<ClientEvent, ClientError> {
        let object = message.as_object().ok_or(ClientError::InvalidMessage)?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(ClientError::InvalidJsonRpcVersion);
        }
        if let Some(method) = object.get("method") {
            let method = method.as_str().ok_or(ClientError::MalformedResponse {
                detail: "method must be a string",
            })?;
            return Err(ClientError::UnsupportedServerMethod {
                method: method.to_owned(),
            });
        }
        validate_response_fields(object)?;

        let id = object
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or(ClientError::InvalidId)?;
        let pending = self.pending.ok_or(ClientError::UnexpectedResponseId {
            expected: None,
            actual: id,
        })?;
        if pending.id.get() != id {
            return Err(ClientError::UnexpectedResponseId {
                expected: Some(pending.id.get()),
                actual: id,
            });
        }

        match (object.get("result"), object.get("error")) {
            (Some(result), None) => {
                let result = result.clone();
                let event = match pending.kind {
                    PendingKind::Initialize => self.accept_initialize(&result),
                    PendingKind::ToolsList => {
                        validate_tools_list(&result).map(|()| ClientEvent::ToolsListed(result))
                    }
                    PendingKind::ToolsCall => {
                        validate_tools_call(&result).map(|()| ClientEvent::ToolCalled(result))
                    }
                };
                match event {
                    Ok(event) => {
                        self.pending = None;
                        if !matches!(pending.kind, PendingKind::Initialize) {
                            self.state = ClientState::Ready;
                        }
                        Ok(event)
                    }
                    Err(error) => self.fail(error),
                }
            }
            (None, Some(error)) => self.accept_remote_error(pending, error),
            _ => self.fail(ClientError::MalformedResponse {
                detail: "response must contain exactly one of result or error",
            }),
        }
    }

    fn require_state(
        &self,
        operation: &'static str,
        required: ClientState,
    ) -> Result<(), ClientError> {
        if self.state != required {
            return Err(ClientError::InvalidState {
                operation,
                state: self.state,
            });
        }
        Ok(())
    }

    fn reserve(&mut self, kind: PendingKind) -> Result<RequestId, ClientError> {
        if let Some(pending) = self.pending {
            return Err(ClientError::RequestInFlight {
                id: pending.id.get(),
            });
        }
        let id = RequestId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ClientError::RequestIdExhausted)?;
        self.pending = Some(PendingRequest { id, kind });
        Ok(id)
    }

    fn accept_initialize(&mut self, result: &Value) -> Result<ClientEvent, ClientError> {
        let result = result.as_object().ok_or(ClientError::MalformedResponse {
            detail: "initialize result must be an object",
        })?;
        let version = required_string(result, "protocolVersion")?;
        if version != MCP_PROTOCOL_VERSION {
            return Err(ClientError::UnsupportedProtocolVersion {
                expected: MCP_PROTOCOL_VERSION,
                actual: version.to_owned(),
            });
        }
        validate_server_capabilities(result.get("capabilities"))?;
        let server = result.get("serverInfo").and_then(Value::as_object).ok_or(
            ClientError::MalformedResponse {
                detail: "serverInfo must be an object",
            },
        )?;
        let identity = ServerIdentity {
            name: required_string(server, "name")?.to_owned(),
            version: required_string(server, "version")?.to_owned(),
        };
        self.state = ClientState::InitializeAcknowledged;
        Ok(ClientEvent::Initialized(identity))
    }

    fn accept_remote_error(
        &mut self,
        pending: PendingRequest,
        error: &Value,
    ) -> Result<ClientEvent, ClientError> {
        let (code, message) = match parse_remote_error(error) {
            Ok(error) => error,
            Err(error) => return self.fail(error),
        };
        self.pending = None;
        if matches!(pending.kind, PendingKind::Initialize) {
            self.state = ClientState::Failed;
        }
        Err(ClientError::RemoteError {
            id: pending.id.get(),
            code,
            message,
        })
    }

    fn fail<T>(&mut self, error: ClientError) -> Result<T, ClientError> {
        self.pending = None;
        self.state = ClientState::Failed;
        Err(error)
    }
}

fn parse_remote_error(error: &Value) -> Result<(i64, String), ClientError> {
    let error = error.as_object().ok_or(ClientError::MalformedResponse {
        detail: "error must be an object",
    })?;
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .ok_or(ClientError::MalformedResponse {
            detail: "error code must be an integer",
        })?;
    Ok((code, required_string(error, "message")?.to_owned()))
}

fn request(id: RequestId, method: &'static str, params: Option<Value>) -> OutboundMessage {
    let mut message = Map::from_iter([
        ("jsonrpc".to_owned(), Value::String("2.0".to_owned())),
        ("id".to_owned(), Value::from(id.get())),
        ("method".to_owned(), Value::String(method.to_owned())),
    ]);
    if let Some(params) = params {
        message.insert("params".to_owned(), params);
    }
    OutboundMessage {
        id: Some(id),
        message: Value::Object(message),
    }
}

fn validate_response_fields(object: &Map<String, Value>) -> Result<(), ClientError> {
    for field in object.keys() {
        if !matches!(field.as_str(), "jsonrpc" | "id" | "result" | "error") {
            return Err(ClientError::MalformedResponse {
                detail: "response contains an unsupported top-level field",
            });
        }
    }
    Ok(())
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, ClientError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ClientError::MalformedResponse {
            detail: "required string field is absent or empty",
        })
}

fn validate_server_capabilities(capabilities: Option<&Value>) -> Result<(), ClientError> {
    let capabilities =
        capabilities
            .and_then(Value::as_object)
            .ok_or(ClientError::MalformedResponse {
                detail: "capabilities must be an object",
            })?;
    for capability in capabilities.keys() {
        if capability != "tools" {
            return Err(ClientError::UnsupportedServerCapability {
                capability: capability.clone(),
            });
        }
    }
    let tools = capabilities
        .get("tools")
        .ok_or(ClientError::MissingToolsCapability)?
        .as_object()
        .ok_or(ClientError::MalformedResponse {
            detail: "tools capability must be an object",
        })?;
    for (capability, value) in tools {
        match (capability.as_str(), value.as_bool()) {
            ("listChanged", Some(false)) => {}
            ("listChanged", Some(true)) => {
                return Err(ClientError::UnsupportedToolsCapability {
                    capability: capability.clone(),
                });
            }
            ("listChanged", None) => {
                return Err(ClientError::MalformedResponse {
                    detail: "tools.listChanged must be a boolean",
                });
            }
            _ => {
                return Err(ClientError::UnsupportedToolsCapability {
                    capability: capability.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_tools_list(result: &Value) -> Result<(), ClientError> {
    let result = result.as_object().ok_or(ClientError::MalformedResponse {
        detail: "tools/list result must be an object",
    })?;
    result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or(ClientError::MalformedResponse {
            detail: "tools/list result must contain a tools array",
        })?;
    if result
        .get("nextCursor")
        .is_some_and(|cursor| !cursor.is_string())
    {
        return Err(ClientError::MalformedResponse {
            detail: "nextCursor must be a string",
        });
    }
    Ok(())
}

fn validate_tools_call(result: &Value) -> Result<(), ClientError> {
    let result = result.as_object().ok_or(ClientError::MalformedResponse {
        detail: "tools/call result must be an object",
    })?;
    result
        .get("content")
        .and_then(Value::as_array)
        .ok_or(ClientError::MalformedResponse {
            detail: "tools/call result must contain a content array",
        })?;
    if result
        .get("isError")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(ClientError::MalformedResponse {
            detail: "isError must be a boolean",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

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

    fn ready_client() -> ToolsOnlyClient {
        let mut client = ToolsOnlyClient::new();
        let request = client.start_initialize("kitsu-test", "0.1").unwrap();
        client
            .receive(initialize_result(request.id().unwrap().get()))
            .unwrap();
        client.finish_initialize().unwrap();
        client
    }

    #[test]
    fn fragmented_and_multiple_crlf_frames_decode() {
        let mut decoder = LineDecoder::new(128).unwrap();
        assert!(decoder.push(br#"{"jsonrpc":"2."#).unwrap().is_empty());
        assert!(decoder.push(br#"0","id":1}"#).unwrap().is_empty());
        assert_eq!(
            decoder.push(b"\r\n{}\n").unwrap(),
            vec![json!({"jsonrpc": "2.0", "id": 1}), json!({})]
        );
        decoder.finish().unwrap();
    }

    #[test]
    fn oversize_frame_poisoning_is_sticky() {
        let mut decoder = LineDecoder::new(4).unwrap();
        assert!(decoder.push(b"1234").unwrap().is_empty());
        assert!(matches!(
            decoder.push(b"5"),
            Err(CodecError::FrameTooLarge { limit: 4 })
        ));
        assert!(matches!(decoder.push(b"\n"), Err(CodecError::Poisoned)));
        assert!(matches!(decoder.finish(), Err(CodecError::Poisoned)));
    }

    #[test]
    fn malformed_empty_and_truncated_frames_are_typed() {
        let mut empty = LineDecoder::new(16).unwrap();
        assert!(matches!(empty.push(b"\n"), Err(CodecError::EmptyFrame)));

        let mut malformed = LineDecoder::new(16).unwrap();
        assert!(matches!(
            malformed.push(b"{]\n"),
            Err(CodecError::InvalidJson { .. })
        ));

        let mut truncated = LineDecoder::new(16).unwrap();
        truncated.push(b"{}").unwrap();
        assert!(matches!(
            truncated.finish(),
            Err(CodecError::TruncatedFrame { bytes: 2 })
        ));
    }

    #[test]
    fn encoder_uses_one_newline_and_the_frame_bound() {
        assert_eq!(encode_line(&json!({"id": 1}), 16).unwrap(), b"{\"id\":1}\n");
        assert!(matches!(
            encode_line(&json!({"long": "value"}), 4),
            Err(CodecError::FrameTooLarge { limit: 4 })
        ));
    }

    #[test]
    fn initialization_is_pinned_and_has_zero_ambient_capabilities() {
        let mut client = ToolsOnlyClient::new();
        let request = client.start_initialize("kitsu", "0.1").unwrap();
        assert_eq!(
            request.message(),
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "kitsu", "version": "0.1" }
                }
            })
        );
        assert_eq!(client.state(), ClientState::InitializePending);

        assert_eq!(
            client.receive(initialize_result(1)).unwrap(),
            ClientEvent::Initialized(ServerIdentity {
                name: "fixture".into(),
                version: "1.0".into()
            })
        );
        assert_eq!(client.state(), ClientState::InitializeAcknowledged);
        let notification = client.finish_initialize().unwrap();
        assert_eq!(
            notification.message(),
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        );
        assert_eq!(notification.id(), None);
        assert_eq!(client.state(), ClientState::Ready);
    }

    #[test]
    fn state_ordering_and_one_in_flight_call_are_enforced() {
        let mut client = ToolsOnlyClient::new();
        assert!(matches!(
            client.start_tools_list(None),
            Err(ClientError::InvalidState { .. })
        ));
        assert!(matches!(
            client.finish_initialize(),
            Err(ClientError::InvalidState { .. })
        ));

        let mut client = ready_client();
        let list = client.start_tools_list(None).unwrap();
        assert_eq!(list.id().unwrap().get(), 2);
        assert!(matches!(
            client.start_tools_call("echo", json!({})),
            Err(ClientError::RequestInFlight { id: 2 })
        ));
        assert_eq!(
            client
                .receive(json!({"jsonrpc":"2.0", "id":2, "result":{"tools":[]}}))
                .unwrap(),
            ClientEvent::ToolsListed(json!({"tools": []}))
        );
        assert_eq!(client.state(), ClientState::Ready);
    }

    #[test]
    fn future_protocol_and_non_tools_capabilities_fail_closed() {
        let mut client = ToolsOnlyClient::new();
        client.start_initialize("kitsu", "0.1").unwrap();
        assert!(matches!(
            client.receive(json!({
                "jsonrpc":"2.0", "id":1,
                "result": {
                    "protocolVersion":"2099-01-01",
                    "capabilities":{"tools":{}},
                    "serverInfo":{"name":"future", "version":"1"}
                }
            })),
            Err(ClientError::UnsupportedProtocolVersion { .. })
        ));
        assert_eq!(client.state(), ClientState::Failed);

        let mut client = ToolsOnlyClient::new();
        client.start_initialize("kitsu", "0.1").unwrap();
        assert!(matches!(
            client.receive(json!({
                "jsonrpc":"2.0", "id":1,
                "result": {
                    "protocolVersion":MCP_PROTOCOL_VERSION,
                    "capabilities":{"tools":{}, "resources":{}},
                    "serverInfo":{"name":"fixture", "version":"1"}
                }
            })),
            Err(ClientError::UnsupportedServerCapability { capability })
                if capability == "resources"
        ));
        assert_eq!(client.state(), ClientState::Failed);
    }

    #[test]
    fn changing_or_missing_tool_inventory_capability_is_rejected() {
        let mut client = ToolsOnlyClient::new();
        client.start_initialize("kitsu", "0.1").unwrap();
        assert!(matches!(
            client.receive(json!({
                "jsonrpc":"2.0", "id":1,
                "result": {
                    "protocolVersion":MCP_PROTOCOL_VERSION,
                    "capabilities":{"tools":{"listChanged":true}},
                    "serverInfo":{"name":"fixture", "version":"1"}
                }
            })),
            Err(ClientError::UnsupportedToolsCapability { capability })
                if capability == "listChanged"
        ));

        let mut client = ToolsOnlyClient::new();
        client.start_initialize("kitsu", "0.1").unwrap();
        assert!(matches!(
            client.receive(json!({
                "jsonrpc":"2.0", "id":1,
                "result": {
                    "protocolVersion":MCP_PROTOCOL_VERSION,
                    "capabilities":{},
                    "serverInfo":{"name":"fixture", "version":"1"}
                }
            })),
            Err(ClientError::MissingToolsCapability)
        ));
    }

    #[test]
    fn string_mismatched_duplicate_and_late_ids_are_rejected() {
        let mut client = ready_client();
        client.start_tools_list(None).unwrap();
        assert!(matches!(
            client.receive(json!({"jsonrpc":"2.0", "id":"2", "result":{"tools":[]}})),
            Err(ClientError::InvalidId)
        ));
        assert!(matches!(
            client.receive(json!({"jsonrpc":"2.0", "id":1, "result":{"tools":[]}})),
            Err(ClientError::UnexpectedResponseId {
                expected: Some(2),
                actual: 1
            })
        ));
        client
            .receive(json!({"jsonrpc":"2.0", "id":2, "result":{"tools":[]}}))
            .unwrap();
        assert!(matches!(
            client.receive(json!({"jsonrpc":"2.0", "id":2, "result":{"tools":[]}})),
            Err(ClientError::UnexpectedResponseId {
                expected: None,
                actual: 2
            })
        ));
    }

    #[test]
    fn unsupported_server_methods_are_typed() {
        let mut client = ready_client();
        for message in [
            json!({"jsonrpc":"2.0", "id":99, "method":"roots/list", "params":{}}),
            json!({"jsonrpc":"2.0", "method":"notifications/tools/list_changed"}),
        ] {
            assert!(matches!(
                client.receive(message),
                Err(ClientError::UnsupportedServerMethod { .. })
            ));
        }
    }

    #[test]
    fn tools_list_and_call_use_only_the_pinned_vocabulary() {
        let mut client = ready_client();
        let list = client.start_tools_list(Some("next".into())).unwrap();
        assert_eq!(
            list.message(),
            &json!({"jsonrpc":"2.0", "id":2, "method":"tools/list", "params":{"cursor":"next"}})
        );
        client
            .receive(json!({"jsonrpc":"2.0", "id":2, "result":{"tools":[], "nextCursor":"later"}}))
            .unwrap();

        let call = client
            .start_tools_call("echo", json!({"text":"hello"}))
            .unwrap();
        assert_eq!(
            call.message(),
            &json!({
                "jsonrpc":"2.0", "id":3, "method":"tools/call",
                "params":{"name":"echo", "arguments":{"text":"hello"}}
            })
        );
        assert_eq!(
            client
                .receive(json!({
                    "jsonrpc":"2.0", "id":3,
                    "result":{"content":[{"type":"text", "text":"hello"}]}
                }))
                .unwrap(),
            ClientEvent::ToolCalled(json!({
                "content":[{"type":"text", "text":"hello"}]
            }))
        );
    }

    #[test]
    fn matching_remote_error_releases_the_request_slot() {
        let mut client = ready_client();
        client.start_tools_list(None).unwrap();
        assert!(matches!(
            client.receive(json!({
                "jsonrpc":"2.0", "id":2,
                "error":{"code":-32601, "message":"not found"}
            })),
            Err(ClientError::RemoteError { id:2, code:-32601, message })
                if message == "not found"
        ));
        assert_eq!(client.state(), ClientState::Ready);
        assert!(client.start_tools_list(None).is_ok());
    }

    #[test]
    fn malformed_matching_responses_fail_the_protocol_state() {
        let mut client = ready_client();
        client.start_tools_list(None).unwrap();
        assert!(matches!(
            client.receive(json!({
                "jsonrpc":"2.0", "id":2,
                "result":{"tools":[]},
                "error":{"code":-32600, "message":"ambiguous"}
            })),
            Err(ClientError::MalformedResponse { .. })
        ));
        assert_eq!(client.state(), ClientState::Failed);

        let mut client = ready_client();
        client.start_tools_list(None).unwrap();
        assert!(matches!(
            client.receive(json!({
                "jsonrpc":"2.0", "id":2,
                "error":{"code":"-32600", "message":"wrong type"}
            })),
            Err(ClientError::MalformedResponse { .. })
        ));
        assert_eq!(client.state(), ClientState::Failed);
    }
}
