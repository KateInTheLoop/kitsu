//! Credential-safe provider errors and recovery messages.

use crate::config::Gateway;
use kitsu_providers::{ProviderError, ProviderRouteContractError};

use crate::cli_error::CliError;
use crate::cli_error::ProviderDiagnosticCode;
use crate::presentation::terminal_inline;

#[derive(Clone, Copy)]
pub(crate) enum ProviderOperation<'a> {
    Chat { model: &'a str },
    InferenceProbe { model: &'a str },
    ModelCatalog,
}

/// Secret-free provider metadata used only to choose bounded recovery prose.
/// The raw base URL never enters an error message: config may contain userinfo
/// or query credentials, so reduce it to the one trusted fact the formatter
/// needs while setup still owns the parsed gateway profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProviderDiagnosticContext {
    pub(crate) loopback_ollama: bool,
}

impl ProviderDiagnosticContext {
    pub(crate) fn from_gateway(gateway_name: &str, gateway: &Gateway) -> Self {
        let ollama_profile = gateway_name.eq_ignore_ascii_case("ollama")
            || gateway.flavor.eq_ignore_ascii_case("ollama");
        Self {
            loopback_ollama: ollama_profile && base_url_has_loopback_host(&gateway.base_url),
        }
    }

    pub(crate) fn code_for(self, error: &ProviderError) -> Option<ProviderDiagnosticCode> {
        if self.loopback_ollama && matches!(error, ProviderError::Connection(_)) {
            Some(ProviderDiagnosticCode::LocalOllamaUnreachable)
        } else {
            None
        }
    }
}

/// Recognize only URL-shaped loopback endpoints. This deliberately does not
/// resolve names or accept prefix matches such as `localhost.example.com`.
pub(crate) fn base_url_has_loopback_host(base_url: &str) -> bool {
    let Some((scheme, remainder)) = base_url.split_once("://") else {
        return false;
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return false;
    }
    let authority = remainder
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some(end) = bracketed.find(']') else {
            return false;
        };
        &bracketed[..end]
    } else {
        authority
            .rsplit_once(':')
            .filter(|(_, port)| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
            .map_or(authority, |(host, _)| host)
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

impl ProviderOperation<'_> {
    pub(crate) fn subject(self) -> &'static str {
        match self {
            Self::Chat { .. } => "chat request",
            Self::InferenceProbe { .. } => "inference probe",
            Self::ModelCatalog => "model catalog request",
        }
    }
}

pub(crate) fn safe_auth_refresh_hint(hint: Option<&str>) -> Option<String> {
    let hint = terminal_inline(hint?).trim().to_string();
    if hint.is_empty() {
        return None;
    }
    let bounded: String = hint.chars().take(240).collect();
    if refresh_hint_contains_credential(&bounded) {
        return None;
    }
    Some(bounded)
}

/// Refresh hints are configuration-controlled prose, but are routinely copied
/// from credential setup snippets. Reject the entire hint if it resembles a
/// URL, header, assignment, or common hosted-provider token. Keeping a partial
/// redaction would still disclose useful credential fragments.
pub(crate) fn refresh_hint_contains_credential(hint: &str) -> bool {
    let lowered = hint.to_ascii_lowercase();
    if [
        "://",
        "api_key",
        "api-key",
        "api key",
        "apikey",
        "authorization",
        "bearer ",
        "password",
        "secret",
        "access_token",
        "access-token",
        "access token",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
        || hint.contains("hf_")
        || hint.contains("sk-")
    {
        return true;
    }

    // Reject a standalone `token` label followed by a value separator. A
    // hyphenated command such as `refresh-token` remains safe and actionable,
    // as do environment-variable names such as `HF_TOKEN`.
    for (start, _) in lowered.match_indices("token") {
        let before = lowered[..start].chars().next_back();
        if before.is_some_and(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')) {
            continue;
        }
        let after = &lowered[start + "token".len()..];
        let mut chars = after.chars();
        match chars.next() {
            Some('=' | ':' | '"' | '\'' | '_' | '-') => return true,
            Some(ch) if ch.is_ascii_whitespace() && chars.any(|ch| !ch.is_ascii_whitespace()) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// A 400/422 says what the gateway disliked about the request, which no
/// generic remedy can. Surface only the JSON `error.message` / `message` field,
/// with URL-like words replaced, under the same bound and credential-marker
/// rejection as refresh hints; anything else stays withheld.
pub(crate) fn safe_gateway_rejection_reason(body: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    let message = body
        .pointer("/error/message")
        .or_else(|| body.get("message"))?
        .as_str()?;
    let without_urls = message
        .split_whitespace()
        .map(|word| if word.contains("://") { "<url>" } else { word })
        .collect::<Vec<_>>()
        .join(" ");
    safe_auth_refresh_hint(Some(&without_urls))
}

pub(crate) fn auth_refresh_hint_suffix(hint: Option<&str>) -> String {
    safe_auth_refresh_hint(hint)
        .map(|hint| format!("; configured refresh hint: {hint}"))
        .unwrap_or_default()
}

pub(crate) fn expired_auth_warning(since: &str, hint: Option<&str>) -> String {
    format!(
        "warning: gateway token expired (since {}) - sending anyway{}",
        terminal_inline(since),
        auth_refresh_hint_suffix(hint)
    )
}

/// One bounded formatter for every provider-facing surface. Provider-supplied
/// URL/body/detail strings are never interpolated, except the JSON `message`
/// of a 400/422 rejection, filtered by [`safe_gateway_rejection_reason`];
/// configured refresh hints are accepted only when bounded and free of
/// URL/credential-like markers.
pub(crate) fn safe_provider_error_message(
    error: &ProviderError,
    operation: ProviderOperation<'_>,
    timeout_secs: Option<u64>,
    diagnostics: ProviderDiagnosticContext,
) -> String {
    let subject = operation.subject();
    match error {
        ProviderError::Connection(_) if diagnostics.loopback_ollama => format!(
            "{subject} could not reach local Ollama at the configured loopback endpoint from this execution environment - start or verify `ollama serve`, or select another gateway"
        ),
        ProviderError::Connection(_) => format!(
            "{subject} could not reach the gateway - check the selected gateway's base_url, network access, and provider availability; run `kitsu doctor --online`"
        ),
        ProviderError::Timeout => match timeout_secs {
            Some(timeout_secs) => format!(
                "{subject} timed out after {timeout_secs}s - raise the budget with --timeout-secs <n> (e.g. --timeout-secs {}) for a slow, queued, or tool-heavy request",
                timeout_secs.saturating_mul(3).max(180)
            ),
            None => format!(
                "{subject} timed out - retry with a larger --timeout-secs budget if the gateway is slow or queued"
            ),
        },
        ProviderError::InvalidResponse(_) => format!(
            "{subject} received an invalid provider response - verify the gateway flavor and API compatibility"
        ),
        ProviderError::ResponseTooLarge { .. } => format!(
            "{subject} exceeded the provider response-size safety limit - reduce the requested output or catalog size and retry"
        ),
        ProviderError::Unauthorized { status: 403, hint } => {
            format!(
                "{subject} was forbidden (HTTP 403) - confirm the credential has inference and gated-model access{}",
                auth_refresh_hint_suffix(hint.as_deref())
            )
        }
        ProviderError::Unauthorized { status, hint } => {
            format!(
                "{subject} authentication/access was rejected (HTTP {status}) - refresh the configured credential and confirm model access{}",
                auth_refresh_hint_suffix(hint.as_deref())
            )
        }
        ProviderError::Http { status: 402, .. } => format!(
            "{subject} was rejected for billing or quota (HTTP 402) - check provider credits and billing settings"
        ),
        ProviderError::Http { status: 403, .. } => format!(
            "{subject} was forbidden (HTTP 403) - confirm the credential has inference and gated-model access"
        ),
        ProviderError::Http { status: 404, .. } => match operation {
            ProviderOperation::Chat { model } | ProviderOperation::InferenceProbe { model } => format!(
                "gateway rejected the chat route or model '{}' (HTTP 404) - verify the catalog with `kitsu models` or /models, run /model <valid-id>, and check the configured chat_path",
                terminal_inline(model)
            ),
            ProviderOperation::ModelCatalog => {
                "model catalog endpoint was not found (HTTP 404) - check models_path or disable catalog discovery".to_string()
            }
        },
        ProviderError::Http { status: 429, .. } => format!(
            "{subject} remained rate-limited after retries (HTTP 429) - wait for quota to recover or increase --timeout-secs so Retry-After can fit"
        ),
        ProviderError::Http { status, .. } if *status >= 500 => format!(
            "{subject} failed because the gateway is unavailable (HTTP {status}) - retry later or check provider status"
        ),
        ProviderError::Http { status, body } if *status == 400 || *status == 422 => format!(
            "{subject} was rejected (HTTP {status}) - verify the selected model, gateway flavor, and request compatibility{}",
            safe_gateway_rejection_reason(body)
                .map(|reason| format!("; gateway said: {reason}"))
                .unwrap_or_default()
        ),
        ProviderError::Http { status, .. } => format!(
            "{subject} was rejected (HTTP {status}) - check gateway configuration and model access"
        ),
        ProviderError::ToolsNotSupported => format!(
            "{subject} requires tool calls that this provider does not support - disable tools or choose a tool-capable model"
        ),
        ProviderError::RouteContract(ProviderRouteContractError::StaleGeneration { .. }) => {
            format!(
                "{subject} was not sent because its provider route generation is stale - retry against the current route"
            )
        }
        ProviderError::RouteContract(ProviderRouteContractError::UnsupportedCapability {
            capability,
        }) => format!(
            "{subject} was not sent because the current provider route does not support {capability:?} - choose a compatible route or disable that request feature"
        ),
        ProviderError::RouteContract(ProviderRouteContractError::RequestModelMismatch) => format!(
            "{subject} was not sent because its model does not match the immutable provider route binding - retry against the current model route"
        ),
        ProviderError::RouteContract(_) => format!(
            "{subject} was not sent because the current provider route failed local admission - retry or rebuild the route from valid configuration"
        ),
        ProviderError::Auth(kitsu_providers::AuthError::Missing { env, hint }) => {
            format!(
                "credential variable ${} is missing - set it or update the gateway auth configuration{}",
                terminal_inline(env),
                auth_refresh_hint_suffix(hint.as_deref())
            )
        }
        ProviderError::Auth(kitsu_providers::AuthError::MissingStatic) => {
            "static credential is missing - configure it or select a different auth method"
                .to_string()
        }
        ProviderError::Auth(kitsu_providers::AuthError::Malformed { env }) => {
            format!(
                "credential variable ${} is malformed - replace it with a valid token",
                terminal_inline(env)
            )
        }
        ProviderError::Auth(kitsu_providers::AuthError::MalformedManaged) => {
            "managed credential is malformed - replace it with `kitsu auth login`"
                .to_string()
        }
    }
}

/// Render an agent error for every chat mode through the same bounded provider
/// diagnostic. This deliberately never renders provider body/URL text.
pub(crate) fn chat_error_message_with_diagnostics(
    e: kitsu_core::AgentError,
    model: &str,
    timeout_secs: u64,
    diagnostics: ProviderDiagnosticContext,
) -> String {
    kitsu_render::sanitize_terminal_text(
        &map_agent_error_with_diagnostics(
            e,
            ProviderOperation::Chat { model },
            Some(timeout_secs),
            diagnostics,
        )
        .to_string(),
    )
}

/// Map an agent/provider error to the right exit code (design §6).
pub(crate) fn map_agent_error_with_diagnostics(
    e: kitsu_core::AgentError,
    operation: ProviderOperation<'_>,
    timeout_secs: Option<u64>,
    diagnostics: ProviderDiagnosticContext,
) -> CliError {
    match e {
        kitsu_core::AgentError::Provider(error) => {
            map_provider_error_with_diagnostics(error, operation, timeout_secs, diagnostics)
        }
        kitsu_core::AgentError::Interrupted(interruption) => {
            CliError::interrupted(format!("run interrupted: {interruption}"))
        }
        kitsu_core::AgentError::ToolCallDuringToolFreeFinalization => CliError::provider(
            "model returned a tool call after tools were withheld for structured finalization; no tool was executed"
                .to_string(),
        ),
        kitsu_core::AgentError::Loop(msg) => CliError::provider(format!("agent loop error: {msg}")),
        context_error @ (kitsu_core::AgentError::ContextOverflow { .. }
        | kitsu_core::AgentError::ContextWindowUnknown) => {
            CliError::user(context_error.to_string())
        }
    }
}

pub(crate) fn map_provider_error(
    error: ProviderError,
    operation: ProviderOperation<'_>,
    timeout_secs: Option<u64>,
) -> CliError {
    map_provider_error_with_diagnostics(
        error,
        operation,
        timeout_secs,
        ProviderDiagnosticContext::default(),
    )
}

pub(crate) fn map_provider_error_with_diagnostics(
    error: ProviderError,
    operation: ProviderOperation<'_>,
    timeout_secs: Option<u64>,
    diagnostics: ProviderDiagnosticContext,
) -> CliError {
    let user_error = matches!(
        error,
        ProviderError::Auth(_) | ProviderError::RouteContract(_)
    );
    let diagnostic = diagnostics.code_for(&error);
    let message = safe_provider_error_message(&error, operation, timeout_secs, diagnostics);
    if user_error {
        CliError::user(message)
    } else {
        CliError::provider(message).with_diagnostic(diagnostic)
    }
}
