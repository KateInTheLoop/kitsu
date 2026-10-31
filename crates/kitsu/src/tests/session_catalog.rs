use super::*;

#[test]
fn session_catalog_and_checkpoint_metadata_are_terminal_sanitized() {
    let summaries = vec![session::SessionSummary {
        name: None,
        id: "s-safe".to_string(),
        updated_at: "now".to_string(),
        model: "model\u{1b}]8;;https://evil.invalid\u{7}click\u{1b}]8;;\u{7}".to_string(),
        gateway: "gateway\nforged-gateway-row".to_string(),
        directory: "/tmp/project".to_string(),
        preview: "preview\r\nforged-preview-row".to_string(),
        turns: 1,
    }];
    let catalog = format_session_summaries(&summaries, None, "");
    assert!(!catalog.contains('\u{1b}'));
    assert!(!catalog.contains("https://evil.invalid"));
    assert!(!catalog.contains("\nforged-gateway-row"));
    assert!(!catalog.contains("\nforged-preview-row"));

    let report = kitsu_tools::builtins::fs::JournalTurnReport {
        id: 1,
        label: "prompt\nforged-checkpoint\u{1b}]0;owned\u{7}".to_string(),
        changes: Vec::new(),
        unified_diff: String::new(),
        diff_truncated: false,
    };
    let checkpoint = format_write_checkpoint(&report, true);
    assert!(!checkpoint.contains('\u{1b}'));
    assert!(!checkpoint.contains("\nforged-checkpoint"));
}

/// Cycle-9 session search, thickened: matching is case-insensitive over
/// id, name, and preview, non-matching filters produce the guided empty
/// state naming the filter, and the current-session marker survives
/// filtering.
#[test]
fn session_filtering_matches_id_name_and_preview_case_insensitively() {
    let summaries = vec![
        session::SessionSummary {
            name: Some("Parser-Plan".to_string()),
            id: "s-alpha-1".to_string(),
            updated_at: "now".to_string(),
            model: "m".to_string(),
            gateway: "g".to_string(),
            directory: "/tmp/a".to_string(),
            preview: "Refactor the PARSER module".to_string(),
            turns: 1,
        },
        session::SessionSummary {
            name: None,
            id: "s-Beta-2".to_string(),
            updated_at: "now".to_string(),
            model: "m".to_string(),
            gateway: "g".to_string(),
            directory: "/tmp/b".to_string(),
            preview: "unrelated work".to_string(),
            turns: 2,
        },
    ];

    // Case-insensitive id match, either direction.
    let by_id = filter_sessions(&summaries, "BETA");
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].id, "s-Beta-2");
    // Case-insensitive preview match.
    let by_preview = filter_sessions(&summaries, "parser");
    assert_eq!(by_preview.len(), 1);
    assert_eq!(by_preview[0].id, "s-alpha-1");
    // Case-insensitive name match.
    let by_name = filter_sessions(&summaries, "PARSER-plan");
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].id, "s-alpha-1");
    // Whitespace-only filters behave like no filter.
    assert_eq!(filter_sessions(&summaries, "   ").len(), 2);

    // A non-matching filter produces the guided empty state naming the
    // filter, not the bare "no saved sessions" message.
    let miss = format_session_summaries(&[], None, "zeta");
    assert!(miss.contains("no sessions matched 'zeta'"), "{miss}");
    assert!(miss.contains("/sessions"), "{miss}");
    assert!(!miss.contains("no saved sessions"), "{miss}");

    // The current-session marker survives filtering.
    let listed = format_session_summaries(
        &filter_sessions(&summaries, "beta"),
        Some("s-Beta-2"),
        "beta",
    );
    assert!(listed.contains(" * s-Beta-2"), "{listed}");
    assert!(!listed.contains("s-alpha-1"), "{listed}");
}
