//! Provider + auth error types. AUTHORITATIVE CONTRACT.

use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum ProviderError {
    #[error("connection error: {0}")]
    Connection(String),

    #[error("request timed out")]
    Timeout,

    #[error("invalid response from provider: {0}")]
    InvalidResponse(String),

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
