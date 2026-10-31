//! Offline and explicitly requested online diagnostics.

use crate::cli::Cli;
use crate::config::Config;
use kitsu_core::context::DEFAULT_CONTEXT_WINDOW;
use kitsu_core::ArtifactStoragePolicy;
use kitsu_providers::compiled::CompiledProvider;
use kitsu_providers::{
    ChatDelta, ChatMessage, ChatRequest, Provider, ProviderError, ProviderRouteHandle,
};
use std::time::Duration;

use crate::cli;
use crate::cli_error::CliError;
use crate::context_window::context_window_cache_key;
use crate::context_window::context_window_cache_path;
use crate::context_window::read_cached_context_window;
use crate::context_window::redact_endpoint;
use crate::llms;
use crate::management::config_resolution_source;
use crate::management::environment_override_present;
use crate::management::managed_credential;
use crate::management::print_json;
use crate::management::reject_jsonl_for_management;
use crate::management::runtime_state_diagnostic;
use crate::management::safe_config_error;
use crate::models::bounded_model_catalog_reason;
use crate::models::model_catalog_advertises_selected_model;
use crate::models::normalize_model_catalog;
use crate::presentation::shorten_home;
use crate::presentation::terminal_inline;
use crate::provider_diagnostics::auth_refresh_hint_suffix;
use crate::provider_diagnostics::safe_auth_refresh_hint;
use crate::provider_diagnostics::safe_provider_error_message;
use crate::provider_diagnostics::ProviderDiagnosticContext;
use crate::provider_diagnostics::ProviderOperation;
use crate::runtime_state::runtime_state_dir_read_only;
use crate::setup::artifact_policy_receipt;
use crate::setup::build_auth;
use crate::setup::build_catalog_management_route;
use crate::setup::build_inference_management_route;
use crate::setup::current_provider_auth_source;
use crate::setup::project_context_disabled;
use crate::setup::route_build_cli_error;
use crate::setup::ArtifactPolicySource;
use crate::turn::await_cancellable;

pub(crate) fn doctor_value<'a>(report: &'a serde_json::Value, pointer: &str) -> &'a str {
    report
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unavailable")
}

pub(crate) fn doctor_text(report: &serde_json::Value, pointer: &str) -> String {
    terminal_inline(doctor_value(report, pointer))
}

pub(crate) fn print_doctor_human(report: &serde_json::Value) {
    let config_mark = if report
        .pointer("/config/parse_ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        "ok"
    } else {
        "FAIL"
    };
    println!("kitsu {}", doctor_text(report, "/version"));
    println!(
        "[{config_mark}] config ({}) {} — {}",
        doctor_text(report, "/config/source"),
        doctor_text(report, "/config/path_display"),
        doctor_text(report, "/config/status"),
    );
    println!(
        "[info] gateway/model: {}/{}",
        doctor_text(report, "/gateway/name"),
        doctor_text(report, "/gateway/model"),
    );
    println!("[info] auth: {}", doctor_text(report, "/auth/status"),);
    if doctor_value(report, "/provider_route/status") == "bound" {
        println!(
            "[info] provider route: {} / {} / generation {} / {}",
            doctor_text(report, "/provider_route/receipt/route/adapter"),
            doctor_text(report, "/provider_route/receipt/route/wire_revision"),
            report
                .pointer("/provider_route/receipt/route/generation")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unavailable".to_string()),
            doctor_text(report, "/provider_route/receipt/route/profile"),
        );
    } else {
        println!(
            "[info] provider route: {}",
            doctor_text(report, "/provider_route/status"),
        );
    }
    println!(
        "[info] context window: {} tokens ({})",
        report
            .pointer("/context/window")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unavailable".to_string()),
        doctor_text(report, "/context/source"),
    );
    println!(
        "[info] project context: {}",
        doctor_text(report, "/project_context/status"),
    );
    let state_mark = if report
        .pointer("/runtime_state/writable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        "ok"
    } else {
        "WARN"
    };
    println!(
        "[{state_mark}] runtime state: {} — {}",
        doctor_text(report, "/runtime_state/path_display"),
        doctor_text(report, "/runtime_state/status"),
    );
    let online_status = doctor_text(report, "/online/status");
    let online_scope_note = match (
        report
            .pointer("/online/requested")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        report
            .pointer("/online/scope")
            .and_then(serde_json::Value::as_str),
        report
            .pointer("/online/status")
            .and_then(serde_json::Value::as_str),
        report
            .pointer("/online/chat_route_probed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    ) {
        (true, Some("model_catalog_only"), _, false) => {
            let models = report
                .pointer("/online/models")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            format!(
                "reachable — model catalog only; chat/inference access was not tested ({models} models in catalog)"
            )
        }
        (true, Some("chat_inference"), Some("reachable"), true) => {
            format!("{online_status} — chat/inference accepted one minimal tool-free completion")
        }
        _ => online_status,
    };
    println!("[info] provider check: {online_scope_note}");
    println!(
        "[info] tools: filesystem={} commands={}",
        doctor_text(report, "/tool_policy/filesystem"),
        doctor_text(report, "/tool_policy/commands"),
    );
    println!(
        "[info] automatic artifacts: {} ({}) — oversized tool output {}",
        doctor_text(report, "/artifact_policy/mode"),
        doctor_text(report, "/artifact_policy/source"),
        if report
            .pointer("/artifact_policy/tool_result_offload")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            "may be offloaded"
        } else {
            "is bounded inline"
        },
    );
    let issues = report
        .pointer("/blocking_issues")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if issues.is_empty()
        && report
            .pointer("/online/scope")
            .and_then(serde_json::Value::as_str)
            == Some("chat_inference")
        && report
            .pointer("/online/status")
            .and_then(serde_json::Value::as_str)
            == Some("reachable")
    {
        println!(
            "doctor: inference probe succeeded — the configured chat route accepted this model"
        );
    } else if issues.is_empty() {
        // Doctor never sends a chat completion: even an authenticated catalog
        // success cannot prove inference permission. Name both the clean scope
        // and the next verification step without claiming the user is ready.
        println!("doctor: no blocking issues found — run `kitsu` to verify chat/inference access");
    } else {
        println!("doctor: {} blocking issue(s)", issues.len());
        for issue in issues {
            if let Some(issue) = issue.as_str() {
                println!("  - {}", terminal_inline(issue));
            }
        }
    }
}

pub(crate) async fn minimal_inference_probe(
    provider: &ProviderRouteHandle<CompiledProvider>,
    model: &str,
) -> Result<(), ProviderError> {
    use futures::StreamExt as _;

    let request = ChatRequest {
        model: model.to_string(),
        messages: vec![ChatMessage::user("Reply OK.")],
        tools: None,
        // Native Messages is streaming-only. Choose from the immutable route
        // identity, not the mutable gateway label, so this auxiliary request
        // cannot drift onto a different wire than the route it probes.
        stream: provider.identity().adapter()
            == kitsu_providers::ProviderAdapterId::AnthropicMessages,
        temperature: None,
        reasoning_effort: None,
        max_tokens: Some(1),
        response_format: None,
        prompt_cache_key: None,
    };
    let mut response = provider.chat_stream(request).await?;
    let mut finished = false;
    while let Some(delta) = response.next().await {
        if matches!(delta?, ChatDelta::Done(_)) {
            finished = true;
        }
    }
    if finished {
        Ok(())
    } else {
        Err(ProviderError::InvalidResponse(
            "inference probe ended without a terminal marker".to_string(),
        ))
    }
}

pub(crate) async fn run_doctor(
    cli: &Cli,
    online: bool,
    inference: bool,
    json: bool,
) -> Result<(), CliError> {
    reject_jsonl_for_management(cli, "kitsu doctor --json")?;
    let config_path = Config::resolve_path(cli.config.as_deref());
    let config_exists = std::fs::symlink_metadata(&config_path).is_ok();
    let config_is_file = config_path.is_file();
    let state_dir = runtime_state_dir_read_only(&config_path);
    let mut runtime_state = runtime_state_diagnostic(&state_dir);
    if let Some(object) = runtime_state.as_object_mut() {
        object.insert(
            "path_display".to_string(),
            serde_json::Value::String(shorten_home(&state_dir)),
        );
    }
    let mut issues = Vec::<String>::new();
    let mut gateway_report = serde_json::json!({
        "name": "unavailable",
        "model": "unavailable",
        "flavor": "unavailable",
        "endpoint": "unavailable",
    });
    let mut auth_report = serde_json::json!({
        "method": "unavailable",
        "credential_present": null,
        "credential_required": null,
        "recovery_hint": null,
        "status": "unavailable until config and gateway are valid",
    });
    let cache_path = context_window_cache_path(&state_dir);
    let mut context_report = serde_json::json!({
        "window": DEFAULT_CONTEXT_WINDOW,
        "source": "conservative default",
        "cache_path": cache_path,
    });
    let project_source = std::env::current_dir()
        .ok()
        .and_then(|directory| llms::discover(&directory, None));
    let project_report = if project_context_disabled(cli) {
        serde_json::json!({"enabled": false, "status": "disabled by flag or environment"})
    } else if let Some(source) = project_source {
        serde_json::json!({
            "enabled": true,
            "status": format!("will load {}", shorten_home(&source)),
            "source": source,
        })
    } else {
        serde_json::json!({"enabled": true, "status": "no llms.txt, AGENTS.md, or CLAUDE.md found"})
    };
    let provider_check_requested = online || inference;
    let provider_check_scope = if inference {
        "chat_inference"
    } else {
        "model_catalog_only"
    };
    let mut provider_route_report = serde_json::json!({
        "status": if provider_check_requested {
            "not_constructed"
        } else {
            "not_constructed_offline"
        },
        "receipt": null,
    });
    let mut online_report = serde_json::json!({
        "requested": provider_check_requested,
        "scope": provider_check_scope,
        "chat_route_probed": false,
        "potentially_billable": inference,
        "request_sent": false,
        "status": if provider_check_requested { "not run" } else { "not requested (offline default)" },
    });

    let config = if !config_exists {
        issues.push(format!(
            "config does not exist at {} (run kitsu init)",
            config_path.display()
        ));
        None
    } else if !config_is_file {
        issues.push(format!(
            "config path {} is not a readable file",
            config_path.display()
        ));
        None
    } else {
        match Config::load(&config_path) {
            Ok(config) => Some(config),
            Err(error) => {
                issues.push(safe_config_error(&error));
                None
            }
        }
    };
    let parse_ok = config.is_some();
    let config_status = if !config_exists {
        "missing"
    } else if parse_ok {
        "parsed successfully"
    } else {
        "parse/read failed"
    };

    if let Some(config) = config.as_ref() {
        match config.select_gateway(cli.gateway.as_deref()) {
            Ok((gateway_name, gateway)) => {
                let provider_diagnostics =
                    ProviderDiagnosticContext::from_gateway(gateway_name, gateway);
                let flavor_ok = match gateway.validate_flavor() {
                    Ok(()) => true,
                    Err(error) => {
                        issues.push(error.to_string());
                        false
                    }
                };
                let model = match gateway.resolve_model(cli.model.as_deref()) {
                    Ok(model) => Some(model),
                    Err(error) => {
                        issues.push(error.to_string());
                        None
                    }
                };
                gateway_report = serde_json::json!({
                    "name": gateway_name,
                    "model": model.as_deref().unwrap_or("unavailable"),
                    "flavor": gateway.flavor,
                    "flavor_valid": flavor_ok,
                    "endpoint": redact_endpoint(&format!("{}{}", gateway.base_url, gateway.chat_path)),
                    "models_path_configured": gateway.models_path.as_deref().is_some_and(|path| !path.is_empty()),
                });

                let auth_valid = match gateway.validate_auth() {
                    Ok(()) => true,
                    Err(error) => {
                        issues.push(error.to_string());
                        false
                    }
                };
                let managed_token = if auth_valid && gateway.auth.method == "bearer-env" {
                    match managed_credential(&state_dir, gateway_name, gateway) {
                        Ok(token) => token,
                        Err(error) => {
                            issues.push(error.to_string());
                            None
                        }
                    }
                } else {
                    None
                };
                let environment_present = environment_override_present(gateway);
                let managed_present = managed_token.is_some();
                let (credential_present, auth_status) = match gateway.auth.method.as_str() {
                    "none" => (None, "no credential required".to_string()),
                    "bearer-env" => {
                        let present = environment_present || managed_present;
                        if auth_valid && !present {
                            issues.push(format!(
                                "no usable credential is available from {} or `kitsu auth login`{}",
                                gateway.auth.env,
                                auth_refresh_hint_suffix(gateway.auth.refresh_hint.as_deref())
                            ));
                        }
                        (
                            Some(present),
                            format!(
                                "bearer credential is {} (source: {})",
                                if present { "present" } else { "missing" },
                                if environment_present {
                                    "environment"
                                } else if managed_present {
                                    "managed"
                                } else {
                                    "missing"
                                }
                            ),
                        )
                    }
                    "static" => {
                        let present = gateway
                            .auth
                            .token
                            .as_deref()
                            .is_some_and(|token| !token.trim().is_empty());
                        (
                            Some(present),
                            if present {
                                "static credential is configured (value hidden)".to_string()
                            } else {
                                "static credential is missing".to_string()
                            },
                        )
                    }
                    other => (Some(false), format!("unsupported auth method {other}")),
                };
                let auth_locally_usable = auth_valid
                    && build_auth(gateway, gateway_name, managed_token.clone())
                        .is_ok_and(|auth| auth.header().is_ok());
                if auth_valid && credential_present != Some(false) && !auth_locally_usable {
                    issues.push(
                        "the effective credential is present but malformed; replace the environment value or run `kitsu auth login`"
                            .to_string(),
                    );
                }
                let recovery_hint = (credential_present == Some(false))
                    .then(|| safe_auth_refresh_hint(gateway.auth.refresh_hint.as_deref()))
                    .flatten();
                auth_report = serde_json::json!({
                    "method": gateway.auth.method,
                    "env": if gateway.auth.method == "bearer-env" {
                        Some(gateway.auth.env.as_str())
                    } else {
                        None
                    },
                    "credential_present": credential_present,
                    "credential_required": gateway.auth.method != "none",
                    "environment_override_present": environment_present,
                    "managed_credential_present": managed_present,
                    "effective_source": if gateway.auth.method == "bearer-env" {
                        if environment_present { "environment" } else if managed_present { "managed" } else { "missing" }
                    } else if gateway.auth.method == "static" { "static" } else { "none" },
                    "recovery_hint": recovery_hint,
                    "valid": auth_valid,
                    "locally_usable": auth_locally_usable,
                    "status": auth_status,
                });

                if let Some(model) = model.as_deref() {
                    let (window, source) = if let Some(window) = cli.context_window {
                        (window, "--context-window")
                    } else if let Some(window) = gateway.context_window {
                        (window, "gateway profile")
                    } else {
                        let key = context_window_cache_key(
                            &gateway.base_url,
                            gateway.models_path.as_deref(),
                            model,
                        );
                        match read_cached_context_window(&cache_path, &key) {
                            Some(window) => (window, "fresh discovery cache"),
                            None => (DEFAULT_CONTEXT_WINDOW, "conservative default"),
                        }
                    };
                    context_report = serde_json::json!({
                        "window": window,
                        "source": source,
                        "cache_path": cache_path,
                    });
                }

                if inference {
                    if !flavor_ok
                        || !auth_valid
                        || !auth_locally_usable
                        || credential_present == Some(false)
                        || model.is_none()
                    {
                        let issue = "inference provider check was requested but skipped: fix gateway, model, or auth diagnostics first".to_string();
                        issues.push(issue.clone());
                        online_report = serde_json::json!({
                            "requested": true,
                            "scope": "chat_inference",
                            "chat_route_probed": false,
                            "potentially_billable": true,
                            "request_sent": false,
                            "status": issue,
                        });
                    } else {
                        let auth_source =
                            current_provider_auth_source(gateway, managed_token.is_some());
                        let auth = build_auth(gateway, gateway_name, managed_token.clone())
                            .map_err(|error| CliError::user(safe_config_error(&error)))?;
                        let provider = build_inference_management_route(
                            &config_path,
                            gateway_name,
                            gateway,
                            model.clone().expect("validated above"),
                            auth_source,
                            auth.as_ref(),
                            Duration::from_secs(cli.timeout_secs),
                        )
                        .map_err(|error| {
                            route_build_cli_error(
                                error,
                                ProviderOperation::InferenceProbe {
                                    model: model.as_deref().expect("validated above"),
                                },
                                provider_diagnostics,
                            )
                        })?;
                        provider_route_report = serde_json::json!({
                            "status": "bound",
                            "receipt": provider.receipt(),
                        });
                        let effective_model = model.as_deref().expect("validated above");
                        match await_cancellable(minimal_inference_probe(&provider, effective_model))
                            .await
                        {
                            Some(Ok(())) => {
                                online_report = serde_json::json!({
                                    "requested": true,
                                    "scope": "chat_inference",
                                    "chat_route_probed": true,
                                    "potentially_billable": true,
                                    "request_sent": true,
                                    "status": "reachable",
                                    "effective_model": effective_model,
                                });
                            }
                            Some(Err(error)) => {
                                let safe = safe_provider_error_message(
                                    &error,
                                    ProviderOperation::InferenceProbe {
                                        model: effective_model,
                                    },
                                    Some(cli.timeout_secs),
                                    provider_diagnostics,
                                );
                                issues.push(format!("inference provider check failed: {safe}"));
                                online_report = serde_json::json!({
                                    "requested": true,
                                    "scope": "chat_inference",
                                    "chat_route_probed": true,
                                    "potentially_billable": true,
                                    "request_sent": true,
                                    "status": safe,
                                    "effective_model": effective_model,
                                });
                            }
                            None => {
                                return Err(CliError::cancelled(
                                    "doctor inference probe cancelled by Ctrl+C",
                                ));
                            }
                        }
                    }
                } else if online {
                    let models_path = gateway.models_path.clone().filter(|path| !path.is_empty());
                    match models_path {
                        None => {
                            let issue = "online provider check was requested but skipped: no models_path is configured for a safe read-only probe".to_string();
                            issues.push(issue.clone());
                            online_report = serde_json::json!({
                                "requested": true,
                                "scope": "model_catalog_only",
                                "chat_route_probed": false,
                                "potentially_billable": false,
                                "request_sent": false,
                                "status": issue,
                            });
                        }
                        Some(_)
                            if !flavor_ok
                                || !auth_valid
                                || !auth_locally_usable
                                || credential_present == Some(false) =>
                        {
                            let issue = "online provider check was requested but skipped: fix gateway or auth diagnostics first".to_string();
                            issues.push(issue.clone());
                            online_report = serde_json::json!({
                                "requested": true,
                                "scope": "model_catalog_only",
                                "chat_route_probed": false,
                                "potentially_billable": false,
                                "request_sent": false,
                                "status": issue,
                            });
                        }
                        Some(models_path) => {
                            let auth_source =
                                current_provider_auth_source(gateway, managed_token.is_some());
                            let auth = build_auth(gateway, gateway_name, managed_token.clone())
                                .map_err(|error| CliError::user(safe_config_error(&error)))?;
                            let provider = build_catalog_management_route(
                                &config_path,
                                gateway_name,
                                gateway,
                                models_path,
                                auth_source,
                                auth.as_ref(),
                                Duration::from_secs(cli.timeout_secs),
                            )
                            .map_err(|error| {
                                route_build_cli_error(
                                    error,
                                    ProviderOperation::ModelCatalog,
                                    provider_diagnostics,
                                )
                            })?;
                            provider_route_report = serde_json::json!({
                                "status": "bound",
                                "receipt": provider.receipt(),
                            });
                            match await_cancellable(provider.list_models()).await {
                                Some(Ok(Some(models))) => {
                                    let effective_model = model.as_deref();
                                    let effective_model_available = effective_model.map(|model| {
                                        model_catalog_advertises_selected_model(
                                            model,
                                            &models,
                                            provider_diagnostics,
                                        )
                                    });
                                    let models = normalize_model_catalog(models);
                                    if effective_model_available == Some(false) {
                                        issues.push(format!(
                                        "effective model '{}' is not advertised by the normalized gateway catalog",
                                        terminal_inline(effective_model.expect("checked above"))
                                    ));
                                    }
                                    online_report = serde_json::json!({
                                        "requested": true,
                                        "scope": "model_catalog_only",
                                        "chat_route_probed": false,
                                        "potentially_billable": false,
                                        "request_sent": true,
                                        "status": "reachable",
                                        "models": models.len(),
                                        "effective_model": effective_model,
                                        "effective_model_available": effective_model_available,
                                    });
                                }
                                Some(Ok(None)) => {
                                    let issue = "online provider check was requested but skipped: provider has no model catalog".to_string();
                                    issues.push(issue.clone());
                                    online_report = serde_json::json!({
                                        "requested": true,
                                        "scope": "model_catalog_only",
                                        "chat_route_probed": false,
                                        "potentially_billable": false,
                                        "request_sent": true,
                                        "status": issue,
                                    });
                                }
                                Some(Err(error)) => {
                                    let safe = safe_provider_error_message(
                                        &error,
                                        ProviderOperation::ModelCatalog,
                                        Some(cli.timeout_secs),
                                        provider_diagnostics,
                                    );
                                    let reason = bounded_model_catalog_reason(&error);
                                    issues.push(format!("online provider check failed: {safe}"));
                                    online_report = serde_json::json!({
                                        "requested": true,
                                        "scope": "model_catalog_only",
                                        "chat_route_probed": false,
                                        "potentially_billable": false,
                                        "request_sent": true,
                                        "status": safe,
                                        "reason": reason,
                                    });
                                }
                                None => {
                                    return Err(CliError::cancelled(
                                        "doctor provider check cancelled by Ctrl+C",
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            Err(error) => issues.push(error.to_string()),
        }
    }

    let filesystem_policy = if cli.dangerously_skip_permissions {
        "DANGER: unattended bypass — global non-protected filesystem access AND auto-approved commands inside their roots"
    } else if cli.dangerously_allow_all_filesystem {
        "DANGER: global non-protected filesystem bypass when tools are enabled"
    } else if cli.yes {
        "non-prompting; launch directory granted read+write, confined to explicit/current roots"
    } else {
        "interactive chat prompts; automation is deny-by-default unless roots are granted"
    };
    let (artifact_policy, artifact_policy_source) = if cli.no_artifacts {
        (ArtifactStoragePolicy::InlineOnly, ArtifactPolicySource::Cli)
    } else {
        match config.as_ref().and_then(|config| config.artifact_storage) {
            Some(false) => (
                ArtifactStoragePolicy::InlineOnly,
                ArtifactPolicySource::Config,
            ),
            Some(true) => (
                ArtifactStoragePolicy::Automatic,
                ArtifactPolicySource::Config,
            ),
            None => (
                ArtifactStoragePolicy::Automatic,
                ArtifactPolicySource::Default,
            ),
        }
    };
    let report = serde_json::json!({
        "version": cli::LONG_VERSION,
        "read_only": true,
        "config": {
            "source": config_resolution_source(cli),
            "path": config_path,
            "path_display": shorten_home(&config_path),
            "exists": config_exists,
            "regular_file": config_is_file,
            "parse_ok": parse_ok,
            "status": config_status,
        },
        "gateway": gateway_report,
        "auth": auth_report,
        "provider_route": provider_route_report,
        "context": context_report,
        "project_context": project_report,
        "runtime_state": runtime_state,
        "online": online_report,
        "tool_policy": {
            "filesystem": filesystem_policy,
            "commands": "disabled unless explicitly enabled; enabled commands require exact approval plus mandatory fail-closed OS containment",
            "scripted_tools": "not registered unless a filesystem capability or command policy is explicit",
        },
        "artifact_policy": artifact_policy_receipt(
            artifact_policy,
            artifact_policy_source,
            false,
        ),
        "blocking_issues": issues,
    });
    if json {
        print_json(&report)?;
    } else {
        print_doctor_human(&report);
    }
    let failures = report
        .pointer("/blocking_issues")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if failures == 0 {
        Ok(())
    } else {
        Err(CliError::user(format!(
            "doctor found {failures} blocking issue(s)"
        )))
    }
}
