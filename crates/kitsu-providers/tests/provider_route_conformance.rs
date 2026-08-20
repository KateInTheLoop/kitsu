#[allow(dead_code)]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use kitsu_providers::anthropic_messages::{
    build_anthropic_messages_route, AnthropicMessagesRouteConfig,
};
use kitsu_providers::auth::NoAuth;
use kitsu_providers::openai_compat::{build_openai_compatible_route, OpenAICompatRouteConfig};
use kitsu_providers::{
    ChatDelta, ChatMessage, ChatRequest, Provider, ProviderAttemptOutcome, ProviderAuthSource,
    ProviderCapability, ProviderCatalogPolicy, ProviderError, ProviderProfileId,
    ProviderRouteContractError, ProviderRouteCurrent, ProviderRouteDeclaration,
    ProviderRoutePolicy, ProviderTelemetrySink, ToolCall, ToolDescriptor,
};
use support::provider_wire_fixture::{
    anthropic_messages_text, anthropic_messages_tool_round, openai_chat_text,
    openai_chat_tool_round, WireFixture,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn text_request() -> ChatRequest {
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

fn tool_request() -> ChatRequest {
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

fn no_catalog_policy() -> ProviderRoutePolicy {
    ProviderRoutePolicy::new(
        0,
        0,
        Duration::from_secs(2),
        Duration::from_secs(2),
        Duration::from_secs(1),
        ProviderCatalogPolicy::Unavailable,
    )
    .unwrap()
}

async fn mount_two_responses(
    server: &MockServer,
    text: &WireFixture,
    tool: &WireFixture,
) -> Arc<AtomicUsize> {
    assert_eq!(text.path, tool.path);
    let calls = Arc::new(AtomicUsize::new(0));
    let responder_calls = Arc::clone(&calls);
    let text_stream = text.stream.clone();
    let tool_stream = tool.stream.clone();
    Mock::given(method("POST"))
        .and(path(text.path))
        .respond_with(move |_: &wiremock::Request| {
            let body = if responder_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                text_stream.clone()
            } else {
                tool_stream.clone()
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
        })
        .expect(2)
        .mount(server)
        .await;
    calls
}

async fn collect(mut stream: kitsu_providers::ChatStream) -> Vec<ChatDelta> {
    let mut deltas = Vec::new();
    while let Some(delta) = stream.next().await {
        deltas.push(delta.expect("conforming response decodes"));
    }
    deltas
}

async fn assert_common_route_conformance<P>(
    current: ProviderRouteCurrent<P>,
    calls: Arc<AtomicUsize>,
    expected_text: Vec<ChatDelta>,
    expected_tool: Vec<ChatDelta>,
) where
    P: Provider + 'static,
{
    let handle = current.bind().expect("initial route binds");
    assert_eq!(handle.model(), "fixture-model-v1");
    for capability in [
        ProviderCapability::Streaming,
        ProviderCapability::Tools,
        ProviderCapability::Usage,
    ] {
        assert!(handle.identity().capabilities().supports(capability));
    }
    assert_eq!(handle.list_models().await.unwrap(), None);
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    let observations = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&observations);
    let sink = ProviderTelemetrySink::with_fallible_context_observer(move |event| {
        observed.lock().unwrap().push(event);
        true
    });
    let text = handle
        .chat_stream_with_telemetry(text_request(), Some(sink.clone()))
        .await
        .expect("neutral text request starts");
    assert_eq!(collect(text).await, expected_text);
    let tool = handle
        .chat_stream_with_telemetry(tool_request(), Some(sink))
        .await
        .expect("neutral tool request starts");
    assert_eq!(collect(tool).await, expected_tool);
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    {
        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 2);
        assert!(observations.iter().all(|observation| {
            observation.route_generation.unwrap().get() == 1
                && observation.event.outcome == ProviderAttemptOutcome::BodyCompleted
                && observation.event.terminal
        }));
    }

    let generation_one = handle.identity().generation();
    let successor = current
        .replace_model(generation_one, "model-v2")
        .expect("exact successor publishes");
    assert_eq!(successor.identity().generation().get(), 2);
    let error = match handle.chat_stream(text_request()).await {
        Ok(_) => panic!("stale route performed a third send"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProviderError::RouteContract(ProviderRouteContractError::StaleGeneration { .. })
    ));
    assert_eq!(successor.list_models().await.unwrap(), None);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn compatible_chat_passes_common_route_conformance() {
    let text = openai_chat_text();
    let tool = openai_chat_tool_round();
    let server = MockServer::start().await;
    let calls = mount_two_responses(&server, &text, &tool).await;
    let snapshot = build_openai_compatible_route(
        OpenAICompatRouteConfig {
            declaration: ProviderRouteDeclaration::compatible_chat_v1(
                ProviderProfileId::derive_v1(b"route-conformance", b"compatible").unwrap(),
                false,
            ),
            model: "fixture-model-v1".to_string(),
            auth_source: ProviderAuthSource::None,
            base_url: server.uri(),
            chat_path: text.path.to_string(),
            models_path: None,
            policy: no_catalog_policy(),
        },
        &NoAuth,
    )
    .unwrap();
    assert_common_route_conformance(
        ProviderRouteCurrent::new(snapshot),
        calls,
        text.expected_deltas,
        tool.expected_deltas,
    )
    .await;
}

#[tokio::test]
async fn native_messages_passes_common_route_conformance() {
    let text = anthropic_messages_text();
    let tool = anthropic_messages_tool_round();
    let server = MockServer::start().await;
    let calls = mount_two_responses(&server, &text, &tool).await;
    let snapshot = build_anthropic_messages_route(
        AnthropicMessagesRouteConfig {
            declaration: ProviderRouteDeclaration::native_messages_v1(
                ProviderProfileId::derive_v1(b"route-conformance", b"native").unwrap(),
            ),
            model: "fixture-model-v1".to_string(),
            auth_source: ProviderAuthSource::None,
            base_url: server.uri(),
            messages_path: text.path.to_string(),
            policy: no_catalog_policy(),
        },
        &NoAuth,
    )
    .unwrap();
    assert_common_route_conformance(
        ProviderRouteCurrent::new(snapshot),
        calls,
        text.expected_deltas,
        tool.expected_deltas,
    )
    .await;
}
