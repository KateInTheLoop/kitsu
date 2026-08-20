use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use kitsu_providers::{
    ChatDelta, ChatRequest, ChatStream, JsonSchemaFormat, Provider, ProviderAdapterId,
    ProviderAttemptCategory, ProviderAttemptEvent, ProviderAttemptObservation,
    ProviderAttemptOutcome, ProviderAuthSource, ProviderCapabilities, ProviderCapability,
    ProviderCatalogPolicy, ProviderError, ProviderProfileId, ProviderRouteContractError,
    ProviderRouteCurrent, ProviderRouteDeclaration, ProviderRoutePolicy, ProviderRouteSnapshot,
    ProviderTelemetrySink, ProviderWireRevision, ToolDescriptor,
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

#[derive(Default)]
struct RetryingProvider {
    chat_sends: AtomicUsize,
    catalog_sends: AtomicUsize,
    chat_started: tokio::sync::Notify,
    catalog_started: tokio::sync::Notify,
    release_chat: tokio::sync::Notify,
    release_catalog: tokio::sync::Notify,
}

#[derive(Default)]
struct CancellableProvider {
    chat_sends: AtomicUsize,
    started: tokio::sync::Notify,
}

struct CancellationAttemptGuard {
    telemetry: Option<ProviderTelemetrySink>,
}

impl Drop for CancellationAttemptGuard {
    fn drop(&mut self) {
        if let Some(telemetry) = self.telemetry.take() {
            let _ = telemetry.record(attempt_event(
                1,
                ProviderAttemptCategory::Cancelled,
                ProviderAttemptOutcome::Cancelled,
                true,
            ));
        }
    }
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

impl Provider for RetryingProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
        self.chat_sends.fetch_add(1, Ordering::AcqRel);
        Ok(stream::empty::<Result<ChatDelta, ProviderError>>().boxed())
    }

    async fn chat_stream_with_telemetry(
        &self,
        _req: ChatRequest,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<ChatStream, ProviderError> {
        self.chat_sends.fetch_add(1, Ordering::AcqRel);
        if let Some(sink) = &telemetry {
            assert!(sink.record(attempt_event(
                1,
                ProviderAttemptCategory::Unavailable,
                ProviderAttemptOutcome::RetryScheduled,
                false,
            )));
        }
        self.chat_started.notify_one();
        self.release_chat.notified().await;
        self.chat_sends.fetch_add(1, Ordering::AcqRel);
        if let Some(sink) = &telemetry {
            assert!(sink.record(attempt_event(
                2,
                ProviderAttemptCategory::Success,
                ProviderAttemptOutcome::BodyCompleted,
                true,
            )));
        }
        Ok(stream::empty::<Result<ChatDelta, ProviderError>>().boxed())
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        self.catalog_sends.fetch_add(1, Ordering::AcqRel);
        Ok(Some(vec!["model-a".to_string()]))
    }

    async fn list_models_with_telemetry(
        &self,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        self.catalog_sends.fetch_add(1, Ordering::AcqRel);
        if let Some(sink) = &telemetry {
            assert!(sink.record(attempt_event(
                1,
                ProviderAttemptCategory::Unavailable,
                ProviderAttemptOutcome::RetryScheduled,
                false,
            )));
        }
        self.catalog_started.notify_one();
        self.release_catalog.notified().await;
        self.catalog_sends.fetch_add(1, Ordering::AcqRel);
        if let Some(sink) = &telemetry {
            assert!(sink.record(attempt_event(
                2,
                ProviderAttemptCategory::Success,
                ProviderAttemptOutcome::BodyCompleted,
                true,
            )));
        }
        Ok(Some(vec!["model-a".to_string()]))
    }
}

impl Provider for CancellableProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
        unreachable!("the fixture requires telemetry")
    }

    async fn chat_stream_with_telemetry(
        &self,
        _req: ChatRequest,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<ChatStream, ProviderError> {
        self.chat_sends.fetch_add(1, Ordering::AcqRel);
        let _guard = CancellationAttemptGuard { telemetry };
        self.started.notify_one();
        std::future::pending::<()>().await;
        unreachable!("the fixture request must be cancelled")
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        Ok(None)
    }
}

fn attempt_event(
    attempt: u32,
    category: ProviderAttemptCategory,
    outcome: ProviderAttemptOutcome,
    terminal: bool,
) -> ProviderAttemptEvent {
    ProviderAttemptEvent {
        attempt,
        max_attempts: 2,
        category,
        status: None,
        outcome,
        terminal,
        retry_delay_ms: (!terminal).then_some(1),
        retry_after_used: false,
        remaining_deadline_ms: 1_000,
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

fn catalog_declaration() -> ProviderRouteDeclaration {
    ProviderRouteDeclaration::compatible_catalog_v1(
        ProviderProfileId::derive_v1(b"route-handle", b"catalog-only").unwrap(),
    )
}

fn request() -> ChatRequest {
    request_for("model-a")
}

fn request_for(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
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
async fn catalog_only_handle_admits_catalog_but_cannot_be_rebound_for_chat() {
    let runtime = Arc::new(CountingProvider::default());
    let snapshot = ProviderRouteSnapshot::initial_catalog_only(
        catalog_declaration(),
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        runtime.clone(),
    )
    .unwrap();
    let current = ProviderRouteCurrent::new(snapshot);
    let catalog = current.bind_catalog().unwrap();

    assert_eq!(catalog.list_models().await.unwrap().unwrap(), ["model-a"]);
    assert_eq!(catalog.identity().generation().get(), 1);
    assert_eq!(runtime.catalog_sends.load(Ordering::Acquire), 1);
    assert_eq!(runtime.chat_sends.load(Ordering::Acquire), 0);
    assert!(matches!(
        current.bind(),
        Err(ProviderRouteContractError::InferenceUnavailable)
    ));
    assert!(matches!(
        current.bind_model("invented-model"),
        Err(ProviderRouteContractError::InferenceUnavailable)
    ));
}

#[tokio::test]
async fn catalog_only_replacement_stales_old_handle_without_a_send() {
    let first_runtime = Arc::new(CountingProvider::default());
    let first = ProviderRouteSnapshot::initial_catalog_only(
        catalog_declaration(),
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Configured),
        first_runtime.clone(),
    )
    .unwrap();
    let first_identity = first.identity().clone();
    let current = ProviderRouteCurrent::new(first);
    let old = current.bind_catalog().unwrap();

    let second_runtime = Arc::new(CountingProvider::default());
    let second = ProviderRouteSnapshot::successor_catalog_only(
        &first_identity,
        catalog_declaration(),
        ProviderAuthSource::ManagedCredential,
        policy(ProviderCatalogPolicy::Configured),
        second_runtime.clone(),
    )
    .unwrap();
    current
        .replace(first_identity.generation(), second)
        .unwrap();

    assert!(matches!(
        old.list_models().await,
        Err(ProviderError::RouteContract(
            ProviderRouteContractError::StaleGeneration { .. }
        ))
    ));
    assert_eq!(first_runtime.catalog_sends.load(Ordering::Acquire), 0);

    let current_catalog = current.bind_catalog().unwrap();
    assert_eq!(current_catalog.identity().generation().get(), 2);
    assert_eq!(
        current_catalog.list_models().await.unwrap().unwrap(),
        ["model-a"]
    );
    assert_eq!(second_runtime.catalog_sends.load(Ordering::Acquire), 1);
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
async fn model_bound_handles_reject_mismatch_and_allow_scoped_models() {
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
    let default_handle = current.bind().unwrap();

    assert!(matches!(
        default_handle.chat_stream(request_for("model-b")).await,
        Err(ProviderError::RouteContract(
            ProviderRouteContractError::RequestModelMismatch
        ))
    ));
    assert_eq!(runtime.chat_sends.load(Ordering::Acquire), 0);

    let scoped = current.bind_model("profile-model").unwrap();
    drop(
        scoped
            .chat_stream(request_for("profile-model"))
            .await
            .unwrap(),
    );
    assert_eq!(scoped.model(), "profile-model");
    assert_eq!(scoped.identity().generation().get(), 1);
    assert_eq!(current.bind().unwrap().model(), "model-a");
    assert_eq!(runtime.chat_sends.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn model_replacement_publishes_one_successor_or_keeps_the_old_route() {
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
    let current = ProviderRouteCurrent::new(first);
    let old = current.bind().unwrap();

    let replacement = current
        .replace_model(old.identity().generation(), "model-b")
        .unwrap();
    assert_eq!(replacement.identity().generation().get(), 2);
    assert_eq!(replacement.model(), "model-b");
    assert_eq!(current.bind().unwrap().model(), "model-b");
    assert!(matches!(
        old.chat_stream(request()).await,
        Err(ProviderError::RouteContract(
            ProviderRouteContractError::StaleGeneration { .. }
        ))
    ));

    let oversized = "m".repeat(ProviderRouteSnapshot::<CountingProvider>::MAX_MODEL_ID_BYTES + 1);
    assert!(matches!(
        current.replace_model(replacement.identity().generation(), oversized),
        Err(ProviderRouteContractError::ModelIdTooLong { .. })
    ));
    let still_current = current.bind().unwrap();
    assert_eq!(still_current.identity().generation().get(), 2);
    assert_eq!(still_current.model(), "model-b");

    let unchanged = current
        .replace_model(still_current.identity().generation(), "model-b")
        .unwrap();
    assert_eq!(unchanged.identity().generation().get(), 2);
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
    drop(
        current_handle
            .chat_stream(request_for("model-b"))
            .await
            .unwrap(),
    );
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

#[tokio::test]
async fn admitted_retry_and_catalog_attempts_keep_one_generation_across_replacement() {
    let first_runtime = Arc::new(RetryingProvider::default());
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
    let old = current.bind().unwrap();
    let chat_events = Arc::new(Mutex::new(Vec::new()));
    let catalog_events = Arc::new(Mutex::new(Vec::new()));

    let chat = {
        let handle = old.clone();
        let events = Arc::clone(&chat_events);
        tokio::spawn(async move {
            let sink = ProviderTelemetrySink::with_fallible_context_observer(move |observation| {
                events.lock().unwrap().push(observation);
                true
            });
            drop(
                handle
                    .chat_stream_with_telemetry(request(), Some(sink))
                    .await
                    .unwrap(),
            );
        })
    };
    let catalog = {
        let handle = old.clone();
        let events = Arc::clone(&catalog_events);
        tokio::spawn(async move {
            let sink = ProviderTelemetrySink::with_fallible_context_observer(move |observation| {
                events.lock().unwrap().push(observation);
                true
            });
            assert_eq!(
                handle.list_models_with_telemetry(Some(sink)).await.unwrap(),
                Some(vec!["model-a".to_string()])
            );
        })
    };
    tokio::time::timeout(
        Duration::from_secs(1),
        first_runtime.chat_started.notified(),
    )
    .await
    .expect("the first chat attempt starts");
    tokio::time::timeout(
        Duration::from_secs(1),
        first_runtime.catalog_started.notified(),
    )
    .await
    .expect("the first catalog attempt starts");

    let second_runtime = Arc::new(RetryingProvider::default());
    let second = ProviderRouteSnapshot::successor(
        &first_identity,
        declaration(true),
        "model-b",
        ProviderAuthSource::Environment,
        policy(ProviderCatalogPolicy::Configured),
        second_runtime.clone(),
        second_runtime.clone(),
    )
    .unwrap();
    current
        .replace(first_identity.generation(), second)
        .unwrap();
    first_runtime.release_chat.notify_one();
    first_runtime.release_catalog.notify_one();
    tokio::time::timeout(Duration::from_secs(1), chat)
        .await
        .expect("old chat retry drains")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), catalog)
        .await
        .expect("old catalog retry drains")
        .unwrap();

    for events in [&chat_events, &catalog_events] {
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.attempt, 1);
        assert_eq!(events[1].event.attempt, 2);
        assert!(events
            .iter()
            .all(|observation: &ProviderAttemptObservation| {
                observation.route_generation == Some(first_identity.generation())
            }));
    }
    assert_eq!(first_runtime.chat_sends.load(Ordering::Acquire), 2);
    assert_eq!(first_runtime.catalog_sends.load(Ordering::Acquire), 2);
    assert_eq!(second_runtime.chat_sends.load(Ordering::Acquire), 0);
    assert_eq!(second_runtime.catalog_sends.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn cancelled_admitted_attempt_keeps_its_generation_after_replacement() {
    let first_runtime = Arc::new(CancellableProvider::default());
    let first = ProviderRouteSnapshot::initial(
        declaration(false),
        "model-a",
        ProviderAuthSource::None,
        policy(ProviderCatalogPolicy::Unavailable),
        first_runtime.clone(),
        first_runtime.clone(),
    )
    .unwrap();
    let first_identity = first.identity().clone();
    let current = ProviderRouteCurrent::new(first);
    let handle = current.bind().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let request_task = {
        let events = Arc::clone(&events);
        tokio::spawn(async move {
            let sink = ProviderTelemetrySink::with_fallible_context_observer(move |observation| {
                events.lock().unwrap().push(observation);
                true
            });
            let _ = handle
                .chat_stream_with_telemetry(request(), Some(sink))
                .await;
        })
    };
    tokio::time::timeout(Duration::from_secs(1), first_runtime.started.notified())
        .await
        .expect("the cancellable request starts");

    let second_runtime = Arc::new(CancellableProvider::default());
    let second = ProviderRouteSnapshot::successor(
        &first_identity,
        declaration(false),
        "model-b",
        ProviderAuthSource::ManagedCredential,
        policy(ProviderCatalogPolicy::Unavailable),
        second_runtime.clone(),
        second_runtime,
    )
    .unwrap();
    current
        .replace(first_identity.generation(), second)
        .unwrap();
    request_task.abort();
    assert!(request_task.await.unwrap_err().is_cancelled());

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event.category, ProviderAttemptCategory::Cancelled);
    assert_eq!(events[0].event.outcome, ProviderAttemptOutcome::Cancelled);
    assert!(events[0].event.terminal);
    assert_eq!(
        events[0].route_generation,
        Some(first_identity.generation())
    );
    assert_eq!(first_runtime.chat_sends.load(Ordering::Acquire), 1);
}
