mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use kitsu_providers::auth::NoAuth;
use kitsu_providers::openai_compat::{OpenAICompatConfig, OpenAICompatProvider};
use kitsu_providers::{ChatMessage, ChatRequest, Provider};
use support::provider_wire_fixture::{
    anthropic_messages_text, openai_chat_text, WireDialect, ANTHROPIC_ERRORS_SPEC,
    ANTHROPIC_MESSAGES_SPEC, ANTHROPIC_STREAMING_SPEC, OPENAI_CHAT_SPEC, PRIMARY_SPEC_REVIEWED_UTC,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Clone, Debug)]
struct ObservedRequest {
    body: serde_json::Value,
    anthropic_version: Option<String>,
}

fn provider_for(base_url: String, chat_path: &str) -> OpenAICompatProvider {
    OpenAICompatProvider::new(
        OpenAICompatConfig {
            base_url,
            chat_path: chat_path.to_string(),
            models_path: None,
            retries: 0,
            timeout: Duration::from_secs(2),
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
