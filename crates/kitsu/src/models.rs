//! Model catalogs and catalog telemetry.

use crate::cli::Cli;
use crate::config::Config;
use kitsu_providers::compiled::CompiledProvider;
use kitsu_providers::{
    Provider, ProviderAttemptEvent, ProviderError, ProviderRouteCurrent, ProviderTelemetrySink,
};
use std::time::Duration;

use crate::chat::commands::SlashResult;
use crate::cli_error::CliError;
use crate::cli_runtime::emit_jsonl;
use crate::cli_runtime::emit_jsonl_summary;
use crate::cli_runtime::record_successful_operation;
use crate::management::managed_credential;
use crate::management::safe_config_error;
use crate::presentation::shorten_home;
use crate::presentation::terminal_inline;
use crate::provider_diagnostics::map_provider_error_with_diagnostics;
use crate::provider_diagnostics::safe_provider_error_message;
use crate::provider_diagnostics::ProviderDiagnosticContext;
use crate::provider_diagnostics::ProviderOperation;
use crate::runtime_state::runtime_state_dir_read_only;
use crate::setup::build_auth;
use crate::setup::build_catalog_management_route;
use crate::setup::current_provider_auth_source;
use crate::setup::require_auth_ready;
use crate::setup::route_build_cli_error;
use crate::setup::route_model_selection;
use crate::turn::await_cancellable;

pub(crate) fn normalize_model_catalog(models: Vec<String>) -> Vec<String> {
    let mut normalized: Vec<String> = models
        .into_iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

/// Match the selected model against an advertised catalog without broadening
/// model-ID semantics for generic OpenAI-compatible gateways. Local Ollama is
/// the sole exception: its catalog canonicalizes an untagged `name` as
/// `name:latest`, while its inference route accepts the untagged name.
pub(crate) fn model_catalog_advertises_selected_model(
    selected_model: &str,
    models: &[String],
    diagnostics: ProviderDiagnosticContext,
) -> bool {
    let selected_model = selected_model.trim();
    if selected_model.is_empty() {
        return false;
    }
    if models
        .iter()
        .any(|candidate| candidate.trim() == selected_model)
    {
        return true;
    }
    if !diagnostics.loopback_ollama || ollama_model_id_has_explicit_version(selected_model) {
        return false;
    }

    let implicit_latest = format!("{selected_model}:latest");
    models
        .iter()
        .filter(|candidate| candidate.trim() == implicit_latest)
        .take(2)
        .count()
        == 1
}

/// A registry port in an Ollama model name is not a tag. Tags live in the
/// final path component; digest-qualified IDs are also explicit and therefore
/// remain exact-only.
pub(crate) fn ollama_model_id_has_explicit_version(model: &str) -> bool {
    model.contains('@')
        || model
            .rsplit('/')
            .next()
            .is_some_and(|component| component.contains(':'))
}

pub(crate) fn filter_model_catalog(models: Vec<String>, filter: &str) -> Vec<String> {
    let filter = filter.trim().to_lowercase();
    normalize_model_catalog(models)
        .into_iter()
        .filter(|model| filter.is_empty() || model.to_lowercase().contains(&filter))
        .collect()
}

/// Return a bounded structural reason for malformed model catalogs. Other
/// invalid-response details remain hidden because provider-controlled text can
/// contain response bodies or credentials.
pub(crate) fn bounded_model_catalog_reason(error: &ProviderError) -> Option<String> {
    let ProviderError::InvalidResponse(detail) = error else {
        return None;
    };
    let detail = detail.strip_prefix("malformed /models JSON:")?.trim();
    let detail = terminal_inline(detail);
    if detail.is_empty() {
        return None;
    }
    let mut bounded: String = detail.chars().take(240).collect();
    if detail.chars().count() > 240 {
        bounded.push('…');
    }
    Some(bounded)
}

pub(crate) fn empty_model_catalog_message(filter: &str) -> String {
    let filter = filter.trim();
    if filter.is_empty() {
        "no available models — check gateway connection or model availability".to_string()
    } else {
        format!(
            "no models matched '{}' — retry without a filter to see the full catalog",
            terminal_inline(filter)
        )
    }
}

pub(crate) const HUMAN_MODEL_CATALOG_CAP: usize = 12;
pub(crate) const MODEL_CATALOG_ROUND: u32 = 1;
pub(crate) const TOP_LEVEL_MODEL_CATALOG_OPERATION_ID: &str = "models:1";

pub(crate) fn model_catalog_attempt_payload(
    route_generation: Option<kitsu_providers::ProviderRouteGeneration>,
    event: ProviderAttemptEvent,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "round": MODEL_CATALOG_ROUND,
        "purpose": "model_catalog",
        "attempt": event.attempt,
        "max_attempts": event.max_attempts,
        "category": event.category,
        "status": event.status,
        "outcome": event.outcome,
        "terminal": event.terminal,
        "retry_delay_ms": event.retry_delay_ms,
        "retry_after_used": event.retry_after_used,
        "deadline_scope": "provider_request",
        "remaining_deadline_ms": event.remaining_deadline_ms,
    });
    if let Some(generation) = route_generation {
        payload["route_generation"] = serde_json::json!(generation.get());
    }
    payload
}

pub(crate) fn top_level_model_catalog_telemetry() -> ProviderTelemetrySink {
    ProviderTelemetrySink::with_fallible_context_observer(|observation| {
        let mut payload =
            model_catalog_attempt_payload(observation.route_generation, observation.event);
        payload["operation_id"] = serde_json::json!(TOP_LEVEL_MODEL_CATALOG_OPERATION_ID);
        emit_jsonl("provider_attempt", payload).is_ok()
    })
}

pub(crate) fn format_model_catalog_preview(
    models: &[String],
    filter: &str,
    current_model: Option<&str>,
    narrow_hint: &str,
    switch_hint: &str,
) -> String {
    let filter = filter.trim();
    let current_model = current_model
        .map(str::trim)
        .filter(|model| !model.is_empty());
    let current_note = current_model
        .map(|model| format!(" (current: {})", terminal_inline(model)))
        .unwrap_or_default();
    let mut output = if filter.is_empty() {
        format!("~ {} model(s) available{current_note}:\n", models.len())
    } else {
        format!(
            "~ {} model(s) matching '{}'{current_note}:\n",
            models.len(),
            terminal_inline(filter)
        )
    };

    let mut displayed = Vec::with_capacity(HUMAN_MODEL_CATALOG_CAP.min(models.len()));
    if let Some(current) = current_model {
        if let Some(model) = models.iter().find(|model| model.as_str() == current) {
            displayed.push(model.as_str());
        }
    }
    let remaining = HUMAN_MODEL_CATALOG_CAP.saturating_sub(displayed.len());
    displayed.extend(
        models
            .iter()
            .map(String::as_str)
            .filter(|model| Some(*model) != current_model)
            .take(remaining),
    );

    for model in &displayed {
        if Some(*model) == current_model {
            output.push_str(&format!("  * {} (current)\n", terminal_inline(model)));
        } else {
            output.push_str(&format!("    {}\n", terminal_inline(model)));
        }
    }
    if models.len() > displayed.len() {
        output.push_str(&format!(
            "    ... and {} more - narrow with {narrow_hint}\n",
            models.len() - displayed.len(),
        ));
    }
    if models.is_empty() {
        output.push_str(&empty_model_catalog_message(filter));
        output.push('\n');
    } else {
        output.push_str(switch_hint);
        output.push('\n');
    }
    output
}

pub(crate) async fn run_models(cli: &Cli, filter: &str) -> Result<(), CliError> {
    let path = Config::resolve_path(cli.config.as_deref());
    if !path.exists() {
        return Err(CliError::user(format!(
            "config error: no config found at {}",
            shorten_home(&path)
        )));
    }
    let config = Config::load(&path).map_err(|error| CliError::user(safe_config_error(&error)))?;
    let (requested_gateway, requested_model) = route_model_selection(cli, &config)?;
    let (gateway_name, gateway) = config
        .select_gateway(requested_gateway)
        .map_err(|error| CliError::user(error.to_string()))?;
    let provider_diagnostics = ProviderDiagnosticContext::from_gateway(gateway_name, gateway);
    gateway
        .validate_flavor()
        .map_err(|error| CliError::user(error.to_string()))?;
    // Catalog discovery remains usable without a configured model. When one
    // does resolve, human TTY output can orient the user by pinning it first.
    let current_model = gateway.resolve_model(requested_model.as_deref()).ok();
    let state_dir = runtime_state_dir_read_only(&path);
    let managed_token = managed_credential(&state_dir, gateway_name, gateway)?;
    let auth_source = current_provider_auth_source(gateway, managed_token.is_some());
    let auth = build_auth(gateway, gateway_name, managed_token)
        .map_err(|error| CliError::user(error.to_string()))?;
    require_auth_ready(auth.as_ref(), ProviderOperation::ModelCatalog)?;
    let models_path = gateway
        .models_path
        .clone()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            CliError::user(
                "this gateway does not expose a model catalog; configure models_path".to_string(),
            )
        })?;
    let provider = build_catalog_management_route(
        &path,
        gateway_name,
        gateway,
        models_path,
        auth_source,
        auth.as_ref(),
        Duration::from_secs(cli.timeout_secs),
    )
    .map_err(|error| {
        route_build_cli_error(error, ProviderOperation::ModelCatalog, provider_diagnostics)
    })?;
    if cli.jsonl {
        emit_jsonl(
            "run_started",
            serde_json::json!({
                "mode": "models",
                "gateway": gateway_name,
                "provider_route": provider.receipt(),
            }),
        )?;
        emit_jsonl(
            "provider_request",
            serde_json::json!({
                "operation_id": TOP_LEVEL_MODEL_CATALOG_OPERATION_ID,
                "round": MODEL_CATALOG_ROUND,
                "purpose": "model_catalog",
                "message_count": 0,
                "tools_offered": 0,
                "structured_output": false,
                "request_timeout_ms": cli.timeout_secs.saturating_mul(1_000),
            }),
        )?;
    }
    let telemetry = cli.jsonl.then(top_level_model_catalog_telemetry);
    let result = await_cancellable(provider.list_models_with_telemetry(telemetry))
        .await
        .ok_or_else(|| CliError::cancelled("model catalog request cancelled by Ctrl+C"))?;
    let Some(models) = result.map_err(|error| {
        map_provider_error_with_diagnostics(
            error,
            ProviderOperation::ModelCatalog,
            Some(cli.timeout_secs),
            provider_diagnostics,
        )
    })?
    else {
        return Err(CliError::user(
            "this gateway does not expose a model catalog; configure models_path".to_string(),
        ));
    };
    let matched = filter_model_catalog(models, filter);
    record_successful_operation();
    if cli.jsonl {
        let count = matched.len();
        emit_jsonl(
            "model_catalog",
            serde_json::json!({"filter": filter.trim(), "models": matched, "count": count}),
        )?;
        emit_jsonl_summary("ok", 0, 0)?;
    } else if matched.is_empty() {
        eprintln!("{}", empty_model_catalog_message(filter));
    } else if kitsu_render::is_tty() {
        print!(
            "{}",
            format_model_catalog_preview(
                &matched,
                filter,
                current_model.as_deref(),
                "`kitsu models <substring>`",
                "select with: kitsu --model <id> (or /model in chat)",
            )
        );
    } else {
        for model in matched {
            println!("{}", terminal_inline(&model));
        }
    }
    Ok(())
}

/// Fetch and print the models the gateway advertises (via its `models_path`),
/// optionally filtered by a case-insensitive substring. Output goes to stderr
/// like other slash commands. The listed IDs are what you pass to `/model <id>`
/// or `--model` — handy for exploring a big catalog (e.g. Hugging Face).
pub(crate) async fn list_models_and_print(
    provider: &ProviderRouteCurrent<CompiledProvider>,
    filter: &str,
    current_model: &str,
    runtime_log: Option<&kitsu_core::AgentEventLog>,
    request_timeout_ms: u64,
) -> Result<Option<Vec<String>>, ProviderError> {
    match list_models_with_runtime_telemetry(provider, runtime_log, request_timeout_ms).await {
        Ok(Some(models)) => {
            let filter = filter.trim();
            let matched = filter_model_catalog(models, filter);
            eprint!(
                "{}",
                format_model_catalog_preview(
                    &matched,
                    filter,
                    Some(current_model),
                    "`/models <substring>`",
                    "switch with: /model <id>",
                )
            );
            // A zero-match search is a successful catalog query, just like the
            // standalone `kitsu models <filter>` command. Only an unavailable
            // catalog returns false.
            Ok(Some(matched))
        }
        Ok(None) => {
            eprintln!("this gateway does not expose a model list (its models_path is empty)");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn runtime_model_catalog_telemetry(
    runtime_log: Option<&kitsu_core::AgentEventLog>,
) -> Option<ProviderTelemetrySink> {
    runtime_log.map(|runtime_log| {
        let runtime_log = runtime_log.clone();
        ProviderTelemetrySink::with_fallible_context_observer(move |observation| {
            let event = observation.event;
            runtime_log.record(kitsu_core::AgentRuntimeEvent::ProviderAttempt {
                round: MODEL_CATALOG_ROUND,
                purpose: kitsu_core::ProviderRequestPurpose::ModelCatalog,
                route_generation: observation
                    .route_generation
                    .map(|generation| generation.get()),
                attempt: event.attempt,
                max_attempts: event.max_attempts,
                category: event.category,
                status: event.status,
                outcome: event.outcome,
                terminal: event.terminal,
                retry_delay_ms: event.retry_delay_ms,
                retry_after_used: event.retry_after_used,
                deadline_scope: kitsu_core::ProviderDeadlineScope::ProviderRequest,
                remaining_deadline_ms: event.remaining_deadline_ms,
                turn_remaining_ms: None,
            });
            !runtime_log.abort_requested()
        })
    })
}

pub(crate) async fn list_models_with_runtime_telemetry(
    provider: &ProviderRouteCurrent<CompiledProvider>,
    runtime_log: Option<&kitsu_core::AgentEventLog>,
    request_timeout_ms: u64,
) -> Result<Option<Vec<String>>, ProviderError> {
    if let Some(runtime_log) = runtime_log {
        runtime_log.record(kitsu_core::AgentRuntimeEvent::ProviderRequest {
            round: MODEL_CATALOG_ROUND,
            purpose: kitsu_core::ProviderRequestPurpose::ModelCatalog,
            model: None,
            message_count: 0,
            tools_offered: 0,
            structured_output: false,
            request_timeout_ms: Some(request_timeout_ms),
            turn_remaining_ms: None,
        });
        if runtime_log.abort_requested() {
            return Err(ProviderError::Connection(
                "runtime event sink stopped the model catalog request".to_string(),
            ));
        }
    }
    provider
        .list_models_with_telemetry(runtime_model_catalog_telemetry(runtime_log))
        .await
}

pub(crate) fn report_slash_provider_error(
    prefix: &str,
    error: ProviderError,
    operation: ProviderOperation<'_>,
    timeout_secs: u64,
    diagnostics: ProviderDiagnosticContext,
) -> SlashResult {
    let user_error = matches!(
        error,
        ProviderError::Auth(_) | ProviderError::RouteContract(_)
    );
    let message = safe_provider_error_message(&error, operation, Some(timeout_secs), diagnostics);
    eprintln!("{}: {}", terminal_inline(prefix), message);
    if user_error {
        SlashResult::Error
    } else {
        SlashResult::ProviderFailure(message)
    }
}

pub(crate) fn finish_slash_model_catalog(
    result: Result<Option<Vec<String>>, ProviderError>,
    timeout_secs: u64,
    diagnostics: ProviderDiagnosticContext,
) -> Result<Vec<String>, SlashResult> {
    match result {
        Ok(Some(models)) => Ok(models),
        Ok(None) => Err(SlashResult::Error),
        Err(error) => Err(report_slash_provider_error(
            "could not fetch models",
            error,
            ProviderOperation::ModelCatalog,
            timeout_secs,
            diagnostics,
        )),
    }
}
