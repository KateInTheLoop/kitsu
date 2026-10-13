//! The extracted editor's public surface, exercised from OUTSIDE the crate.
//!
//! This is deliberately an *integration* test: it compiles as an external
//! consumer, so it can only reach genuinely `pub` items. That makes it the
//! proof that the surface the `kitsu` binary uses today — and that the
//! future terminal-UI crate will use — is reachable from another crate. It
//! fails to compile if anything the consumers need stays crate-private.
//!
//! Behavioural coverage of the keymap itself lives in the crate's own unit
//! tests, which travel with the code; nothing here duplicates them.

use kitsu_editor::{EditorAction, FeedOutcome, TypeaheadEditor};

fn history() -> Vec<String> {
    vec!["cargo build".to_string(), "fix the parser".to_string()]
}

#[test]
fn constructors_expose_the_starting_draft() {
    let empty = TypeaheadEditor::new(history());
    assert_eq!(empty.buffer_string(), "");
    assert_eq!(empty.cursor(), 0);

    let carried = TypeaheadEditor::with_draft(history(), "carried".to_string());
    assert_eq!(carried.buffer_string(), "carried");
    assert_eq!(carried.cursor(), 7, "with_draft puts the cursor at the end");
}

#[test]
fn feeding_printable_bytes_advances_the_buffer_and_reports_a_change() {
    let mut editor = TypeaheadEditor::new(Vec::new());
    let outcome = editor.feed(b"hi");
    assert!(outcome.changed, "typing needs a redraw");
    assert!(outcome.actions.is_empty());
    assert_eq!(editor.buffer_string(), "hi");
    assert_eq!(editor.cursor(), 2);
}

#[test]
fn enter_produces_a_submit_action_on_the_outcome() {
    let mut editor = TypeaheadEditor::new(Vec::new());
    let outcome = editor.feed(b"run tests\r");
    // Match a variant by name so the enum's shape is part of the pinned
    // surface, not just the type name.
    assert_eq!(
        outcome.actions,
        vec![EditorAction::Submit("run tests".to_string())]
    );
    assert_eq!(editor.buffer_string(), "");
}

#[test]
fn control_keys_produce_their_named_actions() {
    let mut editor = TypeaheadEditor::new(Vec::new());
    assert_eq!(
        editor.feed(b"\t").actions,
        vec![EditorAction::CompleteRequest]
    );
    assert_eq!(editor.feed(&[0x04]).actions, vec![EditorAction::Eof]);
    assert_eq!(editor.feed(&[0x03]).actions, vec![EditorAction::Interrupt]);
    assert_eq!(editor.feed(&[0x1a]).actions, vec![EditorAction::Suspend]);
    assert_eq!(editor.feed(&[0x1c]).actions, vec![EditorAction::Quit]);
    assert_eq!(
        editor.feed(b"\x1b[A").actions,
        vec![EditorAction::RecallRequest]
    );
}

#[test]
fn replace_buffer_swaps_the_whole_draft() {
    let mut editor = TypeaheadEditor::new(Vec::new());
    editor.feed(b"dra");
    editor.replace_buffer("/queue");
    assert_eq!(editor.buffer_string(), "/queue");
    assert_eq!(editor.cursor(), 6);
}

#[test]
fn take_draft_consumes_the_editor_and_yields_the_text() {
    let mut editor = TypeaheadEditor::new(Vec::new());
    editor.feed(b"carry me");
    let draft: String = editor.take_draft();
    assert_eq!(draft, "carry me");
}

#[test]
fn search_status_is_none_until_a_search_is_entered() {
    let mut editor = TypeaheadEditor::new(history());
    assert!(editor.search_status().is_none());
    editor.feed(&[0x12]); // Ctrl+R
    let status = editor.search_status().expect("search is active");
    assert!(status.contains("reverse-search"), "got {status:?}");
    editor.feed(b"parser");
    assert_eq!(editor.buffer_string(), "fix the parser");
    assert!(editor.search_status().is_some());
}

#[test]
fn lone_escape_and_pending_reset_are_callable_from_outside() {
    let mut editor = TypeaheadEditor::new(Vec::new());
    assert!(!editor.resolve_lone_escape(), "inert without a pending ESC");
    editor.feed(b"junk");
    editor.feed(b"\x1b");
    assert!(editor.resolve_lone_escape(), "lone Esc clears the draft");
    assert_eq!(editor.buffer_string(), "");

    editor.feed(b"\x1b[200~stuck");
    let unit: () = editor.reset_pending_input();
    assert_eq!(unit, ());
    editor.feed(b" more");
    assert_eq!(
        editor.feed(b"\r").actions,
        vec![EditorAction::Submit("stuck more".to_string())],
        "paste mode must not survive the reset"
    );
}

#[test]
fn history_prev_forced_takes_a_caller_built_outcome() {
    let mut editor = TypeaheadEditor::new(history());

    // FeedOutcome is externally constructible two ways: via Default...
    let mut outcome = FeedOutcome::default();
    assert!(!outcome.changed);
    assert!(outcome.actions.is_empty());
    editor.history_prev_forced(&mut outcome);
    assert!(outcome.changed);
    assert_eq!(editor.buffer_string(), "fix the parser");

    // ...and via its public fields in a struct literal.
    let mut literal = FeedOutcome {
        changed: false,
        actions: Vec::new(),
    };
    editor.history_prev_forced(&mut literal);
    assert!(literal.changed);
    assert_eq!(editor.buffer_string(), "cargo build");
}
