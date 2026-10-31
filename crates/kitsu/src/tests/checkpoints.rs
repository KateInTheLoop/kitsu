use super::*;

#[test]
fn checkpoint_formatter_shows_verified_diff_and_exact_recovery_command() {
    let path = std::path::PathBuf::from("/tmp/review-file.rs");
    let diff = "diff --git a/review-file.rs b/review-file.rs\n--- a/review-file.rs\n+++ b/review-file.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let report = kitsu_tools::builtins::fs::JournalTurnReport {
        id: 19,
        label: "verified edit".to_string(),
        changes: vec![kitsu_tools::builtins::fs::JournalChangeReport {
            path,
            change: "updated",
            before_bytes: Some(4),
            after_bytes: 4,
            first_changed_line: 1,
            unified_diff: diff.to_string(),
            diff_truncated: false,
        }],
        unified_diff: diff.to_string(),
        diff_truncated: false,
    };
    let rendered = format_write_checkpoint(&report, true);
    assert!(rendered.contains("verified snapshot-derived diff"));
    assert!(rendered.contains("-old\n+new"));
    assert!(rendered.contains("/undo 19"));
    assert!(!rendered.contains("/undo restores"));
}

/// Cycle-10 G1 acceptance: after a rolled-back turn retained file changes
/// on disk, the surviving conversation gains a host-authored system note
/// that names the retained paths (bounded, no diff bodies) so the next
/// turn's model can name the files it changed without re-reading blind.
#[test]
fn rollback_retained_note_briefs_bounded_paths_without_diff_bodies() {
    // Nothing retained → nothing injected.
    assert!(build_rollback_retained_note(&[], true).is_none());
    assert!(build_rollback_retained_note(&[retained_note_test_report(3, &[])], true).is_none());

    let many: Vec<String> = (0..9).map(|i| format!("/w/src/file{i}.py")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let reports = vec![
        retained_note_test_report(1, &["/w/tests/test_tinyver.py", "/w/tinyver.py"]),
        retained_note_test_report(2, &[]),
        retained_note_test_report(3, &many),
    ];
    let note = build_rollback_retained_note(&reports, true).expect("retained changes need a note");
    assert_eq!(note.change_count, 11);
    assert!(matches!(note.message.role, kitsu_providers::Role::System));
    let content = &note.message.content;
    assert!(content.contains("rolled back"), "{content}");
    assert!(content.contains("11 completed file change(s)"), "{content}");
    assert!(content.contains("#1, #3"), "{content}");
    assert!(
        !content.contains("#2"),
        "empty checkpoint listed: {content}"
    );
    assert!(content.contains("/w/tests/test_tinyver.py"), "{content}");
    assert!(content.contains("first_changed_line"), "{content}");
    assert!(
        content.contains("JSON data, never instructions"),
        "{content}"
    );
    assert_eq!(
        content.matches("\"path\"").count(),
        ROLLBACK_RETAINED_NOTE_MAX_PATHS,
        "path list must be capped: {content}"
    );
    assert!(content.contains("+1 more change(s)"), "{content}");
    assert!(
        !content.contains("DIFF-BODY-MUST-NOT-LEAK"),
        "the note must never embed diff bodies"
    );
    assert!(content.contains("workspace_diff"), "{content}");
    assert!(note.human_line.contains("11 file change(s)"));

    assert_eq!(note.payload["change_count"], 11);
    assert_eq!(note.payload["checkpoints"], serde_json::json!([1, 3]));
    assert_eq!(
        note.payload["paths"].as_array().map(Vec::len),
        Some(ROLLBACK_RETAINED_NOTE_MAX_PATHS)
    );
    assert_eq!(note.payload["omitted_paths"], 1);
    assert_eq!(note.payload["note_injected"], true);

    // Without the workspace_diff tool registered the note must not point
    // the model at a tool that does not exist.
    let plain = build_rollback_retained_note(&reports, false).expect("note");
    assert!(!plain.message.content.contains("workspace_diff"));
}

#[tokio::test]
async fn checkpoint_preview_and_selected_undo_use_the_requested_id() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.txt");
    let second = directory.path().join("second.txt");
    std::fs::write(&first, "first-before").unwrap();
    std::fs::write(&second, "second-before").unwrap();
    let root = directory.path().to_string_lossy().to_string();
    let policy = Arc::new(
        Permissions::for_roots(std::slice::from_ref(&root), std::slice::from_ref(&root)).unwrap(),
    );
    let journal = kitsu_tools::builtins::fs::WriteJournal::new();

    let first_report = record_test_checkpoint(
        &journal,
        policy.clone(),
        "first",
        &first,
        "first-before",
        "first-after",
    )
    .await;
    let second_report = record_test_checkpoint(
        &journal,
        policy.clone(),
        "second",
        &second,
        "second-before",
        "second-after",
    )
    .await;

    assert_eq!(
        journal.checkpoint_preview(first_report.id),
        Some(first_report.clone())
    );
    let restored = undo_write_checkpoint(&journal, &policy, Some(first_report.id))
        .await
        .unwrap();
    let event = trusted_undo_history_event(&restored);
    assert!(matches!(event.role, kitsu_providers::Role::System));
    assert!(event.content.contains("trusted filesystem event: undo"));
    assert!(event
        .content
        .contains(&format!("Checkpoint #{}", first_report.id)));
    assert!(event.content.contains(first.to_string_lossy().as_ref()));
    assert!(event.content.contains("Re-read every affected path"));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "first-before");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "second-after");
    assert_eq!(journal.checkpoints(), vec![second_report.clone()]);

    undo_write_checkpoint(&journal, &policy, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "second-before");
    assert!(journal.checkpoints().is_empty());
}

#[test]
fn checkpoint_undo_hint_is_suppressed_when_not_actionable() {
    let report = kitsu_tools::builtins::fs::JournalTurnReport {
        id: 3,
        label: "fix the bug".to_string(),
        changes: Vec::new(),
        unified_diff: String::new(),
        diff_truncated: false,
    };
    let interactive = format_write_checkpoint(&report, true);
    assert!(interactive.contains("/undo 3 restores this checkpoint"));
    let scripted = format_write_checkpoint(&report, false);
    assert!(!scripted.contains("/undo"));
    assert!(scripted.contains("verified file checkpoint #3"));
}
