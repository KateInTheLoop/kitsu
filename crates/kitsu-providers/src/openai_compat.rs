//! OpenAI-compatible provider. Covers vLLM, OpenAI, Mistral cloud, AND the example
//! AI gateway — they share one wire shape; only base_url/path/auth differ
//! (design §1, §4).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::stream::{self, StreamExt};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::{AuthProvider, FrozenAuth};
use crate::error::{AuthError, ProviderError, ProviderResponsePart};
use crate::route::{
    ProviderAuthSource, ProviderRouteContractError, ProviderRouteDeclaration, ProviderRoutePolicy,
    ProviderRouteSnapshot,
};
use crate::telemetry::{
    ProviderAttemptCategory, ProviderAttemptEvent, ProviderAttemptOutcome, ProviderTelemetrySink,
};
use crate::types::{ChatDelta, ChatMessage, ChatRequest, FinishReason, Role, ToolCall, Usage};
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
    /// End-to-end inference budget, including all retry sleeps and attempts.
    pub(crate) timeout: Duration,
    /// Advisory catalog lookup budget. Kept separate from `timeout` so a slow
    /// `/models` endpoint cannot consume the much larger inference budget.
    pub(crate) discovery_timeout: Duration,
    /// One provider instance serves startup discovery, `/models`, and `/model`.
    /// Cache their shared raw catalog response so those paths do not issue
    /// duplicate requests or repeatedly stall on the same failed endpoint.
    models_cache: tokio::sync::Mutex<Option<ModelsCacheEntry>>,
}

struct ModelsCacheEntry {
    stored_at: tokio::time::Instant,
    ttl: Duration,
    outcome: Result<(StatusCode, String), ProviderError>,
    /// Memoized per-model context-length lookups parsed from the catalog JSON
    /// on first miss; reused for the cache's lifetime (which is the catalog's
    /// own TTL), so repeat `model_context_length` calls don't re-parse. Fresh on
    /// every cache invalidation, so it inherits the existing TTL contract.
    context_lengths: std::sync::Mutex<HashMap<String, Option<u64>>>,
}

/// Builder inputs for constructing a gateway-backed provider.
pub struct OpenAICompatConfig {
    pub base_url: String,
    pub chat_path: String,
    pub models_path: Option<String>,
    pub retries: u32,
    /// End-to-end inference budget, including all retry sleeps and attempts.
    pub timeout: Duration,
}

/// Consumed composition input for one immutable compatible-Chat generation.
/// It is intentionally not `Clone`, `Debug`, or serializable: endpoint and
/// auth-bearing construction values must not become diagnostic payloads.
pub struct OpenAICompatRouteConfig {
    pub declaration: ProviderRouteDeclaration,
    pub model: String,
    pub auth_source: ProviderAuthSource,
    pub base_url: String,
    pub chat_path: String,
    pub models_path: Option<String>,
    pub policy: ProviderRoutePolicy,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenAICompatRouteBuildError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Contract(#[from] ProviderRouteContractError),
}

/// Resolve auth once and construct both physical runtimes from one consumed
/// route input. Inference and catalog can have distinct retry/cache budgets,
/// but can no longer drift in endpoint, paths, credential generation, model,
/// declaration, or source identity through independent CLI construction.
pub fn build_openai_compatible_route(
    config: OpenAICompatRouteConfig,
    auth: &dyn AuthProvider,
) -> Result<ProviderRouteSnapshot<OpenAICompatProvider>, OpenAICompatRouteBuildError> {
    let frozen_auth: Arc<dyn AuthProvider> = Arc::new(FrozenAuth::resolve(auth)?);
    let inference = Arc::new(
        OpenAICompatProvider::new(
            OpenAICompatConfig {
                base_url: config.base_url.clone(),
                chat_path: config.chat_path.clone(),
                models_path: config.models_path.clone(),
                retries: config.policy.inference_retries(),
                timeout: config.policy.inference_timeout(),
            },
            frozen_auth.clone(),
        )
        .with_discovery_timeout(config.policy.advisory_discovery_timeout()),
    );
    let catalog = Arc::new(
        OpenAICompatProvider::new(
            OpenAICompatConfig {
                base_url: config.base_url,
                chat_path: config.chat_path,
                models_path: config.models_path,
                retries: config.policy.catalog_retries(),
                timeout: config.policy.catalog_timeout(),
            },
            frozen_auth,
        )
        .with_discovery_timeout(config.policy.catalog_timeout()),
    );
    ProviderRouteSnapshot::initial(
        config.declaration,
        config.model,
        config.auth_source,
        config.policy,
        inference,
        catalog,
    )
    .map_err(Into::into)
}

impl OpenAICompatProvider {
    /// Advisory background model metadata uses a deliberately short budget.
    const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
    const MODELS_CACHE_TTL: Duration = Duration::from_secs(60);
    const MODELS_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(5);

    pub fn new(cfg: OpenAICompatConfig, auth: Arc<dyn AuthProvider>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: cfg.base_url,
            chat_path: cfg.chat_path,
            models_path: cfg.models_path,
            auth,
            retries: cfg.retries,
            timeout: cfg.timeout,
            discovery_timeout: Self::DEFAULT_DISCOVERY_TIMEOUT,
            models_cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Override the catalog request budget. Explicit user-facing catalog
    /// commands use the CLI's requested timeout; advisory background discovery
    /// keeps the short default.
    pub fn with_discovery_timeout(mut self, timeout: Duration) -> Self {
        self.discovery_timeout = timeout;
        self
    }

    /// Resolve the refresh hint surfaced on a gateway 401/403.
    fn refresh_hint(&self) -> Option<String> {
        self.auth.refresh_hint()
    }

    /// Attach the configured auth header to a request builder. `None` is the
    /// explicit no-auth outcome; credential errors remain hard failures.
    fn attach_auth(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        match self.auth.header()? {
            Some((name, value)) => Ok(rb.header(name, value)),
            None => Ok(rb),
        }
    }
}

// --- wire types (internal to this module) -----------------------------------

#[derive(Serialize, Debug)]
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
    reasoning_effort: Option<crate::ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<WireResponseFormat<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<&'a str>,
}

#[derive(Serialize, Debug)]
struct WireResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    json_schema: WireJsonSchema<'a>,
}

#[derive(Serialize, Debug)]
struct WireJsonSchema<'a> {
    name: &'a str,
    strict: bool,
    schema: &'a serde_json::Value,
}

#[derive(Serialize, Debug)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize, Debug)]
struct WireMessage<'a> {
    role: &'a str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall<'a>>>,
}

#[derive(Serialize, Debug)]
struct WireToolCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolCallFn<'a>,
}

#[derive(Serialize, Debug)]
struct WireToolCallFn<'a> {
    name: &'a str,
    arguments: &'a str,
}

#[derive(Serialize, Debug)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolFn<'a>,
}

#[derive(Serialize, Debug)]
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

fn build_body<'a>(req: &'a ChatRequest) -> Result<WireRequest<'a>, ProviderError> {
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

    let tools = match req.tools.as_ref() {
        Some(ts) => {
            let mut wire_tools = Vec::with_capacity(ts.len());
            for t in ts {
                // Fast-fail: shipping a non-object `parameters` to the gateway
                // fails opaquely downstream. A valid tool schema must be a JSON
                // object; reject anything else before serialization.
                if !t.parameters.is_object() {
                    return Err(ProviderError::InvalidResponse(format!(
                        "tool {}: parameters must be a JSON Schema object",
                        t.name
                    )));
                }
                wire_tools.push(WireTool {
                    kind: "function",
                    function: WireToolFn {
                        name: &t.name,
                        description: &t.description,
                        parameters: &t.parameters,
                    },
                });
            }
            Some(wire_tools)
        }
        None => None,
    };

    Ok(WireRequest {
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
        reasoning_effort: req.reasoning_effort,
        max_tokens: req.max_tokens,
        response_format: req
            .response_format
            .as_ref()
            .map(|format| WireResponseFormat {
                kind: "json_schema",
                json_schema: WireJsonSchema {
                    name: &format.name,
                    strict: format.strict,
                    schema: &format.schema,
                },
            }),
        prompt_cache_key: req.prompt_cache_key.as_deref(),
    })
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

/// Tool-call indices come from an untrusted gateway response. Keep the maximum
/// deliberately generous while preventing a sparse index from amplifying into
/// an arbitrarily large `Vec` allocation.
const MAX_STREAMED_TOOL_CALLS: usize = 64;

// Provider responses are untrusted network input. These ceilings are generous
// relative to ordinary chat traffic while ensuring no endpoint can make Kitsu
// grow a String/Vec without bound. They deliberately describe decoded wire
// bytes, not tokens: byte accounting remains exact for malformed UTF-8 too.
const MAX_COMPLETION_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_MODEL_CATALOG_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
// Per-line ceiling for an SSE record. It must be large enough that a single
// `data:` line carrying a full-size tool-call argument delta can reach the
// argument budget below (`MAX_STREAMED_TOOL_ARGUMENT_BYTES`) rather than being
// rejected here first: such a line is `data: {"choices":[{"delta":{"tool_calls"
// ...[{"function":{"arguments":"<args>"}}]...}}]}`, so the line is the raw
// argument bytes plus the JSON envelope, the `data:` prefix, and JSON-string
// escaping (each escapable char doubles). 4 MiB comfortably exceeds the 1 MiB
// argument budget + envelope + worst-case escaping margin while staying well
// under the 16 MiB aggregate stream ceiling.
const MAX_SSE_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SSE_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_STREAMED_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;

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
    /// At least one choice carried semantic completion data (text, a tool call,
    /// or an explicit finish reason). Usage-only chunks are valid, but they
    /// cannot turn an otherwise empty/error payload into a successful answer.
    saw_semantic_choice: bool,
    /// Exact wire bytes admitted across all chunks.
    received_bytes: usize,
}

impl SseDecoder {
    /// Feed a chunk of bytes; return any deltas that became ready (text only —
    /// tool calls + the terminal `Done` are emitted by [`Self::finalize`]).
    fn push_bytes(&mut self, bytes: &[u8]) -> Result<Vec<ChatDelta>, ProviderError> {
        let mut out = Vec::new();
        self.received_bytes = self
            .received_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_SSE_STREAM_BYTES)
            .ok_or(ProviderError::ResponseTooLarge {
                part: ProviderResponsePart::SseStream,
                limit_bytes: MAX_SSE_STREAM_BYTES,
            })?;

        // Process one newline-bounded segment at a time. Checking before each
        // append means a large reqwest chunk containing many small SSE records
        // is accepted without ever transiently growing the line buffer beyond
        // its own independent ceiling.
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
                self.process_line(line.trim(), &mut out)?;
            }
        }
        Ok(out)
    }

    /// Parse one SSE line, pushing any ready text delta and updating accumulators.
    fn process_line(&mut self, line: &str, out: &mut Vec<ChatDelta>) -> Result<(), ProviderError> {
        if self.saw_done {
            return Ok(());
        }
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(());
        };
        let data = data.trim();
        if data == "[DONE]" {
            self.saw_done = true;
            return Ok(());
        }
        if data.is_empty() {
            return Ok(());
        }
        let value: serde_json::Value = serde_json::from_str(data).map_err(|_| {
            ProviderError::InvalidResponse("malformed JSON in SSE data record".to_string())
        })?;
        if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
            // A real (non-null) error object in a "successful" SSE response means the
            // gateway failed mid-stream. Surface the gateway's own message text so the
            // caller can see *why* (e.g. "rate limited"); fall back to a sane fixed
            // string only when no message/string form is present. A JSON `null` error
            // is NOT an error and must be ignored (some gateways emit `"error": null`).
            let detail = err
                .get("message")
                .and_then(|m| m.as_str())
                .or_else(|| err.as_str())
                .unwrap_or("provider returned an error object in a successful SSE response");
            return Err(ProviderError::InvalidResponse(format!(
                "provider error in SSE stream: {detail}"
            )));
        }
        let chunk: StreamChunk = serde_json::from_value(value).map_err(|_| {
            ProviderError::InvalidResponse(
                "invalid completion object in SSE data record".to_string(),
            )
        })?;
        // Usage typically rides the final chunk (which often has empty choices);
        // capture it regardless of whether this chunk also carries a delta.
        if let Some(u) = chunk.usage {
            if !u.is_empty() {
                self.usage = Some(u);
            }
        }
        let Some(choice) = chunk.choices.into_iter().next() else {
            return Ok(());
        };
        let semantic_choice = choice
            .delta
            .content
            .as_ref()
            .is_some_and(|text| !text.is_empty())
            || !choice.delta.tool_calls.is_empty()
            || choice.finish_reason.is_some();
        self.saw_semantic_choice |= semantic_choice;
        if let Some(text) = choice.delta.content {
            if !text.is_empty() {
                out.push(ChatDelta::Text(text));
            }
        }
        for dtc in choice.delta.tool_calls {
            if dtc.index >= MAX_STREAMED_TOOL_CALLS {
                continue;
            }
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
                    if acc.args.len().saturating_add(args.len()) > MAX_STREAMED_TOOL_ARGUMENT_BYTES
                    {
                        return Err(ProviderError::ResponseTooLarge {
                            part: ProviderResponsePart::ToolArguments,
                            limit_bytes: MAX_STREAMED_TOOL_ARGUMENT_BYTES,
                        });
                    }
                    acc.args.push_str(&args);
                }
            }
        }
        if let Some(fr) = choice.finish_reason {
            self.finish = Some(finish_from_str(&fr));
        }
        Ok(())
    }

    /// Stream ended: flush any trailing partial line, validate that the provider
    /// explicitly ended the semantic response, then emit accumulated tool calls
    /// and the terminal `Done`.
    fn finalize(mut self) -> Result<Vec<ChatDelta>, ProviderError> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest).to_string();
            self.process_line(line.trim(), &mut out)?;
        }
        if !self.saw_done && self.finish.is_none() {
            return Err(ProviderError::InvalidResponse(
                "SSE stream ended without data: [DONE] or an explicit finish_reason".to_string(),
            ));
        }
        if !self.saw_semantic_choice {
            return Err(ProviderError::InvalidResponse(
                "SSE completion ended without a semantic choice".to_string(),
            ));
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
        Ok(out)
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
        attempt_guard: ResponseBodyAttemptGuard,
    },
    Draining {
        queue: std::collections::VecDeque<ChatDelta>,
    },
    Done,
}

/// Parse a non-streaming completion JSON into a vec of ChatDelta.
fn parse_completion(body: &str) -> Result<Vec<ChatDelta>, ProviderError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| ProviderError::InvalidResponse(format!("malformed completion JSON: {e}")))?;
    if value.get("error").is_some_and(|e| !e.is_null()) {
        return Err(ProviderError::InvalidResponse(
            "provider returned an error object in a successful completion response".to_string(),
        ));
    }
    let resp: CompletionResponse = serde_json::from_value(value)
        .map_err(|e| ProviderError::InvalidResponse(format!("invalid completion object: {e}")))?;
    let mut out = Vec::new();
    let finish = match resp.choices.into_iter().next() {
        Some(choice) => {
            let semantic_choice = choice
                .message
                .content
                .as_ref()
                .is_some_and(|text| !text.is_empty())
                || !choice.message.tool_calls.is_empty()
                || choice.finish_reason.is_some();
            if !semantic_choice {
                return Err(ProviderError::InvalidResponse(
                    "completion response contained an empty choice".to_string(),
                ));
            }
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
        None => {
            return Err(ProviderError::InvalidResponse(
                "completion response did not contain a choice".to_string(),
            ));
        }
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

/// Consume one HTTP response under an exact provider-controlled byte ceiling.
/// Content-Length is only an early rejection hint; streamed accounting remains
/// authoritative because the header can be absent or dishonest.
async fn read_response_bounded(
    response: reqwest::Response,
    part: ProviderResponsePart,
    limit_bytes: usize,
) -> Result<String, ProviderError> {
    if response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .is_some_and(|length| length > limit_bytes)
    {
        return Err(ProviderError::ResponseTooLarge { part, limit_bytes });
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_send_error)?;
        if body.len().saturating_add(chunk.len()) > limit_bytes {
            return Err(ProviderError::ResponseTooLarge { part, limit_bytes });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// A gateway-provided retry delay is useful, but it is still untrusted input.
/// Capping it keeps one response from parking the client for an arbitrary time;
/// the request-wide deadline below may impose an even smaller bound.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_RETRY_JITTER: Duration = Duration::from_millis(250);

/// Parse either Retry-After form from RFC 9110: delay-seconds or an HTTP date.
/// Numeric overflow is treated as a very large delay and therefore capped,
/// rather than silently falling back to the much shorter exponential delay.
fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        let seconds = value
            .parse::<u64>()
            .unwrap_or(MAX_RETRY_DELAY.as_secs())
            .min(MAX_RETRY_DELAY.as_secs());
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let now = chrono::DateTime::<chrono::Utc>::from(now);
    let delay = retry_at
        .with_timezone(&chrono::Utc)
        .signed_duration_since(now)
        .to_std()
        .unwrap_or(Duration::ZERO);
    Some(delay.min(MAX_RETRY_DELAY))
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

/// Add positive jitter without ever exceeding the retry-delay cap. The sample
/// is an argument so the arithmetic is deterministic in unit tests.
fn add_retry_jitter(delay: Duration, sample: u64) -> Duration {
    let jitter_cap = (delay / 10).min(MAX_RETRY_JITTER);
    if jitter_cap.is_zero() {
        return delay.min(MAX_RETRY_DELAY);
    }
    // `jitter_cap` is at most 250ms, so this conversion cannot truncate.
    let slots = jitter_cap.as_nanos() as u64 + 1;
    let jitter = Duration::from_nanos(sample % slots);
    delay.saturating_add(jitter).min(MAX_RETRY_DELAY)
}

/// Select a retry delay. The local backoff remains a floor, so a zero/past
/// Retry-After cannot turn a failing gateway into a hot loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RetryDelayDecision {
    delay: Duration,
    retry_after_used: bool,
}

fn retry_delay_decision(
    attempt: u32,
    retry_after: Option<&reqwest::header::HeaderValue>,
    now: SystemTime,
    jitter_sample: u64,
) -> RetryDelayDecision {
    let advertised = retry_after
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value, now));
    let local = backoff_delay(attempt);
    let retry_after_used = advertised.is_some_and(|delay| delay >= local);
    RetryDelayDecision {
        delay: add_retry_jitter(local.max(advertised.unwrap_or_default()), jitter_sample),
        retry_after_used,
    }
}

#[cfg(test)]
fn retry_delay(
    attempt: u32,
    retry_after: Option<&reqwest::header::HeaderValue>,
    now: SystemTime,
    jitter_sample: u64,
) -> Duration {
    retry_delay_decision(attempt, retry_after, now, jitter_sample).delay
}

/// Cheap process-local entropy is sufficient for retry jitter: it is not a
/// security primitive, only a way to keep concurrent clients from marching in
/// lockstep after a shared transient gateway or rate-limit response.
fn retry_jitter_sample(attempt: u32) -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let clock = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| u64::from(duration.subsec_nanos()))
        .unwrap_or(0);
    clock ^ counter.rotate_left(17) ^ u64::from(attempt).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

fn remaining_request_time(started: tokio::time::Instant, timeout: Duration) -> Option<Duration> {
    timeout
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn remaining_deadline_millis(started: tokio::time::Instant, timeout: Duration) -> u64 {
    remaining_request_time(started, timeout)
        .map(duration_millis)
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

/// Is an HTTP response status a transient failure worth retrying within the
/// inference budget? Rate limiting (429) and the transient gateway/proxy
/// failures — 408 Request Timeout, 502 Bad Gateway, 503 Service Unavailable,
/// 504 Gateway Timeout, Cloudflare's non-standard 520-524, and 529 Overloaded
/// (used by some routers) — are the ones a
/// hosted router raises when an upstream is momentarily slow or overloaded; a
/// second attempt commonly succeeds. Everything else (a genuine 500 from bad
/// input, any 4xx, an auth failure) is deterministic and must not be retried.
/// A tool-heavy multi-round run through a busy router hits these often enough
/// that treating them as terminal silently discards a long, productive turn.
fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        408 | 429 | 502 | 503 | 504 | 520 | 521 | 522 | 523 | 524 | 529
    )
}

fn record_attempt(telemetry: Option<&ProviderTelemetrySink>, event: ProviderAttemptEvent) -> bool {
    telemetry.map_or(true, |telemetry| telemetry.record(event))
}

/// Ensures an admitted physical send remains observable when its async future
/// is dropped (for example by the CLI Ctrl+C race). Completed sends disarm the
/// guard before their ordinary status/transport event is recorded.
struct InFlightAttemptGuard {
    telemetry: Option<ProviderTelemetrySink>,
    attempt: u32,
    max_attempts: u32,
    started: tokio::time::Instant,
    timeout: Duration,
    armed: bool,
}

impl InFlightAttemptGuard {
    fn new(
        telemetry: Option<&ProviderTelemetrySink>,
        attempt: u32,
        max_attempts: u32,
        started: tokio::time::Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            telemetry: telemetry.cloned(),
            attempt,
            max_attempts,
            started,
            timeout,
            armed: true,
        }
    }

    fn completed(mut self) {
        self.armed = false;
    }
}

impl Drop for InFlightAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        record_attempt(
            self.telemetry.as_ref(),
            ProviderAttemptEvent {
                attempt: self.attempt,
                max_attempts: self.max_attempts,
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

/// Extends a successful physical send through its response-body lifecycle.
/// A 2xx header is intentionally non-terminal: only a fully read and decoded
/// body is success. Dropping an unconsumed/partially consumed stream records a
/// terminal cancellation, while explicit failures disarm this guard after
/// recording their precise bounded category.
struct ResponseBodyAttemptGuard {
    telemetry: Option<ProviderTelemetrySink>,
    attempt: u32,
    max_attempts: u32,
    status: u16,
    started: tokio::time::Instant,
    timeout: Duration,
    armed: bool,
}

impl ResponseBodyAttemptGuard {
    fn new(
        telemetry: Option<&ProviderTelemetrySink>,
        attempt: u32,
        max_attempts: u32,
        status: u16,
        started: tokio::time::Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            telemetry: telemetry.cloned(),
            attempt,
            max_attempts,
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
                attempt: self.attempt,
                max_attempts: self.max_attempts,
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

impl Drop for ResponseBodyAttemptGuard {
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

/// Keeps a nonterminal `RetryScheduled` disposition observable if the caller
/// cancels while waiting to start the next physical attempt. This guard is only
/// installed after a retry event with `terminal=false`; paths which already
/// emitted `DeadlineExceeded` must not install it or they would double-report a
/// terminal disposition.
struct RetryBackoffAttemptGuard {
    telemetry: Option<ProviderTelemetrySink>,
    attempt: u32,
    max_attempts: u32,
    started: tokio::time::Instant,
    timeout: Duration,
    armed: bool,
}

impl RetryBackoffAttemptGuard {
    fn new(
        telemetry: Option<&ProviderTelemetrySink>,
        attempt: u32,
        max_attempts: u32,
        started: tokio::time::Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            telemetry: telemetry.cloned(),
            attempt,
            max_attempts,
            started,
            timeout,
            armed: true,
        }
    }

    fn completed(mut self) {
        self.armed = false;
    }

    fn deadline_exceeded(mut self) {
        self.record(
            ProviderAttemptCategory::Timeout,
            ProviderAttemptOutcome::DeadlineExceeded,
        );
        self.armed = false;
    }

    fn record(&self, category: ProviderAttemptCategory, outcome: ProviderAttemptOutcome) {
        record_attempt(
            self.telemetry.as_ref(),
            ProviderAttemptEvent {
                attempt: self.attempt,
                max_attempts: self.max_attempts,
                category,
                // The next attempt was scheduled but no HTTP response exists.
                status: None,
                outcome,
                terminal: true,
                retry_delay_ms: None,
                retry_after_used: false,
                remaining_deadline_ms: remaining_deadline_millis(self.started, self.timeout),
            },
        );
    }
}

impl Drop for RetryBackoffAttemptGuard {
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

/// Sleep only while the total request budget permits. If the requested delay
/// reaches the deadline, return Timeout instead of starting a doomed attempt.
async fn sleep_before_retry(
    started: tokio::time::Instant,
    timeout: Duration,
    delay: Duration,
) -> Result<(), ProviderError> {
    let remaining = remaining_request_time(started, timeout).ok_or(ProviderError::Timeout)?;
    if delay >= remaining {
        tokio::time::sleep(remaining).await;
        return Err(ProviderError::Timeout);
    }
    tokio::time::sleep(delay).await;
    Ok(())
}

/// Wait after a nonterminal retry disposition while preserving exactly one
/// later terminal disposition if the wait is cancelled or loses the deadline.
async fn sleep_before_retry_observed(
    started: tokio::time::Instant,
    timeout: Duration,
    delay: Duration,
    guard: RetryBackoffAttemptGuard,
) -> Result<(), ProviderError> {
    match sleep_before_retry(started, timeout, delay).await {
        Ok(()) => {
            // A timer may wake late under scheduler pressure. Recheck before
            // disarming the only guard for the scheduled attempt; otherwise
            // the loop's next admission check could return a bare Timeout with
            // no terminal disposition.
            if remaining_request_time(started, timeout).is_some() {
                guard.completed();
                Ok(())
            } else {
                guard.deadline_exceeded();
                Err(ProviderError::Timeout)
            }
        }
        Err(error) => {
            guard.deadline_exceeded();
            Err(error)
        }
    }
}

fn finish_sse(
    dec: SseDecoder,
    mut queue: std::collections::VecDeque<ChatDelta>,
    attempt_guard: ResponseBodyAttemptGuard,
) -> Option<(Result<ChatDelta, ProviderError>, SseState)> {
    match dec.finalize() {
        Ok(deltas) => {
            attempt_guard.completed();
            queue.extend(deltas);
            queue
                .pop_front()
                .map(|delta| (Ok(delta), SseState::Draining { queue }))
        }
        Err(error) => {
            attempt_guard.failed(&error);
            Some((Err(error), SseState::Done))
        }
    }
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
            attempt_guard,
        } => {
            // Drain anything already decoded before reading more bytes.
            if let Some(d) = queue.pop_front() {
                return Some((
                    Ok(d),
                    SseState::Reading {
                        stream,
                        dec,
                        queue,
                        attempt_guard,
                    },
                ));
            }
            let attempt_guard = attempt_guard;
            loop {
                match stream.next().await {
                    Some(Ok(bytes)) => {
                        match dec.push_bytes(&bytes) {
                            Ok(deltas) => {
                                for d in deltas {
                                    queue.push_back(d);
                                }
                            }
                            Err(error) => {
                                attempt_guard.failed(&error);
                                return Some((Err(error), SseState::Done));
                            }
                        }
                        // `[DONE]` ends the semantic response. Do not require a
                        // compatible gateway to close or finish draining its
                        // HTTP body before the completed answer is usable.
                        if dec.saw_done {
                            return finish_sse(dec, queue, attempt_guard);
                        }
                        if let Some(d) = queue.pop_front() {
                            return Some((
                                Ok(d),
                                SseState::Reading {
                                    stream,
                                    dec,
                                    queue,
                                    attempt_guard,
                                },
                            ));
                        }
                        // No complete line yet — keep reading.
                    }
                    Some(Err(e)) => {
                        let error = map_send_error(e);
                        attempt_guard.failed(&error);
                        return Some((Err(error), SseState::Done));
                    }
                    None => {
                        // Wire EOF is successful only when a finish reason was
                        // already observed (or a trailing `[DONE]` is flushed).
                        return finish_sse(dec, queue, attempt_guard);
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

/// Does `id` loosely match `model` in a way safe enough to bind a context
/// window to? Plain substring containment (`id.contains(model)`) is too loose:
/// it would bind a configured model `"gpt-4"` to an unrelated, differently-sized
/// `"gpt-4-turbo"` entry whenever no exact `"gpt-4"` entry exists. Require the
/// shorter string to appear as a delimiter-bounded segment of the longer one
/// (bounded by a non-alphanumeric character or a string edge), which at least
/// rules out one id being an arbitrary mid-token substring of the other.
fn loosely_matches(id: &str, model: &str) -> bool {
    let (short, long) = if id.len() <= model.len() {
        (id, model)
    } else {
        (model, id)
    };
    if short.is_empty() {
        return false;
    }
    let Some(pos) = long.find(short) else {
        return false;
    };
    let before_ok = pos == 0 || !long.as_bytes()[pos - 1].is_ascii_alphanumeric();
    let end = pos + short.len();
    let after_ok = end == long.len() || !long.as_bytes()[end].is_ascii_alphanumeric();
    before_ok && after_ok
}

impl OpenAICompatProvider {
    /// Shared GET-and-parse-as-JSON for `/models`-shaped endpoints, used by both
    /// `list_models` and `model_context_length` so the request construction (URL,
    /// auth, timeout) and response shape can't drift between the two. Returns the
    /// status and raw body text; callers decide how to interpret non-success
    /// statuses (`list_models` maps 401/403 to `Unauthorized` and anything else to
    /// `Http`; `model_context_length` is advisory and gives up on any error).
    async fn get_models_response(
        &self,
        telemetry: Option<&ProviderTelemetrySink>,
    ) -> Result<(StatusCode, String), ProviderError> {
        let path = self
            .models_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| ProviderError::InvalidResponse("models_path not configured".into()))?;
        let url = format!("{}{}", self.base_url, path);

        // A cached catalog must never turn a now-missing credential into an
        // apparent healthcheck success. Validate current auth before consulting
        // the cache; auth failures themselves are deliberately never cached.
        self.auth.header().map_err(ProviderError::Auth)?;

        // Keep the mutex across the request. Catalog calls are rare, and this
        // prevents a startup discovery plus `/models` from stampeding the same
        // endpoint before the first response populates the cache.
        let mut cache = self.models_cache.lock().await;
        if let Some(entry) = cache.as_ref() {
            if entry.stored_at.elapsed() < entry.ttl {
                return entry.outcome.clone();
            }
        }

        let outcome = self.fetch_models_response(&url, telemetry).await;
        let ttl = match &outcome {
            Ok((status, _)) if status.is_success() => Some(Self::MODELS_CACHE_TTL),
            Ok((status, _))
                if *status != StatusCode::UNAUTHORIZED && *status != StatusCode::FORBIDDEN =>
            {
                Some(Self::MODELS_NEGATIVE_CACHE_TTL)
            }
            Err(
                ProviderError::Connection(_)
                | ProviderError::Timeout
                | ProviderError::InvalidResponse(_),
            ) => Some(Self::MODELS_NEGATIVE_CACHE_TTL),
            _ => None,
        };
        // Preserve the warm per-model context-length memo across a refresh ONLY
        // when the new catalog text EXACTLY equals the previous successful
        // catalog text — carrying the memo across a CHANGED catalog would serve
        // stale context lengths (a correctness bug). Otherwise start fresh.
        // Extract the carried map before the assignment below, which drops the
        // old entry wholesale.
        let carried_context_lengths = {
            let prev = cache.as_ref();
            match (&outcome, prev) {
                (Ok((new_status, new_text)), Some(prev_entry))
                    if new_status.is_success()
                        && matches!(
                            &prev_entry.outcome,
                            Ok((prev_status, prev_text))
                                if prev_status.is_success() && prev_text == new_text
                        ) =>
                {
                    prev_entry.context_lengths.lock().unwrap().clone()
                }
                _ => HashMap::new(),
            }
        };
        *cache = ttl.map(|ttl| ModelsCacheEntry {
            stored_at: tokio::time::Instant::now(),
            ttl,
            outcome: outcome.clone(),
            context_lengths: std::sync::Mutex::new(carried_context_lengths),
        });
        outcome
    }

    async fn fetch_models_response(
        &self,
        url: &str,
        telemetry: Option<&ProviderTelemetrySink>,
    ) -> Result<(StatusCode, String), ProviderError> {
        let started = tokio::time::Instant::now();
        let max_attempts = 1;
        let rb = self.http.get(url).timeout(self.discovery_timeout);
        let rb = self.attach_auth(rb)?;
        let attempt_guard =
            InFlightAttemptGuard::new(telemetry, 1, max_attempts, started, self.discovery_timeout);
        let send_result = rb.send().await;
        attempt_guard.completed();
        let resp = match send_result {
            Ok(response) => response,
            Err(error) => {
                record_attempt(
                    telemetry,
                    ProviderAttemptEvent {
                        attempt: 1,
                        max_attempts,
                        category: if error.is_timeout() {
                            ProviderAttemptCategory::Timeout
                        } else {
                            ProviderAttemptCategory::Connection
                        },
                        status: None,
                        outcome: ProviderAttemptOutcome::TransportFailed,
                        terminal: true,
                        retry_delay_ms: None,
                        retry_after_used: false,
                        remaining_deadline_ms: remaining_deadline_millis(
                            started,
                            self.discovery_timeout,
                        ),
                    },
                );
                return Err(map_send_error(error));
            }
        };
        let status = resp.status();
        let body_guard = status.is_success().then(|| {
            ResponseBodyAttemptGuard::new(
                telemetry,
                1,
                max_attempts,
                status.as_u16(),
                started,
                self.discovery_timeout,
            )
        });
        if !status.is_success() {
            record_attempt(
                telemetry,
                ProviderAttemptEvent {
                    attempt: 1,
                    max_attempts,
                    category: status_category(status),
                    status: Some(status.as_u16()),
                    outcome: ProviderAttemptOutcome::Rejected,
                    terminal: true,
                    retry_delay_ms: None,
                    retry_after_used: false,
                    remaining_deadline_ms: remaining_deadline_millis(
                        started,
                        self.discovery_timeout,
                    ),
                },
            );
        }
        // On a non-success status, tolerate a body-read failure so the caller
        // still gets the real status — the diagnostic body is best-effort, and
        // losing it must not also lose the status that made the request fail in
        // the first place. A body-read failure on success IS load-bearing (the
        // caller needs that body to parse the actual data), so it still errors.
        let text = if status.is_success() {
            match read_response_bounded(
                resp,
                ProviderResponsePart::ModelCatalogBody,
                MAX_MODEL_CATALOG_BODY_BYTES,
            )
            .await
            {
                Ok(text) => text,
                Err(error) => {
                    body_guard
                        .expect("successful catalog response has a body guard")
                        .failed(&error);
                    return Err(error);
                }
            }
        } else {
            match read_response_bounded(resp, ProviderResponsePart::ErrorBody, MAX_ERROR_BODY_BYTES)
                .await
            {
                Ok(text) => text,
                Err(error @ ProviderError::ResponseTooLarge { .. }) => return Err(error),
                Err(_) => String::new(),
            }
        };
        if status.is_success() {
            if let Err(error) = validate_models_catalog(&text) {
                body_guard
                    .expect("successful catalog response has a body guard")
                    .failed(&error);
                return Err(error);
            }
            body_guard
                .expect("successful catalog response has a body guard")
                .completed();
        }
        Ok((status, text))
    }

    /// Best-effort discovery of a model's context length from `/v1/models`
    /// metadata — e.g. vLLM's `max_model_len`, or a `context_length` /
    /// `context_window` field. Returns `None` on any error or when the gateway
    /// doesn't expose it (Ollama's OpenAI-compat route does not), so callers fall
    /// back to config / a default. Never propagates errors — this is advisory.
    pub async fn model_context_length(&self, model: &str) -> Option<u64> {
        // get_models_response already errors when models_path is unset/empty;
        // `.ok()?` turns that into the same `None` a redundant check here would.
        let (status, text) = self.get_models_response(None).await.ok()?;
        if !status.is_success() {
            return None;
        }
        // Memoize the parsed per-model lookup inside the cache entry so repeat
        // calls skip re-parsing the catalog JSON. The memo lives in
        // ModelsCacheEntry and is carried across a refresh only when the new
        // catalog text exactly equals the previous one (see `get_models_response`);
        // a CHANGED catalog starts fresh so the memo never serves lengths
        // derived from a stale catalog. This preserves the existing
        // TTL/invalidation contract: the memo never outlives the cached catalog
        // it was derived from.
        let cache = self.models_cache.lock().await;
        if let Some(entry) = cache.as_ref() {
            let mut memo = entry.context_lengths.lock().unwrap();
            if let Some(cached) = memo.get(model) {
                return *cached;
            }
            let parsed = context_length_from_catalog(&text, model);
            memo.insert(model.to_string(), parsed);
            parsed
        } else {
            // The entry was evicted between get_models_response and now (e.g. a
            // concurrent invalidation). Parse directly from the text we already
            // hold — correctness is unaffected, just not memoized.
            context_length_from_catalog(&text, model)
        }
    }
}

/// Pure parse: extract a model's advisory context length from a catalog JSON
/// body. Prefers an exact id match, then a unique loose match (ambiguous loose
/// matches give up rather than binding to the wrong window). There is no lone
/// entry fallback: a sole entry that matches neither exactly nor loosely must
/// not bind a window, since it would silently budget against the wrong one.
/// Checks `max_model_len`, `context_length`, `context_window`,
/// `max_context_length`, and `max_input_tokens` in that order. Returns `None`
/// on any parse error or when no usable field is present.
fn context_length_from_catalog(text: &str, model: &str) -> Option<u64> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let entries = json.get("data")?.as_array()?;
    // A model may carry a `:provider` routing pin (e.g. "org/model:together")
    // that the catalog `id` does not. Match on the pin-stripped base too, and
    // remember the pinned provider so the nested lookup below can prefer that
    // provider's window. Only the last `:` segment is a pin; a bare tag like
    // ollama's "qwen2.5-coder:latest" still matches its full id first.
    let (base, pinned_provider) = match model.rsplit_once(':') {
        Some((base, prov)) if !base.is_empty() => (base, (!prov.is_empty()).then_some(prov)),
        _ => (model, None),
    };
    // Prefer an exact id match; then a loose match, but ONLY if it's unique —
    // an ambiguous loose match (e.g. "gpt-4" loosely matching both
    // "gpt-4-turbo" and "gpt-4-mini") is worse than giving up, since binding
    // to the wrong entry silently budgets against the wrong window. There is
    // NO lone-entry fallback: a sole entry that matches neither exactly nor
    // loosely would silently budget against the wrong window.
    let exact = entries.iter().find(|e| {
        e.get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id == model || id == base)
    });
    let entry = if let Some(e) = exact {
        e
    } else {
        let mut loose = entries.iter().filter(|e| {
            e.get("id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| loosely_matches(id, base))
        });
        match (loose.next(), loose.next()) {
            (Some(only), None) => only,
            _ => return None,
        }
    };
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
    // Some routers (e.g. the Hugging Face router) advertise no top-level window
    // and instead nest one per serving provider under `providers[]`. A `:provider`
    // pin selects that provider's window verbatim (it is the accurate window for
    // an explicit route). Unpinned, take the MIN across usable providers — a safe
    // lower bound, since the request may be dispatched to any of them and Kitsu
    // must never budget above the smallest one it could hit.
    let providers = entry.get("providers")?.as_array()?;
    let provider_window = |p: &serde_json::Value| -> Option<u64> {
        p.get("context_length")
            .and_then(serde_json::Value::as_u64)
            .filter(|n| *n > 0)
    };
    if let Some(prov) = pinned_provider {
        if let Some(p) = providers
            .iter()
            .find(|p| p.get("provider").and_then(serde_json::Value::as_str) == Some(prov))
        {
            return provider_window(p);
        }
    }
    // Allowlist rather than denylist: only `"live"` (or an unreported status, for
    // routers that omit it) counts. A MIN is maximally sensitive to one small
    // entry, so a provider that is down under any other label ("staging",
    // "offline", ...) must not drag the whole budget down.
    let min = providers
        .iter()
        .filter(|p| {
            matches!(
                p.get("status").and_then(serde_json::Value::as_str),
                None | Some("live")
            )
        })
        .filter_map(provider_window)
        .min()?;
    // The unpinned MIN is a heuristic lower bound, so never report BELOW the
    // window Kitsu would use with no discovery at all (kitsu-core's
    // DEFAULT_CONTEXT_WINDOW). Otherwise one provider advertising a tiny window
    // would make discovery strictly worse than the fallback it replaces.
    const NO_DISCOVERY_FLOOR: u64 = 8_192;
    Some(min.max(NO_DISCOVERY_FLOOR))
}

/// Validate the common catalog envelope before assigning the positive cache
/// lifetime. Otherwise a malformed HTTP-200 response would be retained as a
/// success for 60 seconds even though every consumer rejects it.
fn validate_models_catalog(text: &str) -> Result<(), ProviderError> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|error| {
        ProviderError::InvalidResponse(format!("malformed /models JSON: {error}"))
    })?;
    let entries = json
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            ProviderError::InvalidResponse("malformed /models JSON: missing data array".into())
        })?;
    if entries.iter().any(|entry| {
        entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_none()
    }) {
        return Err(ProviderError::InvalidResponse(
            "malformed /models JSON: every data entry must have a string id".into(),
        ));
    }
    Ok(())
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
            self.auth.header().map(|_| ()).map_err(ProviderError::Auth)
        } else {
            self.list_models().await.map(|_| ())
        }
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        self.chat_stream_with_telemetry(req, None).await
    }

    async fn chat_stream_with_telemetry(
        &self,
        req: ChatRequest,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<ChatStream, ProviderError> {
        let url = format!("{}{}", self.base_url, self.chat_path);
        let body = build_body(&req)?;

        // Retry the bounded transient status set within one end-to-end deadline.
        // Each attempt receives only the remaining budget; reqwest keeps that
        // timeout active through the response body, including streaming bodies.
        // Auth failures and all other 4xx responses return immediately below.
        let started = tokio::time::Instant::now();
        let max_attempts = self.retries.saturating_add(1);
        // Number of retries already completed; the physical attempt exposed to
        // telemetry is therefore always `attempt + 1`.
        let mut attempt: u32 = 0;
        let (response, final_attempt) = loop {
            let remaining =
                remaining_request_time(started, self.timeout).ok_or(ProviderError::Timeout)?;
            let physical_attempt = attempt.saturating_add(1);
            let rb = self
                .http
                .post(&url)
                .timeout(remaining)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&body);
            let rb = self.attach_auth(rb)?;

            let attempt_guard = InFlightAttemptGuard::new(
                telemetry.as_ref(),
                physical_attempt,
                max_attempts,
                started,
                self.timeout,
            );
            let send_result = rb.send().await;
            attempt_guard.completed();

            match send_result {
                Ok(resp) => {
                    let status = resp.status();
                    if is_retryable_status(status) && attempt < self.retries {
                        let retry_number = attempt.saturating_add(1);
                        let decision = retry_delay_decision(
                            retry_number,
                            resp.headers().get(reqwest::header::RETRY_AFTER),
                            SystemTime::now(),
                            retry_jitter_sample(retry_number),
                        );
                        let remaining_after = remaining_request_time(started, self.timeout);
                        let can_retry =
                            remaining_after.is_some_and(|remaining| decision.delay < remaining);
                        let observer_ready = record_attempt(
                            telemetry.as_ref(),
                            ProviderAttemptEvent {
                                attempt: physical_attempt,
                                max_attempts,
                                category: status_category(status),
                                status: Some(status.as_u16()),
                                outcome: if can_retry {
                                    ProviderAttemptOutcome::RetryScheduled
                                } else {
                                    ProviderAttemptOutcome::DeadlineExceeded
                                },
                                terminal: !can_retry,
                                retry_delay_ms: can_retry.then(|| duration_millis(decision.delay)),
                                retry_after_used: can_retry && decision.retry_after_used,
                                remaining_deadline_ms: remaining_after
                                    .map(duration_millis)
                                    .unwrap_or(0),
                            },
                        );
                        drop(resp);
                        if !observer_ready {
                            // Close the already-published nonterminal retry
                            // disposition in the retained trace, but never make
                            // the newly scheduled physical send.
                            if can_retry {
                                drop(RetryBackoffAttemptGuard::new(
                                    telemetry.as_ref(),
                                    physical_attempt.saturating_add(1),
                                    max_attempts,
                                    started,
                                    self.timeout,
                                ));
                            }
                            return Err(ProviderError::Connection(
                                "provider telemetry sink stopped the request".to_string(),
                            ));
                        }
                        if !can_retry {
                            sleep_before_retry(started, self.timeout, decision.delay).await?;
                            return Err(ProviderError::Timeout);
                        }
                        let backoff_guard = RetryBackoffAttemptGuard::new(
                            telemetry.as_ref(),
                            physical_attempt.saturating_add(1),
                            max_attempts,
                            started,
                            self.timeout,
                        );
                        sleep_before_retry_observed(
                            started,
                            self.timeout,
                            decision.delay,
                            backoff_guard,
                        )
                        .await?;
                        attempt = retry_number;
                        continue;
                    }
                    // A success status is not a completed inference. Its one
                    // terminal attempt event is delayed until the body is read
                    // and decoded (or fails/is cancelled). Final non-success
                    // statuses have no successful body lifecycle to await.
                    if !status.is_success() {
                        record_attempt(
                            telemetry.as_ref(),
                            ProviderAttemptEvent {
                                attempt: physical_attempt,
                                max_attempts,
                                category: status_category(status),
                                status: Some(status.as_u16()),
                                outcome: ProviderAttemptOutcome::Rejected,
                                terminal: true,
                                retry_delay_ms: None,
                                retry_after_used: false,
                                remaining_deadline_ms: remaining_deadline_millis(
                                    started,
                                    self.timeout,
                                ),
                            },
                        );
                    }
                    break (resp, physical_attempt);
                }
                Err(e) => {
                    if e.is_timeout() && attempt < self.retries {
                        let retry_number = attempt.saturating_add(1);
                        let delay = add_retry_jitter(
                            backoff_delay(retry_number),
                            retry_jitter_sample(retry_number),
                        );
                        let remaining_after = remaining_request_time(started, self.timeout);
                        let can_retry = remaining_after.is_some_and(|remaining| delay < remaining);
                        let observer_ready = record_attempt(
                            telemetry.as_ref(),
                            ProviderAttemptEvent {
                                attempt: physical_attempt,
                                max_attempts,
                                category: ProviderAttemptCategory::Timeout,
                                status: None,
                                outcome: if can_retry {
                                    ProviderAttemptOutcome::RetryScheduled
                                } else {
                                    ProviderAttemptOutcome::DeadlineExceeded
                                },
                                terminal: !can_retry,
                                retry_delay_ms: can_retry.then(|| duration_millis(delay)),
                                retry_after_used: false,
                                remaining_deadline_ms: remaining_after
                                    .map(duration_millis)
                                    .unwrap_or(0),
                            },
                        );
                        if !observer_ready {
                            if can_retry {
                                drop(RetryBackoffAttemptGuard::new(
                                    telemetry.as_ref(),
                                    physical_attempt.saturating_add(1),
                                    max_attempts,
                                    started,
                                    self.timeout,
                                ));
                            }
                            return Err(ProviderError::Connection(
                                "provider telemetry sink stopped the request".to_string(),
                            ));
                        }
                        if !can_retry {
                            sleep_before_retry(started, self.timeout, delay).await?;
                            return Err(ProviderError::Timeout);
                        }
                        let backoff_guard = RetryBackoffAttemptGuard::new(
                            telemetry.as_ref(),
                            physical_attempt.saturating_add(1),
                            max_attempts,
                            started,
                            self.timeout,
                        );
                        sleep_before_retry_observed(started, self.timeout, delay, backoff_guard)
                            .await?;
                        attempt = retry_number;
                        continue;
                    }
                    record_attempt(
                        telemetry.as_ref(),
                        ProviderAttemptEvent {
                            attempt: physical_attempt,
                            max_attempts,
                            category: if e.is_timeout() {
                                ProviderAttemptCategory::Timeout
                            } else {
                                ProviderAttemptCategory::Connection
                            },
                            status: None,
                            outcome: ProviderAttemptOutcome::TransportFailed,
                            terminal: true,
                            retry_delay_ms: None,
                            retry_after_used: false,
                            remaining_deadline_ms: remaining_deadline_millis(started, self.timeout),
                        },
                    );
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
            let body = read_response_bounded(
                response,
                ProviderResponsePart::ErrorBody,
                MAX_ERROR_BODY_BYTES,
            )
            .await?;
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body,
            });
        }

        let attempt_guard = ResponseBodyAttemptGuard::new(
            telemetry.as_ref(),
            final_attempt,
            max_attempts,
            status.as_u16(),
            started,
            self.timeout,
        );

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
                attempt_guard,
            };
            let s = stream::unfold(init, sse_step);
            Ok(s.boxed())
        } else {
            let text = match read_response_bounded(
                response,
                ProviderResponsePart::CompletionBody,
                MAX_COMPLETION_BODY_BYTES,
            )
            .await
            {
                Ok(text) => text,
                Err(error) => {
                    attempt_guard.failed(&error);
                    return Err(error);
                }
            };
            let deltas = match parse_completion(&text) {
                Ok(deltas) => deltas,
                Err(error) => {
                    attempt_guard.failed(&error);
                    return Err(error);
                }
            };
            attempt_guard.completed();
            let s = stream::iter(deltas.into_iter().map(Ok));
            Ok(s.boxed())
        }
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        self.list_models_with_telemetry(None).await
    }

    async fn list_models_with_telemetry(
        &self,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        if self
            .models_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .is_none()
        {
            return Ok(None);
        }
        let (status, text) = self.get_models_response(telemetry.as_ref()).await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ProviderError::Unauthorized {
                status: status.as_u16(),
                hint: self.refresh_hint(),
            });
        }
        if !status.is_success() {
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: text,
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

        let parsed: ModelsResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::InvalidResponse(format!("malformed /models JSON: {e}")))?;
        Ok(Some(parsed.data.into_iter().map(|m| m.id).collect()))
    }
}

#[cfg(test)]
mod prompt_cache_key_tests {
    use super::{build_body, is_retryable_status, StatusCode};
    use crate::types::{ChatMessage, ChatRequest};

    fn base_request() -> ChatRequest {
        ChatRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage::user("hi")],
            tools: None,
            stream: false,
            temperature: None,
            reasoning_effort: None,
            max_tokens: None,
            response_format: None,
            prompt_cache_key: None,
        }
    }

    #[test]
    fn prompt_cache_key_serializes_when_some() {
        let mut req = base_request();
        req.prompt_cache_key = Some("some-key".to_string());
        let json = serde_json::to_string(&build_body(&req).unwrap()).unwrap();
        assert!(json.contains("prompt_cache_key"), "body: {json}");
        assert!(json.contains("some-key"), "body: {json}");
    }

    #[test]
    fn prompt_cache_key_omitted_when_none() {
        let req = base_request();
        let json = serde_json::to_string(&build_body(&req).unwrap()).unwrap();
        assert!(!json.contains("prompt_cache_key"), "body: {json}");
    }

    #[test]
    fn transient_gateway_statuses_are_retryable() {
        // The transient failures a hosted router raises when an upstream is
        // momentarily slow/overloaded — a second attempt commonly succeeds.
        for code in [408u16, 429, 502, 503, 504, 520, 521, 522, 523, 524, 529] {
            assert!(
                is_retryable_status(StatusCode::from_u16(code).unwrap()),
                "status {code} should be retryable"
            );
        }
    }

    #[test]
    fn deterministic_failures_are_not_retryable() {
        // 4xx (bad request/auth/not found) and a genuine 500 are deterministic:
        // retrying re-sends the same doomed request. 2xx is not a failure.
        for code in [200u16, 201, 400, 401, 403, 404, 409, 422, 500, 501, 505] {
            assert!(
                !is_retryable_status(StatusCode::from_u16(code).unwrap()),
                "status {code} must not be retryable"
            );
        }
    }
}
