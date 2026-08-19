use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use kitsu_providers::{
    ChatDelta, ChatRequest, ChatStream, JsonSchemaFormat, Provider, ProviderAdapterId,
    ProviderAuthSource, ProviderCapabilities, ProviderCapability, ProviderCatalogPolicy,
    ProviderError, ProviderProfileId, ProviderRouteContractError, ProviderRouteCurrent,
    ProviderRouteDeclaration, ProviderRoutePolicy, ProviderRouteSnapshot, ProviderWireRevision,
    ToolDescriptor,
};

#[derive(Default)]
struct CountingProvider {
    chat_sends: AtomicUsize,
    catalog_sends: AtomicUsize,
}

#[derive(Default)]
struct BlockingProvider {
    chat_sends: AtomicUsize,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl Provider for BlockingProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
        self.chat_sends.fetch_add(1, Ordering::AcqRel);
        self.started.notify_one();
        self.release.notified().await;
        Ok(stream::empty::<Result<ChatDelta, ProviderError>>().boxed())
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        Ok(Some(Vec::new()))
    }
}

impl Provider for CountingProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
        self.chat_sends.fetch_add(1, Ordering::AcqRel);
        Ok(stream::empty::<Result<ChatDelta, ProviderError>>().boxed())
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        self.catalog_sends.fetch_add(1, Ordering::AcqRel);
        Ok(Some(vec!["model-a".to_string()]))
    }
}

fn policy(catalog: ProviderCatalogPolicy) -> ProviderRoutePolicy {
    ProviderRoutePolicy::new(
        2,
        0,
        Duration::from_secs(120),
        Duration::from_secs(120),
        Duration::from_secs(2),
        catalog,
    )
    .unwrap()
}

fn declaration(catalog: bool) -> ProviderRouteDeclaration {
    ProviderRouteDeclaration::compatible_chat_v1(
        ProviderProfileId::derive_v1(b"route-handle", b"primary").unwrap(),
        catalog,
    )
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "model-a".to_string(),
        messages: Vec::new(),
        tools: None,
        stream: true,
        temperature: None,
        reasoning_effort: None,
        max_tokens: None,
        response_format: None,
        prompt_cache_key: None,
    }
}

#[tokio::test]
async fn handle_admits_current_inference_and_catalog_against_one_snapshot() {
    let runtime = Arc::new(CountingProvider::default());
    let snapshot = ProviderRouteSnapshot::initial(
        declaration(true),
        "model-a",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        runtime.clone(),
        runtime.clone(),
    )
    .unwrap();
    let current = ProviderRouteCurrent::new(snapshot);
    let handle = current.bind().unwrap();

    drop(handle.chat_stream(request()).await.unwrap());
    assert_eq!(handle.list_models().await.unwrap().unwrap(), ["model-a"]);
    assert_eq!(runtime.chat_sends.load(Ordering::Acquire), 1);
    assert_eq!(runtime.catalog_sends.load(Ordering::Acquire), 1);
    assert_eq!(handle.identity().generation().get(), 1);
    assert_eq!(handle.model(), "model-a");
}

#[tokio::test]
async fn unavailable_optional_catalog_returns_none_before_the_runtime() {
    let runtime = Arc::new(CountingProvider::default());
    let snapshot = ProviderRouteSnapshot::initial(
        declaration(false),
        "model-a",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Unavailable),
        runtime.clone(),
        runtime.clone(),
    )
    .unwrap();
    let handle = ProviderRouteCurrent::new(snapshot).bind().unwrap();

    assert_eq!(handle.list_models().await.unwrap(), None);
    assert_eq!(runtime.catalog_sends.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn request_shape_capabilities_are_rejected_before_inference_runtime() {
    let runtime = Arc::new(CountingProvider::default());
    let profile = ProviderProfileId::derive_v1(b"route-handle", b"limited").unwrap();
    let limited = ProviderRouteDeclaration::new(
        ProviderAdapterId::OpenAiCompatible,
        profile,
        ProviderWireRevision::OpenAiChatCompletionsV1,
        ProviderCapabilities::new(1, [ProviderCapability::Streaming]).unwrap(),
    )
    .unwrap();
    let snapshot = ProviderRouteSnapshot::initial(
        limited,
        "model-a",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Unavailable),
        runtime.clone(),
        runtime.clone(),
    )
    .unwrap();
    let handle = ProviderRouteCurrent::new(snapshot).bind().unwrap();

    let mut with_tools = request();
    with_tools.tools = Some(vec![ToolDescriptor {
        name: "read".to_string(),
        description: "read".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }]);
    assert!(matches!(
        handle.chat_stream_with_telemetry(with_tools, None).await,
        Err(ProviderError::RouteContract(
            ProviderRouteContractError::UnsupportedCapability {
                capability: ProviderCapability::Tools
            }
        ))
    ));

    let mut structured = request();
    structured.response_format = Some(JsonSchemaFormat {
        name: "answer".to_string(),
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    });
    assert!(matches!(
        handle.chat_stream(structured).await,
        Err(ProviderError::RouteContract(
            ProviderRouteContractError::UnsupportedCapability {
                capability: ProviderCapability::StructuredOutput
            }
        ))
    ));
    assert_eq!(runtime.chat_sends.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn replacement_makes_old_handle_stale_and_failed_publish_keeps_current() {
    let first_runtime = Arc::new(CountingProvider::default());
    let first = ProviderRouteSnapshot::initial(
        declaration(true),
        "model-a",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        first_runtime.clone(),
        first_runtime.clone(),
    )
    .unwrap();
    let first_identity = first.identity().clone();
    let current = ProviderRouteCurrent::new(first);
    let old_handle = current.bind().unwrap();

    let second_runtime = Arc::new(CountingProvider::default());
    let second = ProviderRouteSnapshot::successor(
        &first_identity,
        declaration(true),
        "model-b",
        ProviderAuthSource::ManagedCredential,
        policy(ProviderCatalogPolicy::Configured),
        second_runtime.clone(),
        second_runtime.clone(),
    )
    .unwrap();
    current
        .replace(first_identity.generation(), second)
        .unwrap();

    assert!(matches!(
        old_handle.chat_stream(request()).await,
        Err(ProviderError::RouteContract(
            ProviderRouteContractError::StaleGeneration { .. }
        ))
    ));
    assert_eq!(first_runtime.chat_sends.load(Ordering::Acquire), 0);
    assert!(matches!(
        old_handle.list_models().await,
        Err(ProviderError::RouteContract(
            ProviderRouteContractError::StaleGeneration { .. }
        ))
    ));
    assert_eq!(first_runtime.catalog_sends.load(Ordering::Acquire), 0);

    let current_handle = current.bind().unwrap();
    drop(current_handle.chat_stream(request()).await.unwrap());
    assert_eq!(current_handle.identity().generation().get(), 2);
    assert_eq!(second_runtime.chat_sends.load(Ordering::Acquire), 1);

    let third_runtime = Arc::new(CountingProvider::default());
    let third = ProviderRouteSnapshot::successor(
        current_handle.identity(),
        declaration(true),
        "model-c",
        ProviderAuthSource::Environment,
        policy(ProviderCatalogPolicy::Configured),
        third_runtime.clone(),
        third_runtime,
    )
    .unwrap();
    assert!(matches!(
        current.replace(first_identity.generation(), third),
        Err(ProviderRouteContractError::StaleGeneration { .. })
    ));
    assert_eq!(current.bind().unwrap().identity().generation().get(), 2);
}

#[test]
fn invalid_replacement_shape_leaves_the_current_generation_installed() {
    let runtime = Arc::new(CountingProvider::default());
    let first = ProviderRouteSnapshot::initial(
        declaration(true),
        "model-a",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        runtime.clone(),
        runtime,
    )
    .unwrap();
    let first_identity = first.identity().clone();
    let current = ProviderRouteCurrent::new(first);

    let wrong_profile = ProviderRouteDeclaration::compatible_chat_v1(
        ProviderProfileId::derive_v1(b"route-handle", b"other").unwrap(),
        true,
    );
    let wrong_runtime = Arc::new(CountingProvider::default());
    let wrong = ProviderRouteSnapshot::initial(
        wrong_profile,
        "model-b",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        wrong_runtime.clone(),
        wrong_runtime,
    )
    .unwrap();
    assert!(matches!(
        current.replace(first_identity.generation(), wrong),
        Err(ProviderRouteContractError::ReplacementProfileMismatch)
    ));
    assert_eq!(current.bind().unwrap().identity(), &first_identity);

    let second_runtime = Arc::new(CountingProvider::default());
    let second = ProviderRouteSnapshot::successor(
        &first_identity,
        declaration(true),
        "model-b",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        second_runtime.clone(),
        second_runtime,
    )
    .unwrap();
    let second_identity = second.identity().clone();
    let third_runtime = Arc::new(CountingProvider::default());
    let third = ProviderRouteSnapshot::successor(
        &second_identity,
        declaration(true),
        "model-c",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        third_runtime.clone(),
        third_runtime,
    )
    .unwrap();
    assert!(matches!(
        current.replace(first_identity.generation(), third),
        Err(ProviderRouteContractError::NonSuccessorGeneration { .. })
    ));
    assert_eq!(current.bind().unwrap().identity(), &first_identity);
}

#[tokio::test]
async fn request_admitted_before_replacement_drains_on_its_bound_runtime() {
    let first_runtime = Arc::new(BlockingProvider::default());
    let first = ProviderRouteSnapshot::initial(
        declaration(true),
        "model-a",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        first_runtime.clone(),
        first_runtime.clone(),
    )
    .unwrap();
    let first_identity = first.identity().clone();
    let current = ProviderRouteCurrent::new(first);
    let old_handle = current.bind().unwrap();
    let in_flight_handle = old_handle.clone();
    let in_flight = tokio::spawn(async move {
        drop(in_flight_handle.chat_stream(request()).await.unwrap());
    });
    tokio::time::timeout(Duration::from_secs(1), first_runtime.started.notified())
        .await
        .expect("the first request reaches its admitted runtime");

    let second_runtime = Arc::new(BlockingProvider::default());
    let second = ProviderRouteSnapshot::successor(
        &first_identity,
        declaration(true),
        "model-b",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        second_runtime.clone(),
        second_runtime,
    )
    .unwrap();
    current
        .replace(first_identity.generation(), second)
        .unwrap();
    first_runtime.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), in_flight)
        .await
        .expect("an admitted old request drains")
        .unwrap();
    assert_eq!(first_runtime.chat_sends.load(Ordering::Acquire), 1);

    assert!(matches!(
        old_handle.chat_stream(request()).await,
        Err(ProviderError::RouteContract(
            ProviderRouteContractError::StaleGeneration { .. }
        ))
    ));
    assert_eq!(first_runtime.chat_sends.load(Ordering::Acquire), 1);
}
