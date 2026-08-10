//! Auth providers. Token is read from env at request time, never logged,
//! never persisted (design §3). Expiry uses ANTHROPIC_TOKEN_EXPIRES only
//! (env-var-only decision; no JWT decode, no base64 dep).

use crate::error::{AuthError, ExpiryStatus};
use chrono::Utc;
use reqwest::header::{HeaderName, HeaderValue};

#[cfg(test)]
#[path = "auth_tests.rs"]
mod auth_tests;

/// "How we attach credentials to a request."
pub trait AuthProvider: Send + Sync {
    /// Header name + value to attach, `None` for an explicitly unauthenticated
    /// provider, or an actionable credential error. Any returned `HeaderValue`
    /// MUST be marked sensitive so it is never logged.
    fn header(&self) -> Result<Option<(HeaderName, HeaderValue)>, AuthError>;

    /// Best-effort expiry check; used for a pre-request warning only.
    fn expiry_status(&self) -> ExpiryStatus;

    /// Actionable credential-refresh guidance for a gateway rejection.
    fn refresh_hint(&self) -> Option<String> {
        None
    }
}

/// Reads a bearer token from an environment variable at request time.
/// Defaults: `env = "ANTHROPIC_AUTH_TOKEN"`, `expires_env = "ANTHROPIC_TOKEN_EXPIRES"`,
/// `header = "Authorization"`, `scheme = "Bearer"`.
pub struct EnvBearerAuth {
    pub env: String,
    pub expires_env: Option<String>,
    pub header: String,
    pub scheme: String,
    pub refresh_hint: Option<String>,
}

impl EnvBearerAuth {
    /// Constructor for the common case (gateway): Authorization: Bearer from
    /// the given env var, expiry from the given expires env var.
    pub fn new(
        env: impl Into<String>,
        expires_env: Option<String>,
        refresh_hint: Option<String>,
    ) -> Self {
        Self {
            env: env.into(),
            expires_env,
            header: "Authorization".to_string(),
            scheme: "Bearer".to_string(),
            refresh_hint,
        }
    }
}

impl AuthProvider for EnvBearerAuth {
    fn header(&self) -> Result<Option<(HeaderName, HeaderValue)>, AuthError> {
        // Read the token from env at request time. Missing or empty is an
        // actionable error carrying the configured refresh hint.
        let token = match std::env::var(&self.env) {
            Ok(v) if !v.is_empty() => v,
            _ => {
                return Err(AuthError::Missing {
                    env: self.env.clone(),
                    hint: self.refresh_hint.clone(),
                })
            }
        };

        // Header name; reqwest stores names lowercase but accepts mixed case.
        let name = HeaderName::from_bytes(self.header.to_lowercase().as_bytes()).map_err(|_| {
            AuthError::Malformed {
                env: self.env.clone(),
            }
        })?;

        // Build the value: `scheme token`, trimming the leading space when the
        // scheme is empty (raw token, e.g. x-api-key).
        let raw = format!("{} {}", self.scheme, token);
        let trimmed = if self.scheme.is_empty() {
            raw.trim_start()
        } else {
            raw.as_str()
        };

        let mut value = HeaderValue::from_str(trimmed).map_err(|_| AuthError::Malformed {
            env: self.env.clone(),
        })?;
        // Mark sensitive so reqwest/tracing never logs the token.
        value.set_sensitive(true);

        Ok(Some((name, value)))
    }

    fn expiry_status(&self) -> ExpiryStatus {
        // Env-var-only: parse the RFC3339 timestamp from `expires_env`. Any
        // missing/unparseable value yields Unknown (never an error).
        let Some(expires_env) = self.expires_env.as_ref() else {
            return ExpiryStatus::Unknown;
        };
        let Ok(raw) = std::env::var(expires_env) else {
            return ExpiryStatus::Unknown;
        };
        match chrono::DateTime::parse_from_rfc3339(raw.trim()) {
            Ok(ts) => {
                if ts.with_timezone(&Utc) > Utc::now() {
                    ExpiryStatus::Valid { until: raw }
                } else {
                    ExpiryStatus::Expired { since: raw }
                }
            }
            Err(_) => ExpiryStatus::Unknown,
        }
    }

    fn refresh_hint(&self) -> Option<String> {
        self.refresh_hint.clone()
    }
}

/// Inline static token (discouraged; see design §2). Attaches a fixed header.
pub struct StaticAuth {
    pub token: String,
    pub header: String,
    pub scheme: String,
}

impl StaticAuth {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            header: "Authorization".to_string(),
            scheme: "Bearer".to_string(),
        }
    }
}

impl AuthProvider for StaticAuth {
    fn header(&self) -> Result<Option<(HeaderName, HeaderValue)>, AuthError> {
        if self.token.is_empty() {
            return Err(AuthError::MissingStatic);
        }
        let name = HeaderName::from_bytes(self.header.to_lowercase().as_bytes())
            .map_err(|_| AuthError::Malformed { env: String::new() })?;
        let raw = format!("{} {}", self.scheme, self.token);
        let trimmed = if self.scheme.is_empty() {
            raw.trim_start()
        } else {
            raw.as_str()
        };
        let mut value = HeaderValue::from_str(trimmed)
            .map_err(|_| AuthError::Malformed { env: String::new() })?;
        value.set_sensitive(true);
        Ok(Some((name, value)))
    }

    fn expiry_status(&self) -> ExpiryStatus {
        ExpiryStatus::Unknown
    }
}

/// No-auth (local vLLM/Ollama).
pub struct NoAuth;

impl AuthProvider for NoAuth {
    fn header(&self) -> Result<Option<(HeaderName, HeaderValue)>, AuthError> {
        Ok(None)
    }
    fn expiry_status(&self) -> ExpiryStatus {
        ExpiryStatus::Unknown
    }
}
