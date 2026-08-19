//! Auth providers. Bearer tokens resolve from a non-empty environment value
//! first, then an optional caller-loaded managed credential. Raw credentials
//! are never logged. Expiry remains environment-only (no JWT decode or base64
//! dependency).

use crate::error::{AuthError, ExpiryStatus};
use chrono::Utc;
use reqwest::header::{HeaderName, HeaderValue};
use zeroize::Zeroizing;

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

/// Credential material resolved exactly once for one immutable route
/// generation. The raw header bytes are zeroized on drop and every request
/// receives a fresh sensitive `HeaderValue`; later environment or managed-store
/// changes cannot alter an already admitted retry or auxiliary call.
pub struct FrozenAuth {
    header: Option<(HeaderName, Zeroizing<Vec<u8>>)>,
    expiry_status: ExpiryStatus,
    refresh_hint: Option<String>,
}

impl FrozenAuth {
    pub fn resolve(source: &dyn AuthProvider) -> Result<Self, AuthError> {
        let header = source
            .header()?
            .map(|(name, value)| (name, Zeroizing::new(value.as_bytes().to_vec())));
        Ok(Self {
            header,
            expiry_status: source.expiry_status(),
            refresh_hint: source.refresh_hint(),
        })
    }
}

impl AuthProvider for FrozenAuth {
    fn header(&self) -> Result<Option<(HeaderName, HeaderValue)>, AuthError> {
        self.header
            .as_ref()
            .map(|(name, raw)| {
                let mut value = HeaderValue::from_bytes(raw.as_slice())
                    .expect("a header value validated before route publication remains valid");
                value.set_sensitive(true);
                (name.clone(), value)
            })
            .map_or(Ok(None), |header| Ok(Some(header)))
    }

    fn expiry_status(&self) -> ExpiryStatus {
        self.expiry_status.clone()
    }

    fn refresh_hint(&self) -> Option<String> {
        self.refresh_hint.clone()
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
    /// Optional config-scoped credential loaded by the CLI from its protected
    /// store. A non-empty environment value always takes precedence.
    pub managed_token: Option<Zeroizing<String>>,
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
            managed_token: None,
        }
    }

    fn header_for_token(
        &self,
        token: &str,
        managed: bool,
    ) -> Result<(HeaderName, HeaderValue), AuthError> {
        let malformed = || {
            if managed {
                AuthError::MalformedManaged
            } else {
                AuthError::Malformed {
                    env: self.env.clone(),
                }
            }
        };
        let name = HeaderName::from_bytes(self.header.to_lowercase().as_bytes())
            .map_err(|_| malformed())?;

        // Zero the intermediate combined header text after reqwest has copied
        // it. The returned HeaderValue is itself marked sensitive.
        let mut raw = Zeroizing::new(String::with_capacity(
            self.scheme
                .len()
                .saturating_add(token.len())
                .saturating_add(1),
        ));
        if !self.scheme.is_empty() {
            raw.push_str(&self.scheme);
            raw.push(' ');
        }
        raw.push_str(token);
        let mut value = HeaderValue::from_str(raw.as_str()).map_err(|_| malformed())?;
        value.set_sensitive(true);
        Ok((name, value))
    }

    /// Validate a candidate managed credential through the exact header path
    /// used for requests, without consulting or mutating process environment.
    pub fn validate_managed_token(&self, token: &str) -> Result<(), AuthError> {
        self.header_for_token(token, true).map(|_| ())
    }
}

impl AuthProvider for EnvBearerAuth {
    fn header(&self) -> Result<Option<(HeaderName, HeaderValue)>, AuthError> {
        // A non-empty, trimmed environment value is authoritative. Non-Unicode
        // input is malformed and must not silently fall back to a managed
        // credential. Empty or whitespace-only input permits managed fallback.
        let environment_token = match std::env::var_os(&self.env) {
            Some(value) => {
                let value = value.into_string().map_err(|_| AuthError::Malformed {
                    env: self.env.clone(),
                })?;
                let value = value.trim();
                (!value.is_empty()).then(|| Zeroizing::new(value.to_string()))
            }
            None => None,
        };
        let (token, managed) = if let Some(token) = environment_token.as_ref() {
            (token.as_str(), false)
        } else if let Some(token) = self.managed_token.as_ref() {
            (token.as_str(), true)
        } else {
            return Err(AuthError::Missing {
                env: self.env.clone(),
                hint: self.refresh_hint.clone(),
            });
        };
        self.header_for_token(token, managed).map(Some)
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
