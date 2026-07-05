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
    ChatDelta, ChatMessage, ChatRequest, FinishReason, Role, ToolCall, ToolDescriptor, Usage,
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
    // Ask the gateway to include a final usage chunk when streaming. Harmless
    // when unsupported (extra field ignored); only sent for stream requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
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
        stream_options: if req.stream {
            Some(StreamOptions {
                include_usage: true,
            })
        } else {
            None
        },
        temperature: req.temperature,
        max_tokens: req.max_tokens,
    }
}

// --- streaming/non-streaming response wire types ----------------------------

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    // Most gateways emit `usage` only on the final chunk (and only when
    // stream_options.include_usage is set); tolerate it being absent on every
    // chunk. When present we surface it as a ChatDelta::Usage.
    #[serde(default)]
    usage: Option<Usage>,
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
    #[serde(default)]
    usage: Option<Usage>,
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

/// Incremental SSE decoder. Fed raw byte chunks as they arrive off the wire
/// (any chunk boundary — mid-line or mid-UTF8 is fine: we buffer bytes and only
/// decode complete `\n`-terminated lines). Text deltas are emitted as soon as
/// their line completes; tool-call fragments accumulate and flush at stream end.
#[derive(Default)]
struct SseDecoder {
    /// Raw byte buffer; complete lines are drained as `\n` is seen.
    buf: Vec<u8>,
    accs: Vec<ToolCallAcc>,
    finish: Option<FinishReason>,
    /// Usage from the final chunk, if the gateway reported it.
    usage: Option<Usage>,
    /// Set once `data: [DONE]` is seen; later lines are ignored.
    saw_done: bool,
}

impl SseDecoder {
    /// Feed a chunk of bytes; return any deltas that became ready (text only —
    /// tool calls + the terminal `Done` are emitted by [`Self::finalize`]).
    fn push_bytes(&mut self, bytes: &[u8]) -> Vec<ChatDelta> {
        let mut out = Vec::new();
        self.buf.extend_from_slice(bytes);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            self.process_line(line.trim(), &mut out);
        }
        out
    }

    /// Parse one SSE line, pushing any ready text delta and updating accumulators.
    fn process_line(&mut self, line: &str, out: &mut Vec<ChatDelta>) {
        if self.saw_done {
            return;
        }
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data == "[DONE]" {
            self.saw_done = true;
            return;
        }
        if data.is_empty() {
            return;
        }
        let chunk: StreamChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(_) => return, // tolerate keep-alives / non-JSON comments
        };
        // Usage typically rides the final chunk (which often has empty choices);
        // capture it regardless of whether this chunk also carries a delta.
        if let Some(u) = chunk.usage {
            if !u.is_empty() {
                self.usage = Some(u);
            }
        }
        let Some(choice) = chunk.choices.into_iter().next() else {
            return;
        };
        if let Some(text) = choice.delta.content {
            if !text.is_empty() {
                out.push(ChatDelta::Text(text));
            }
        }
        for dtc in choice.delta.tool_calls {
            if self.accs.len() <= dtc.index {
                self.accs.resize_with(dtc.index + 1, ToolCallAcc::default);
            }
            let acc = &mut self.accs[dtc.index];
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
            self.finish = Some(finish_from_str(&fr));
        }
    }

    /// Stream ended: flush any trailing partial line, then emit accumulated
    /// tool calls and the terminal `Done`.
    fn finalize(mut self) -> Vec<ChatDelta> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest).to_string();
            self.process_line(line.trim(), &mut out);
        }
        for acc in std::mem::take(&mut self.accs) {
            if !acc.name.is_empty() || !acc.id.is_empty() {
                out.push(ChatDelta::ToolCall(acc.into_tool_call()));
            }
        }
        if let Some(usage) = self.usage.take() {
            out.push(ChatDelta::Usage(usage));
        }
        out.push(ChatDelta::Done(self.finish.unwrap_or(FinishReason::Stop)));
        out
    }
}

/// Byte stream off the wire, normalized to owned `Vec<u8>` chunks so we never
/// need to name `bytes::Bytes` (avoids a direct dependency).
type ByteStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send>>;

/// Drives [`SseDecoder`] across an async byte stream for `stream::unfold`.
enum SseState {
    Reading {
        stream: ByteStream,
        dec: SseDecoder,
        queue: std::collections::VecDeque<ChatDelta>,
    },
    Draining {
        queue: std::collections::VecDeque<ChatDelta>,
    },
    Done,
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
    // Surface usage (if reported) just before the terminal marker.
    if let Some(usage) = resp.usage {
        if !usage.is_empty() {
            out.push(ChatDelta::Usage(usage));
        }
    }
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

/// Exponential backoff for retry `attempt` (1-based): 200ms, 400ms, 800ms, …
/// The shift exponent is clamped to 6 BEFORE shifting, so the multiplier maxes
/// at 64 (~12.8s) and can never overflow regardless of how large `retries` is
/// configured — avoids the panic the naive `1 << (attempt-1)` risked.
fn backoff_delay(attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(6);
    let mult = 1u64 << exp; // exp ≤ 6 ⇒ at most 64, never overflows
    Duration::from_millis(200 * mult)
}

/// One step of the streaming SSE state machine for `stream::unfold`. Yields the
/// next `ChatDelta` (or a `ProviderError` for a mid-stream wire failure), or
/// `None` when fully drained.
async fn sse_step(state: SseState) -> Option<(Result<ChatDelta, ProviderError>, SseState)> {
    match state {
        SseState::Reading {
            mut stream,
            mut dec,
            mut queue,
        } => {
            // Drain anything already decoded before reading more bytes.
            if let Some(d) = queue.pop_front() {
                return Some((Ok(d), SseState::Reading { stream, dec, queue }));
            }
            loop {
                match stream.next().await {
                    Some(Ok(bytes)) => {
                        for d in dec.push_bytes(&bytes) {
                            queue.push_back(d);
                        }
                        if let Some(d) = queue.pop_front() {
                            return Some((Ok(d), SseState::Reading { stream, dec, queue }));
                        }
                        // No complete line yet — keep reading.
                    }
                    Some(Err(e)) => {
                        return Some((Err(map_send_error(e)), SseState::Done));
                    }
                    None => {
                        // Wire closed: flush trailing line + tool calls + Done.
                        let mut rest: std::collections::VecDeque<ChatDelta> = dec.finalize().into();
                        return rest
                            .pop_front()
                            .map(|d| (Ok(d), SseState::Draining { queue: rest }));
                    }
                }
            }
        }
        SseState::Draining { mut queue } => queue
            .pop_front()
            .map(|d| (Ok(d), SseState::Draining { queue })),
        SseState::Done => None,
    }
}

impl OpenAICompatProvider {
    /// Best-effort discovery of a model's context length from `/v1/models`
    /// metadata — e.g. vLLM's `max_model_len`, or a `context_length` /
    /// `context_window` field. Returns `None` on any error or when the gateway
    /// doesn't expose it (Ollama's OpenAI-compat route does not), so callers fall
    /// back to config / a default. Never propagates errors — this is advisory.
    pub async fn model_context_length(&self, model: &str) -> Option<u64> {
        let path = self.models_path.as_deref().filter(|p| !p.is_empty())?;
        let url = format!("{}{}", self.base_url, path);
        let rb = self.http.get(&url).timeout(self.timeout);
        let rb = self.attach_auth(rb).ok()?;
        let resp = rb.send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let text = resp.text().await.ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        let entries = json.get("data")?.as_array()?;
        // Prefer an exact id match, then a substring either way, then a lone entry.
        let entry = entries
            .iter()
            .find(|e| e.get("id").and_then(serde_json::Value::as_str) == Some(model))
            .or_else(|| {
                entries.iter().find(|e| {
                    e.get("id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| id.contains(model) || model.contains(id))
                })
            })
            .or_else(|| (entries.len() == 1).then(|| &entries[0]))?;
        for key in [
            "max_model_len",
            "context_length",
            "context_window",
            "max_context_length",
            "max_input_tokens",
        ] {
            match entry.get(key).and_then(serde_json::Value::as_u64) {
                Some(n) if n > 0 => return Some(n),
                _ => {}
            }
        }
        None
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
                        tokio::time::sleep(backoff_delay(attempt)).await;
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

        if req.stream {
            // Real streaming: drive the SSE decoder across wire chunks so text
            // deltas surface as they arrive (no whole-body buffering).
            let byte_stream: ByteStream = response
                .bytes_stream()
                .map(|r| r.map(|b| b.to_vec()))
                .boxed();
            let init = SseState::Reading {
                stream: byte_stream,
                dec: SseDecoder::default(),
                queue: std::collections::VecDeque::new(),
            };
            let s = stream::unfold(init, sse_step);
            Ok(s.boxed())
        } else {
            let text = response.text().await.map_err(map_send_error)?;
            let deltas = parse_completion(&text)?;
            let s = stream::iter(deltas.into_iter().map(Ok));
            Ok(s.boxed())
        }
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
