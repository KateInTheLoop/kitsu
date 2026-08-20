mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use kitsu_providers::anthropic_messages::{
    AnthropicMessagesConfig, AnthropicMessagesProvider, ANTHROPIC_VERSION,
};
use kitsu_providers::auth::NoAuth;
use kitsu_providers::openai_compat::{OpenAICompatConfig, OpenAICompatProvider};
use kitsu_providers::{
    ChatDelta, ChatMessage, ChatRequest, Provider, ProviderAdapterId, ProviderAttemptCategory,
    ProviderAttemptOutcome, ProviderCapabilities, ProviderCapability, ProviderError,
    ProviderProfileId, ProviderResponsePart, ProviderRouteContractError, ProviderRouteGeneration,
    ProviderRouteIdentity, ProviderTelemetrySink, ProviderWireRevision, ToolCall, ToolDescriptor,
};
use support::provider_wire_fixture::{
    anthropic_messages_malformed_event, anthropic_messages_midstream_error,
    anthropic_messages_overloaded_error, anthropic_messages_oversized_error,
    anthropic_messages_oversized_sse_line, anthropic_messages_oversized_tool_arguments,
    anthropic_messages_text, anthropic_messages_tool_round, openai_chat_malformed_event,
    openai_chat_midstream_error, openai_chat_oversized_aggregate, openai_chat_oversized_error,
    openai_chat_oversized_sse_line, openai_chat_oversized_tool_arguments,
    openai_chat_rate_limit_error, openai_chat_text, openai_chat_tool_round, HttpFaultFixture,
    WireDialect, ANTHROPIC_ERRORS_SPEC, ANTHROPIC_MESSAGES_SPEC, ANTHROPIC_STREAMING_SPEC,
    ANTHROPIC_TOOL_SPEC, KITSU_ERROR_BODY_LIMIT_BYTES, KITSU_SSE_LINE_LIMIT_BYTES,
    KITSU_SSE_STREAM_LIMIT_BYTES, KITSU_TOOL_ARGUMENT_LIMIT_BYTES, OPENAI_CHAT_SPEC,
    PRIMARY_SPEC_REVIEWED_UTC,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Clone, Debug)]
struct ObservedRequest {
    body: serde_json::Value,
    anthropic_version: Option<String>,
}

fn provider_for(base_url: String, chat_path: &str) -> OpenAICompatProvider {
    provider_for_with_timeout(base_url, chat_path, Duration::from_secs(2))
}

fn provider_for_with_timeout(
    base_url: String,
    chat_path: &str,
    timeout: Duration,
) -> OpenAICompatProvider {
    OpenAICompatProvider::new(
        OpenAICompatConfig {
            base_url,
            chat_path: chat_path.to_string(),
            models_path: None,
            retries: 0,
            timeout,
        },
        Arc::new(NoAuth),
    )
}

fn messages_provider_for(base_url: String, timeout: Duration) -> AnthropicMessagesProvider {
    AnthropicMessagesProvider::new(
        AnthropicMessagesConfig {
            base_url,
            messages_path: "/v1/messages".to_string(),
            timeout,
        },
        Arc::new(NoAuth),
    )
}

fn neutral_text_request() -> ChatRequest {
    ChatRequest {
        model: "fixture-model-v1".to_string(),
        messages: vec![
            ChatMessage::system("You are concise."),
            ChatMessage::user("hello"),
        ],
        tools: None,
        stream: true,
        temperature: None,
        reasoning_effort: None,
        max_tokens: Some(64),
        response_format: None,
        prompt_cache_key: None,
    }
}

fn neutral_tool_request() -> ChatRequest {
    ChatRequest {
        model: "fixture-model-v1".to_string(),
        messages: vec![
            ChatMessage::system("You are concise."),
            ChatMessage::user("Check the time."),
            ChatMessage::assistant(
                "",
                vec![ToolCall {
                    tool_name: "get_time".to_string(),
                    args: r#"{"zone":"UTC"}"#.to_string(),
                    call_id: "toolu_previous".to_string(),
                }],
            ),
            ChatMessage::tool("toolu_previous", r#"{"time":"12:00"}"#),
        ],
        tools: Some(vec![ToolDescriptor {
            name: "get_time".to_string(),
            description: "Get the time for a zone.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"zone": {"type": "string"}},
                "required": ["zone"],
                "additionalProperties": false
            }),
        }]),
        stream: true,
        temperature: None,
        reasoning_effort: None,
        max_tokens: Some(64),
        response_format: None,
        prompt_cache_key: None,
    }
}

fn assert_sse_data_is_json(stream: &str) {
    for line in stream
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
    {
        if line != "[DONE]" {
            serde_json::from_str::<serde_json::Value>(line).expect("fixture data line is JSON");
        }
    }
}

async fn mount_capture(
    server: &MockServer,
    fixture: &support::provider_wire_fixture::WireFixture,
) -> Arc<Mutex<Option<ObservedRequest>>> {
    let captured = Arc::new(Mutex::new(None));
    let observed = Arc::clone(&captured);
    let response = fixture.stream.to_string();
    Mock::given(method("POST"))
        .and(path(fixture.path))
        .respond_with(move |request: &wiremock::Request| {
            let anthropic_version = request
                .headers
                .get("anthropic-version")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            *observed.lock().unwrap() = Some(ObservedRequest {
                body: serde_json::from_slice(&request.body).expect("request body is JSON"),
                anthropic_version,
            });
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(response.clone())
        })
        .expect(1)
        .mount(server)
        .await;
    captured
}

#[test]
fn primary_pinned_text_fixtures_are_structurally_distinct() {
    let chat = openai_chat_text();
    let messages = anthropic_messages_text();

    assert!(OPENAI_CHAT_SPEC.starts_with("https://developers.openai.com/"));
    assert!(ANTHROPIC_MESSAGES_SPEC.starts_with("https://platform.claude.com/"));
    assert!(ANTHROPIC_STREAMING_SPEC.starts_with("https://platform.claude.com/"));
    assert!(ANTHROPIC_ERRORS_SPEC.starts_with("https://platform.claude.com/"));
    assert_eq!(PRIMARY_SPEC_REVIEWED_UTC, "2026-08-19");

    assert_eq!(chat.dialect, WireDialect::OpenAiChatCompletionsV1);
    assert_eq!(messages.dialect, WireDialect::AnthropicMessages2023_06_01);
    assert_eq!(chat.path, "/v1/chat/completions");
    assert_eq!(messages.path, "/v1/messages");
    assert_eq!(messages.version_header, Some("2023-06-01"));
    assert!(chat.version_header.is_none());

    assert_eq!(chat.request["messages"][0]["role"], "system");
    assert!(chat.request.get("system").is_none());
    assert_eq!(messages.request["system"], "You are concise.");
    assert_eq!(messages.request["messages"][0]["role"], "user");
    assert!(messages.request["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .all(|message| message["role"] != "system"));
    assert_ne!(chat.request, messages.request);
    assert!(chat.stream.contains("\"choices\""));
    assert!(messages.stream.starts_with("event: message_start\ndata: "));
    assert!(!messages.stream.contains(r"\n"));
    assert!(messages.stream.contains("\"type\":\"message_start\""));
    assert!(messages.stream.contains("\"type\":\"message_stop\""));
    assert_sse_data_is_json(&chat.stream);
    assert_sse_data_is_json(&messages.stream);
}

#[tokio::test]
async fn compatible_chat_control_matches_request_and_normalized_text_stream() {
    let fixture = openai_chat_text();
    let server = MockServer::start().await;
    let captured = mount_capture(&server, &fixture).await;
    let provider = provider_for(server.uri(), fixture.path);

    let mut stream = provider
        .chat_stream(neutral_text_request())
        .await
        .expect("compatible Chat fixture starts");
    let mut deltas = Vec::new();
    while let Some(item) = stream.next().await {
        deltas.push(item.expect("compatible Chat fixture decodes"));
    }

    let observed = captured
        .lock()
        .unwrap()
        .clone()
        .expect("one Chat request captured");
    assert_eq!(observed.body, fixture.request);
    assert!(observed.anthropic_version.is_none());
    assert_eq!(deltas, fixture.expected_deltas);
}

#[tokio::test]
async fn anthropic_messages_fixture_fails_against_the_compatible_chat_adapter() {
    let fixture = anthropic_messages_text();
    let server = MockServer::start().await;
    let captured = mount_capture(&server, &fixture).await;
    let provider = provider_for(server.uri(), fixture.path);

    let mut stream = provider
        .chat_stream(neutral_text_request())
        .await
        .expect("HTTP 200 starts before the stream dialect is decoded");
    let mut decoded = Vec::new();
    let error = loop {
        match stream.next().await {
            Some(Ok(delta)) => decoded.push(delta),
            Some(Err(error)) => break error,
            None => panic!("wrong adapter accepted Anthropic EOF as completion"),
        }
    };

    let observed = captured
        .lock()
        .unwrap()
        .clone()
        .expect("one wrong-adapter request captured");
    assert_ne!(observed.body, fixture.request);
    assert_eq!(observed.body["messages"][0]["role"], "system");
    assert!(observed.body.get("system").is_none());
    assert!(observed.anthropic_version.is_none());
    assert!(decoded.is_empty(), "wrong wire cannot emit neutral deltas");
    assert!(matches!(
        error,
        kitsu_providers::ProviderError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn native_messages_matches_exact_request_and_normalized_text_stream() {
    let fixture = anthropic_messages_text();
    let server = MockServer::start().await;
    let captured = mount_capture(&server, &fixture).await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));

    let mut stream = provider
        .chat_stream(neutral_text_request())
        .await
        .expect("native Messages fixture starts");
    let mut deltas = Vec::new();
    while let Some(item) = stream.next().await {
        deltas.push(item.expect("native Messages fixture decodes"));
    }

    let observed = captured
        .lock()
        .unwrap()
        .clone()
        .expect("one Messages request captured");
    assert_eq!(observed.body, fixture.request);
    assert_eq!(
        observed.anthropic_version.as_deref(),
        Some(ANTHROPIC_VERSION)
    );
    assert_eq!(deltas, fixture.expected_deltas);
}

#[tokio::test]
async fn native_messages_eof_is_not_terminal_success() {
    let mut fixture = anthropic_messages_text();
    let stop = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    fixture.stream = fixture
        .stream
        .strip_suffix(stop)
        .expect("fixture has an explicit message_stop")
        .to_string();
    let server = MockServer::start().await;
    mount_capture(&server, &fixture).await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));

    let mut stream = provider
        .chat_stream(neutral_text_request())
        .await
        .expect("HTTP success starts the stream");
    let mut saw_done = false;
    let mut saw_error = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(kitsu_providers::ChatDelta::Done(_)) => saw_done = true,
            Ok(_) => {}
            Err(ProviderError::InvalidResponse(_)) => saw_error = true,
            Err(other) => panic!("unexpected EOF error: {other}"),
        }
    }
    assert!(!saw_done, "EOF cannot synthesize terminal success");
    assert!(saw_error, "EOF must remain an explicit wire failure");
}

#[tokio::test]
async fn native_messages_matches_tool_request_and_correlated_stream() {
    let fixture = anthropic_messages_tool_round();
    let server = MockServer::start().await;
    let captured = mount_capture(&server, &fixture).await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));

    let mut stream = provider
        .chat_stream(neutral_tool_request())
        .await
        .expect("native Messages tool fixture starts");
    let mut deltas = Vec::new();
    while let Some(item) = stream.next().await {
        deltas.push(item.expect("native Messages tool fixture decodes"));
    }

    let observed = captured
        .lock()
        .unwrap()
        .clone()
        .expect("one Messages tool request captured");
    assert_eq!(observed.body, fixture.request);
    assert_eq!(
        observed.anthropic_version.as_deref(),
        Some(ANTHROPIC_VERSION)
    );
    assert_eq!(deltas, fixture.expected_deltas);
}

#[tokio::test]
async fn native_messages_rejects_malformed_prior_tool_json_before_send() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));
    let mut request = neutral_tool_request();
    request.messages[2].tool_calls[0].args = "{".to_string();

    let error = match provider.chat_stream(request).await {
        Ok(_) => panic!("malformed prior tool JSON reached transport"),
        Err(error) => error,
    };
    assert!(matches!(error, ProviderError::InvalidResponse(_)));
}

#[tokio::test]
async fn native_messages_bounds_fragmented_tool_arguments() {
    let fixture = anthropic_messages_oversized_tool_arguments();
    let server = MockServer::start().await;
    mount_capture(&server, &fixture).await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));

    let mut stream = provider
        .chat_stream(neutral_tool_request())
        .await
        .expect("HTTP success starts the oversized tool stream");
    let mut error = None;
    while let Some(item) = stream.next().await {
        if let Err(found) = item {
            error = Some(found);
            break;
        }
    }
    assert!(matches!(
        error,
        Some(ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::ToolArguments,
            limit_bytes: KITSU_TOOL_ARGUMENT_LIMIT_BYTES,
        })
    ));
}

#[test]
fn primary_pinned_tool_rounds_keep_correlation_and_distinct_wire_shapes() {
    let chat = openai_chat_tool_round();
    let messages = anthropic_messages_tool_round();

    assert!(ANTHROPIC_TOOL_SPEC.starts_with("https://platform.claude.com/"));
    assert_eq!(
        chat.request["messages"][2]["tool_calls"][0]["id"],
        "toolu_previous"
    );
    assert_eq!(chat.request["messages"][3]["role"], "tool");
    assert_eq!(
        chat.request["messages"][3]["tool_call_id"],
        "toolu_previous"
    );
    assert_eq!(chat.request["tools"][0]["type"], "function");
    assert_eq!(
        messages.request["messages"][1]["content"][0]["type"],
        "tool_use"
    );
    assert_eq!(
        messages.request["messages"][2]["content"][0]["type"],
        "tool_result"
    );
    assert_eq!(
        messages.request["messages"][2]["content"][0]["tool_use_id"],
        "toolu_previous"
    );
    assert_eq!(messages.request["tools"][0]["name"], "get_time");
    assert_eq!(
        messages.request["tools"][0]["input_schema"]["type"],
        "object"
    );
    assert!(chat.stream.contains("\"tool_calls\""));
    assert!(messages.stream.contains("\"type\":\"tool_use\""));
    assert!(messages.stream.contains("\"type\":\"input_json_delta\""));
    assert_sse_data_is_json(&chat.stream);
    assert_sse_data_is_json(&messages.stream);
    assert_eq!(chat.expected_deltas, messages.expected_deltas);
}

#[tokio::test]
async fn compatible_chat_control_matches_tool_round_request_and_stream() {
    let fixture = openai_chat_tool_round();
    let server = MockServer::start().await;
    let captured = mount_capture(&server, &fixture).await;
    let provider = provider_for(server.uri(), fixture.path);

    let mut stream = provider
        .chat_stream(neutral_tool_request())
        .await
        .expect("compatible Chat tool fixture starts");
    let mut deltas = Vec::new();
    while let Some(item) = stream.next().await {
        deltas.push(item.expect("compatible Chat tool fixture decodes"));
    }

    let observed = captured
        .lock()
        .unwrap()
        .clone()
        .expect("one Chat tool request captured");
    assert_eq!(observed.body, fixture.request);
    assert!(observed.anthropic_version.is_none());
    assert_eq!(deltas, fixture.expected_deltas);
}

#[tokio::test]
async fn anthropic_tool_stream_fails_against_chat_without_a_partial_tool_call() {
    let fixture = anthropic_messages_tool_round();
    let server = MockServer::start().await;
    let captured = mount_capture(&server, &fixture).await;
    let provider = provider_for(server.uri(), fixture.path);

    let mut stream = provider
        .chat_stream(neutral_tool_request())
        .await
        .expect("HTTP 200 starts before the stream dialect is decoded");
    let mut decoded = Vec::new();
    let error = loop {
        match stream.next().await {
            Some(Ok(delta)) => decoded.push(delta),
            Some(Err(error)) => break error,
            None => panic!("wrong adapter accepted Anthropic tool EOF"),
        }
    };

    let observed = captured
        .lock()
        .unwrap()
        .clone()
        .expect("one wrong-adapter tool request captured");
    assert_ne!(observed.body, fixture.request);
    assert!(observed.anthropic_version.is_none());
    assert!(decoded.is_empty(), "wrong wire cannot emit a partial call");
    assert!(matches!(error, ProviderError::InvalidResponse(_)));
}

async fn send_http_fault(fixture: &HttpFaultFixture) -> ProviderError {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(fixture.path))
        .respond_with(
            ResponseTemplate::new(fixture.status)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture.body.clone()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_for(server.uri(), fixture.path);
    match provider.chat_stream(neutral_text_request()).await {
        Ok(_) => panic!("HTTP fault unexpectedly started a response stream"),
        Err(error) => error,
    }
}

async fn send_native_http_fault(fixture: &HttpFaultFixture) -> ProviderError {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(fixture.path))
        .respond_with(
            ResponseTemplate::new(fixture.status)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture.body.clone()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));
    match provider.chat_stream(neutral_text_request()).await {
        Ok(_) => panic!("native HTTP fault unexpectedly started a stream"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn dialect_specific_http_faults_preserve_typed_status_and_bounded_body() {
    for fixture in [
        openai_chat_rate_limit_error(),
        anthropic_messages_overloaded_error(),
    ] {
        let error = send_http_fault(&fixture).await;
        match error {
            ProviderError::Http { status, body } => {
                assert_eq!(status, fixture.status);
                assert_eq!(body, fixture.body);
            }
            other => panic!("unexpected HTTP fault: {other}"),
        }
    }
}

#[tokio::test]
async fn native_messages_preserves_http_status_and_bounds_error_body() {
    let overloaded = anthropic_messages_overloaded_error();
    let error = send_native_http_fault(&overloaded).await;
    assert!(matches!(
        error,
        ProviderError::Http { status: 529, body } if body == overloaded.body
    ));

    let error = send_native_http_fault(&anthropic_messages_oversized_error()).await;
    assert!(matches!(
        error,
        ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::ErrorBody,
            limit_bytes: KITSU_ERROR_BODY_LIMIT_BYTES,
        }
    ));
}

async fn send_native_stream_fault(
    fixture: &support::provider_wire_fixture::WireFixture,
) -> ProviderError {
    let server = MockServer::start().await;
    mount_capture(&server, fixture).await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));
    let mut stream = provider
        .chat_stream(neutral_text_request())
        .await
        .expect("native HTTP success starts bounded decoding");
    loop {
        match stream.next().await {
            Some(Err(error)) => return error,
            Some(Ok(ChatDelta::Done(reason))) => {
                panic!("native fault synthesized terminal success: {reason:?}")
            }
            Some(Ok(_)) => {}
            None => panic!("native fault reached EOF without a typed error"),
        }
    }
}

#[tokio::test]
async fn native_messages_rejects_malformed_midstream_and_size_faults() {
    for fixture in [
        anthropic_messages_malformed_event(),
        anthropic_messages_midstream_error(),
    ] {
        assert!(matches!(
            send_native_stream_fault(&fixture).await,
            ProviderError::InvalidResponse(_)
        ));
    }

    let error = send_native_stream_fault(&anthropic_messages_oversized_sse_line()).await;
    assert!(matches!(
        error,
        ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::SseLine,
            limit_bytes: KITSU_SSE_LINE_LIMIT_BYTES,
        }
    ));

    let aggregate = openai_chat_oversized_aggregate("/v1/messages");
    let error = send_native_stream_fault(&aggregate).await;
    assert!(matches!(
        error,
        ProviderError::ResponseTooLarge {
            part: ProviderResponsePart::SseStream,
            limit_bytes: KITSU_SSE_STREAM_LIMIT_BYTES,
        }
    ));
}

#[tokio::test]
async fn native_messages_timeout_sends_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(1))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_messages_text().stream),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = messages_provider_for(server.uri(), Duration::from_millis(100));

    let error = match provider.chat_stream(neutral_text_request()).await {
        Ok(_) => panic!("native delayed response exceeded its deadline"),
        Err(error) => error,
    };
    assert!(matches!(error, ProviderError::Timeout));
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn native_messages_cancel_sends_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_messages_text().stream),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(10));
    let request = tokio::spawn(async move { provider.chat_stream(neutral_text_request()).await });
    for _ in 0..100 {
        if !server.received_requests().await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    request.abort();
    match request.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("cancelled native request completed"),
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

fn captured_attempts() -> (
    Arc<Mutex<Vec<kitsu_providers::ProviderAttemptEvent>>>,
    ProviderTelemetrySink,
) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
    (events, sink)
}

fn assert_one_native_attempt(
    events: &Arc<Mutex<Vec<kitsu_providers::ProviderAttemptEvent>>>,
    category: ProviderAttemptCategory,
    status: Option<u16>,
    outcome: ProviderAttemptOutcome,
) {
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.attempt, 1);
    assert_eq!(event.max_attempts, 1);
    assert_eq!(event.category, category);
    assert_eq!(event.status, status);
    assert_eq!(event.outcome, outcome);
    assert!(event.terminal);
    assert_eq!(event.retry_delay_ms, None);
    assert!(!event.retry_after_used);
}

#[tokio::test]
async fn native_messages_reports_one_terminal_attempt_for_every_lifecycle() {
    let fixture = anthropic_messages_text();
    let server = MockServer::start().await;
    mount_capture(&server, &fixture).await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));
    let (events, sink) = captured_attempts();
    let mut stream = provider
        .chat_stream_with_telemetry(neutral_text_request(), Some(sink))
        .await
        .expect("native success starts");
    while let Some(item) = stream.next().await {
        item.expect("native success decodes");
    }
    assert_one_native_attempt(
        &events,
        ProviderAttemptCategory::Success,
        Some(200),
        ProviderAttemptOutcome::BodyCompleted,
    );

    let overloaded = anthropic_messages_overloaded_error();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(overloaded.path))
        .respond_with(ResponseTemplate::new(overloaded.status).set_body_string(overloaded.body))
        .expect(1)
        .mount(&server)
        .await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));
    let (events, sink) = captured_attempts();
    assert!(provider
        .chat_stream_with_telemetry(neutral_text_request(), Some(sink))
        .await
        .is_err());
    assert_one_native_attempt(
        &events,
        ProviderAttemptCategory::ServerError,
        Some(529),
        ProviderAttemptOutcome::Rejected,
    );

    let fixture = anthropic_messages_malformed_event();
    let server = MockServer::start().await;
    mount_capture(&server, &fixture).await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));
    let (events, sink) = captured_attempts();
    let mut stream = provider
        .chat_stream_with_telemetry(neutral_text_request(), Some(sink))
        .await
        .expect("native malformed body starts");
    assert!(stream.next().await.unwrap().is_err());
    assert_one_native_attempt(
        &events,
        ProviderAttemptCategory::InvalidResponse,
        Some(200),
        ProviderAttemptOutcome::BodyFailed,
    );

    let fixture = anthropic_messages_text();
    let server = MockServer::start().await;
    mount_capture(&server, &fixture).await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(2));
    let (events, sink) = captured_attempts();
    let stream = provider
        .chat_stream_with_telemetry(neutral_text_request(), Some(sink))
        .await
        .expect("native accepted body starts");
    drop(stream);
    assert_one_native_attempt(
        &events,
        ProviderAttemptCategory::Cancelled,
        Some(200),
        ProviderAttemptOutcome::Cancelled,
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(1)))
        .expect(1)
        .mount(&server)
        .await;
    let provider = messages_provider_for(server.uri(), Duration::from_millis(100));
    let (events, sink) = captured_attempts();
    assert!(matches!(
        provider
            .chat_stream_with_telemetry(neutral_text_request(), Some(sink))
            .await,
        Err(ProviderError::Timeout)
    ));
    assert_one_native_attempt(
        &events,
        ProviderAttemptCategory::Timeout,
        None,
        ProviderAttemptOutcome::TransportFailed,
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .expect(1)
        .mount(&server)
        .await;
    let provider = messages_provider_for(server.uri(), Duration::from_secs(10));
    let (events, sink) = captured_attempts();
    let request = tokio::spawn(async move {
        provider
            .chat_stream_with_telemetry(neutral_text_request(), Some(sink))
            .await
    });
    for _ in 0..100 {
        if !server.received_requests().await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    request.abort();
    match request.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("cancelled native telemetry request completed"),
    }
    assert_one_native_attempt(
        &events,
        ProviderAttemptCategory::Cancelled,
        None,
        ProviderAttemptOutcome::Cancelled,
    );
}

#[tokio::test]
async fn malformed_events_fail_before_any_neutral_delta() {
    for fixture in [
        openai_chat_malformed_event(),
        anthropic_messages_malformed_event(),
    ] {
        let server = MockServer::start().await;
        mount_capture(&server, &fixture).await;
        let provider = provider_for(server.uri(), fixture.path);
        let mut stream = provider
            .chat_stream(neutral_text_request())
            .await
            .expect("HTTP 200 starts before malformed stream decoding");
        assert!(matches!(
            stream.next().await,
            Some(Err(ProviderError::InvalidResponse(_)))
        ));
    }
}

#[tokio::test]
async fn midstream_error_events_fail_before_any_neutral_delta() {
    for fixture in [
        openai_chat_midstream_error(),
        anthropic_messages_midstream_error(),
    ] {
        assert_sse_data_is_json(&fixture.stream);
        let server = MockServer::start().await;
        mount_capture(&server, &fixture).await;
        let provider = provider_for(server.uri(), fixture.path);
        let mut stream = provider
            .chat_stream(neutral_text_request())
            .await
            .expect("HTTP 200 starts before the error event is decoded");
        assert!(matches!(
            stream.next().await,
            Some(Err(ProviderError::InvalidResponse(_)))
        ));
    }
}

#[tokio::test]
async fn both_dialect_paths_reject_oversized_error_bodies() {
    for fixture in [
        openai_chat_oversized_error(),
        anthropic_messages_oversized_error(),
    ] {
        let error = send_http_fault(&fixture).await;
        assert!(matches!(
            error,
            ProviderError::ResponseTooLarge {
                part: ProviderResponsePart::ErrorBody,
                limit_bytes: KITSU_ERROR_BODY_LIMIT_BYTES,
            }
        ));
    }
}

#[tokio::test]
async fn both_dialect_paths_reject_oversized_sse_lines() {
    for fixture in [
        openai_chat_oversized_sse_line(),
        anthropic_messages_oversized_sse_line(),
    ] {
        let server = MockServer::start().await;
        mount_capture(&server, &fixture).await;
        let provider = provider_for(server.uri(), fixture.path);
        let mut stream = provider
            .chat_stream(neutral_text_request())
            .await
            .expect("HTTP 200 starts before bounded stream decoding");
        assert!(matches!(
            stream.next().await,
            Some(Err(ProviderError::ResponseTooLarge {
                part: ProviderResponsePart::SseLine,
                limit_bytes: KITSU_SSE_LINE_LIMIT_BYTES,
            }))
        ));
    }
}

async fn send_stream_fault(fixture: &support::provider_wire_fixture::WireFixture) -> ProviderError {
    let server = MockServer::start().await;
    mount_capture(&server, fixture).await;
    let provider = provider_for(server.uri(), fixture.path);
    let mut stream = provider
        .chat_stream(neutral_tool_request())
        .await
        .expect("HTTP 200 starts before bounded stream decoding");
    match stream.next().await {
        Some(Err(error)) => error,
        Some(Ok(delta)) => panic!("bounded fixture emitted a partial delta: {delta:?}"),
        None => panic!("bounded fixture reached EOF without a typed error"),
    }
}

#[tokio::test]
async fn current_configured_paths_share_aggregate_stream_and_tool_argument_bounds() {
    for path in ["/v1/chat/completions", "/v1/messages"] {
        let aggregate = openai_chat_oversized_aggregate(path);
        let error = send_stream_fault(&aggregate).await;
        assert!(matches!(
            error,
            ProviderError::ResponseTooLarge {
                part: ProviderResponsePart::SseStream,
                limit_bytes: KITSU_SSE_STREAM_LIMIT_BYTES,
            }
        ));

        let arguments = openai_chat_oversized_tool_arguments(path);
        let error = send_stream_fault(&arguments).await;
        assert!(matches!(
            error,
            ProviderError::ResponseTooLarge {
                part: ProviderResponsePart::ToolArguments,
                limit_bytes: KITSU_TOOL_ARGUMENT_LIMIT_BYTES,
            }
        ));
    }
}

#[test]
fn native_messages_tool_argument_bound_is_pinned_for_the_future_decoder() {
    let fixture = anthropic_messages_oversized_tool_arguments();
    assert_eq!(fixture.dialect, WireDialect::AnthropicMessages2023_06_01);
    assert_sse_data_is_json(&fixture.stream);
    assert!(fixture.stream.contains("\"type\":\"input_json_delta\""));
    assert!(fixture.stream.len() > KITSU_TOOL_ARGUMENT_LIMIT_BYTES);
    assert!(fixture.stream.len() < KITSU_SSE_LINE_LIMIT_BYTES);
    assert!(fixture.expected_deltas.is_empty());
}

#[tokio::test]
async fn current_configured_paths_timeout_once_without_post_terminal_send() {
    for path in ["/v1/chat/completions", "/v1/messages"] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path(path))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(1))
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(openai_chat_text().stream),
            )
            .expect(1)
            .mount(&server)
            .await;
        let provider = provider_for_with_timeout(server.uri(), path, Duration::from_millis(200));
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&events);
        let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));

        let error = match provider
            .chat_stream_with_telemetry(neutral_text_request(), Some(sink))
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("delayed response exceeded the end-to-end deadline"),
        };
        assert!(matches!(error, ProviderError::Timeout));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].category, ProviderAttemptCategory::Timeout);
        assert_eq!(events[0].outcome, ProviderAttemptOutcome::TransportFailed);
        assert!(events[0].terminal);
    }
}

#[tokio::test]
async fn current_configured_paths_cancel_once_without_post_terminal_send() {
    for path in ["/v1/chat/completions", "/v1/messages"] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path(path))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(openai_chat_text().stream),
            )
            .expect(1)
            .mount(&server)
            .await;
        let provider = provider_for(server.uri(), path);
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&events);
        let sink = ProviderTelemetrySink::new(move |event| observed.lock().unwrap().push(event));
        let request = tokio::spawn(async move {
            provider
                .chat_stream_with_telemetry(neutral_text_request(), Some(sink))
                .await
        });
        for _ in 0..100 {
            if !server.received_requests().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        request.abort();
        let join_error = match request.await {
            Err(error) => error,
            Ok(_) => panic!("cancelled provider request completed unexpectedly"),
        };
        assert!(join_error.is_cancelled());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].category, ProviderAttemptCategory::Cancelled);
        assert_eq!(events[0].outcome, ProviderAttemptOutcome::Cancelled);
        assert!(events[0].terminal);
    }
}

#[test]
fn unsupported_image_capability_uses_route_admission_and_zero_send() {
    let generation = ProviderRouteGeneration::new(1).unwrap();
    let route = ProviderRouteIdentity::new(
        ProviderAdapterId::OpenAiCompatible,
        ProviderProfileId::new("profile-v1-0123456789abcdef0123456789abcdef").unwrap(),
        generation,
        ProviderWireRevision::OpenAiChatCompletionsV1,
        ProviderCapabilities::new(
            1,
            [
                ProviderCapability::Streaming,
                ProviderCapability::Tools,
                ProviderCapability::Usage,
                ProviderCapability::ModelCatalog,
            ],
        )
        .unwrap(),
    )
    .unwrap();
    let mut sends = 0;
    let admission = route.validate_for_send(generation, &[ProviderCapability::ImageInput]);
    if admission.is_ok() {
        sends += 1;
    }
    assert_eq!(
        admission.unwrap_err(),
        ProviderRouteContractError::UnsupportedCapability {
            capability: ProviderCapability::ImageInput,
        }
    );
    assert_eq!(sends, 0);
}
