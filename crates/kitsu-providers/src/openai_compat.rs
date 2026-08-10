//! OpenAI-compatible provider. Covers vLLM, OpenAI, Mistral cloud, AND the example
//! AI gateway — they share one wire shape; only base_url/path/auth differ
//! (design §1, §4).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::stream::{self, StreamExt};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::AuthProvider;
use crate::error::ProviderError;
use crate::telemetry::{
    ProviderAttemptCategory, ProviderAttemptEvent, ProviderAttemptOutcome, ProviderTelemetrySink,
};
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

/// Tool-call indices come from an untrusted gateway response. Keep the maximum
/// deliberately generous while preventing a sparse index from amplifying into
/// an arbitrarily large `Vec` allocation.
const MAX_STREAMED_TOOL_CALLS: usize = 64;

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
/// lockstep after a shared 429/503 response.
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

fn record_attempt(telemetry: Option<&ProviderTelemetrySink>, event: ProviderAttemptEvent) {
    if let Some(telemetry) = telemetry {
        telemetry.record(event);
    }
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
    async fn get_models_response(&self) -> Result<(StatusCode, String), ProviderError> {
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

        let outcome = match self.fetch_models_response(&url).await {
            Ok((status, text)) if status.is_success() => {
                validate_models_catalog(&text).map(|()| (status, text))
            }
            other => other,
        };
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
        *cache = ttl.map(|ttl| ModelsCacheEntry {
            stored_at: tokio::time::Instant::now(),
            ttl,
            outcome: outcome.clone(),
        });
        outcome
    }

    async fn fetch_models_response(
        &self,
        url: &str,
    ) -> Result<(StatusCode, String), ProviderError> {
        let rb = self.http.get(url).timeout(self.discovery_timeout);
        let rb = self.attach_auth(rb)?;
        let resp = rb.send().await.map_err(map_send_error)?;
        let status = resp.status();
        // On a non-success status, tolerate a body-read failure so the caller
        // still gets the real status — the diagnostic body is best-effort, and
        // losing it must not also lose the status that made the request fail in
        // the first place. A body-read failure on success IS load-bearing (the
        // caller needs that body to parse the actual data), so it still errors.
        let text = if status.is_success() {
            resp.text().await.map_err(map_send_error)?
        } else {
            resp.text().await.unwrap_or_default()
        };
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
        let (status, text) = self.get_models_response().await.ok()?;
        if !status.is_success() {
            return None;
        }
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        let entries = json.get("data")?.as_array()?;
        // Prefer an exact id match; then a loose match, but ONLY if it's unique —
        // an ambiguous loose match (e.g. "gpt-4" loosely matching both
        // "gpt-4-turbo" and "gpt-4-mini") is worse than giving up, since binding
        // to the wrong entry silently budgets against the wrong window. Finally
        // fall back to a lone entry, whatever its id.
        let exact = entries
            .iter()
            .find(|e| e.get("id").and_then(serde_json::Value::as_str) == Some(model));
        let entry = if let Some(e) = exact {
            e
        } else {
            let mut loose = entries.iter().filter(|e| {
                e.get("id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| loosely_matches(id, model))
            });
            match (loose.next(), loose.next()) {
                (Some(only), None) => only,
                _ => (entries.len() == 1).then(|| &entries[0])?,
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
        None
    }
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
        let body = build_body(&req);

        // Retry transient 429/503 responses within one end-to-end deadline.
        // Each attempt receives only the remaining budget; reqwest keeps that
        // timeout active through the response body, including streaming bodies.
        // Auth failures and all other 4xx responses return immediately below.
        let started = tokio::time::Instant::now();
        let max_attempts = self.retries.saturating_add(1);
        // Number of retries already completed; the physical attempt exposed to
        // telemetry is therefore always `attempt + 1`.
        let mut attempt: u32 = 0;
        let response = loop {
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
                    if (status == StatusCode::TOO_MANY_REQUESTS
                        || status == StatusCode::SERVICE_UNAVAILABLE)
                        && attempt < self.retries
                    {
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
                        record_attempt(
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
                                retry_delay_ms: Some(duration_millis(decision.delay)),
                                retry_after_used: decision.retry_after_used,
                                remaining_deadline_ms: remaining_after
                                    .map(duration_millis)
                                    .unwrap_or(0),
                            },
                        );
                        drop(resp);
                        if !can_retry {
                            sleep_before_retry(started, self.timeout, decision.delay).await?;
                            return Err(ProviderError::Timeout);
                        }
                        sleep_before_retry(started, self.timeout, decision.delay).await?;
                        attempt = retry_number;
                        continue;
                    }
                    record_attempt(
                        telemetry.as_ref(),
                        ProviderAttemptEvent {
                            attempt: physical_attempt,
                            max_attempts,
                            category: status_category(status),
                            status: Some(status.as_u16()),
                            outcome: if status.is_success() {
                                ProviderAttemptOutcome::ResponseAccepted
                            } else {
                                ProviderAttemptOutcome::Rejected
                            },
                            terminal: true,
                            retry_delay_ms: None,
                            retry_after_used: false,
                            remaining_deadline_ms: remaining_deadline_millis(started, self.timeout),
                        },
                    );
                    break resp;
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
                        record_attempt(
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
                                retry_delay_ms: Some(duration_millis(delay)),
                                retry_after_used: false,
                                remaining_deadline_ms: remaining_after
                                    .map(duration_millis)
                                    .unwrap_or(0),
                            },
                        );
                        if !can_retry {
                            sleep_before_retry(started, self.timeout, delay).await?;
                            return Err(ProviderError::Timeout);
                        }
                        sleep_before_retry(started, self.timeout, delay).await?;
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
        if self
            .models_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .is_none()
        {
            return Ok(None);
        }
        let (status, text) = self.get_models_response().await?;
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
