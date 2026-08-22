//! Closed compiled provider set used by product composition.
//!
//! This is a static dispatch boundary, not a runtime plugin registry. Concrete
//! adapters remain separately testable while every caller keeps one `Provider`
//! type and one immutable route contract.

use std::sync::Arc;

use crate::anthropic_messages::{
    build_anthropic_messages_route, AnthropicMessagesProvider, AnthropicMessagesRouteBuildError,
    AnthropicMessagesRouteConfig,
};
use crate::auth::AuthProvider;
use crate::openai_compat::{
    build_openai_compatible_catalog_route, build_openai_compatible_route,
    OpenAICompatCatalogRouteConfig, OpenAICompatProvider, OpenAICompatRouteBuildError,
    OpenAICompatRouteConfig,
};
use crate::{
    ChatRequest, ChatStream, Provider, ProviderCapability, ProviderError, ProviderRouteCurrent,
    ProviderRouteHandle, ProviderRouteSnapshot, ProviderTelemetrySink,
};

pub enum CompiledProvider {
    CompatibleChat(Arc<OpenAICompatProvider>),
    AnthropicMessages(Arc<AnthropicMessagesProvider>),
}

impl Provider for CompiledProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        match self {
            Self::CompatibleChat(provider) => provider.healthcheck().await,
            Self::AnthropicMessages(provider) => provider.healthcheck().await,
        }
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream, ProviderError> {
        match self {
            Self::CompatibleChat(provider) => provider.chat_stream(request).await,
            Self::AnthropicMessages(provider) => provider.chat_stream(request).await,
        }
    }

    async fn chat_stream_with_telemetry(
        &self,
        request: ChatRequest,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<ChatStream, ProviderError> {
        match self {
            Self::CompatibleChat(provider) => {
                provider
                    .chat_stream_with_telemetry(request, telemetry)
                    .await
            }
            Self::AnthropicMessages(provider) => {
                provider
                    .chat_stream_with_telemetry(request, telemetry)
                    .await
            }
        }
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        match self {
            Self::CompatibleChat(provider) => provider.list_models().await,
            Self::AnthropicMessages(provider) => provider.list_models().await,
        }
    }

    async fn list_models_with_telemetry(
        &self,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        match self {
            Self::CompatibleChat(provider) => provider.list_models_with_telemetry(telemetry).await,
            Self::AnthropicMessages(provider) => {
                provider.list_models_with_telemetry(telemetry).await
            }
        }
    }
}

impl ProviderRouteHandle<CompiledProvider> {
    pub async fn model_context_length(&self, model: &str) -> Result<Option<u64>, ProviderError> {
        let Some(runtime) = self.admit_optional_catalog_runtime()? else {
            return Ok(None);
        };
        match runtime.as_ref() {
            CompiledProvider::CompatibleChat(provider) => {
                Ok(provider.model_context_length(model).await)
            }
            CompiledProvider::AnthropicMessages(_) => Ok(None),
        }
    }

    pub async fn advisory_model_context_length(
        &self,
        model: &str,
    ) -> Result<Option<u64>, ProviderError> {
        let Some(runtime) =
            self.admit_optional_inference_runtime(ProviderCapability::ModelCatalog)?
        else {
            return Ok(None);
        };
        match runtime.as_ref() {
            CompiledProvider::CompatibleChat(provider) => {
                Ok(provider.model_context_length(model).await)
            }
            CompiledProvider::AnthropicMessages(_) => Ok(None),
        }
    }
}

impl ProviderRouteCurrent<CompiledProvider> {
    pub async fn model_context_length(&self, model: &str) -> Result<Option<u64>, ProviderError> {
        self.bind_model(model)?.model_context_length(model).await
    }

    pub async fn advisory_model_context_length(
        &self,
        model: &str,
    ) -> Result<Option<u64>, ProviderError> {
        self.bind_model(model)?
            .advisory_model_context_length(model)
            .await
    }
}

pub enum CompiledProviderRouteConfig {
    CompatibleChat(OpenAICompatRouteConfig),
    AnthropicMessages(AnthropicMessagesRouteConfig),
}

#[derive(Debug, thiserror::Error)]
pub enum CompiledProviderRouteBuildError {
    #[error(transparent)]
    Contract(#[from] crate::ProviderRouteContractError),
    #[error(transparent)]
    CompatibleChat(#[from] OpenAICompatRouteBuildError),
    #[error(transparent)]
    AnthropicMessages(#[from] AnthropicMessagesRouteBuildError),
    #[error("the anthropic-messages adapter does not implement model catalog discovery")]
    AnthropicMessagesCatalogUnsupported,
}

pub fn build_compiled_provider_route(
    config: CompiledProviderRouteConfig,
    auth: &dyn AuthProvider,
) -> Result<ProviderRouteSnapshot<CompiledProvider>, CompiledProviderRouteBuildError> {
    match config {
        CompiledProviderRouteConfig::CompatibleChat(config) => {
            let snapshot = build_openai_compatible_route(config, auth)?;
            Ok(snapshot.map_runtime(|runtime| Arc::new(CompiledProvider::CompatibleChat(runtime))))
        }
        CompiledProviderRouteConfig::AnthropicMessages(config) => {
            let snapshot = build_anthropic_messages_route(config, auth)?;
            Ok(snapshot
                .map_runtime(|runtime| Arc::new(CompiledProvider::AnthropicMessages(runtime))))
        }
    }
}

pub fn build_compiled_catalog_route(
    config: OpenAICompatCatalogRouteConfig,
    auth: &dyn AuthProvider,
) -> Result<ProviderRouteSnapshot<CompiledProvider>, CompiledProviderRouteBuildError> {
    let snapshot = build_openai_compatible_catalog_route(config, auth)?;
    Ok(snapshot.map_runtime(|runtime| Arc::new(CompiledProvider::CompatibleChat(runtime))))
}
