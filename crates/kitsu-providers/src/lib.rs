//! kitsu-providers — the Provider trait and concrete adapters (OpenAI-compatible,
//! gateways like the AI gateway). See docs/architecture.md.

pub mod auth;
pub mod error;
pub mod openai_compat;
pub mod route;
pub mod telemetry;
pub mod types;

pub use error::{AuthError, ExpiryStatus, ProviderError, ProviderResponsePart};
pub use route::{
    ProviderAdapterId, ProviderAuthSource, ProviderCapabilities, ProviderCapability,
    ProviderCatalogPolicy, ProviderProfileId, ProviderRouteContractError, ProviderRouteDeclaration,
    ProviderRouteGeneration, ProviderRouteIdentity, ProviderRoutePolicy, ProviderRouteSnapshot,
    ProviderRouteTimeout, ProviderWireRevision, PROVIDER_ROUTE_CONTRACT_REVISION,
};
pub use telemetry::{
    ProviderAttemptCategory, ProviderAttemptEvent, ProviderAttemptOutcome, ProviderTelemetrySink,
};
pub use types::{
    ChatDelta, ChatMessage, ChatRequest, FinishReason, JsonSchemaFormat, ReasoningEffort, Role,
    ToolCall, ToolDescriptor, Usage,
};

use futures::stream::BoxStream;

/// A stream of response deltas. Each item is one [`ChatDelta`].
pub type ChatStream = BoxStream<'static, Result<ChatDelta, ProviderError>>;

/// "How we talk to a model runtime." Signature is stable across all flavors;
/// gateways differ only in construction (base_url, path, auth) — see design §1.
pub trait Provider: Send + Sync {
    fn healthcheck(&self) -> impl std::future::Future<Output = Result<(), ProviderError>> + Send;

    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> impl std::future::Future<Output = Result<ChatStream, ProviderError>> + Send;

    /// Start a chat request while reporting each physical transport attempt.
    ///
    /// The default preserves compatibility for providers without retry
    /// instrumentation. Concrete adapters that retry internally should
    /// override this method and emit one event for every actual network send.
    fn chat_stream_with_telemetry(
        &self,
        req: ChatRequest,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> impl std::future::Future<Output = Result<ChatStream, ProviderError>> + Send {
        let _ = telemetry;
        self.chat_stream(req)
    }

    fn list_models(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<Vec<String>>, ProviderError>> + Send;

    /// Fetch the model catalog while reporting each physical transport attempt.
    ///
    /// The default preserves compatibility for providers without catalog
    /// instrumentation. Concrete adapters should emit no attempt when the
    /// result is served from a local cache and one terminal event for every
    /// physical HTTP send.
    fn list_models_with_telemetry(
        &self,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> impl std::future::Future<Output = Result<Option<Vec<String>>, ProviderError>> + Send {
        let _ = telemetry;
        self.list_models()
    }
}

/// Share one provider across the main loop and spawned subagents: `Arc<P>` is a
/// `Provider` that delegates to its inner `P`.
impl<P: Provider> Provider for std::sync::Arc<P> {
    fn healthcheck(&self) -> impl std::future::Future<Output = Result<(), ProviderError>> + Send {
        (**self).healthcheck()
    }
    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> impl std::future::Future<Output = Result<ChatStream, ProviderError>> + Send {
        (**self).chat_stream(req)
    }
    fn chat_stream_with_telemetry(
        &self,
        req: ChatRequest,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> impl std::future::Future<Output = Result<ChatStream, ProviderError>> + Send {
        (**self).chat_stream_with_telemetry(req, telemetry)
    }
    fn list_models(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<Vec<String>>, ProviderError>> + Send {
        (**self).list_models()
    }
    fn list_models_with_telemetry(
        &self,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> impl std::future::Future<Output = Result<Option<Vec<String>>, ProviderError>> + Send {
        (**self).list_models_with_telemetry(telemetry)
    }
}
