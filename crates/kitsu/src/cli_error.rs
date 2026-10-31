//! CLI errors and exit classifications.

use crate::process_exit::{EXIT_CANCELLED, EXIT_PROVIDER, EXIT_USER};
use kitsu_tools::RegistryError;

/// Top-level error carrying the right exit code.
pub(crate) struct CliError {
    pub(crate) message: String,
    pub(crate) code: i32,
    pub(crate) diagnostic: Option<ProviderDiagnosticCode>,
    pub(crate) outcome_class: kitsu_core::RunOutcomeClass,
}

impl CliError {
    pub(crate) fn user(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_USER,
            diagnostic: None,
            outcome_class: kitsu_core::RunOutcomeClass::UserFault,
        }
    }
    pub(crate) fn provider(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_PROVIDER,
            diagnostic: None,
            outcome_class: kitsu_core::RunOutcomeClass::ProviderFault,
        }
    }
    pub(crate) fn cancelled(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_CANCELLED,
            diagnostic: None,
            outcome_class: kitsu_core::RunOutcomeClass::Cancelled,
        }
    }

    pub(crate) fn tool(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_USER,
            diagnostic: None,
            outcome_class: kitsu_core::RunOutcomeClass::ToolFault,
        }
    }

    pub(crate) fn interrupted(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_USER,
            diagnostic: None,
            outcome_class: kitsu_core::RunOutcomeClass::Interrupted,
        }
    }

    pub(crate) fn with_diagnostic(mut self, diagnostic: Option<ProviderDiagnosticCode>) -> Self {
        self.diagnostic = diagnostic;
        self
    }

    pub(crate) fn jsonl_output(error: kitsu_render::jsonl::JsonlEmitError) -> Self {
        Self::user(format!("JSONL output failed: {error}"))
    }
    pub(crate) fn exit_code(&self) -> i32 {
        self.code
    }

    pub(crate) fn outcome_class(&self) -> kitsu_core::RunOutcomeClass {
        self.outcome_class
    }

    pub(crate) fn jsonl_kind(&self) -> &'static str {
        match self.code {
            EXIT_PROVIDER => "provider",
            EXIT_CANCELLED => "cancelled",
            _ => "user",
        }
    }

    pub(crate) fn jsonl_error_payload(&self) -> serde_json::Value {
        let mut payload = serde_json::json!({
            "kind": self.jsonl_kind(),
            "message": self.to_string(),
            "exit_code": self.exit_code(),
        });
        if let Some(diagnostic) = self.diagnostic {
            payload["diagnostic_code"] = serde_json::json!(diagnostic.code());
            payload["recovery_actions"] = serde_json::json!(diagnostic.recovery_actions());
        }
        payload
    }
}

impl From<RegistryError> for CliError {
    fn from(error: RegistryError) -> Self {
        Self::user(format!("tool registry composition failed: {error}"))
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", kitsu_render::sanitize_terminal_text(&self.message))
    }
}

/// Stable, secret-free machine diagnostics for provider failures whose cause
/// Kitsu can determine from trusted configuration. These codes supplement the
/// human message; they never contain provider text or endpoint components.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderDiagnosticCode {
    LocalOllamaUnreachable,
}

impl ProviderDiagnosticCode {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::LocalOllamaUnreachable => "local_ollama_unreachable",
        }
    }

    pub(crate) const fn recovery_actions(self) -> &'static [&'static str] {
        match self {
            Self::LocalOllamaUnreachable => &[
                "start_ollama_serve",
                "verify_ollama_serve",
                "select_another_gateway",
            ],
        }
    }
}
