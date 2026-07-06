//! Wiremock tests for the OpenAI-compatible / gateway provider (design §4, §7).
//! TESTER-owned; no production logic here. The Developer fills the method bodies.
//!
//! All requests target a local wiremock server (no traffic leaves the machine).
//! Env-touching tests are `#[serial]` and always use the dummy token "test-token"
//! — the real ANTHROPIC_AUTH_TOKEN is never read.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::auth::EnvBearerAuth;
use crate::error::ProviderError;
use crate::openai_compat::{OpenAICompatConfig, OpenAICompatProvider};
use crate::types::{ChatDelta, ChatMessage, ChatRequest, FinishReason};
use crate::Provider;

const CHAT_PATH: &str = "/v1/chat/completions";
const TEST_ENV: &str = "KITSU_TEST_GW_TOKEN";

/// Scoped env guard; restores prior value on drop.
struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let prev = env::var(key).ok();
        env::set_var(key, value);
        Self {
            key: key.to_string(),
            prev,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => env::set_var(&self.key, v),
            None => env::remove_var(&self.key),
        }
    }
}

fn provider_for(base_url: String) -> OpenAICompatProvider {
    let auth = Arc::new(EnvBearerAuth::new(
        TEST_ENV,
        None,
        Some("run `refresh-token` to refresh the gateway JWT".to_string()),
    ));
    OpenAICompatProvider::new(
        OpenAICompatConfig {
            base_url,
            chat_path: CHAT_PATH.to_string(),
            models_path: None,
            retries: 0,
            timeout: Duration::from_secs(5),
        },
        auth,
    )
}

fn simple_request() -> ChatRequest {
    ChatRequest {
        model: "example-model-v1".to_string(),
        messages: vec![ChatMessage::user("hello")],
        tools: None,
        stream: true,
        temperature: Some(0.2),
        max_tokens: None,
    }
}

/// An SSE body with two content deltas then `[DONE]`, mirroring the OpenAI
/// chat/completions streaming envelope (design §4).
fn sse_two_text_deltas() -> String {
    [
        r#"data: {"choices":[{"index":0,"delta":{"content":"hello "},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"world"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n")
}

/// An SSE body with a tool_calls delta + a `tool_calls` finish reason (design §4).
fn sse_tool_call() -> String {
    [
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"fs.read_file","arguments":"{\"path\":\"README.md\"}"}}]},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n")
}

/// Drain a ChatStream into (collected text, tool calls, terminal finish reason).
async fn collect_stream(
    mut stream: crate::ChatStream,
) -> (String, Vec<crate::types::ToolCall>, Option<FinishReason>) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut finish = None;
    while let Some(item) = stream.next().await {
        match item.expect("stream item should be Ok in the happy path") {
            ChatDelta::Text(t) => text.push_str(&t),
            ChatDelta::ToolCall(tc) => tool_calls.push(tc),
            ChatDelta::Usage(_) => {}
            ChatDelta::Done(reason) => finish = Some(reason),
        }
    }
    (text, tool_calls, finish)
}

// --- happy path -------------------------------------------------------------

/// design §7 `test_gateway_openai_chat_stream_ok`: two fragments + stop →
/// concatenated text and a final Done(Stop).
#[tokio::test]
#[serial]
async fn test_gateway_chat_stream_ok() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_two_text_deltas()),
        )
        .mount(&server)
        .await;

    let provider = provider_for(server.uri());
    let stream = provider
        .chat_stream(simple_request())
        .await
        .expect("chat_stream should succeed against a 200 SSE stub");

    let (text, tool_calls, finish) = collect_stream(stream).await;
    assert_eq!(
        text, "hello world",
        "collected text should be the concatenation"
    );
    assert!(
        tool_calls.is_empty(),
        "no tool calls expected in the text path"
    );
    assert_eq!(
        finish,
        Some(FinishReason::Stop),
        "final delta should be Done(Stop)"
    );
}

/// design §7 `test_gateway_auth_header_attached`: the matcher requires
/// `authorization: Bearer test-token`; without it the stub 401s. Reaching the OK
/// path proves the header was attached.
#[tokio::test]
#[serial]
async fn test_gateway_auth_header_attached() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");

    // Only matches when the bearer header is present and correct.
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_two_text_deltas()),
        )
        .mount(&server)
        .await;

    // Fallback: any request lacking the header gets a 401.
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let provider = provider_for(server.uri());
    let stream = provider
        .chat_stream(simple_request())
        .await
        .expect("chat_stream should hit the OK matcher (header attached)");

    let (text, _calls, finish) = collect_stream(stream).await;
    assert_eq!(text, "hello world");
    assert_eq!(finish, Some(FinishReason::Stop));
}

// --- error mapping ----------------------------------------------------------

/// design §7 `test_gateway_401_maps_unauthorized`: a 401 maps to
/// ProviderError::Unauthorized { status: 401, .. }. The error may surface either
/// from `chat_stream()` directly or as the stream's first item, so accept both.
#[tokio::test]
#[serial]
async fn test_gateway_401_maps_unauthorized() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .mount(&server)
        .await;

    let provider = provider_for(server.uri());

    let err = match provider.chat_stream(simple_request()).await {
        Err(e) => e,
        Ok(mut stream) => stream
            .next()
            .await
            .expect("a 401 stream must yield at least one item")
            .expect_err("the first stream item on 401 must be an error"),
    };

    match err {
        ProviderError::Unauthorized { status, .. } => {
            assert_eq!(status, 401, "Unauthorized must carry the 401 status");
        }
        other => panic!("expected ProviderError::Unauthorized, got {other:?}"),
    }
}

/// Security invariant (design §3/§7): the token value must never appear in any
/// error Display output.
#[tokio::test]
#[serial]
async fn test_token_never_in_error_display() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .mount(&server)
        .await;

    let provider = provider_for(server.uri());

    let err = match provider.chat_stream(simple_request()).await {
        Err(e) => e,
        Ok(mut stream) => stream
            .next()
            .await
            .expect("a 401 stream must yield at least one item")
            .expect_err("the first stream item on 401 must be an error"),
    };

    let display = err.to_string();
    assert!(
        !display.contains("test-token"),
        "the token must never be echoed in an error message: {display}"
    );
}

// --- tool calls -------------------------------------------------------------

/// design §7 `test_chat_stream_tool_calls`: a tool_calls delta + finish_reason
/// "tool_calls" → a ChatDelta::ToolCall is produced and the stream ends
/// Done(FinishReason::ToolCalls).
#[tokio::test]
#[serial]
async fn test_chat_stream_tool_calls() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_tool_call()),
        )
        .mount(&server)
        .await;

    let provider = provider_for(server.uri());
    let stream = provider
        .chat_stream(simple_request())
        .await
        .expect("chat_stream should succeed against the tool-call SSE stub");

    let (_text, tool_calls, finish) = collect_stream(stream).await;
    assert_eq!(
        tool_calls.len(),
        1,
        "exactly one tool call should be surfaced"
    );
    assert_eq!(tool_calls[0].tool_name, "fs.read_file");
    assert_eq!(tool_calls[0].call_id, "call_1");
    assert_eq!(
        finish,
        Some(FinishReason::ToolCalls),
        "stream should end Done(ToolCalls)"
    );
}

/// Regression: backoff must never panic, even for absurd attempt counts (the
/// old `1 << (attempt-1)` could overflow-shift). Caps at 200ms * 64.
#[test]
fn test_backoff_delay_never_panics_and_is_capped() {
    use super::backoff_delay;
    use std::time::Duration;

    // First retry is 200ms.
    assert_eq!(backoff_delay(1), Duration::from_millis(200));
    // Grows exponentially then saturates — no panic at extreme values.
    let capped = Duration::from_millis(200 * 64);
    for attempt in [7u32, 8, 50, 1000, u32::MAX] {
        assert_eq!(
            backoff_delay(attempt),
            capped,
            "attempt {attempt} should clamp to the cap, not overflow"
        );
    }
}

#[test]
fn test_parse_completion_surfaces_usage() {
    use super::parse_completion;
    use crate::types::ChatDelta;
    let body = r#"{
        "choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":21,"completion_tokens":9,"total_tokens":30}
    }"#;
    let deltas = parse_completion(body).expect("parse ok");
    let usage = deltas.iter().find_map(|d| match d {
        ChatDelta::Usage(u) => Some(*u),
        _ => None,
    });
    let u = usage.expect("usage delta present");
    assert_eq!(u.prompt_tokens, 21);
    assert_eq!(u.completion_tokens, 9);
    assert_eq!(u.total_tokens, 30);
    // Usage must come before the terminal Done.
    let pos_usage = deltas.iter().position(|d| matches!(d, ChatDelta::Usage(_)));
    let pos_done = deltas.iter().position(|d| matches!(d, ChatDelta::Done(_)));
    assert!(pos_usage < pos_done);
}

#[test]
fn test_parse_completion_without_usage_emits_no_usage_delta() {
    use super::parse_completion;
    use crate::types::ChatDelta;
    let body = r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#;
    let deltas = parse_completion(body).expect("parse ok");
    assert!(!deltas.iter().any(|d| matches!(d, ChatDelta::Usage(_))));
}

#[test]
fn test_sse_decoder_surfaces_trailing_usage_chunk() {
    use super::SseDecoder;
    use crate::types::ChatDelta;
    // Final chunk carries usage with empty choices (OpenAI include_usage shape).
    let mut dec = SseDecoder::default();
    let mut out = dec.push_bytes(
        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\
          data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\
          data: [DONE]\n",
    );
    out.extend(dec.finalize());
    let u = out
        .iter()
        .find_map(|d| match d {
            ChatDelta::Usage(u) => Some(*u),
            _ => None,
        })
        .expect("usage surfaced from stream");
    assert_eq!(u.total_tokens, 7);
    assert_eq!(u.prompt_tokens, 5);
}

// --- model_context_length (Stage 1b window discovery) -----------------------

fn provider_with_models(base_url: String) -> OpenAICompatProvider {
    let auth = Arc::new(EnvBearerAuth::new(TEST_ENV, None, None));
    OpenAICompatProvider::new(
        OpenAICompatConfig {
            base_url,
            chat_path: CHAT_PATH.to_string(),
            models_path: Some("/v1/models".to_string()),
            retries: 0,
            timeout: Duration::from_secs(5),
        },
        auth,
    )
}

/// A gateway that reports `max_model_len` per model → discovered as the window.
#[tokio::test]
#[serial]
async fn test_model_context_length_from_max_model_len() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"other","max_model_len":2048},{"id":"my-model","max_model_len":32768}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    assert_eq!(provider.model_context_length("my-model").await, Some(32768));
}

/// No context-length field (e.g. Ollama's OpenAI-compat route) → None.
#[tokio::test]
#[serial]
async fn test_model_context_length_none_when_field_absent() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":[{"id":"my-model"}]}"#))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    assert_eq!(provider.model_context_length("my-model").await, None);
}

/// A lone entry is used even if its id doesn't match exactly.
#[tokio::test]
#[serial]
async fn test_model_context_length_lone_entry_fallback() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"data":[{"id":"served-model","context_length":16384}]}"#),
        )
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    assert_eq!(provider.model_context_length("whatever").await, Some(16384));
}

/// No models_path configured → no request, None (never errors).
#[tokio::test]
#[serial]
async fn test_model_context_length_none_without_models_path() {
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    let provider = provider_for("http://127.0.0.1:0".to_string());
    assert_eq!(provider.model_context_length("m").await, None);
}

/// Regression: plain substring containment could bind a configured model to an
/// unrelated, differently-sized entry (e.g. "gpt-4" matching "gpt-4-turbo").
/// When more than one entry loosely matches, the ambiguity must NOT be resolved
/// by picking the first — that's exactly the silent-wrong-window bug.
#[tokio::test]
#[serial]
async fn test_model_context_length_ambiguous_loose_match_gives_up() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"gpt-4-turbo","context_length":128000},{"id":"gpt-4-mini","context_length":8192}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    // "gpt-4" loosely matches BOTH entries (delimiter-bounded on '-' in each) —
    // ambiguous, and there's more than one entry total, so must give up (None)
    // rather than silently binding to whichever came first.
    assert_eq!(provider.model_context_length("gpt-4").await, None);
}

/// A loose match that is uniquely resolvable (only one entry matches) is still
/// useful — e.g. a configured short alias for one specific served model id.
#[tokio::test]
#[serial]
async fn test_model_context_length_unique_loose_match_still_resolves() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"my-model-v2","context_length":32768},{"id":"other-model","context_length":4096}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    // "my-model" is not an exact id, but loosely (delimiter-bounded) matches
    // ONLY "my-model-v2" — unambiguous, so it should still resolve.
    assert_eq!(provider.model_context_length("my-model").await, Some(32768));
}
