//! Provider + auth error types. AUTHORITATIVE CONTRACT.

use thiserror::Error;

/// Bounded response component whose provider-controlled byte ceiling was
/// exceeded. Keeping this typed lets automation distinguish resource-safety
/// rejection from malformed JSON or a transport failure without including any
/// untrusted response content in the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderResponsePart {
    CompletionBody,
    ModelCatalogBody,
    ErrorBody,
    SseLine,
    SseStream,
    ToolArguments,
}

impl std::fmt::Display for ProviderResponsePart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CompletionBody => "completion body",
            Self::ModelCatalogBody => "model catalog body",
            Self::ErrorBody => "provider error body",
            Self::SseLine => "SSE line",
            Self::SseStream => "SSE stream",
            Self::ToolArguments => "streamed tool arguments",
        })
    }
}

#[derive(Debug, Clone, Error)]
pub enum ProviderError {
    #[error("connection error: {0}")]
    Connection(String),

    #[error("request timed out")]
    Timeout,

    #[error("invalid response from provider: {0}")]
    InvalidResponse(String),

    #[error("provider {part} exceeded the {limit_bytes}-byte safety limit")]
    ResponseTooLarge {
        part: ProviderResponsePart,
        limit_bytes: usize,
    },

    #[error("auth rejected by gateway ({status}){}", .hint.as_ref().map(|h| format!(" — {h}")).unwrap_or_default())]
    Unauthorized { status: u16, hint: Option<String> },

    #[error("provider returned HTTP {status}: {body}")]
    Http { status: u16, body: String },

    #[error("native tool calling is not supported by this provider")]
    ToolsNotSupported,

    #[error(transparent)]
    Auth(#[from] AuthError),
}

#[derive(Debug, Clone, Error)]
pub enum AuthError {
    #[error("auth error: ${env} is not set{}", .hint.as_ref().map(|h| format!(" — {h}")).unwrap_or_default())]
    Missing { env: String, hint: Option<String> },

    #[error("auth error: static token is not configured")]
    MissingStatic,

    #[error("auth error: token in ${env} is malformed")]
    Malformed { env: String },
}

/// Best-effort token expiry status. Never fatal on its own (per design §3:
/// warn-but-send; the gateway's real 401 is authoritative).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpiryStatus {
    Valid { until: String },
    Expired { since: String },
    Unknown,
}
