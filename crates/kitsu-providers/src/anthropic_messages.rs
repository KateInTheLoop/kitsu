//! Native Anthropic Messages wire adapter.
//!
//! The compiled slices support streamed text and correlated tool use/results.
//! Retries, catalog discovery, route publication, and live CLI selection remain
//! disabled until their own conformance fixtures pass.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use reqwest::StatusCode;
use serde::Serialize;

use crate::auth::{AuthProvider, FrozenAuth};
use crate::error::AuthError;
use crate::route::{
    ProviderAuthSource, ProviderRouteContractError, ProviderRouteDeclaration, ProviderRoutePolicy,
    ProviderRouteSnapshot,
};
use crate::telemetry::{
    ProviderAttemptCategory, ProviderAttemptEvent, ProviderAttemptOutcome, ProviderTelemetrySink,
};
use crate::{
    ChatDelta, ChatRequest, ChatStream, FinishReason, Provider, ProviderError,
    ProviderResponsePart, Role, ToolCall, Usage,
};

/// Wire revision pinned by [`crate::ProviderWireRevision::AnthropicMessages2023_06_01`].
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_SSE_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SSE_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_STREAMED_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_STREAMED_TOOL_CALLS: usize = 64;

pub struct AnthropicMessagesConfig {
    pub base_url: String,
    pub messages_path: String,
    /// End-to-end request budget, including the streamed response body.
    pub timeout: Duration,
}

/// Consumed composition input for one immutable native Messages generation.
/// Endpoint/auth-bearing values stay private and are never receipt payloads.
pub struct AnthropicMessagesRouteConfig {
    pub declaration: ProviderRouteDeclaration,
    pub model: String,
    pub auth_source: ProviderAuthSource,
    pub base_url: String,
    pub messages_path: String,
    pub policy: ProviderRoutePolicy,
}

#[derive(Debug, thiserror::Error)]
pub enum AnthropicMessagesRouteBuildError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Contract(#[from] ProviderRouteContractError),
    #[error("native Messages route requires its exact compiled declaration")]
    DeclarationMismatch,
    #[error("native Messages path cannot be empty")]
    EmptyMessagesPath,
    #[error("native Messages retries are not implemented (configured {configured})")]
    UnsupportedInferenceRetries { configured: u32 },
    #[error("native Messages model catalog policy is not implemented")]
    UnsupportedCatalogPolicy,
}

/// A distinct compiled implementation of the Anthropic Messages 2023-06-01
/// request and streaming event dialect.
pub struct AnthropicMessagesProvider {
    http: reqwest::Client,
    base_url: String,
    messages_path: String,
    auth: Arc<dyn AuthProvider>,
    timeout: Duration,
}

impl AnthropicMessagesProvider {
    pub fn new(config: AnthropicMessagesConfig, auth: Arc<dyn AuthProvider>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: config.base_url,
            messages_path: config.messages_path,
            auth,
            timeout: config.timeout,
        }
    }

    fn attach_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        match self.auth.header()? {
            Some((name, value)) => Ok(request.header(name, value)),
            None => Ok(request),
        }
    }

    fn refresh_hint(&self) -> Option<String> {
        self.auth.refresh_hint()
    }
}

/// Freeze auth once and publish a truthful native Messages generation.
/// Retrying and catalog policy are rejected because this compiled adapter does
/// not implement them yet; accepting either would make the snapshot lie.
pub fn build_anthropic_messages_route(
    config: AnthropicMessagesRouteConfig,
    auth: &dyn AuthProvider,
) -> Result<ProviderRouteSnapshot<AnthropicMessagesProvider>, AnthropicMessagesRouteBuildError> {
    let expected =
        ProviderRouteDeclaration::native_messages_v1(config.declaration.profile().clone());
    if config.declaration != expected {
        return Err(AnthropicMessagesRouteBuildError::DeclarationMismatch);
    }
    if config.messages_path.is_empty() {
        return Err(AnthropicMessagesRouteBuildError::EmptyMessagesPath);
    }
    if config.policy.inference_retries() != 0 {
        return Err(
            AnthropicMessagesRouteBuildError::UnsupportedInferenceRetries {
                configured: config.policy.inference_retries(),
            },
        );
    }
    if config.policy.catalog().is_configured() || config.policy.catalog_retries() != 0 {
        return Err(AnthropicMessagesRouteBuildError::UnsupportedCatalogPolicy);
    }

    let frozen_auth: Arc<dyn AuthProvider> = Arc::new(FrozenAuth::resolve(auth)?);
    let runtime = Arc::new(AnthropicMessagesProvider::new(
        AnthropicMessagesConfig {
            base_url: config.base_url,
            messages_path: config.messages_path,
            timeout: config.policy.inference_timeout(),
        },
        frozen_auth,
    ));
    ProviderRouteSnapshot::initial(
        config.declaration,
        config.model,
        config.auth_source,
        config.policy,
        runtime.clone(),
        runtime,
    )
    .map_err(Into::into)
}

#[derive(Debug, Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<MessagesMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<MessagesTool>>,
}

#[derive(Debug, Serialize)]
struct MessagesMessage {
    role: &'static str,
    content: MessagesContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum MessagesContent {
    Text(String),
    Blocks(Vec<MessagesContentBlock>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MessagesContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct MessagesTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

fn unsupported_request(feature: &str) -> ProviderError {
    ProviderError::InvalidResponse(format!(
        "native Messages request option is not supported yet: {feature}"
    ))
}

fn build_body(request: &ChatRequest) -> Result<MessagesRequest, ProviderError> {
    if !request.stream {
        return Err(unsupported_request("non-streaming response"));
    }
    if request.reasoning_effort.is_some() {
        return Err(unsupported_request("reasoning_effort"));
    }
    if request.response_format.is_some() {
        return Err(unsupported_request("response_format"));
    }
    if request.prompt_cache_key.is_some() {
        return Err(unsupported_request("prompt_cache_key"));
    }
    let max_tokens = request
        .max_tokens
        .filter(|value| *value > 0)
        .ok_or_else(|| unsupported_request("missing or zero max_tokens"))?;

    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in &request.messages {
        match message.role {
            Role::System => system.push(message.content.as_str()),
            Role::User => messages.push(MessagesMessage {
                role: "user",
                content: MessagesContent::Text(message.content.clone()),
            }),
            Role::Assistant if message.tool_calls.is_empty() => messages.push(MessagesMessage {
                role: "assistant",
                content: MessagesContent::Text(message.content.clone()),
            }),
            Role::Assistant => {
                let mut blocks = Vec::new();
                if !message.content.is_empty() {
                    blocks.push(MessagesContentBlock::Text {
                        text: message.content.clone(),
                    });
                }
                for call in &message.tool_calls {
                    if call.args.len() > MAX_STREAMED_TOOL_ARGUMENT_BYTES {
                        return Err(ProviderError::ResponseTooLarge {
                            part: ProviderResponsePart::ToolArguments,
                            limit_bytes: MAX_STREAMED_TOOL_ARGUMENT_BYTES,
                        });
                    }
                    let input: serde_json::Value =
                        serde_json::from_str(&call.args).map_err(|_| {
                            ProviderError::InvalidResponse(
                                "native Messages prior tool input is not valid JSON".to_string(),
                            )
                        })?;
                    if !input.is_object() {
                        return Err(ProviderError::InvalidResponse(
                            "native Messages prior tool input must be a JSON object".to_string(),
                        ));
                    }
                    blocks.push(MessagesContentBlock::ToolUse {
                        id: call.call_id.clone(),
                        name: call.tool_name.clone(),
                        input,
                    });
                }
                messages.push(MessagesMessage {
                    role: "assistant",
                    content: MessagesContent::Blocks(blocks),
                });
            }
            Role::Tool => {
                let tool_use_id = message.tool_call_id.clone().ok_or_else(|| {
                    ProviderError::InvalidResponse(
                        "native Messages tool result has no correlation id".to_string(),
                    )
                })?;
                messages.push(MessagesMessage {
                    role: "user",
                    content: MessagesContent::Blocks(vec![MessagesContentBlock::ToolResult {
                        tool_use_id,
                        content: message.content.clone(),
                    }]),
                });
            }
        }
    }
    if messages.is_empty() {
        return Err(unsupported_request(
            "conversation without user/assistant messages",
        ));
    }

    let tools = request
        .tools
        .as_ref()
        .filter(|tools| !tools.is_empty())
        .map(|tools| {
            tools
                .iter()
                .map(|tool| {
                    if !tool.parameters.is_object() {
                        return Err(ProviderError::InvalidResponse(format!(
                            "native Messages tool schema for {} must be a JSON object",
                            tool.name
                        )));
                    }
                    Ok(MessagesTool {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        input_schema: tool.parameters.clone(),
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()
        })
        .transpose()?;

    Ok(MessagesRequest {
        model: request.model.clone(),
        max_tokens,
        messages,
        stream: true,
        system: (!system.is_empty()).then(|| system.join("\n\n")),
        temperature: request.temperature,
        tools,
    })
}

fn map_send_error(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Connection(error.to_string())
    }
}

fn remaining_deadline_millis(started: tokio::time::Instant, timeout: Duration) -> u64 {
    timeout
        .checked_sub(started.elapsed())
        .map(|remaining| remaining.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn status_category(status: StatusCode) -> ProviderAttemptCategory {
    match status {
        StatusCode::TOO_MANY_REQUESTS => ProviderAttemptCategory::RateLimited,
        StatusCode::SERVICE_UNAVAILABLE => ProviderAttemptCategory::Unavailable,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderAttemptCategory::Authentication,
        status if status.is_success() => ProviderAttemptCategory::Success,
        status if status.is_client_error() => ProviderAttemptCategory::ClientError,
        status if status.is_server_error() => ProviderAttemptCategory::ServerError,
        _ => ProviderAttemptCategory::UnexpectedStatus,
    }
}

fn record_attempt(telemetry: Option<&ProviderTelemetrySink>, event: ProviderAttemptEvent) {
    if let Some(telemetry) = telemetry {
        let _ = telemetry.record(event);
    }
}

struct NativeSendAttemptGuard {
    telemetry: Option<ProviderTelemetrySink>,
    started: tokio::time::Instant,
    timeout: Duration,
    armed: bool,
}

impl NativeSendAttemptGuard {
    fn new(
        telemetry: Option<&ProviderTelemetrySink>,
        started: tokio::time::Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            telemetry: telemetry.cloned(),
            started,
            timeout,
            armed: true,
        }
    }

    fn completed(mut self) {
        self.armed = false;
    }
}

impl Drop for NativeSendAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        record_attempt(
            self.telemetry.as_ref(),
            ProviderAttemptEvent {
                attempt: 1,
                max_attempts: 1,
                category: ProviderAttemptCategory::Cancelled,
                status: None,
                outcome: ProviderAttemptOutcome::Cancelled,
                terminal: true,
                retry_delay_ms: None,
                retry_after_used: false,
                remaining_deadline_ms: remaining_deadline_millis(self.started, self.timeout),
            },
        );
    }
}

struct NativeBodyAttemptGuard {
    telemetry: Option<ProviderTelemetrySink>,
    status: u16,
    started: tokio::time::Instant,
    timeout: Duration,
    armed: bool,
}

impl NativeBodyAttemptGuard {
    fn new(
        telemetry: Option<&ProviderTelemetrySink>,
        status: u16,
        started: tokio::time::Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            telemetry: telemetry.cloned(),
            status,
            started,
            timeout,
            armed: true,
        }
    }

    fn completed(mut self) {
        self.record(
            ProviderAttemptCategory::Success,
            ProviderAttemptOutcome::BodyCompleted,
        );
        self.armed = false;
    }

    fn failed(mut self, error: &ProviderError) {
        let category = match error {
            ProviderError::Timeout => ProviderAttemptCategory::Timeout,
            ProviderError::Connection(_) => ProviderAttemptCategory::Connection,
            ProviderError::ResponseTooLarge { .. } => ProviderAttemptCategory::ResponseTooLarge,
            _ => ProviderAttemptCategory::InvalidResponse,
        };
        self.record(category, ProviderAttemptOutcome::BodyFailed);
        self.armed = false;
    }

    fn record(&self, category: ProviderAttemptCategory, outcome: ProviderAttemptOutcome) {
        record_attempt(
            self.telemetry.as_ref(),
            ProviderAttemptEvent {
                attempt: 1,
                max_attempts: 1,
                category,
                status: Some(self.status),
                outcome,
                terminal: true,
                retry_delay_ms: None,
                retry_after_used: false,
                remaining_deadline_ms: remaining_deadline_millis(self.started, self.timeout),
            },
        );
    }
}

impl Drop for NativeBodyAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.record(
            ProviderAttemptCategory::Cancelled,
            ProviderAttemptOutcome::Cancelled,
        );
    }
}

async fn read_error_body(response: reqwest::Response) -> Result<String, ProviderError> {
    if response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .is_some_and(|length| length > MAX_ERROR_BODY_BYTES)
    {
        return Err(ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::ErrorBody,
            limit_bytes: MAX_ERROR_BODY_BYTES,
        });
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_send_error)?;
        if body.len().saturating_add(chunk.len()) > MAX_ERROR_BODY_BYTES {
            return Err(ProviderError::ResponseTooLarge {
                part: ProviderResponsePart::ErrorBody,
                limit_bytes: MAX_ERROR_BODY_BYTES,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

struct MessagesToolAcc {
    id: String,
    name: String,
    initial_input: serde_json::Value,
    args: String,
    saw_delta: bool,
}

fn event_index(value: &serde_json::Value) -> Result<usize, ProviderError> {
    value
        .get("index")
        .and_then(serde_json::Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| {
            ProviderError::InvalidResponse(
                "native Messages content event has no bounded index".to_string(),
            )
        })
}

impl MessagesToolAcc {
    fn finish(self) -> Result<ToolCall, ProviderError> {
        let args = if self.saw_delta {
            self.args
        } else {
            serde_json::to_string(&self.initial_input).map_err(|_| {
                ProviderError::InvalidResponse(
                    "native Messages tool input could not be serialized".to_string(),
                )
            })?
        };
        if args.len() > MAX_STREAMED_TOOL_ARGUMENT_BYTES {
            return Err(ProviderError::ResponseTooLarge {
                part: ProviderResponsePart::ToolArguments,
                limit_bytes: MAX_STREAMED_TOOL_ARGUMENT_BYTES,
            });
        }
        let input: serde_json::Value = serde_json::from_str(&args).map_err(|_| {
            ProviderError::InvalidResponse(
                "native Messages streamed tool input is not valid JSON".to_string(),
            )
        })?;
        if !input.is_object() {
            return Err(ProviderError::InvalidResponse(
                "native Messages streamed tool input must be a JSON object".to_string(),
            ));
        }
        Ok(ToolCall {
            tool_name: self.name,
            args,
            call_id: self.id,
        })
    }
}

#[derive(Default)]
struct MessagesSseDecoder {
    buf: Vec<u8>,
    received_bytes: usize,
    message_started: bool,
    saw_semantic_content: bool,
    saw_message_stop: bool,
    finish: Option<FinishReason>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    tool_calls: Vec<Option<MessagesToolAcc>>,
}

impl MessagesSseDecoder {
    fn push_bytes(&mut self, bytes: &[u8]) -> Result<Vec<ChatDelta>, ProviderError> {
        self.received_bytes = self
            .received_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_SSE_STREAM_BYTES)
            .ok_or(ProviderError::ResponseTooLarge {
                part: ProviderResponsePart::SseStream,
                limit_bytes: MAX_SSE_STREAM_BYTES,
            })?;

        let mut output = Vec::new();
        for segment in bytes.split_inclusive(|byte| *byte == b'\n') {
            if self.buf.len().saturating_add(segment.len()) > MAX_SSE_LINE_BYTES {
                return Err(ProviderError::ResponseTooLarge {
                    part: ProviderResponsePart::SseLine,
                    limit_bytes: MAX_SSE_LINE_BYTES,
                });
            }
            self.buf.extend_from_slice(segment);
            if segment.last() == Some(&b'\n') {
                let line = std::mem::take(&mut self.buf);
                let line = String::from_utf8_lossy(&line);
                self.process_line(line.trim(), &mut output)?;
            }
        }
        Ok(output)
    }

    fn process_line(
        &mut self,
        line: &str,
        output: &mut Vec<ChatDelta>,
    ) -> Result<(), ProviderError> {
        if self.saw_message_stop || line.is_empty() || line.starts_with("event:") {
            return Ok(());
        }
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(());
        };
        let value: serde_json::Value = serde_json::from_str(data.trim()).map_err(|_| {
            ProviderError::InvalidResponse(
                "malformed JSON in native Messages SSE data record".to_string(),
            )
        })?;
        let event_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                ProviderError::InvalidResponse(
                    "native Messages SSE record has no string type".to_string(),
                )
            })?;

        match event_type {
            "message_start" => {
                if self.message_started {
                    return Err(ProviderError::InvalidResponse(
                        "native Messages stream repeated message_start".to_string(),
                    ));
                }
                self.message_started = true;
                self.input_tokens = value
                    .pointer("/message/usage/input_tokens")
                    .and_then(serde_json::Value::as_u64);
                self.output_tokens = value
                    .pointer("/message/usage/output_tokens")
                    .and_then(serde_json::Value::as_u64);
            }
            "content_block_start" => {
                self.require_started(event_type)?;
                match value
                    .pointer("/content_block/type")
                    .and_then(serde_json::Value::as_str)
                {
                    Some("text") => {}
                    Some("tool_use") => {
                        let index = event_index(&value)?;
                        if index >= MAX_STREAMED_TOOL_CALLS {
                            return Err(ProviderError::InvalidResponse(
                                "native Messages exceeded the streamed tool-call count".to_string(),
                            ));
                        }
                        if self.tool_calls.len() <= index {
                            self.tool_calls.resize_with(index + 1, || None);
                        }
                        if self.tool_calls[index].is_some() {
                            return Err(ProviderError::InvalidResponse(
                                "native Messages repeated a tool-use block index".to_string(),
                            ));
                        }
                        let id = value
                            .pointer("/content_block/id")
                            .and_then(serde_json::Value::as_str)
                            .filter(|id| !id.is_empty())
                            .ok_or_else(|| {
                                ProviderError::InvalidResponse(
                                    "native Messages tool-use block has no id".to_string(),
                                )
                            })?;
                        let name = value
                            .pointer("/content_block/name")
                            .and_then(serde_json::Value::as_str)
                            .filter(|name| !name.is_empty())
                            .ok_or_else(|| {
                                ProviderError::InvalidResponse(
                                    "native Messages tool-use block has no name".to_string(),
                                )
                            })?;
                        let initial_input = value
                            .pointer("/content_block/input")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}));
                        if !initial_input.is_object() {
                            return Err(ProviderError::InvalidResponse(
                                "native Messages tool-use input must be a JSON object".to_string(),
                            ));
                        }
                        self.tool_calls[index] = Some(MessagesToolAcc {
                            id: id.to_string(),
                            name: name.to_string(),
                            initial_input,
                            args: String::new(),
                            saw_delta: false,
                        });
                    }
                    _ => {
                        return Err(ProviderError::InvalidResponse(
                            "native Messages content block has an unsupported type".to_string(),
                        ));
                    }
                }
            }
            "content_block_stop" => {
                self.require_started(event_type)?;
                let index = event_index(&value)?;
                if let Some(acc) = self.tool_calls.get_mut(index).and_then(Option::take) {
                    output.push(ChatDelta::ToolCall(acc.finish()?));
                    self.saw_semantic_content = true;
                }
            }
            "content_block_delta" => {
                match value
                    .pointer("/delta/type")
                    .and_then(serde_json::Value::as_str)
                {
                    Some("text_delta") => {
                        self.require_started(event_type)?;
                        let text = value
                            .pointer("/delta/text")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| {
                                ProviderError::InvalidResponse(
                                    "native Messages text_delta has no text".to_string(),
                                )
                            })?;
                        if !text.is_empty() {
                            self.saw_semantic_content = true;
                            output.push(ChatDelta::Text(text.to_string()));
                        }
                    }
                    Some("input_json_delta") => {
                        let index = event_index(&value)?;
                        let partial = value
                            .pointer("/delta/partial_json")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| {
                                ProviderError::InvalidResponse(
                                    "native Messages input_json_delta has no partial_json"
                                        .to_string(),
                                )
                            })?;
                        if partial.len() > MAX_STREAMED_TOOL_ARGUMENT_BYTES {
                            return Err(ProviderError::ResponseTooLarge {
                                part: ProviderResponsePart::ToolArguments,
                                limit_bytes: MAX_STREAMED_TOOL_ARGUMENT_BYTES,
                            });
                        }
                        self.require_started(event_type)?;
                        let acc = self
                            .tool_calls
                            .get_mut(index)
                            .and_then(Option::as_mut)
                            .ok_or_else(|| {
                                ProviderError::InvalidResponse(
                                    "native Messages tool delta has no open tool-use block"
                                        .to_string(),
                                )
                            })?;
                        if !acc.saw_delta
                            && acc
                                .initial_input
                                .as_object()
                                .is_some_and(|input| !input.is_empty())
                        {
                            return Err(ProviderError::InvalidResponse(
                                "native Messages mixed complete and incremental tool input"
                                    .to_string(),
                            ));
                        }
                        if acc.args.len().saturating_add(partial.len())
                            > MAX_STREAMED_TOOL_ARGUMENT_BYTES
                        {
                            return Err(ProviderError::ResponseTooLarge {
                                part: ProviderResponsePart::ToolArguments,
                                limit_bytes: MAX_STREAMED_TOOL_ARGUMENT_BYTES,
                            });
                        }
                        acc.args.push_str(partial);
                        acc.saw_delta = true;
                    }
                    _ => {
                        return Err(ProviderError::InvalidResponse(
                            "native Messages content delta has an unsupported type".to_string(),
                        ));
                    }
                }
            }
            "message_delta" => {
                self.require_started(event_type)?;
                self.output_tokens = value
                    .pointer("/usage/output_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .or(self.output_tokens);
                let stop_reason = value
                    .pointer("/delta/stop_reason")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ProviderError::InvalidResponse(
                            "native Messages message_delta has no stop_reason".to_string(),
                        )
                    })?;
                self.finish = Some(match stop_reason {
                    "max_tokens" => FinishReason::Length,
                    "tool_use" => FinishReason::ToolCalls,
                    "end_turn" | "stop_sequence" => FinishReason::Stop,
                    _ => {
                        return Err(ProviderError::InvalidResponse(format!(
                            "native Messages returned unsupported stop_reason: {stop_reason}"
                        )));
                    }
                });
            }
            "message_stop" => {
                self.require_started(event_type)?;
                if self.tool_calls.iter().any(Option::is_some) {
                    return Err(ProviderError::InvalidResponse(
                        "native Messages ended with an open tool-use block".to_string(),
                    ));
                }
                if !self.saw_semantic_content {
                    return Err(ProviderError::InvalidResponse(
                        "native Messages ended without semantic content".to_string(),
                    ));
                }
                let finish = self.finish.ok_or_else(|| {
                    ProviderError::InvalidResponse(
                        "native Messages ended without an explicit stop_reason".to_string(),
                    )
                })?;
                if self.input_tokens.is_some() || self.output_tokens.is_some() {
                    let prompt_tokens = self.input_tokens.unwrap_or(0);
                    let completion_tokens = self.output_tokens.unwrap_or(0);
                    let total_tokens =
                        prompt_tokens
                            .checked_add(completion_tokens)
                            .ok_or_else(|| {
                                ProviderError::InvalidResponse(
                                    "native Messages usage counters overflowed".to_string(),
                                )
                            })?;
                    output.push(ChatDelta::Usage(Usage {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        prompt_tokens_details: None,
                    }));
                }
                output.push(ChatDelta::Done(finish));
                self.saw_message_stop = true;
            }
            "ping" => {}
            "error" => {
                let detail = value
                    .pointer("/error/message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("provider returned a native Messages error event");
                return Err(ProviderError::InvalidResponse(format!(
                    "provider error in native Messages stream: {detail}"
                )));
            }
            _ => {
                return Err(ProviderError::InvalidResponse(format!(
                    "unsupported native Messages SSE event: {event_type}"
                )));
            }
        }
        Ok(())
    }

    fn require_started(&self, event_type: &str) -> Result<(), ProviderError> {
        if self.message_started {
            Ok(())
        } else {
            Err(ProviderError::InvalidResponse(format!(
                "native Messages {event_type} arrived before message_start"
            )))
        }
    }

    fn finish_eof(mut self) -> Result<Vec<ChatDelta>, ProviderError> {
        let mut output = Vec::new();
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            self.process_line(line.trim(), &mut output)?;
        }
        if !self.saw_message_stop {
            return Err(ProviderError::InvalidResponse(
                "native Messages stream ended without message_stop".to_string(),
            ));
        }
        Ok(output)
    }
}

type ByteStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send>>;

enum MessagesSseState {
    Reading {
        stream: ByteStream,
        decoder: MessagesSseDecoder,
        queue: VecDeque<ChatDelta>,
        attempt_guard: NativeBodyAttemptGuard,
    },
    Draining {
        queue: VecDeque<ChatDelta>,
    },
    Done,
}

async fn messages_sse_step(
    state: MessagesSseState,
) -> Option<(Result<ChatDelta, ProviderError>, MessagesSseState)> {
    match state {
        MessagesSseState::Reading {
            mut stream,
            mut decoder,
            mut queue,
            attempt_guard,
        } => {
            if let Some(delta) = queue.pop_front() {
                return Some((
                    Ok(delta),
                    MessagesSseState::Reading {
                        stream,
                        decoder,
                        queue,
                        attempt_guard,
                    },
                ));
            }
            let attempt_guard = attempt_guard;
            loop {
                match stream.next().await {
                    Some(Ok(bytes)) => match decoder.push_bytes(&bytes) {
                        Ok(deltas) => {
                            queue.extend(deltas);
                            if decoder.saw_message_stop {
                                attempt_guard.completed();
                                return queue.pop_front().map(|delta| {
                                    (Ok(delta), MessagesSseState::Draining { queue })
                                });
                            }
                            if let Some(delta) = queue.pop_front() {
                                return Some((
                                    Ok(delta),
                                    MessagesSseState::Reading {
                                        stream,
                                        decoder,
                                        queue,
                                        attempt_guard,
                                    },
                                ));
                            }
                        }
                        Err(error) => {
                            attempt_guard.failed(&error);
                            return Some((Err(error), MessagesSseState::Done));
                        }
                    },
                    Some(Err(error)) => {
                        let error = map_send_error(error);
                        attempt_guard.failed(&error);
                        return Some((Err(error), MessagesSseState::Done));
                    }
                    None => {
                        return match decoder.finish_eof() {
                            Ok(deltas) => {
                                attempt_guard.completed();
                                queue.extend(deltas);
                                queue
                                    .pop_front()
                                    .map(|delta| (Ok(delta), MessagesSseState::Draining { queue }))
                            }
                            Err(error) => {
                                attempt_guard.failed(&error);
                                Some((Err(error), MessagesSseState::Done))
                            }
                        };
                    }
                }
            }
        }
        MessagesSseState::Draining { mut queue } => queue
            .pop_front()
            .map(|delta| (Ok(delta), MessagesSseState::Draining { queue })),
        MessagesSseState::Done => None,
    }
}

impl Provider for AnthropicMessagesProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        // Anthropic exposes no harmless universal discovery endpoint. Until a
        // route-specific probe is composed, validate only that auth resolves.
        self.auth.header().map(|_| ()).map_err(ProviderError::Auth)
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream, ProviderError> {
        self.chat_stream_with_telemetry(request, None).await
    }

    async fn chat_stream_with_telemetry(
        &self,
        request: ChatRequest,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<ChatStream, ProviderError> {
        let body = build_body(&request)?;
        if self.messages_path.is_empty() {
            return Err(unsupported_request("empty messages path"));
        }
        let url = format!("{}{}", self.base_url, self.messages_path);
        let request = self
            .http
            .post(url)
            .timeout(self.timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);
        let request = self.attach_auth(request)?;
        let started = tokio::time::Instant::now();
        let send_guard = NativeSendAttemptGuard::new(telemetry.as_ref(), started, self.timeout);
        let send_result = request.send().await;
        send_guard.completed();
        let response = match send_result {
            Ok(response) => response,
            Err(error) => {
                let error = map_send_error(error);
                record_attempt(
                    telemetry.as_ref(),
                    ProviderAttemptEvent {
                        attempt: 1,
                        max_attempts: 1,
                        category: match error {
                            ProviderError::Timeout => ProviderAttemptCategory::Timeout,
                            _ => ProviderAttemptCategory::Connection,
                        },
                        status: None,
                        outcome: ProviderAttemptOutcome::TransportFailed,
                        terminal: true,
                        retry_delay_ms: None,
                        retry_after_used: false,
                        remaining_deadline_ms: remaining_deadline_millis(started, self.timeout),
                    },
                );
                return Err(error);
            }
        };
        let status = response.status();
        if !status.is_success() {
            record_attempt(
                telemetry.as_ref(),
                ProviderAttemptEvent {
                    attempt: 1,
                    max_attempts: 1,
                    category: status_category(status),
                    status: Some(status.as_u16()),
                    outcome: ProviderAttemptOutcome::Rejected,
                    terminal: true,
                    retry_delay_ms: None,
                    retry_after_used: false,
                    remaining_deadline_ms: remaining_deadline_millis(started, self.timeout),
                },
            );
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ProviderError::Unauthorized {
                status: status.as_u16(),
                hint: self.refresh_hint(),
            });
        }
        if !status.is_success() {
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: read_error_body(response).await?,
            });
        }

        let attempt_guard =
            NativeBodyAttemptGuard::new(telemetry.as_ref(), status.as_u16(), started, self.timeout);

        let stream: ByteStream = response
            .bytes_stream()
            .map(|result| result.map(|bytes| bytes.to_vec()))
            .boxed();
        Ok(stream::unfold(
            MessagesSseState::Reading {
                stream,
                decoder: MessagesSseDecoder::default(),
                queue: VecDeque::new(),
                attempt_guard,
            },
            messages_sse_step,
        )
        .boxed())
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        Ok(None)
    }
}

#[cfg(test)]
mod route_factory_tests {
    use super::*;
    use std::sync::Mutex;

    use reqwest::header::{HeaderName, HeaderValue};

    use crate::error::ExpiryStatus;
    use crate::route::{ProviderCatalogPolicy, ProviderProfileId};

    struct MutableAuth {
        value: Mutex<String>,
    }

    impl AuthProvider for MutableAuth {
        fn header(&self) -> Result<Option<(HeaderName, HeaderValue)>, AuthError> {
            let raw = self.value.lock().unwrap().clone();
            let mut value = HeaderValue::from_str(&raw).unwrap();
            value.set_sensitive(true);
            Ok(Some((HeaderName::from_static("x-fixture-auth"), value)))
        }

        fn expiry_status(&self) -> ExpiryStatus {
            ExpiryStatus::Unknown
        }
    }

    #[test]
    fn native_route_factory_freezes_auth_once() {
        let auth = MutableAuth {
            value: Mutex::new("generation-one".to_string()),
        };
        let policy = ProviderRoutePolicy::new(
            0,
            0,
            Duration::from_secs(2),
            Duration::from_secs(2),
            Duration::from_secs(1),
            ProviderCatalogPolicy::Unavailable,
        )
        .unwrap();
        let snapshot = build_anthropic_messages_route(
            AnthropicMessagesRouteConfig {
                declaration: ProviderRouteDeclaration::native_messages_v1(
                    ProviderProfileId::derive_v1(b"native-test", b"frozen-auth").unwrap(),
                ),
                model: "model-a".to_string(),
                auth_source: ProviderAuthSource::Environment,
                base_url: "http://127.0.0.1:1".to_string(),
                messages_path: "/v1/messages".to_string(),
                policy,
            },
            &auth,
        )
        .unwrap();
        *auth.value.lock().unwrap() = "generation-two".to_string();

        let (_, header) = snapshot
            .inference_runtime()
            .unwrap()
            .auth
            .header()
            .unwrap()
            .unwrap();
        assert_eq!(header.as_bytes(), b"generation-one");
        assert!(header.is_sensitive());
        assert_eq!(snapshot.identity().generation().get(), 1);
    }
}
