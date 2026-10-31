use super::*;

#[test]
fn provider_errors_are_actionable_without_provider_controlled_details() {
    let secrets = [
        "user:token@example.invalid",
        "api_key=super-secret",
        "provider-body-secret",
    ];
    let errors = [
        ProviderError::Connection(
            "request failed for https://user:token@example.invalid/?api_key=super-secret"
                .to_string(),
        ),
        ProviderError::InvalidResponse("provider-body-secret".to_string()),
        ProviderError::Http {
            status: 500,
            body: "provider-body-secret".to_string(),
        },
        ProviderError::Unauthorized {
            status: 401,
            hint: Some("provider-body-secret".to_string()),
        },
        ProviderError::RouteContract(ProviderRouteContractError::CurrentRouteUnavailable),
    ];
    for error in &errors {
        let message = safe_provider_error_message(
            error,
            ProviderOperation::Chat {
                model: "safe-model",
            },
            Some(17),
            ProviderDiagnosticContext::default(),
        );
        for secret in secrets {
            assert!(!message.contains(secret), "leaked {secret}: {message}");
        }
        assert!(message.contains('-'), "diagnostic should include a remedy");
    }

    let timeout = chat_error_message_with_diagnostics(
        kitsu_core::AgentError::Provider(ProviderError::Timeout),
        "safe-model",
        17,
        ProviderDiagnosticContext::default(),
    );
    assert!(timeout.contains("chat request timed out after 17s"));
    assert!(timeout.contains("--timeout-secs"));
    let catalog = safe_provider_error_message(
        &ProviderError::Timeout,
        ProviderOperation::ModelCatalog,
        Some(17),
        ProviderDiagnosticContext::default(),
    );
    assert!(catalog.contains("model catalog request timed out after 17s"));
    assert_eq!(
        script_failure_kind(&kitsu_core::AgentError::Provider(
            ProviderError::RouteContract(ProviderRouteContractError::CurrentRouteUnavailable)
        )),
        ScriptFailureKind::User
    );

    let missing = safe_provider_error_message(
        &ProviderError::Auth(kitsu_providers::AuthError::Missing {
            env: "HF_TOKEN".to_string(),
            hint: Some("run `refresh-token` to renew access".to_string()),
        }),
        ProviderOperation::Chat {
            model: "safe-model",
        },
        Some(17),
        ProviderDiagnosticContext::default(),
    );
    assert!(missing.contains("$HF_TOKEN"));
    assert!(missing.contains("refresh-token"));
    assert!(safe_auth_refresh_hint(Some(
        "visit https://user:secret@example.invalid/?token=secret"
    ))
    .is_none());

    let bad_model = safe_provider_error_message(
        &ProviderError::Http {
            status: 404,
            body: "provider-body-secret".to_string(),
        },
        ProviderOperation::Chat {
            model: "bad\nmodel\u{1b}]0;owned\u{7}",
        },
        Some(17),
        ProviderDiagnosticContext::default(),
    );
    assert!(bad_model.contains("HTTP 404"));
    assert!(!bad_model.contains("provider-body-secret"));
    assert!(!bad_model.contains('\n'));
    assert!(!bad_model.contains('\u{1b}'));
}

/// A 400/422 shows the gateway's own reason (the two Bedrock rejections that
/// hid behind the generic remedy), with URLs elided; credential-like,
/// non-JSON, and non-400 bodies stay withheld and control bytes are
/// neutralized.
#[test]
fn rejected_request_shows_only_a_safe_gateway_reason() {
    let rejected = |status: u16, body: &str| {
        safe_provider_error_message(
            &ProviderError::Http {
                status,
                body: body.to_string(),
            },
            ProviderOperation::Chat { model: "m" },
            Some(17),
            ProviderDiagnosticContext::default(),
        )
    };
    let tool_config = rejected(
        400,
        r#"{"message":"The toolConfig field must be defined when using toolUse and toolResult content blocks."}"#,
    );
    assert!(tool_config.contains("(HTTP 400) - verify the selected model"));
    assert!(tool_config.ends_with(
            "; gateway said: The toolConfig field must be defined when using toolUse and toolResult content blocks."
        ));
    let arguments = rejected(
        400,
        r#"{"error":{"message":"error transforming to bedrock://llm/v1/chat: failed to decode function response arguments from assistant's message, not JSON format"}}"#,
    );
    assert!(arguments.contains("gateway said: error transforming to <url> failed to decode"));
    assert!(!arguments.contains("://"));

    for withheld in [
        rejected(
            400,
            r#"{"message":"bad header Authorization: Bearer sk-live"}"#,
        ),
        rejected(400, "provider-body-secret"),
        rejected(500, r#"{"message":"provider-body-secret"}"#),
    ] {
        assert!(!withheld.contains("gateway said"), "{withheld}");
        assert!(!withheld.contains("sk-live") && !withheld.contains("provider-body-secret"));
    }
    // A URL is elided whole, so credentials embedded in it never surface.
    let url = rejected(
        400,
        r#"{"message":"see https://user:pw@example.invalid?api_key=x"}"#,
    );
    assert!(url.ends_with("; gateway said: see <url>"), "{url}");
    let escaped = rejected(422, "{\"message\":\"bad\\nfield\\u001b]0;owned\\u0007\"}");
    assert!(escaped.contains("gateway said: bad field"));
    assert!(!escaped.contains('\n') && !escaped.contains('\u{1b}'));
}

#[test]
fn loopback_ollama_connection_hint_is_scoped_and_secret_free() {
    let gateway = |flavor: &str, base_url: &str| Gateway {
        flavor: flavor.to_string(),
        adapter: config::GatewayAdapter::CompatibleChat,
        base_url: base_url.to_string(),
        chat_path: "/v1/chat/completions".to_string(),
        models_path: Some("/v1/models".to_string()),
        model: Some("test-model".to_string()),
        context_window: Some(8_192),
        auth: config::AuthConfig::default(),
    };
    let secret_url = "http://user:super-secret@127.0.0.7:11434?api_key=hidden";
    let local = gateway("openai", secret_url);
    let diagnostics = ProviderDiagnosticContext::from_gateway("ollama", &local);
    assert!(diagnostics.loopback_ollama);
    let message = safe_provider_error_message(
        &ProviderError::Connection(format!("request failed for {secret_url}")),
        ProviderOperation::Chat {
            model: "test-model",
        },
        Some(17),
        diagnostics,
    );
    assert!(message.contains("local Ollama"));
    assert!(message.contains("configured loopback endpoint"));
    assert!(message.contains("`ollama serve`"));
    assert!(message.contains("select another gateway"));
    assert!(!message.contains("super-secret"));
    assert!(!message.contains("api_key"));
    assert!(!message.contains("127.0.0.7"));
    let typed = map_provider_error_with_diagnostics(
        ProviderError::Connection(format!("request failed for {secret_url}")),
        ProviderOperation::Chat {
            model: "test-model",
        },
        Some(17),
        diagnostics,
    )
    .jsonl_error_payload();
    assert_eq!(typed["diagnostic_code"], "local_ollama_unreachable");
    assert_eq!(
        typed["recovery_actions"],
        serde_json::json!([
            "start_ollama_serve",
            "verify_ollama_serve",
            "select_another_gateway",
        ])
    );
    assert!(!typed.to_string().contains("super-secret"));
    assert!(!typed.to_string().contains("api_key"));
    assert!(!typed.to_string().contains("127.0.0.7"));

    let flavored = gateway("ollama", "http://[::1]:11434");
    assert!(ProviderDiagnosticContext::from_gateway("custom-local", &flavored).loopback_ollama);

    for (name, flavor, base_url) in [
        ("ollama", "openai", "https://gateway.example.test"),
        ("example", "openai", "http://localhost:11434"),
        ("ollama", "openai", "http://localhost.example.test:11434"),
    ] {
        let profile = gateway(flavor, base_url);
        let diagnostics = ProviderDiagnosticContext::from_gateway(name, &profile);
        assert!(!diagnostics.loopback_ollama, "{name} {base_url}");
        let generic = safe_provider_error_message(
            &ProviderError::Connection("provider-controlled detail".to_string()),
            ProviderOperation::ModelCatalog,
            Some(17),
            diagnostics,
        );
        assert!(generic.contains("selected gateway's base_url"));
        assert!(!generic.contains("ollama serve"));
        assert!(!generic.contains("provider-controlled detail"));
        let typed = map_provider_error_with_diagnostics(
            ProviderError::Connection("provider-controlled detail".to_string()),
            ProviderOperation::ModelCatalog,
            Some(17),
            diagnostics,
        )
        .jsonl_error_payload();
        assert!(typed.get("diagnostic_code").is_none(), "{typed}");
        assert!(typed.get("recovery_actions").is_none(), "{typed}");
    }

    for base_url in [
        "http://localhost:11434",
        "http://127.255.1.9:11434/v1",
        "http://[::1]:11434",
    ] {
        assert!(base_url_has_loopback_host(base_url), "{base_url}");
    }
}

/// The shipped huggingface profile's hint must SURVIVE the credential
/// filter — cycle-14's HF audit found the previous wording self-suppressed
/// on every 401, leaving users with no guidance at all.
#[test]
fn shipped_huggingface_refresh_hint_survives_the_credential_filter() {
    let template = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kitsu.toml");
    let config = config::Config::load(&template).expect("repo kitsu.toml parses");
    let hint = config
        .gateways
        .get("huggingface")
        .expect("huggingface profile exists")
        .auth
        .refresh_hint
        .clone()
        .expect("huggingface profile ships a refresh hint");
    assert!(
        safe_auth_refresh_hint(Some(&hint)).is_some(),
        "template hint must not trip the credential filter: {hint}"
    );
}

#[test]
fn refresh_hints_reject_common_credential_forms_on_every_rendering_path() {
    let unsafe_hints = [
        "paste hf_kitsuRefreshCredential123",
        "use sk-kitsuRefreshCredential123",
        "set api-key=kitsuRefreshCredential123",
        "set api_key: kitsuRefreshCredential123",
        "token=kitsuRefreshCredential123",
        "token: kitsuRefreshCredential123",
        "token_kitsuRefreshCredential123",
        "Authorization: Bearer kitsuRefreshCredential123",
    ];

    for hint in unsafe_hints {
        assert!(
            safe_auth_refresh_hint(Some(hint)).is_none(),
            "unsafe hint was accepted: {hint}"
        );

        let missing = safe_provider_error_message(
            &ProviderError::Auth(kitsu_providers::AuthError::Missing {
                env: "HF_TOKEN".to_string(),
                hint: Some(hint.to_string()),
            }),
            ProviderOperation::Chat {
                model: "safe-model",
            },
            Some(17),
            ProviderDiagnosticContext::default(),
        );
        let unauthorized = safe_provider_error_message(
            &ProviderError::Unauthorized {
                status: 401,
                hint: Some(hint.to_string()),
            },
            ProviderOperation::ModelCatalog,
            Some(17),
            ProviderDiagnosticContext::default(),
        );
        let expired = expired_auth_warning("2026-01-01T00:00:00Z", Some(hint));
        for rendered in [&missing, &unauthorized, &expired] {
            assert!(
                !rendered.contains("kitsuRefreshCredential123"),
                "credential-bearing hint leaked: {rendered}"
            );
            assert!(
                !rendered.contains("configured refresh hint"),
                "unsafe hint label was rendered: {rendered}"
            );
        }
    }

    let safe_hint = "run `refresh-token` to renew access";
    assert_eq!(
        safe_auth_refresh_hint(Some(safe_hint)).as_deref(),
        Some(safe_hint)
    );
    assert!(
        expired_auth_warning("2026-01-01T00:00:00Z", Some(safe_hint)).contains("refresh-token")
    );

    let hf_permission_hint = "create a fine-grained Hugging Face credential with 'Make calls to Inference Providers', then set HF_TOKEN";
    assert_eq!(
        safe_auth_refresh_hint(Some(hf_permission_hint)).as_deref(),
        Some(hf_permission_hint)
    );
    let unauthorized = safe_provider_error_message(
        &ProviderError::Unauthorized {
            status: 401,
            hint: Some(hf_permission_hint.to_string()),
        },
        ProviderOperation::Chat {
            model: "safe-model",
        },
        Some(17),
        ProviderDiagnosticContext::default(),
    );
    assert!(unauthorized.contains("Make calls to Inference Providers"));
    assert!(!unauthorized.contains("hf_"));
}
