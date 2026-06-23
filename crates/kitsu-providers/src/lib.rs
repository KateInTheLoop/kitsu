//! kitsu-providers — the Provider trait and concrete adapters (OpenAI-compatible,
//! gateways like the AI gateway). See docs/architecture.md.

pub mod auth;
pub mod error;
pub mod openai_compat;
pub mod types;

pub use error::{AuthError, ExpiryStatus, ProviderError};
pub use types::{
    ChatDelta, ChatMessage, ChatRequest, FinishReason, Role, ToolCall, ToolDescriptor, Usage,
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

    fn list_models(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<Vec<String>>, ProviderError>> + Send;
}
