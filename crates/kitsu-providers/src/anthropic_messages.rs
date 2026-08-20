//! Native Anthropic Messages wire adapter.
//!
//! This first compiled slice deliberately supports only streamed text. Tool
//! translation, retries, catalog discovery, route publication, and live CLI
//! selection remain disabled until their own conformance fixtures pass.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use reqwest::StatusCode;
use serde::Serialize;

use crate::auth::AuthProvider;
use crate::{
    ChatDelta, ChatRequest, ChatStream, FinishReason, Provider, ProviderError,
    ProviderResponsePart, Role, Usage,
};

/// Wire revision pinned by [`crate::ProviderWireRevision::AnthropicMessages2023_06_01`].
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_SSE_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SSE_STREAM_BYTES: usize = 16 * 1024 * 1024;

pub struct AnthropicMessagesConfig {
    pub base_url: String,
    pub messages_path: String,
    /// End-to-end request budget, including the streamed response body.
    pub timeout: Duration,
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
}

#[derive(Debug, Serialize)]
struct MessagesMessage {
    role: &'static str,
    content: String,
}

fn unsupported_request(feature: &str) -> ProviderError {
    ProviderError::InvalidResponse(format!(
        "native Messages request option is not supported yet: {feature}"
    ))
}

fn build_body(request: &ChatRequest) -> Result<MessagesRequest, ProviderError> {
    if request
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty())
        || request.messages.iter().any(|message| {
            message.role == Role::Tool
                || message.tool_call_id.is_some()
                || !message.tool_calls.is_empty()
        })
    {
        return Err(ProviderError::ToolsNotSupported);
    }
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
                content: message.content.clone(),
            }),
            Role::Assistant => messages.push(MessagesMessage {
                role: "assistant",
                content: message.content.clone(),
            }),
            Role::Tool => return Err(ProviderError::ToolsNotSupported),
        }
    }
    if messages.is_empty() {
        return Err(unsupported_request(
            "conversation without user/assistant messages",
        ));
    }

    Ok(MessagesRequest {
        model: request.model.clone(),
        max_tokens,
        messages,
        stream: true,
        system: (!system.is_empty()).then(|| system.join("\n\n")),
        temperature: request.temperature,
    })
}

fn map_send_error(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Connection(error.to_string())
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
            "content_block_start" | "content_block_stop" => {
                self.require_started(event_type)?;
                if value
                    .pointer("/content_block/type")
                    .and_then(serde_json::Value::as_str)
                    == Some("tool_use")
                {
                    return Err(ProviderError::ToolsNotSupported);
                }
            }
            "content_block_delta" => {
                self.require_started(event_type)?;
                match value
                    .pointer("/delta/type")
                    .and_then(serde_json::Value::as_str)
                {
                    Some("text_delta") => {
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
                    Some("input_json_delta") => return Err(ProviderError::ToolsNotSupported),
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
        } => {
            if let Some(delta) = queue.pop_front() {
                return Some((
                    Ok(delta),
                    MessagesSseState::Reading {
                        stream,
                        decoder,
                        queue,
                    },
                ));
            }
            loop {
                match stream.next().await {
                    Some(Ok(bytes)) => match decoder.push_bytes(&bytes) {
                        Ok(deltas) => {
                            queue.extend(deltas);
                            if decoder.saw_message_stop {
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
                                    },
                                ));
                            }
                        }
                        Err(error) => {
                            return Some((Err(error), MessagesSseState::Done));
                        }
                    },
                    Some(Err(error)) => {
                        return Some((Err(map_send_error(error)), MessagesSseState::Done));
                    }
                    None => {
                        return match decoder.finish_eof() {
                            Ok(deltas) => {
                                queue.extend(deltas);
                                queue
                                    .pop_front()
                                    .map(|delta| (Ok(delta), MessagesSseState::Draining { queue }))
                            }
                            Err(error) => Some((Err(error), MessagesSseState::Done)),
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
        let response = self
            .attach_auth(request)?
            .send()
            .await
            .map_err(map_send_error)?;
        let status = response.status();
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

        let stream: ByteStream = response
            .bytes_stream()
            .map(|result| result.map(|bytes| bytes.to_vec()))
            .boxed();
        Ok(stream::unfold(
            MessagesSseState::Reading {
                stream,
                decoder: MessagesSseDecoder::default(),
                queue: VecDeque::new(),
            },
            messages_sse_step,
        )
        .boxed())
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        Ok(None)
    }
}
