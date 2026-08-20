//! Wiremock tests for the OpenAI-compatible / gateway provider (design §4, §7).
//! TESTER-owned; no production logic here. The Developer fills the method bodies.
//!
//! All requests target a local wiremock server (no traffic leaves the machine).
//! Env-touching tests are `#[serial]` and always use the dummy token "test-token"
//! — the real ANTHROPIC_AUTH_TOKEN is never read.

use std::env;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::auth::{EnvBearerAuth, NoAuth};
use crate::error::{ProviderError, ProviderResponsePart};
use crate::openai_compat::{
    build_openai_compatible_catalog_route, build_openai_compatible_route,
    OpenAICompatCatalogRouteConfig, OpenAICompatConfig, OpenAICompatProvider,
    OpenAICompatRouteConfig,
};
use crate::route::{
    ProviderAuthSource, ProviderCatalogPolicy, ProviderProfileId, ProviderRouteDeclaration,
    ProviderRoutePolicy,
};
use crate::telemetry::{ProviderAttemptCategory, ProviderAttemptOutcome, ProviderTelemetrySink};
use crate::types::{ChatDelta, ChatMessage, ChatRequest, FinishReason, JsonSchemaFormat};
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
        reasoning_effort: None,
        max_tokens: None,
        response_format: None,
        prompt_cache_key: None,
    }
}

#[test]
#[serial]
fn route_factory_freezes_auth_and_builds_both_runtimes_from_one_policy() {
    let _guard = EnvGuard::set(TEST_ENV, "generation-one-token");
    let auth = EnvBearerAuth::new(TEST_ENV, None, Some("refresh safely".to_string()));
    let declaration = ProviderRouteDeclaration::compatible_chat_v1(
        ProviderProfileId::derive_v1(b"route-factory", b"primary").unwrap(),
        true,
    );
    let policy = ProviderRoutePolicy::new(
        2,
        0,
        Duration::from_secs(120),
        Duration::from_secs(45),
        Duration::from_secs(2),
        ProviderCatalogPolicy::Configured,
    )
    .unwrap();
    let snapshot = build_openai_compatible_route(
        OpenAICompatRouteConfig {
            declaration,
            model: "model-a".to_string(),
            auth_source: ProviderAuthSource::Environment,
            base_url: "https://example.invalid".to_string(),
            chat_path: CHAT_PATH.to_string(),
            models_path: Some("/v1/models".to_string()),
            policy,
        },
        &auth,
    )
    .unwrap();

    env::set_var(TEST_ENV, "generation-two-token");
    let inference = snapshot.inference_runtime().unwrap();
    let catalog = snapshot.catalog_runtime();
    assert_eq!(snapshot.identity().generation().get(), 1);
    assert_eq!(snapshot.model(), Some("model-a"));
    assert_eq!(inference.base_url, catalog.base_url);
    assert_eq!(inference.chat_path, catalog.chat_path);
    assert_eq!(inference.models_path, catalog.models_path);
    assert_eq!(inference.retries, 2);
    assert_eq!(catalog.retries, 0);
    assert_eq!(inference.timeout, Duration::from_secs(120));
    assert_eq!(catalog.timeout, Duration::from_secs(45));
    assert_eq!(inference.discovery_timeout, Duration::from_secs(2));
    assert_eq!(catalog.discovery_timeout, Duration::from_secs(45));
    assert!(Arc::ptr_eq(&inference.auth, &catalog.auth));
    let (_, value) = inference.auth.header().unwrap().unwrap();
    assert_eq!(value.as_bytes(), b"Bearer generation-one-token");
    assert!(value.is_sensitive());
}

#[test]
#[serial]
fn catalog_route_factory_freezes_auth_without_constructing_an_inference_binding() {
    let _guard = EnvGuard::set(TEST_ENV, "catalog-generation-token");
    let auth = EnvBearerAuth::new(TEST_ENV, None, None);
    let declaration = ProviderRouteDeclaration::compatible_catalog_v1(
        ProviderProfileId::derive_v1(b"catalog-route-factory", b"primary").unwrap(),
    );
    let policy = ProviderRoutePolicy::new(
        9,
        0,
        Duration::from_secs(120),
        Duration::from_secs(45),
        Duration::from_secs(2),
        ProviderCatalogPolicy::Configured,
    )
    .unwrap();
    let snapshot = build_openai_compatible_catalog_route(
        OpenAICompatCatalogRouteConfig {
            declaration,
            auth_source: ProviderAuthSource::Environment,
            base_url: "https://example.invalid".to_string(),
            chat_path: CHAT_PATH.to_string(),
            models_path: "/v1/models".to_string(),
            policy,
        },
        &auth,
    )
    .unwrap();

    env::set_var(TEST_ENV, "later-token");
    assert_eq!(snapshot.model(), None);
    assert!(snapshot.inference_runtime().is_none());
    let catalog = snapshot.catalog_runtime();
    assert_eq!(catalog.retries, 0);
    assert_eq!(catalog.timeout, Duration::from_secs(45));
    assert_eq!(catalog.discovery_timeout, Duration::from_secs(45));
    let (_, value) = catalog.auth.header().unwrap().unwrap();
    assert_eq!(value.as_bytes(), b"Bearer catalog-generation-token");
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

fn sse_tool_call_at_index(index: usize) -> String {
    let delta = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": index,
                    "id": "call_oversized",
                    "type": "function",
                    "function": {
                        "name": "fs.read_file",
                        "arguments": "{}"
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    [
        format!("data: {delta}"),
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#.to_string(),
        "data: [DONE]".to_string(),
        String::new(),
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

#[tokio::test]
async fn test_gateway_forwards_strict_json_schema_response_format() {
    let server = MockServer::start().await;
    let captured: Arc<std::sync::Mutex<Option<serde_json::Value>>> =
        Arc::new(std::sync::Mutex::new(None));
    let observed = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |request: &wiremock::Request| {
            *observed.lock().unwrap() = serde_json::from_slice(&request.body).ok();
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_two_text_deltas())
        })
        .mount(&server)
        .await;

    let mut request = simple_request();
    request.response_format = Some(JsonSchemaFormat {
        name: "kitsu_output".to_string(),
        strict: true,
        schema: serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"],
            "additionalProperties": false
        }),
    });
    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    let stream = provider
        .chat_stream(request)
        .await
        .expect("structured request should reach the provider");
    let _ = collect_stream(stream).await;

    let body = captured
        .lock()
        .unwrap()
        .clone()
        .expect("request JSON should be captured");
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(
        body["response_format"]["json_schema"]["name"],
        "kitsu_output"
    );
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["properties"]["answer"]["type"],
        "string"
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
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.retries = 5;

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

/// Most client errors are final: retrying a malformed request only adds latency
/// and can duplicate side effects at gateways that accepted part of it.
#[tokio::test]
#[serial]
async fn test_gateway_400_is_not_retried() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_: &wiremock::Request| {
            responder_calls.fetch_add(1, Ordering::Relaxed);
            ResponseTemplate::new(400).set_body_string("bad request")
        })
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.retries = 5;
    let err = match provider.chat_stream(simple_request()).await {
        Err(error) => error,
        Ok(_) => panic!("HTTP 400 must fail without retrying"),
    };

    assert!(matches!(err, ProviderError::Http { status: 400, .. }));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

/// A generic 500 is intentionally excluded from the transient gateway set.
/// Lock that policy through the physical send loop so a future classifier
/// refactor cannot silently turn deterministic server failures into retries.
#[tokio::test]
async fn test_gateway_500_is_not_retried() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_: &wiremock::Request| {
            responder_calls.fetch_add(1, Ordering::Relaxed);
            ResponseTemplate::new(500).set_body_string("deterministic server failure")
        })
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    provider.retries = 5;
    let err = match provider.chat_stream(simple_request()).await {
        Err(error) => error,
        Ok(_) => panic!("HTTP 500 must fail without retrying"),
    };

    assert!(matches!(err, ProviderError::Http { status: 500, .. }));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn test_gateway_retry_after_seconds_is_honored() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_: &wiremock::Request| {
            if responder_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "1")
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_two_text_deltas())
            }
        })
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    provider.retries = 1;
    provider.timeout = Duration::from_secs(3);
    let started = tokio::time::Instant::now();
    let _stream = provider
        .chat_stream(simple_request())
        .await
        .expect("the request should retry once and then succeed");

    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn test_physical_attempt_telemetry_reports_retry_and_terminal_success() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_: &wiremock::Request| {
            if responder_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "1")
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_two_text_deltas())
            }
        })
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    provider.retries = 1;
    provider.timeout = Duration::from_secs(3);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));

    let stream = provider
        .chat_stream_with_telemetry(simple_request(), Some(sink))
        .await
        .expect("the request should retry once and then succeed");
    let _ = collect_stream(stream).await;

    assert_eq!(calls.load(Ordering::Relaxed), 2);
    let events = events.lock().unwrap();
    assert_eq!(
        events.len(),
        2,
        "one terminal event is required per HTTP send"
    );
    let retry = &events[0];
    assert_eq!(retry.attempt, 1);
    assert_eq!(retry.max_attempts, 2);
    assert_eq!(retry.category, ProviderAttemptCategory::RateLimited);
    assert_eq!(retry.status, Some(429));
    assert_eq!(retry.outcome, ProviderAttemptOutcome::RetryScheduled);
    assert!(!retry.terminal);
    assert!(retry.retry_delay_ms.is_some_and(|delay| delay >= 1000));
    assert!(retry.retry_after_used);
    assert!(retry.remaining_deadline_ms <= 3000);

    let completed = &events[1];
    assert_eq!(completed.attempt, 2);
    assert_eq!(completed.category, ProviderAttemptCategory::Success);
    assert_eq!(completed.status, Some(200));
    assert_eq!(completed.outcome, ProviderAttemptOutcome::BodyCompleted);
    assert!(completed.terminal);
}

#[tokio::test]
async fn test_transient_gateway_504_is_retried_end_to_end() {
    // A 504 Gateway Timeout (no Retry-After header) must now drive the send
    // loop to retry on local backoff and then succeed — the whole point of
    // widening the retryable set beyond 429/503. Without the fix this returned
    // terminally on the first 504 and the turn was lost.
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_: &wiremock::Request| {
            if responder_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                ResponseTemplate::new(504)
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_two_text_deltas())
            }
        })
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    provider.retries = 1;
    provider.timeout = Duration::from_secs(3);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));

    let stream = provider
        .chat_stream_with_telemetry(simple_request(), Some(sink))
        .await
        .expect("a 504 then 200 should retry once and then succeed");
    let _ = collect_stream(stream).await;

    assert_eq!(calls.load(Ordering::Relaxed), 2, "the 504 must be retried");
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    let retry = &events[0];
    assert_eq!(retry.attempt, 1);
    assert_eq!(retry.category, ProviderAttemptCategory::ServerError);
    assert_eq!(retry.status, Some(504));
    assert_eq!(retry.outcome, ProviderAttemptOutcome::RetryScheduled);
    assert!(!retry.terminal);
    // No Retry-After header → local backoff floor is used, not an advertised delay.
    assert!(retry.retry_delay_ms.is_some());
    assert!(!retry.retry_after_used);

    let completed = &events[1];
    assert_eq!(completed.attempt, 2);
    assert_eq!(completed.category, ProviderAttemptCategory::Success);
    assert_eq!(completed.status, Some(200));
    assert!(completed.terminal);
}

#[tokio::test]
async fn test_cancelling_during_retry_backoff_records_one_terminal_disposition() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_: &wiremock::Request| {
            responder_calls.fetch_add(1, Ordering::Relaxed);
            ResponseTemplate::new(429).insert_header("retry-after", "1")
        })
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    provider.retries = 1;
    provider.timeout = Duration::from_secs(5);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let retry_scheduled = Arc::new(tokio::sync::Notify::new());
    let retry_observed = Arc::clone(&retry_scheduled);
    let sink = ProviderTelemetrySink::new(move |event| {
        let is_retry = event.outcome == ProviderAttemptOutcome::RetryScheduled;
        observed.lock().unwrap().push(event);
        if is_retry {
            retry_observed.notify_one();
        }
    });

    let mut request = Box::pin(provider.chat_stream_with_telemetry(simple_request(), Some(sink)));
    tokio::select! {
        _ = retry_scheduled.notified() => {}
        _ = &mut request => panic!("request left retry backoff unexpectedly"),
    }
    drop(request);

    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::RetryScheduled);
    assert!(!events[0].terminal);
    assert_eq!(events[1].attempt, 2);
    assert_eq!(events[1].status, None);
    assert_eq!(events[1].category, ProviderAttemptCategory::Cancelled);
    assert_eq!(events[1].outcome, ProviderAttemptOutcome::Cancelled);
    assert!(events[1].terminal);
    assert_eq!(
        events.iter().filter(|event| event.terminal).count(),
        1,
        "cancelling backoff must close, not duplicate, the terminal disposition"
    );
}

#[tokio::test]
async fn test_fallible_telemetry_sink_stops_before_retry_send() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_: &wiremock::Request| {
            responder_calls.fetch_add(1, Ordering::Relaxed);
            ResponseTemplate::new(429).insert_header("retry-after", "1")
        })
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    provider.retries = 2;
    provider.timeout = Duration::from_secs(5);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::with_fallible_observer(move |event| {
        let keep_running = event.outcome != ProviderAttemptOutcome::RetryScheduled;
        observed.lock().unwrap().push(event);
        keep_running
    });

    let error = match provider
        .chat_stream_with_telemetry(simple_request(), Some(sink))
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("observer rejection must stop the logical request"),
    };
    assert!(matches!(error, ProviderError::Connection(_)));
    assert_eq!(calls.load(Ordering::Relaxed), 1, "no retry may be sent");
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::RetryScheduled);
    assert!(events.iter().any(|event| {
        event.attempt == 2 && event.terminal && event.outcome == ProviderAttemptOutcome::Cancelled
    }));
    assert_eq!(events.iter().filter(|event| event.terminal).count(), 1);
}

#[tokio::test]
async fn test_physical_attempt_telemetry_excludes_http_body_and_marks_final_rejection() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_string("secret response detail"))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    let error = match provider
        .chat_stream_with_telemetry(simple_request(), Some(sink))
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("HTTP 400 must be terminal"),
    };
    assert!(matches!(error, ProviderError::Http { status: 400, .. }));

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].category, ProviderAttemptCategory::ClientError);
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::Rejected);
    assert!(events[0].terminal);
    let serialized = serde_json::to_string(&events[0]).unwrap();
    assert!(!serialized.contains("secret response detail"));
    assert!(!serialized.contains(&server.uri()));
}

#[tokio::test]
async fn test_gateway_retry_after_cannot_exceed_total_deadline() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "30"))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    provider.retries = 100;
    provider.timeout = Duration::from_millis(250);
    let started = tokio::time::Instant::now();
    let err = match provider.chat_stream(simple_request()).await {
        Err(error) => error,
        Ok(_) => panic!("the total deadline must stop retries"),
    };

    assert!(matches!(err, ProviderError::Timeout));
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn test_attempt_telemetry_marks_retry_that_cannot_fit_as_terminal_deadline() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "30"))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    provider.retries = 100;
    provider.timeout = Duration::from_millis(250);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    let error = match provider
        .chat_stream_with_telemetry(simple_request(), Some(sink))
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("the total deadline must stop retries"),
    };
    assert!(matches!(error, ProviderError::Timeout));

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1, "no second HTTP send was made");
    let attempt = &events[0];
    assert_eq!(attempt.attempt, 1);
    assert_eq!(attempt.category, ProviderAttemptCategory::Unavailable);
    assert_eq!(attempt.status, Some(503));
    assert_eq!(attempt.outcome, ProviderAttemptOutcome::DeadlineExceeded);
    assert!(attempt.terminal);
    assert!(!attempt.retry_after_used);
    assert_eq!(attempt.retry_delay_ms, None);
    assert!(attempt.remaining_deadline_ms <= 250);
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

/// Regression: a gateway-controlled sparse tool-call index must not make the
/// decoder allocate every missing slot or surface the invalid call.
#[tokio::test]
#[serial]
async fn test_chat_stream_ignores_excessive_tool_call_index() {
    let _g = EnvGuard::set(TEST_ENV, "test-token");

    for index in [super::MAX_STREAMED_TOOL_CALLS, usize::MAX] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(CHAT_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_tool_call_at_index(index)),
            )
            .mount(&server)
            .await;

        let provider = provider_for(server.uri());
        let stream = provider
            .chat_stream(simple_request())
            .await
            .expect("chat_stream should tolerate an excessive tool-call index");

        let (_text, tool_calls, finish) = collect_stream(stream).await;
        assert!(
            tool_calls.is_empty(),
            "tool call at index {index} must be ignored"
        );
        assert_eq!(finish, Some(FinishReason::ToolCalls));
    }
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
fn test_retry_after_accepts_seconds_and_http_date_and_caps_both() {
    use super::{parse_retry_after, MAX_RETRY_DELAY};

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(parse_retry_after("7", now), Some(Duration::from_secs(7)));
    assert_eq!(
        parse_retry_after("999999999999999999999999999999", now),
        Some(MAX_RETRY_DELAY),
        "overflowing delay-seconds must cap instead of falling back"
    );

    assert_eq!(
        parse_retry_after("Tue, 14 Nov 2023 22:13:31 GMT", now),
        Some(Duration::from_secs(11)),
        "the preferred IMF-fixdate HTTP-date form must be accepted"
    );
    let far_future = chrono::DateTime::<chrono::Utc>::from(now + Duration::from_secs(300));
    assert_eq!(
        parse_retry_after(&far_future.to_rfc2822(), now),
        Some(MAX_RETRY_DELAY)
    );
    assert_eq!(parse_retry_after("not-a-date", now), None);
}

#[test]
fn test_retry_delay_respects_header_with_small_bounded_jitter() {
    use super::{add_retry_jitter, retry_delay, MAX_RETRY_DELAY};

    let header = reqwest::header::HeaderValue::from_static("5");
    let delay = retry_delay(1, Some(&header), SystemTime::UNIX_EPOCH, u64::MAX);
    assert!(delay >= Duration::from_secs(5));
    assert!(delay <= Duration::from_millis(5_250));

    assert_eq!(
        add_retry_jitter(MAX_RETRY_DELAY, u64::MAX),
        MAX_RETRY_DELAY,
        "jitter must not push a capped Retry-After past the hard bound"
    );
}

#[tokio::test(start_paused = true)]
async fn test_retry_sleep_stops_at_total_request_deadline() {
    use super::sleep_before_retry;

    let started = tokio::time::Instant::now();
    let err = sleep_before_retry(started, Duration::from_secs(3), Duration::from_secs(30))
        .await
        .expect_err("a retry delay beyond the total deadline must time out");
    assert!(matches!(err, ProviderError::Timeout));
    assert_eq!(started.elapsed(), Duration::from_secs(3));
}

#[tokio::test(start_paused = true)]
async fn test_late_retry_wake_records_terminal_deadline() {
    use super::{sleep_before_retry_observed, RetryBackoffAttemptGuard};

    let started = tokio::time::Instant::now();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    let guard = RetryBackoffAttemptGuard::new(Some(&sink), 2, 3, started, Duration::from_secs(2));
    let waiter = tokio::spawn(sleep_before_retry_observed(
        started,
        Duration::from_secs(2),
        Duration::from_secs(1),
        guard,
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(3)).await;

    assert!(matches!(waiter.await.unwrap(), Err(ProviderError::Timeout)));
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].attempt, 2);
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::DeadlineExceeded);
    assert!(events[0].terminal);
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
    )
    .unwrap();
    out.extend(dec.finalize().unwrap());
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

#[tokio::test]
async fn test_done_terminates_without_waiting_for_wire_eof() {
    use super::{sse_step, ByteStream, ResponseBodyAttemptGuard, SseDecoder, SseState};

    let body = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\ndata: [DONE]\n".to_vec();
    let byte_stream: ByteStream =
        futures::stream::once(async { Ok::<Vec<u8>, reqwest::Error>(body) })
            .chain(futures::stream::pending())
            .boxed();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    let guard = ResponseBodyAttemptGuard::new(
        Some(&sink),
        1,
        1,
        200,
        tokio::time::Instant::now(),
        Duration::from_secs(5),
    );
    let state = SseState::Reading {
        stream: byte_stream,
        dec: SseDecoder::default(),
        queue: std::collections::VecDeque::new(),
        attempt_guard: guard,
    };

    let (item, state) = tokio::time::timeout(Duration::from_millis(100), sse_step(state))
        .await
        .expect("[DONE] must not wait for the pending transport")
        .expect("[DONE] must emit a terminal delta");
    assert!(matches!(item, Ok(ChatDelta::Text(text)) if text == "done"));
    let (item, state) = tokio::time::timeout(Duration::from_millis(100), sse_step(state))
        .await
        .expect("queued terminal delta must not wait for the pending transport")
        .expect("[DONE] must emit a terminal delta");
    assert!(matches!(item, Ok(ChatDelta::Done(FinishReason::Stop))));
    assert!(sse_step(state).await.is_none());

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::BodyCompleted);
    assert!(events[0].terminal);
}

#[test]
fn test_sse_decoder_rejects_malformed_error_and_empty_success_payloads() {
    use super::SseDecoder;

    let mut decoder = SseDecoder::default();
    let error = decoder
        .push_bytes(b"data: not-json\n")
        .expect_err("invalid data records must fail the response");
    assert!(matches!(error, ProviderError::InvalidResponse(_)));
    assert!(error.to_string().contains("malformed JSON"));

    // A genuine (non-null) `error` object in an otherwise-200 SSE response must
    // still fail the response. Unlike the old fixed-string behavior, the fix for
    // the cycle-5 false-positive/discarded-detail bug now surfaces the gateway's
    // own message text here (see test_process_line_real_error_surfaces_gateway_message_text
    // for the direct process_line-level coverage of that contract).
    let mut decoder = SseDecoder::default();
    let error = decoder
        .push_bytes(b"data: {\"error\":{\"message\":\"provider detail\"}}\n")
        .expect_err("invalid data records must fail the response");
    assert!(matches!(error, ProviderError::InvalidResponse(_)));
    assert!(error.to_string().contains("provider detail"));

    let mut decoder = SseDecoder::default();
    decoder.push_bytes(b"data: [DONE]\n").unwrap();
    let error = decoder
        .finalize()
        .expect_err("a sentinel without a semantic choice is not a completion");
    assert!(matches!(error, ProviderError::InvalidResponse(_)));
}

#[test]
fn test_nonstream_completion_rejects_error_and_empty_choice_objects() {
    use super::parse_completion;

    let error = parse_completion(r#"{"error":{"message":"secret provider detail"}}"#)
        .expect_err("HTTP-200 error objects must not become empty answers");
    assert!(matches!(error, ProviderError::InvalidResponse(_)));
    assert!(!error.to_string().contains("secret provider detail"));

    let error = parse_completion(r#"{"choices":[]}"#)
        .expect_err("an empty choice list must not become a successful answer");
    assert!(matches!(error, ProviderError::InvalidResponse(_)));

    let error = parse_completion(r#"{"choices":[{}]}"#)
        .expect_err("a defaulted choice without content or a terminal reason is not an answer");
    assert!(matches!(error, ProviderError::InvalidResponse(_)));
}

/// Regression for a false positive diverging from the streaming path: the OLD
/// non-streaming code did `value.get("error").is_some()` and returned Err, which
/// wrongly rejected a valid completion body carrying `"error": null` alongside
/// real choices, because `get("error")` returns `Some(&Value::Null)`, not
/// `None`. The streaming path already ignored a null error
/// (`get("error").filter(|e| !e.is_null())`); the fix aligns the non-streaming
/// path to do the same. This test would FAIL against the unfixed code (it would
/// receive InvalidResponse instead of the content delta + Done(Stop)).
#[test]
fn test_parse_completion_null_error_alongside_valid_choice_parses_ok() {
    use super::parse_completion;
    use crate::types::ChatDelta;

    let body = r#"{
        "error": null,
        "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}]
    }"#;
    let deltas = parse_completion(body).expect(
        "a completion body carrying `\"error\": null` alongside a valid choice must parse, \
         not be rejected",
    );
    assert!(
        matches!(deltas.last(), Some(ChatDelta::Done(FinishReason::Stop))),
        "expected the terminal Done(Stop); got {deltas:?}"
    );
    let text = deltas
        .iter()
        .find_map(|d| match d {
            ChatDelta::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .expect("expected a content Text delta");
    assert_eq!(text, "hi");
}

/// A real (non-null) `error` object in a successful completion body must STILL
/// be rejected with InvalidResponse — the null-tolerance fix must not swallow a
/// genuine error. This guards against over-correcting the false-positive fix.
#[test]
fn test_parse_completion_real_error_object_still_rejected() {
    use super::parse_completion;

    let body = r#"{
        "error": {"message": "rate limited", "type": "requests", "code": "429"},
        "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}]
    }"#;
    let error =
        parse_completion(body).expect_err("a real (non-null) error object must still be rejected");
    assert!(matches!(error, ProviderError::InvalidResponse(_)));
}

#[tokio::test]
async fn test_clean_eof_without_semantic_terminal_is_body_failure() {
    let server = MockServer::start().await;
    let body = r#"data: {"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}

"#;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    let mut stream = provider
        .chat_stream_with_telemetry(simple_request(), Some(sink))
        .await
        .unwrap();
    assert!(matches!(stream.next().await, Some(Ok(ChatDelta::Text(text))) if text == "partial"));
    assert!(matches!(
        stream.next().await,
        Some(Err(ProviderError::InvalidResponse(message)))
            if message.contains("explicit finish_reason")
    ));
    assert!(stream.next().await.is_none());

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].category, ProviderAttemptCategory::InvalidResponse);
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::BodyFailed);
    assert!(events[0].terminal);
}

#[tokio::test]
async fn test_clean_eof_with_finish_reason_remains_valid() {
    let server = MockServer::start().await;
    let body = r#"data: {"choices":[{"index":0,"delta":{"content":"complete"},"finish_reason":"stop"}]}

"#;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    let stream = provider.chat_stream(simple_request()).await.unwrap();
    let (text, calls, finish) = collect_stream(stream).await;
    assert_eq!(text, "complete");
    assert!(calls.is_empty());
    assert_eq!(finish, Some(FinishReason::Stop));
}

#[test]
fn test_sse_decoder_rejects_newline_free_oversized_record() {
    use super::{SseDecoder, MAX_SSE_LINE_BYTES};

    let mut dec = SseDecoder::default();
    let error = dec
        .push_bytes(&vec![b'x'; MAX_SSE_LINE_BYTES + 1])
        .expect_err("an unterminated SSE record must not grow without bound");
    assert!(matches!(
        error,
        ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::SseLine,
            limit_bytes: MAX_SSE_LINE_BYTES,
        }
    ));
}

#[test]
fn test_sse_decoder_enforces_aggregate_stream_ceiling() {
    use super::{SseDecoder, MAX_SSE_STREAM_BYTES};

    let mut dec = SseDecoder {
        received_bytes: MAX_SSE_STREAM_BYTES,
        ..SseDecoder::default()
    };
    let error = dec
        .push_bytes(b"\n")
        .expect_err("many individually small SSE records still need a total ceiling");
    assert!(matches!(
        error,
        ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::SseStream,
            limit_bytes: MAX_SSE_STREAM_BYTES,
        }
    ));
}

#[test]
fn test_sse_decoder_bounds_fragmented_tool_arguments() {
    use super::{SseDecoder, MAX_STREAMED_TOOL_ARGUMENT_BYTES};

    let mut dec = SseDecoder::default();
    let mut deltas = Vec::new();
    let fragment = "x".repeat(16 * 1024);
    let mut error = None;
    for index in 0..=(MAX_STREAMED_TOOL_ARGUMENT_BYTES / fragment.len()) {
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": (index == 0).then_some("call_1"),
                        "function": {
                            "name": (index == 0).then_some("fs.write_file"),
                            "arguments": fragment,
                        }
                    }]
                }
            }]
        });
        let line = format!("data: {chunk}");
        if let Err(found) = dec.process_line(&line, &mut deltas) {
            error = Some(found);
            break;
        }
    }
    assert!(matches!(
        error,
        Some(ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::ToolArguments,
            limit_bytes: MAX_STREAMED_TOOL_ARGUMENT_BYTES,
        })
    ));
}

/// Regression for the cycle-5 finding: a single SSE `data:` line carrying a
/// tool-argument delta LARGER than the old 256 KiB line cap but within the 1 MiB
/// argument budget must parse successfully. Under the old `MAX_SSE_LINE_BYTES`
/// (256 KiB) this exact line was rejected by the line ceiling with
/// `ResponseTooLarge { part: SseLine }` at `push_bytes` — before the argument
/// budget was ever consulted — so this test fails against the unfixed code.
#[test]
fn test_sse_decoder_accepts_large_single_line_tool_argument_within_budget() {
    use super::{SseDecoder, MAX_SSE_LINE_BYTES, MAX_STREAMED_TOOL_ARGUMENT_BYTES};

    // Pick an argument just above the OLD 256 KiB line cap and well within the
    // 1 MiB argument budget. 320 KiB < 1 MiB, and the line wrapping it is larger
    // still (envelope + escaping), so it always exceeded the old 256 KiB line.
    let arg_len = 320 * 1024;
    assert!(
        arg_len > 256 * 1024 && arg_len < MAX_STREAMED_TOOL_ARGUMENT_BYTES,
        "fixture must straddle the old line cap and the argument budget"
    );
    let arguments = "x".repeat(arg_len);

    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "fs.write_file",
                        "arguments": arguments,
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let line = format!("data: {chunk}\n\n");
    assert!(
        line.len() > 256 * 1024,
        "the full line must exceed the old 256 KiB line cap"
    );
    assert!(
        line.len() <= MAX_SSE_LINE_BYTES,
        "the full line must fit the new raised line cap"
    );

    let mut dec = SseDecoder::default();
    dec.push_bytes(line.as_bytes())
        .expect("a single-line argument delta within the 1 MiB budget must parse");
    // Tool calls accumulate and flush at finalize; supply a terminal finish + DONE.
    let tail = b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n";
    dec.push_bytes(tail).expect("terminal chunk must parse");
    let out = dec
        .finalize()
        .expect("finalize must succeed for a valid stream");
    assert!(
        out.iter()
            .any(|d| matches!(d, ChatDelta::ToolCall(tc) if tc.tool_name == "fs.write_file")),
        "the large single-line tool call must be surfaced"
    );
}

/// Guard that the line ceiling still exists after being raised: a line that
/// genuinely exceeds the NEW `MAX_SSE_LINE_BYTES` is rejected with the `SseLine`
/// part. The asserted limit is hardcoded to the raised 4 MiB value (NOT the
/// `MAX_SSE_LINE_BYTES` symbol) on purpose: under the old code this same payload
/// was rejected as `SseLine` too, but with `limit_bytes = 256 KiB`, so a symbolic
/// `== MAX_SSE_LINE_BYTES` check would pass against both versions. Pinning the
/// literal makes this fail against the unfixed 256 KiB constant.
#[test]
fn test_sse_decoder_still_rejects_line_above_raised_ceiling() {
    use super::{SseDecoder, MAX_SSE_LINE_BYTES};

    // Sanity: the constant under test is the raised value this guard pins.
    assert_eq!(MAX_SSE_LINE_BYTES, 4 * 1024 * 1024);

    let mut dec = SseDecoder::default();
    let error = dec
        .push_bytes(&vec![b'x'; MAX_SSE_LINE_BYTES + 1])
        .expect_err("a line above the raised ceiling must still be rejected");
    assert!(
        matches!(error, ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::SseLine,
            limit_bytes,
        } if limit_bytes == 4 * 1024 * 1024),
        "must be rejected as SseLine reporting the raised 4 MiB limit (not the old 256 KiB)"
    );
}

/// Regression for the cycle-5 finding: a single SSE line whose tool-call
/// argument exceeds the 1 MiB argument budget must be rejected with the
/// `ToolArguments` part label, not `SseLine`. Under the old 256 KiB line cap the
/// ~1.2 MiB line was rejected by the line ceiling first (`SseLine`), so the
/// argument budget was unreachable for single-line deltas; this test fails
/// against the unfixed code (it gets `SseLine` instead of `ToolArguments`).
#[test]
fn test_sse_decoder_oversized_single_line_argument_hits_argument_cap() {
    use super::{SseDecoder, MAX_SSE_LINE_BYTES, MAX_STREAMED_TOOL_ARGUMENT_BYTES};

    let arg_len = MAX_STREAMED_TOOL_ARGUMENT_BYTES + 1024;
    let arguments = "x".repeat(arg_len);

    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "fs.write_file",
                        "arguments": arguments,
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let line = format!("data: {chunk}\n\n");
    // The line must clear the (raised) line cap so the line ceiling does NOT fire
    // first, letting the argument budget be the one that rejects it.
    assert!(
        line.len() <= MAX_SSE_LINE_BYTES,
        "oversized-argument line must fit the line cap so the argument cap fires"
    );

    let mut dec = SseDecoder::default();
    let error = dec
        .push_bytes(line.as_bytes())
        .expect_err("an argument over the 1 MiB budget must be rejected");
    assert!(matches!(
        error,
        ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::ToolArguments,
            limit_bytes: MAX_STREAMED_TOOL_ARGUMENT_BYTES,
        }
    ));
}

#[test]
fn test_process_line_null_error_alongside_valid_data_parses_successfully() {
    // Regression for a false positive: the OLD code did
    // `value.get("error").is_some()` and returned Err, which wrongly rejected a
    // chunk carrying `"error": null` next to otherwise-valid data because
    // `get("error")` returns `Some(&Value::Null)`, not `None`. The fix ignores a
    // null error and parses the chunk normally. This test would FAIL against the
    // unfixed code (it would receive an InvalidResponse instead of a Text delta).
    use super::SseDecoder;

    let mut dec = SseDecoder::default();
    let mut deltas = Vec::new();
    let chunk = serde_json::json!({
        "error": null,
        "choices": [{
            "index": 0,
            "delta": { "content": "hello" },
            "finish_reason": null,
        }]
    });
    let line = format!("data: {chunk}");
    dec.process_line(&line, &mut deltas).expect(
        "a chunk carrying `\"error\": null` alongside valid data must parse, not be rejected",
    );
    assert!(
        matches!(deltas.as_slice(), [ChatDelta::Text(t)] if t == "hello"),
        "expected the content delta to be emitted; got {deltas:?}"
    );
}

#[test]
fn test_process_line_real_error_surfaces_gateway_message_text() {
    // Regression for discarded detail: the OLD code threw away the gateway's
    // message and returned only the fixed generic string. The fix surfaces the
    // gateway's own `error.message` text inside the InvalidResponse. This test
    // asserts the formatted error CONTAINS the gateway's actual text
    // ("rate limited"), which would FAIL against the unfixed code.
    use super::SseDecoder;

    let mut dec = SseDecoder::default();
    let mut deltas = Vec::new();
    let chunk = serde_json::json!({
        "error": { "message": "rate limited", "type": "requests", "code": "429" }
    });
    let line = format!("data: {chunk}");
    let err = dec
        .process_line(&line, &mut deltas)
        .expect_err("a real (non-null) error object must still cause process_line to return Err");
    let message = match err {
        ProviderError::InvalidResponse(m) => m,
        other => panic!("expected InvalidResponse, got {other:?}"),
    };
    assert!(
        message.contains("rate limited"),
        "error message must surface the gateway's own text; got: {message}"
    );
}

#[test]
fn test_process_line_no_error_field_still_parses_successfully() {
    // Happy-path regression guard: a normal chunk with NO error field at all
    // must continue to parse successfully. This already passed before the fix
    // and is kept to prove the happy path was not broken.
    use super::SseDecoder;

    let mut dec = SseDecoder::default();
    let mut deltas = Vec::new();
    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": { "content": "ok" }
        }]
    });
    let line = format!("data: {chunk}");
    dec.process_line(&line, &mut deltas)
        .expect("a normal chunk with no error field must parse successfully");
    assert!(
        matches!(deltas.as_slice(), [ChatDelta::Text(t)] if t == "ok"),
        "expected the content delta to be emitted; got {deltas:?}"
    );
}

#[tokio::test]
async fn test_nonstream_completion_body_has_hard_ceiling() {
    use super::MAX_COMPLETION_BODY_BYTES;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200).set_body_bytes(vec![b'x'; MAX_COMPLETION_BODY_BYTES + 1]),
        )
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    let mut request = simple_request();
    request.stream = false;
    let error = match provider.chat_stream(request).await {
        Err(error) => error,
        Ok(_) => panic!("an oversized non-stream response must fail before parsing"),
    };
    assert!(matches!(
        error,
        ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::CompletionBody,
            limit_bytes: MAX_COMPLETION_BODY_BYTES,
        }
    ));
}

#[tokio::test]
async fn test_error_body_has_small_safe_ceiling() {
    use super::MAX_ERROR_BODY_BYTES;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(500).set_body_bytes(vec![b'x'; MAX_ERROR_BODY_BYTES + 1]),
        )
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    let error = match provider.chat_stream(simple_request()).await {
        Err(error) => error,
        Ok(_) => panic!("an oversized diagnostic body must not be retained"),
    };
    assert!(matches!(
        error,
        ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::ErrorBody,
            limit_bytes: MAX_ERROR_BODY_BYTES,
        }
    ));
    assert!(!error.to_string().contains(&"x".repeat(1024)));
}

#[tokio::test]
async fn test_accepted_header_is_followed_by_terminal_decode_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not-json"))
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    let mut request = simple_request();
    request.stream = false;
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    assert!(matches!(
        provider
            .chat_stream_with_telemetry(request, Some(sink))
            .await,
        Err(ProviderError::InvalidResponse(_))
    ));

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].category, ProviderAttemptCategory::InvalidResponse);
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::BodyFailed);
    assert!(events[0].terminal);
}

#[tokio::test]
async fn test_dropped_accepted_stream_is_terminal_cancellation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_two_text_deltas()),
        )
        .mount(&server)
        .await;

    let mut provider = provider_for(server.uri());
    provider.auth = Arc::new(NoAuth);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    let stream = provider
        .chat_stream_with_telemetry(simple_request(), Some(sink))
        .await
        .unwrap();
    drop(stream);

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].category, ProviderAttemptCategory::Cancelled);
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::Cancelled);
    assert!(events[0].terminal);
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

#[tokio::test]
async fn test_model_catalog_body_has_hard_ceiling() {
    use super::MAX_MODEL_CATALOG_BODY_BYTES;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
            b'x';
            MAX_MODEL_CATALOG_BODY_BYTES
                + 1
        ]))
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    assert!(matches!(
        provider.list_models_with_telemetry(Some(sink)).await,
        Err(ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::ModelCatalogBody,
            limit_bytes: MAX_MODEL_CATALOG_BODY_BYTES,
        })
    ));
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].attempt, 1);
    assert_eq!(events[0].max_attempts, 1);
    assert_eq!(events[0].status, Some(200));
    assert_eq!(
        events[0].category,
        ProviderAttemptCategory::ResponseTooLarge
    );
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::BodyFailed);
    assert!(events[0].terminal);
}

#[tokio::test]
async fn test_model_catalog_telemetry_reports_one_send_and_no_cache_attempt() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "cached-model"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));

    for _ in 0..2 {
        assert_eq!(
            provider
                .list_models_with_telemetry(Some(sink.clone()))
                .await
                .unwrap(),
            Some(vec!["cached-model".to_string()])
        );
    }

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1, "the cache hit made no physical send");
    assert_eq!(events[0].attempt, 1);
    assert_eq!(events[0].max_attempts, 1);
    assert_eq!(events[0].category, ProviderAttemptCategory::Success);
    assert_eq!(events[0].status, Some(200));
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::BodyCompleted);
    assert!(events[0].terminal);
}

#[tokio::test]
async fn test_model_catalog_telemetry_reports_http_rejection_without_body_details() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(503).set_body_string("secret catalog detail"))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    assert!(matches!(
        provider.list_models_with_telemetry(Some(sink)).await,
        Err(ProviderError::Http { status: 503, .. })
    ));

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].category, ProviderAttemptCategory::Unavailable);
    assert_eq!(events[0].status, Some(503));
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::Rejected);
    assert!(events[0].terminal);
    let serialized = serde_json::to_string(&events[0]).unwrap();
    assert!(!serialized.contains("secret catalog detail"));
    assert!(!serialized.contains(&server.uri()));
}

#[tokio::test]
async fn test_model_catalog_telemetry_reports_invalid_success_body_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not valid json"))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    for _ in 0..2 {
        assert!(matches!(
            provider
                .list_models_with_telemetry(Some(sink.clone()))
                .await,
            Err(ProviderError::InvalidResponse(_))
        ));
    }

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1, "the negative cache made no second send");
    assert_eq!(events[0].category, ProviderAttemptCategory::InvalidResponse);
    assert_eq!(events[0].status, Some(200));
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::BodyFailed);
    assert!(events[0].terminal);
}

#[tokio::test]
async fn test_model_catalog_telemetry_closes_a_cancelled_send() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .set_body_json(serde_json::json!({"data": []})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    let request =
        tokio::spawn(async move { provider.list_models_with_telemetry(Some(sink)).await });
    for _ in 0..100 {
        if !server.received_requests().await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!server.received_requests().await.unwrap().is_empty());
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].attempt, 1);
    assert_eq!(events[0].status, None);
    assert_eq!(events[0].category, ProviderAttemptCategory::Cancelled);
    assert_eq!(events[0].outcome, ProviderAttemptOutcome::Cancelled);
    assert!(events[0].terminal);
}

#[tokio::test]
async fn test_model_catalog_is_reused_across_list_and_context_discovery() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"data":[{"id":"cached-model","context_length":32768}]}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    assert_eq!(
        provider.list_models().await.unwrap(),
        Some(vec!["cached-model".to_string()])
    );
    assert_eq!(
        provider.model_context_length("cached-model").await,
        Some(32768)
    );
}

#[tokio::test]
async fn test_model_discovery_failures_are_negatively_cached() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(503).set_body_string("catalog unavailable"))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    assert!(matches!(
        provider.list_models().await,
        Err(ProviderError::Http { status: 503, .. })
    ));
    assert_eq!(provider.model_context_length("any-model").await, None);
    assert!(matches!(
        provider.list_models().await,
        Err(ProviderError::Http { status: 503, .. })
    ));
}

#[tokio::test]
async fn test_malformed_success_catalog_uses_the_negative_cache() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not valid json"))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    for _ in 0..2 {
        assert!(matches!(
            provider.list_models().await,
            Err(ProviderError::InvalidResponse(_))
        ));
    }
    let cache = provider.models_cache.lock().await;
    let entry = cache.as_ref().expect("malformed catalog is cached briefly");
    assert_eq!(entry.ttl, OpenAICompatProvider::MODELS_NEGATIVE_CACHE_TTL);
}

/// Advisory discovery must not inherit a long inference timeout. The discovery
/// budget is shortened further here so the integration test stays fast.
#[tokio::test]
async fn test_model_discovery_has_short_independent_deadline() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(1))
                .set_body_string(r#"{"data":[{"id":"slow","context_length":8192}]}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    provider.timeout = Duration::from_secs(60);
    provider.discovery_timeout = Duration::from_millis(100);
    let started = tokio::time::Instant::now();
    assert_eq!(provider.model_context_length("slow").await, None);
    assert!(started.elapsed() < Duration::from_millis(900));
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

/// The Hugging Face router advertises no top-level window and nests one per
/// serving provider under `providers[]`. A `:provider` routing pin selects that
/// provider's window (base id matches the un-pinned catalog id).
#[tokio::test]
#[serial]
async fn test_model_context_length_from_pinned_nested_provider() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"deepseek-ai/DeepSeek-V4-Pro","providers":[{"provider":"novita","status":"live","context_length":1048576},{"provider":"together","status":"live","context_length":512000}]}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    assert_eq!(
        provider
            .model_context_length("deepseek-ai/DeepSeek-V4-Pro:together")
            .await,
        Some(512000)
    );
}

/// Unpinned: take the MIN across live nested providers — a safe lower bound,
/// since an unpinned request may be dispatched to any of them. An error-status
/// provider is excluded even if it advertises a (tiny) window.
#[tokio::test]
#[serial]
async fn test_model_context_length_nested_provider_min_excludes_errored() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"Qwen/Qwen3.5-27B","providers":[{"provider":"deepinfra","status":"live","context_length":262144},{"provider":"novita","status":"live","context_length":131072},{"provider":"broken","status":"error","context_length":4096}]}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    // min(262144, 131072) — the errored 4096 is skipped.
    assert_eq!(
        provider.model_context_length("Qwen/Qwen3.5-27B").await,
        Some(131072)
    );
}

/// A top-level window still wins over `providers[]` (unchanged precedence).
#[tokio::test]
#[serial]
async fn test_model_context_length_top_level_beats_nested() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"m","context_length":200000,"providers":[{"provider":"x","status":"live","context_length":8192}]}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    assert_eq!(provider.model_context_length("m").await, Some(200000));
}

/// The unpinned MIN is floored at the no-discovery default: a provider
/// advertising a sub-default window must not make discovery worse than the
/// 8192 fallback it replaces.
#[tokio::test]
#[serial]
async fn test_model_context_length_nested_min_floored_at_default() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"m","providers":[{"provider":"a","status":"live","context_length":4096},{"provider":"b","status":"live","context_length":131072}]}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    // raw min is 4096; floored up to 8192 so discovery is never worse than the default.
    assert_eq!(provider.model_context_length("m").await, Some(8192));
}

/// A pin naming a provider that is absent from `providers[]` falls through to
/// the unpinned MIN rather than failing.
#[tokio::test]
#[serial]
async fn test_model_context_length_pin_absent_falls_through_to_min() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"org/m","providers":[{"provider":"novita","status":"live","context_length":262144},{"provider":"together","status":"live","context_length":131072}]}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    // pinned "scaleway" isn't listed → min(262144, 131072).
    assert_eq!(
        provider.model_context_length("org/m:scaleway").await,
        Some(131072)
    );
}

/// A non-live status ("staging") is excluded from the MIN (allowlist, not a
/// single-value denylist), so a small staging window can't drag the budget down.
#[tokio::test]
#[serial]
async fn test_model_context_length_nested_excludes_nonlive_status() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"m","providers":[{"provider":"live1","status":"live","context_length":131072},{"provider":"stg","status":"staging","context_length":16384}]}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    assert_eq!(provider.model_context_length("m").await, Some(131072));
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

/// Regression: a lone catalog entry that matches neither exactly nor loosely
/// must NOT bind a window. Before the fix, the sole-entry fallback accepted
/// `entries[0]` whatever its id, silently budgeting against the wrong window
/// (here a "whatever" query against a "served-model" entry returned
/// `Some(16384)` instead of `None`).
#[tokio::test]
#[serial]
async fn test_model_context_length_lone_nonmatching_entry_is_none() {
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
    // "whatever" is neither an exact id nor a delimiter-bounded loose match of
    // "served-model": a sole entry must not be accepted as a fallback.
    assert_eq!(provider.model_context_length("whatever").await, None);
}

/// A lone entry that DOES match — exactly or loosely — still resolves to its
/// own context length. (Guards against over-correcting the lone-entry fix into
/// rejecting every single-entry catalog.)
#[tokio::test]
#[serial]
async fn test_model_context_length_lone_matching_entry_resolves() {
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
    // Exact id match on the sole entry.
    assert_eq!(
        provider.model_context_length("served-model").await,
        Some(16384)
    );
    // A short alias that loosely (delimiter-bounded) matches the sole entry.
    assert_eq!(provider.model_context_length("served").await, Some(16384));
}

/// An exact match among multiple entries still resolves (unchanged behavior).
#[tokio::test]
#[serial]
async fn test_model_context_length_exact_match_among_many() {
    let server = MockServer::start().await;
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"alpha","context_length":4096},{"id":"beta","context_length":32768},{"id":"gamma","context_length":128000}]}"#,
        ))
        .mount(&server)
        .await;
    let provider = provider_with_models(server.uri());
    assert_eq!(provider.model_context_length("beta").await, Some(32768));
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

/// P10: a cache hit must return the memoized context length WITHOUT re-parsing
/// the catalog JSON. We prove the memo is used by stubbing the catalog with a
/// single response, calling `model_context_length` (cold miss → parse + memo),
/// then asserting the second call for the same model returns the same value
/// while the stub is hit exactly once.
#[tokio::test]
async fn test_model_context_length_memoized_on_cache_hit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"data":[{"id":"m","context_length":8192}]}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    // Cold miss: populates the catalog and memoizes 8192 for "m".
    assert_eq!(provider.model_context_length("m").await, Some(8192));
    // Cache hit: served from the memo, no second HTTP fetch.
    assert_eq!(provider.model_context_length("m").await, Some(8192));
}

/// P10: the memo stores `None` results too, so a model whose length can't be
/// discovered isn't re-parsed on every call. The stub is hit exactly once.
#[tokio::test]
async fn test_model_context_length_memoizes_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":[{"id":"no-field"}]}"#))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    assert_eq!(provider.model_context_length("no-field").await, None);
    // Second call returns the memoized `None`; no re-parse, no second fetch.
    assert_eq!(provider.model_context_length("no-field").await, None);
}

/// P10: per-model memo entries are independent — resolving one model does not
/// short-circuit another. The catalog is fetched once (shared cache) and each
/// distinct model parses/memoizes independently.
#[tokio::test]
async fn test_model_context_length_memo_is_per_model() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"a","context_length":4096},{"id":"b","context_length":32768}]}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    assert_eq!(provider.model_context_length("a").await, Some(4096));
    assert_eq!(provider.model_context_length("b").await, Some(32768));
    // Distinct models re-serve from their own memo slots, still one catalog fetch.
    assert_eq!(provider.model_context_length("a").await, Some(4096));
    assert_eq!(provider.model_context_length("b").await, Some(32768));
}

/// P10, requirement (a) proven rigorously: the earlier `expect(1)` tests above
/// only show the raw catalog body isn't re-fetched over HTTP — that was ALREADY
/// true before this change (the catalog body itself was already cached). What's
/// new here is that the *parsed* length is memoized too, so a cache hit never
/// touches `serde_json::from_str` again. We prove that by corrupting the cached
/// raw body in place after the first call: if the second call fell back to
/// re-parsing the (now-corrupted) cached text instead of returning the memo, it
/// would return `None`.
#[tokio::test]
async fn test_model_context_length_second_call_does_not_reparse_cached_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"data":[{"id":"m","context_length":8192}]}"#),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    assert_eq!(provider.model_context_length("m").await, Some(8192));

    // Corrupt the cached raw catalog body in place. A naive re-parse of `text`
    // on the second call would now fail and return `None`.
    {
        let mut cache = provider.models_cache.lock().await;
        let entry = cache.as_mut().expect("catalog cached after first call");
        if let Ok((_, text)) = &mut entry.outcome {
            *text = "not valid json".to_string();
        }
    }

    // Still 8192: the memo served the hit, the corrupted body was never touched.
    assert_eq!(provider.model_context_length("m").await, Some(8192));
}

/// P10, requirement (b): once the catalog cache's TTL has actually expired, the
/// next call must refetch AND re-parse — the memo must not go on serving a
/// value derived from the now-stale catalog. We force expiry by rewinding
/// `stored_at` (rather than sleeping out the real 60s TTL) and prove the fresh
/// value wins.
#[tokio::test]
async fn test_model_context_length_memo_refills_after_ttl_expiry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"data":[{"id":"m","context_length":8192}]}"#),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"data":[{"id":"m","context_length":16384}]}"#),
        )
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    assert_eq!(provider.model_context_length("m").await, Some(8192));

    // Force the cached entry to look TTL-expired without waiting out the real
    // 60s window.
    {
        let mut cache = provider.models_cache.lock().await;
        let entry = cache.as_mut().expect("catalog cached after first call");
        entry.stored_at = tokio::time::Instant::now() - Duration::from_secs(3600);
    }

    // TTL expired -> refetch (second mock, now 16384) -> the memo rebuilds from
    // the fresh body instead of serving the stale 8192 it had cached before.
    assert_eq!(provider.model_context_length("m").await, Some(16384));
}

/// P10, requirement (c): a model that isn't in the catalog at all (as opposed
/// to being present but missing the length field) still returns `None`, and
/// stays `None` on a repeat call served from the memo.
#[tokio::test]
async fn test_model_context_length_unknown_model_stays_none_through_memo() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"a","context_length":4096},{"id":"b","context_length":8192}]}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    // "unknown-model" matches neither entry exactly nor loosely, and there's
    // more than one entry, so the lone-entry fallback doesn't apply either.
    assert_eq!(provider.model_context_length("unknown-model").await, None);
    assert_eq!(provider.model_context_length("unknown-model").await, None);
}

/// A refresh that returns the SAME catalog text must PRESERVE the warm
/// per-model context-length memo — not discard it to an empty HashMap.
/// Before the fix, the cache write in `get_models_response` always reset
/// `context_lengths` to an empty `HashMap::new()`, so the assertion below
/// FAILED: the memo was cleared on every refresh regardless of whether the
/// catalog actually changed. After the fix, the memo is carried across a
/// refresh only when the new catalog text exactly equals the previous
/// successful catalog text.
///
/// We prove the ACTUAL behavior change by inspecting the memo directly (not a
/// proxy return value): the first call memoizes `{"m": Some(8192)}`; we force
/// TTL expiry; we trigger a refresh via `get_models_response` directly (so
/// `model_context_length` doesn't re-populate the memo via a parse); then we
/// assert the carried-over memo still contains the entry.
#[tokio::test]
async fn test_context_length_memo_preserved_on_same_catalog_refresh() {
    let server = MockServer::start().await;
    let catalog = r#"{"data":[{"id":"m","context_length":8192}]}"#;
    // First fetch — consumed once, then the second mock (same text) takes over.
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(catalog))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(catalog))
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    // Cold miss: fetches catalog, parses, memoizes {"m": Some(8192)}.
    assert_eq!(provider.model_context_length("m").await, Some(8192));

    // Force TTL expiry so the next call triggers a refresh.
    {
        let mut cache = provider.models_cache.lock().await;
        let entry = cache.as_mut().expect("catalog cached after first call");
        entry.stored_at = tokio::time::Instant::now() - Duration::from_secs(3600);
    }

    // Trigger the refresh directly. This rebuilds the cache entry WITHOUT
    // going through `model_context_length`, so the memo is NOT re-populated
    // by a parse — letting us inspect the carried-over memo in isolation.
    provider
        .get_models_response(None)
        .await
        .expect("refresh succeeds");

    // Inspect the memo directly: it must still contain the entry from before
    // the refresh. Before the fix this was an empty HashMap → assertion fails.
    {
        let cache = provider.models_cache.lock().await;
        let entry = cache.as_ref().expect("catalog cached after refresh");
        let memo = entry.context_lengths.lock().unwrap();
        assert_eq!(
            memo.get("m"),
            Some(&Some(8192)),
            "memo must be preserved across a refresh with identical catalog text"
        );
    }
}

/// A refresh that returns a DIFFERENT catalog text must CLEAR the memo —
/// carrying the warm memo across a changed catalog would serve stale context
/// lengths (a correctness bug: the crux). This is the guard that the
/// carry-only-on-identical-text rule doesn't introduce staleness.
///
/// We inspect the memo directly: after a refresh with changed catalog text
/// the memo must be empty. We additionally confirm a subsequent
/// `model_context_length` call returns the NEW value (16384), not the stale
/// memoized one (8192).
#[tokio::test]
async fn test_context_length_memo_cleared_on_different_catalog_refresh() {
    let server = MockServer::start().await;
    let catalog_a = r#"{"data":[{"id":"m","context_length":8192}]}"#;
    let catalog_b = r#"{"data":[{"id":"m","context_length":16384}]}"#;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(catalog_a))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(catalog_b))
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    // Cold miss: fetches catalog A, memoizes {"m": Some(8192)}.
    assert_eq!(provider.model_context_length("m").await, Some(8192));

    // Force TTL expiry so the next call triggers a refresh.
    {
        let mut cache = provider.models_cache.lock().await;
        let entry = cache.as_mut().expect("catalog cached after first call");
        entry.stored_at = tokio::time::Instant::now() - Duration::from_secs(3600);
    }

    // Refresh with a DIFFERENT catalog (catalog B, context_length 16384).
    provider
        .get_models_response(None)
        .await
        .expect("refresh succeeds");

    // Inspect the memo directly: it must be EMPTY. Carrying {"m": Some(8192)}
    // across to a catalog that now says 16384 would be stale.
    {
        let cache = provider.models_cache.lock().await;
        let entry = cache.as_ref().expect("catalog cached after refresh");
        let memo = entry.context_lengths.lock().unwrap();
        assert!(
            memo.get("m").is_none(),
            "memo must be cleared when the catalog text changes — carrying the old value would serve stale context lengths"
        );
    }

    // A subsequent call must return the NEW value from catalog B, not the stale
    // memoized 8192.
    assert_eq!(provider.model_context_length("m").await, Some(16384));
}

/// P3 — `build_body` must fast-fail when a tool's `parameters` is not a JSON
/// object. A non-object schema would otherwise be serialized verbatim and
/// fail opaquely downstream at the gateway. This test could not compile against
/// the old infallible signature (`build_body` returned `WireRequest`, not a
/// `Result`), so it genuinely proves the behavior change.
#[test]
fn test_build_body_rejects_non_object_parameters() {
    let mut req = simple_request();
    req.tools = Some(vec![crate::types::ToolDescriptor {
        name: "bad_tool".to_string(),
        description: "a tool with a string-typed parameters field".to_string(),
        parameters: serde_json::json!("not an object"),
    }]);

    let err = super::build_body(&req).expect_err("non-object parameters must error");
    // Actionable message names the tool and states the constraint.
    let msg = err.to_string();
    assert!(
        msg.contains("tool bad_tool: parameters must be a JSON Schema object"),
        "unexpected error message: {msg}"
    );

    // A numeric `parameters` is also a non-object and must be rejected.
    req.tools = Some(vec![crate::types::ToolDescriptor {
        name: "num_tool".to_string(),
        description: "a tool with a numeric parameters field".to_string(),
        parameters: serde_json::json!(42),
    }]);
    let err = super::build_body(&req).expect_err("numeric parameters must error");
    assert!(
        err.to_string()
            .contains("tool num_tool: parameters must be a JSON Schema object"),
        "unexpected error message for numeric parameters: {}",
        err
    );
}

/// P3, requirement (b): a tool with a valid JSON object `parameters` still
/// builds successfully — no regression for well-formed descriptors.
#[test]
fn test_build_body_accepts_valid_object_parameters() {
    let mut req = simple_request();
    req.tools = Some(vec![crate::types::ToolDescriptor {
        name: "good_tool".to_string(),
        description: "a tool with a proper schema".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }]);

    let body = super::build_body(&req).expect("valid object parameters should build");
    assert!(
        body.tools.is_some(),
        "tools should be present on the wire request"
    );
}

/// P3, requirement (c): a request with no tools still builds successfully.
#[test]
fn test_build_body_ok_with_no_tools() {
    // `simple_request()` already has `tools: None`.
    let req = simple_request();
    let body = super::build_body(&req).expect("no-tools request should build");
    assert!(
        body.tools.is_none(),
        "tools should be absent on the wire request"
    );

    // An empty tools vec is likewise fine (vacuously no malformed schemas).
    let mut req = simple_request();
    req.tools = Some(vec![]);
    let body = super::build_body(&req).expect("empty-tools request should build");
    assert!(
        body.tools.as_ref().map(|t| t.is_empty()).unwrap_or(true),
        "empty tools should serialize to an empty vec (or be absent)"
    );
}

/// Rebase guard: tool-schema validation is a request-boundary invariant, not
/// merely a property of the private body builder. It must win before auth and
/// before any network attempt so malformed tools fail deterministically.
#[tokio::test]
#[serial]
async fn test_invalid_tool_schema_fails_before_authentication_or_network() {
    let _g = EnvGuard::set(TEST_ENV, "");
    let provider = provider_for("http://127.0.0.1:9".to_string());
    let mut req = simple_request();
    req.tools = Some(vec![crate::types::ToolDescriptor {
        name: "bad_tool".to_string(),
        description: "invalid scalar schema".to_string(),
        parameters: serde_json::json!(42),
    }]);

    let error = match provider.chat_stream(req).await {
        Ok(_) => panic!("malformed tool schema must fail before request dispatch"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            ProviderError::InvalidResponse(ref message)
                if message == "tool bad_tool: parameters must be a JSON Schema object"
        ),
        "schema validation must precede the missing-auth path; got {error:?}"
    );
}

/// Rebase guard: gateways use both response modes interchangeably, so a
/// top-level `error: null` must preserve the same semantic text and terminal
/// reason in streaming and non-streaming parsing.
#[test]
fn test_null_error_semantics_match_across_response_modes() {
    use super::{parse_completion, SseDecoder};

    let nonstream = parse_completion(
        r#"{
            "error": null,
            "choices": [{"message": {"content": "same"}, "finish_reason": "stop"}]
        }"#,
    )
    .expect("non-streaming error:null response must parse");

    let mut stream_text = Vec::new();
    let mut decoder = SseDecoder::default();
    decoder
        .process_line(
            r#"data: {"error":null,"choices":[{"index":0,"delta":{"content":"same"},"finish_reason":"stop"}]}"#,
            &mut stream_text,
        )
        .expect("streaming error:null response must parse");
    let stream_terminal = decoder
        .finalize()
        .expect("streaming error:null response must finalize");

    assert!(matches!(
        nonstream.as_slice(),
        [ChatDelta::Text(text), ChatDelta::Done(FinishReason::Stop)] if text == "same"
    ));
    assert!(matches!(
        stream_text.as_slice(),
        [ChatDelta::Text(text)] if text == "same"
    ));
    assert!(matches!(
        stream_terminal.as_slice(),
        [ChatDelta::Done(FinishReason::Stop)]
    ));
}

/// Rebase guard: an identical catalog refresh must carry the complete memo,
/// including cached misses. Retaining only successful lengths would silently
/// reparse unknown models after every refresh and weaken the cache contract.
#[tokio::test]
async fn test_same_catalog_refresh_preserves_positive_and_negative_memo_entries() {
    let server = MockServer::start().await;
    let catalog = r#"{"data":[{"id":"known","context_length":8192}]}"#;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(catalog))
        .mount(&server)
        .await;

    let mut provider = provider_with_models(server.uri());
    provider.auth = Arc::new(NoAuth);
    assert_eq!(provider.model_context_length("known").await, Some(8192));
    assert_eq!(provider.model_context_length("missing").await, None);

    {
        let mut cache = provider.models_cache.lock().await;
        let entry = cache.as_mut().expect("catalog cached after lookups");
        entry.stored_at = tokio::time::Instant::now() - Duration::from_secs(3600);
    }
    provider
        .get_models_response(None)
        .await
        .expect("identical catalog refresh succeeds");

    let cache = provider.models_cache.lock().await;
    let entry = cache.as_ref().expect("catalog remains cached");
    let memo = entry.context_lengths.lock().unwrap();
    assert_eq!(memo.len(), 2);
    assert_eq!(memo.get("known"), Some(&Some(8192)));
    assert_eq!(memo.get("missing"), Some(&None));
}
