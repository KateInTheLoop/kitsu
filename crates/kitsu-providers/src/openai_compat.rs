//! OpenAI-compatible provider. Covers vLLM, OpenAI, Mistral cloud, AND the example
//! AI gateway — they share one wire shape; only base_url/path/auth differ
//! (design §1, §4).

use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::AuthProvider;
use crate::error::{AuthError, ProviderError};
use crate::types::{
    ChatDelta, ChatMessage, ChatRequest, FinishReason, Role, ToolCall, ToolDescriptor,
};
use crate::{ChatStream, Provider};

#[cfg(test)]
#[path = "openai_compat_tests.rs"]
mod openai_compat_tests;

pub struct OpenAICompatProvider {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) chat_path: String,
    pub(crate) models_path: Option<String>,
    pub(crate) auth: Arc<dyn AuthProvider>,
    pub(crate) retries: u32,
    pub(crate) timeout: Duration,
}

/// Builder inputs for constructing a gateway-backed provider.
pub struct OpenAICompatConfig {
    pub base_url: String,
    pub chat_path: String,
    pub models_path: Option<String>,
    pub retries: u32,
    pub timeout: Duration,
}

impl OpenAICompatProvider {
    pub fn new(cfg: OpenAICompatConfig, auth: Arc<dyn AuthProvider>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: cfg.base_url,
            chat_path: cfg.chat_path,
            models_path: cfg.models_path,
            auth,
            retries: cfg.retries,
            timeout: cfg.timeout,
        }
    }

    /// Resolve the refresh hint surfaced on a gateway 401/403.
    fn refresh_hint(&self) -> Option<String> {
        match self.auth.header() {
            Err(AuthError::Missing { hint, .. }) => hint,
            _ => None,
        }
    }

    /// Attach the auth header to a request builder. A `NoAuth` sentinel
    /// (`AuthError::Missing` with an empty `env`) means "attach nothing" and is
    /// skipped silently; any other auth error is a hard failure.
    fn attach_auth(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        match self.auth.header() {
            Ok((name, value)) => Ok(rb.header(name, value)),
            Err(AuthError::Missing { ref env, .. }) if env.is_empty() => Ok(rb),
            Err(e) => Err(ProviderError::Auth(e)),
        }
    }
}

// --- wire types (internal to this module) -----------------------------------

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool<'a>>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'a str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall<'a>>>,
}

#[derive(Serialize)]
struct WireToolCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolCallFn<'a>,
}

#[derive(Serialize)]
struct WireToolCallFn<'a> {
    name: &'a str,
    arguments: &'a str,
}

#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolFn<'a>,
}

#[derive(Serialize)]
struct WireToolFn<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn build_body<'a>(req: &'a ChatRequest) -> WireRequest<'a> {
    let messages = req
        .messages
        .iter()
        .map(|m: &ChatMessage| {
            let tool_calls = if m.tool_calls.is_empty() {
                None
            } else {
                Some(
                    m.tool_calls
                        .iter()
                        .map(|tc| WireToolCall {
                            id: &tc.call_id,
                            kind: "function",
                            function: WireToolCallFn {
                                name: &tc.tool_name,
                                arguments: &tc.args,
                            },
                        })
                        .collect(),
                )
            };
            WireMessage {
                role: role_str(m.role),
                content: &m.content,
                tool_call_id: m.tool_call_id.as_deref(),
                tool_calls,
            }
        })
        .collect();

    let tools = req.tools.as_ref().map(|ts| {
        ts.iter()
            .map(|t: &ToolDescriptor| WireTool {
                kind: "function",
                function: WireToolFn {
                    name: &t.name,
                    description: &t.description,
                    parameters: &t.parameters,
                },
            })
            .collect()
    });

    WireRequest {
        model: &req.model,
        messages,
        tools,
        stream: req.stream,
        temperature: req.temperature,
        max_tokens: req.max_tokens,
    }
}

// --- streaming/non-streaming response wire types ----------------------------

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<DeltaToolCall>,
}

#[derive(Deserialize)]
struct DeltaToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<DeltaToolCallFn>,
}

#[derive(Deserialize)]
struct DeltaToolCallFn {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct CompletionResponse {
    #[serde(default)]
    choices: Vec<CompletionChoice>,
}

#[derive(Deserialize)]
struct CompletionChoice {
    #[serde(default)]
    message: CompletionMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct CompletionMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<CompletionToolCall>,
}

#[derive(Deserialize)]
struct CompletionToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<DeltaToolCallFn>,
}

fn finish_from_str(s: &str) -> FinishReason {
    match s {
        "length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    }
}

/// Accumulates streamed tool-call deltas by index.
#[derive(Default)]
struct ToolCallAcc {
    id: String,
    name: String,
    args: String,
}

impl ToolCallAcc {
    fn into_tool_call(self) -> ToolCall {
        ToolCall {
            tool_name: self.name,
            args: self.args,
            call_id: self.id,
        }
    }
}

/// Parse a full SSE body into a vec of ChatDelta. We buffer the whole body
/// (wiremock returns it at once; real gateways stream it) then split on lines.
fn parse_sse(body: &str) -> Vec<ChatDelta> {
    let mut out: Vec<ChatDelta> = Vec::new();
    let mut accs: Vec<ToolCallAcc> = Vec::new();
    let mut finish: Option<FinishReason> = None;

    for raw_line in body.lines() {
        let line = raw_line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        if data.is_empty() {
            continue;
        }
        let chunk: StreamChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(_) => continue, // tolerate keep-alives / non-JSON comments
        };
        let Some(choice) = chunk.choices.into_iter().next() else {
            continue;
        };
        if let Some(text) = choice.delta.content {
            if !text.is_empty() {
                out.push(ChatDelta::Text(text));
            }
        }
        for dtc in choice.delta.tool_calls {
            if accs.len() <= dtc.index {
                accs.resize_with(dtc.index + 1, ToolCallAcc::default);
            }
            let acc = &mut accs[dtc.index];
            if let Some(id) = dtc.id {
                if !id.is_empty() {
                    acc.id = id;
                }
            }
            if let Some(f) = dtc.function {
                if let Some(name) = f.name {
                    if !name.is_empty() {
                        acc.name = name;
                    }
                }
                if let Some(args) = f.arguments {
                    acc.args.push_str(&args);
                }
            }
        }
        if let Some(fr) = choice.finish_reason {
            finish = Some(finish_from_str(&fr));
        }
    }

    for acc in accs {
        if !acc.name.is_empty() || !acc.id.is_empty() {
            out.push(ChatDelta::ToolCall(acc.into_tool_call()));
        }
    }
    out.push(ChatDelta::Done(finish.unwrap_or(FinishReason::Stop)));
    out
}

/// Parse a non-streaming completion JSON into a vec of ChatDelta.
fn parse_completion(body: &str) -> Result<Vec<ChatDelta>, ProviderError> {
    let resp: CompletionResponse = serde_json::from_str(body)
        .map_err(|e| ProviderError::InvalidResponse(format!("malformed completion JSON: {e}")))?;
    let mut out = Vec::new();
    let finish = match resp.choices.into_iter().next() {
        Some(choice) => {
            if let Some(text) = choice.message.content {
                if !text.is_empty() {
                    out.push(ChatDelta::Text(text));
                }
            }
            for tc in choice.message.tool_calls {
                let f = tc.function.unwrap_or(DeltaToolCallFn {
                    name: None,
                    arguments: None,
                });
                out.push(ChatDelta::ToolCall(ToolCall {
                    tool_name: f.name.unwrap_or_default(),
                    args: f.arguments.unwrap_or_default(),
                    call_id: tc.id.unwrap_or_default(),
                }));
            }
            choice
                .finish_reason
                .as_deref()
                .map(finish_from_str)
                .unwrap_or(FinishReason::Stop)
        }
        None => FinishReason::Stop,
    };
    out.push(ChatDelta::Done(finish));
    Ok(out)
}

fn map_send_error(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Connection(e.to_string())
    }
}

impl Provider for OpenAICompatProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        // Degrade when there is no /models endpoint: just verify an auth token
        // is present (NoAuth sentinel is fine — nothing to check).
        let degraded = self
            .models_path
            .as_deref()
            .map(|p| p.is_empty())
            .unwrap_or(true);
        if degraded {
            match self.auth.header() {
                Ok(_) => Ok(()),
                Err(AuthError::Missing { ref env, .. }) if env.is_empty() => Ok(()),
                Err(e) => Err(ProviderError::Auth(e)),
            }
        } else {
            self.list_models().await.map(|_| ())
        }
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        let url = format!("{}{}", self.base_url, self.chat_path);
        let body = build_body(&req);

        // Retry loop for transient 429/503; other outcomes return immediately.
        let mut attempt: u32 = 0;
        let response = loop {
            let rb = self
                .http
                .post(&url)
                .timeout(self.timeout)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&body);
            let rb = self.attach_auth(rb)?;

            match rb.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if (status == StatusCode::TOO_MANY_REQUESTS
                        || status == StatusCode::SERVICE_UNAVAILABLE)
                        && attempt < self.retries
                    {
                        attempt += 1;
                        let backoff = Duration::from_millis(200 * (1 << (attempt - 1)).min(8));
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    break resp;
                }
                Err(e) => {
                    if e.is_timeout() && attempt < self.retries {
                        attempt += 1;
                        continue;
                    }
                    return Err(map_send_error(e));
                }
            }
        };

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ProviderError::Unauthorized {
                status: status.as_u16(),
                hint: self.refresh_hint(),
            });
        }
        if !status.is_success() {
            // Read body for diagnostics; never contains the (sensitive) token.
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body,
            });
        }

        let text = response.text().await.map_err(map_send_error)?;

        let deltas = if req.stream {
            parse_sse(&text)
        } else {
            parse_completion(&text)?
        };

        let s = stream::iter(deltas.into_iter().map(Ok));
        Ok(s.boxed())
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        let Some(path) = self.models_path.as_deref().filter(|p| !p.is_empty()) else {
            return Ok(None);
        };
        let url = format!("{}{}", self.base_url, path);
        let rb = self.http.get(&url).timeout(self.timeout);
        let rb = self.attach_auth(rb)?;
        let resp = rb.send().await.map_err(map_send_error)?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ProviderError::Unauthorized {
                status: status.as_u16(),
                hint: self.refresh_hint(),
            });
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body,
            });
        }

        #[derive(Deserialize)]
        struct ModelsResponse {
            #[serde(default)]
            data: Vec<ModelEntry>,
        }
        #[derive(Deserialize)]
        struct ModelEntry {
            id: String,
        }

        let text = resp.text().await.map_err(map_send_error)?;
        let parsed: ModelsResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::InvalidResponse(format!("malformed /models JSON: {e}")))?;
        Ok(Some(parsed.data.into_iter().map(|m| m.id).collect()))
    }
}
