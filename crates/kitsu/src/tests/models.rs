use super::*;

#[test]
fn local_ollama_model_catalog_matcher_is_narrow_and_ambiguity_safe() {
    let local_ollama = ProviderDiagnosticContext {
        loopback_ollama: true,
    };
    let generic_gateway = ProviderDiagnosticContext::default();
    let models = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    };

    assert!(model_catalog_advertises_selected_model(
        "llama3.2",
        &models(&["llama3.2:latest"]),
        local_ollama,
    ));
    assert!(!model_catalog_advertises_selected_model(
        "llama3.2:7b",
        &models(&["llama3.2:latest"]),
        local_ollama,
    ));
    assert!(model_catalog_advertises_selected_model(
        "llama3.2:7b",
        &models(&["llama3.2:7b"]),
        generic_gateway,
    ));
    assert!(!model_catalog_advertises_selected_model(
        "llama3.2",
        &models(&["llama3.2:latest"]),
        generic_gateway,
    ));
    assert!(!model_catalog_advertises_selected_model(
        "llama3.2",
        &models(&["qwen2.5-coder:latest"]),
        local_ollama,
    ));
    assert!(!model_catalog_advertises_selected_model(
        "llama3.2",
        &models(&["llama3.2:latest", "llama3.2:latest"]),
        local_ollama,
    ));
    assert!(!model_catalog_advertises_selected_model(
        "",
        &models(&["", ":latest"]),
        local_ollama,
    ));
    assert!(model_catalog_advertises_selected_model(
        "localhost:5000/team/model",
        &models(&["localhost:5000/team/model:latest"]),
        local_ollama,
    ));
    assert!(!model_catalog_advertises_selected_model(
        "team/model@sha256:1234",
        &models(&["team/model@sha256:1234:latest"]),
        local_ollama,
    ));
}

#[test]
fn model_catalog_filter_is_sorted_deduplicated_and_no_match_is_successful() {
    let models = vec![
        "zeta".to_string(),
        "Alpha-Coder".to_string(),
        " Alpha-Coder ".to_string(),
        "beta-coder".to_string(),
        "Alpha-Coder".to_string(),
        "  ".to_string(),
    ];
    assert_eq!(
        filter_model_catalog(models.clone(), " CODER "),
        vec!["Alpha-Coder".to_string(), "beta-coder".to_string()]
    );
    assert_eq!(
        filter_model_catalog(models, ""),
        vec![
            "Alpha-Coder".to_string(),
            "beta-coder".to_string(),
            "zeta".to_string(),
        ]
    );
    assert!(filter_model_catalog(vec!["alpha".to_string()], "missing").is_empty());
    let catalog = match finish_slash_model_catalog(
        Ok(Some(vec!["alpha".to_string()])),
        17,
        ProviderDiagnosticContext::default(),
    ) {
        Ok(catalog) => catalog,
        Err(_) => panic!("available catalog should succeed"),
    };
    assert_eq!(catalog, vec!["alpha".to_string()]);
    assert_eq!(
        empty_model_catalog_message(" Missing "),
        "no models matched 'Missing' — retry without a filter to see the full catalog"
    );
}

#[test]
fn human_model_catalog_preview_pins_current_model_and_caps_rows() {
    let models = (0..20)
        .map(|index| format!("model-{index:02}"))
        .collect::<Vec<_>>();
    let preview = format_model_catalog_preview(
        &models,
        "",
        Some("model-19"),
        "`/models <substring>`",
        "switch with: /model <id>",
    );

    assert!(preview.starts_with("~ 20 model(s) available (current: model-19):\n"));
    let current = preview.find("  * model-19 (current)").unwrap();
    let alphabetical_start = preview.find("    model-00").unwrap();
    assert!(
        current < alphabetical_start,
        "current model was not pinned first"
    );
    assert_eq!(
        preview
            .lines()
            .filter(|line| line.starts_with("  * model-") || line.starts_with("    model-"))
            .count(),
        HUMAN_MODEL_CATALOG_CAP
    );
    assert!(preview.contains("... and 8 more - narrow with `/models <substring>`"));
    assert!(preview.ends_with("switch with: /model <id>\n"));

    let filtered = format_model_catalog_preview(
        &["alpha".to_string(), "beta".to_string()],
        "alp",
        Some("different-model"),
        "filter again",
        "switch",
    );
    assert!(filtered.contains("matching 'alp' (current: different-model)"));
    assert!(!filtered.contains("(current)\n"));
}

#[test]
fn empty_state_messages_follow_consistent_pattern() {
    // Sessions empty state includes guidance
    let sessions_empty = format_session_summaries(&[], None, "");
    assert!(sessions_empty.contains("no saved sessions"));
    assert!(sessions_empty.contains("—"));
    assert!(sessions_empty.contains("/new"));
    assert!(sessions_empty.contains("/restore <exact-id>"));
    assert!(!sessions_empty.contains("/resume"));

    // Checkpoints empty state includes guidance
    let checkpoints_empty = format_checkpoint_summaries(&[]);
    assert!(checkpoints_empty.contains("no recoverable checkpoints"));
    assert!(checkpoints_empty.contains("—"));
    assert!(checkpoints_empty.contains("/checkpoint"));

    // Models empty state messages use em dash
    assert!(empty_model_catalog_message("").contains("—"));
    assert!(empty_model_catalog_message("test").contains("—"));
}

#[test]
fn doctor_catalog_reason_is_bounded_and_only_exposes_catalog_parse_structure() {
    let malformed =
        ProviderError::InvalidResponse(format!("malformed /models JSON: {}", "x".repeat(500)));
    let reason = bounded_model_catalog_reason(&malformed).unwrap();
    assert_eq!(reason.chars().count(), 241);
    assert!(reason.ends_with('…'));
    assert!(
        bounded_model_catalog_reason(&ProviderError::InvalidResponse(
            "provider body secret".to_string()
        ))
        .is_none()
    );
}
