use super::*;

#[test]
fn a_near_miss_for_view_is_suggested_as_a_jobs_subcommand() {
    assert_eq!(closest_jobs_subcommand("viw"), Some("view"));
    assert_eq!(closest_jobs_subcommand("vieww"), Some("view"));
    assert_eq!(closest_jobs_subcommand("view"), Some("view"));
}

#[test]
fn a_near_miss_for_explain_is_suggested_as_a_jobs_subcommand() {
    assert_eq!(closest_jobs_subcommand("explan"), Some("explain"));
    assert_eq!(closest_jobs_subcommand("explain"), Some("explain"));
    assert_eq!(closest_jobs_subcommand("xplain"), Some("explain"));
}

#[test]
fn queue_typos_suggest_the_closest_subaction_without_breaking_the_shorthand() {
    // A one-word near-miss is intercepted with the fix and the escape
    // hatch, instead of being silently queued as a prompt.
    let error = parse_queue_command("lst").unwrap_err();
    assert!(error.contains("did you mean '/queue list'"), "{error}");
    assert!(error.contains("/queue add lst"), "{error}");
    assert!(matches!(
        parse_queue_command("clera").unwrap_err(),
        error if error.contains("did you mean '/queue clear'")
    ));
    // Multi-word input is always a prompt, even when the first word is
    // one edit from a subaction.
    assert!(matches!(
        parse_queue_command("lists the reasons to refactor").unwrap(),
        QueueCommand::Add(prompt) if prompt == "lists the reasons to refactor"
    ));
    // A single word far from every subaction stays a queued prompt.
    assert!(matches!(
        parse_queue_command("refactor").unwrap(),
        QueueCommand::Add(prompt) if prompt == "refactor"
    ));
    // Exact subactions are untouched.
    assert!(matches!(
        parse_queue_command("clear").unwrap(),
        QueueCommand::Clear
    ));
}

#[test]
fn queue_commands_are_one_based_editable_and_backward_compatible() {
    assert!(matches!(
        parse_queue_command("").unwrap(),
        QueueCommand::List
    ));
    assert!(matches!(
        parse_queue_command("legacy prompt").unwrap(),
        QueueCommand::Add(prompt) if prompt == "legacy prompt"
    ));
    assert!(matches!(
        parse_queue_command("remove 2").unwrap(),
        QueueCommand::Remove(1)
    ));
    assert!(matches!(
        parse_queue_command("edit 3 replacement text").unwrap(),
        QueueCommand::Edit(2, replacement) if replacement == "replacement text"
    ));
    assert!(matches!(
        parse_queue_command("move 4 1").unwrap(),
        QueueCommand::Move { from: 3, to: 0 }
    ));
    assert!(matches!(
        parse_queue_command("promote 2").unwrap(),
        QueueCommand::Promote(1)
    ));
    assert!(parse_queue_command("remove 0").is_err());
    assert!(parse_queue_command("edit 1").is_err());

    let mut queue = prompt_queue::PromptQueue::new(4);
    assert!(apply_queue_command(
        &mut queue,
        QueueCommand::Add("one".to_string())
    ));
    assert!(apply_queue_command(
        &mut queue,
        QueueCommand::Add("two".to_string())
    ));
    assert!(apply_queue_command(
        &mut queue,
        QueueCommand::Edit(1, "TWO".to_string())
    ));
    assert!(apply_queue_command(&mut queue, QueueCommand::Promote(1)));
    assert_eq!(queue.iter().collect::<Vec<_>>(), vec!["TWO", "one"]);
    assert_eq!(queue_prompt_preview("unsafe\n\x1b]0;title\x07"), "unsafe ");
}

#[test]
fn resume_directory_comparison_uses_canonical_paths() {
    let dir = tempfile::tempdir().unwrap();
    assert!(same_directory(
        &dir.path().join(".").to_string_lossy(),
        dir.path()
    ));
    let other = tempfile::tempdir().unwrap();
    assert!(!same_directory(&dir.path().to_string_lossy(), other.path()));
}

#[test]
fn resume_project_context_opt_out_replaces_saved_system_message() {
    let saved = session::SavedSession {
        name: None,
        version: 1,
        id: "s-test".to_string(),
        created_at: "now".to_string(),
        updated_at: "now".to_string(),
        directory: "/tmp/project".to_string(),
        gateway: "local".to_string(),
        model: "model".to_string(),
        project_context: Some("/tmp/project/AGENTS.md".to_string()),
        messages: vec![
            ChatMessage::system("BASE\nSECRET_PROJECT_CONTEXT"),
            ChatMessage::user("keep this turn"),
        ],
    };
    let fresh = ChatMessage::system("FRESH_WITHOUT_PROJECT_CONTEXT");

    let restored = restored_messages(&saved, &fresh);
    assert_eq!(restored[0], fresh);
    assert_eq!(restored[1], saved.messages[1]);
    assert!(!restored[0].content.contains("SECRET_PROJECT_CONTEXT"));
}

// --- sites 4-7: discoverability ---

#[test]
fn a_near_miss_for_reply_is_suggested_as_a_jobs_subcommand() {
    assert_eq!(closest_jobs_subcommand("reply"), Some("reply"));
    assert_eq!(closest_jobs_subcommand("repl"), Some("reply"));
    assert_eq!(closest_jobs_subcommand("replyy"), Some("reply"));
}

/// Finding F2: `SUBCOMMANDS` drifts from the dispatcher silently. Every
/// subcommand the idle dispatcher serves must be suggestible, so the list
/// is pinned whole rather than one entry at a time.
#[test]
fn every_jobs_subcommand_is_its_own_nearest_suggestion() {
    for subcommand in [
        "list",
        "view",
        "show",
        "explain",
        "steer",
        "close-steering",
        "stop",
        "stop-all",
        "prune",
        "reply",
    ] {
        assert_eq!(
            closest_jobs_subcommand(subcommand),
            Some(subcommand),
            "`{subcommand}` is dispatched but not in the suggester table"
        );
    }
}

/// **T5-S2.** The two literals `/tui`'s own messages are built from, and
/// the third renderer request.
///
/// Pinned as equalities rather than `contains` for the reason F90 gives:
/// `TUI_ACCEPTED_MODES` was `"accepted: classic, inline"` and every
/// message that interpolates it keeps containing that string after a
/// third mode is appended, so a `contains` here would survive this card
/// without noticing it. What is asserted is what the constant **is**.
///
/// This is a vocabulary test, not a reachability one. That a `/tui
/// fullscreen` typed at a prompt actually reaches
/// `TuiRendererRequest::Fullscreen`, enters the alternate screen and
/// paints the ring is **F89**'s question, and no unit test can answer it:
/// it is answered in `tests/tui.rs` on the raw byte stream.
///
/// The third variant's own label is **deliberately not named here.**
/// `TuiRendererRequest::Fullscreen` does not exist yet, and a unit test
/// that named it would not fail — it would fail to **compile**, taking
/// all 927 `--bins` tests with it and hiding every other result in this
/// card. A failing-first test has to leave the suite legible. The label
/// is pinned where it is user-visible instead:
/// `TUI_SWITCHED_TO_FULLSCREEN` in `tests/tui.rs` is the receipt
/// `run_chat` builds from `label()`, so a variant labelled anything but
/// `"fullscreen"` fails there.
#[test]
fn the_tui_vocabulary_names_all_three_renderers() {
    assert_eq!(TUI_ACCEPTED_MODES, "accepted: classic, inline, fullscreen");
    assert_eq!(TuiRendererRequest::Classic.label(), "classic");
    assert_eq!(TuiRendererRequest::Inline.label(), "inline");
    // Tightened once the variant existed. The doc above states where the
    // third label is pinned *instead* (`TUI_SWITCHED_TO_FULLSCREEN`, in
    // the pty suite, because naming the variant here before it existed
    // would have been a compile failure rather than a test failure). The
    // two pins are not redundant now that both can be had: one fails in
    // three seconds, the other in forty-seven.
    assert_eq!(TuiRendererRequest::Fullscreen.label(), "fullscreen");
}

/// `/tui` is its own nearest suggestion, so a user who mistypes a
/// neighbouring command is not sent to it and a user who types it is not
/// sent elsewhere.
///
/// Also the non-vacuity of the PTY tests' `unknown command: /tui`
/// assertion: today `closest_slash_command("tui")` answers with some
/// *other* command, which is exactly the failure mode those tests ban.
#[test]
fn tui_is_its_own_nearest_suggestion() {
    assert_eq!(closest_slash_command("tui"), Some("tui"));
    // And the neighbours keep theirs: `/tui` must not capture them.
    assert_eq!(closest_slash_command("tools"), Some("tools"));
    assert_eq!(closest_slash_command("tool"), Some("tools"));
}

/// `/detach` is its own nearest suggestion, so a user who types it is not
/// sent to a neighbour — and the neighbours keep theirs, so the new entry
/// does not capture them.
///
/// Also the non-vacuity of the PTY session's `unknown command: /detach`
/// observation: today `closest_slash_command("detach")` answers with some
/// *other* command, which is exactly the failure mode that test bans.
#[test]
fn detach_is_its_own_nearest_suggestion() {
    assert_eq!(closest_slash_command("detach"), Some("detach"));
    assert_eq!(closest_slash_command("delete"), Some("delete"));
    assert_eq!(closest_slash_command("diff"), Some("diff"));
}

/// Card **T4-S10**, requirement 3: **`/help keys` prints the one bindings
/// table.**
///
/// Note what `/help keys` *is*: not a new slash command but a new
/// argument to the already-registered, already-dispatched `/help`, whose
/// arm feeds any argument to `slash_command_help`. So F57's
/// registered-without-an-arm hazard does not arise here, and the opposite
/// one does: today the arm runs and answers "no command named /keys",
/// which is a dispatched wrong answer. Both halves are pinned --
/// `/help`'s own registry entry advertises the argument, so it is
/// discoverable, and the argument prints the table.
#[test]
fn slash_help_keys_prints_the_one_bindings_table() {
    let spec = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "help")
        .expect("/help is registered");
    assert!(
        spec.args.contains("keys"),
        "the registry entry advertises the argument, or nothing offers it \
             in `/help` or in completion: {:?}",
        spec.args
    );

    let text = slash_command_help("keys");
    for binding in kitsu_tui::keys::BINDINGS {
        let rows: Vec<&str> = text
            .lines()
            .filter(|line| {
                line.split_whitespace()
                    .next()
                    .is_some_and(|first| first == binding.label)
            })
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "exactly one row of `/help keys` is {:?}'s, as on the help \
                 overlay:\n{text}",
            binding.label
        );
        assert!(
            rows[0].contains(binding.what),
            "and it carries the table's own wording for it, not a second \
                 phrasing that can drift: {:?} is not in {:?}",
            binding.what,
            rows[0]
        );
    }

    // NON-VACUOUS: `keys` is answered because it is `keys`, not because
    // every unknown argument now prints the table.
    let unknown = slash_command_help("nosuchthing");
    assert!(
        unknown.starts_with("no command named /nosuchthing"),
        "an argument that is neither a command nor `keys` still gets the \
             nudge: {unknown:?}"
    );
}

/// Card **T4-S10**, requirement 1: **the two readers print the same
/// bindings.**
///
/// The overlay's agreement with the table and `/help keys`' agreement
/// with it are each pinned above; this is the consequence a user meets,
/// and it fails if either reader keeps a list of its own.
#[test]
fn the_overlay_help_and_slash_help_keys_print_the_same_bindings() {
    let overlay: Vec<String> = kitsu_tui::agents_view::overlay_help_rows(200)
        .into_iter()
        .skip(2)
        .filter(|row| !row.trim().is_empty())
        .filter_map(|row| row.split_whitespace().next().map(str::to_string))
        .collect();
    assert!(
        !overlay.is_empty(),
        "the help overlay paints key rows at all"
    );
    let text = slash_command_help("keys");
    let printed: Vec<String> = text
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .filter(|first| overlay.iter().any(|label| label == first))
        .collect();
    assert_eq!(
        printed, overlay,
        "`/help keys` lists the overlay's keys, all of them, in the \
             overlay's order:\n{text}"
    );
}
