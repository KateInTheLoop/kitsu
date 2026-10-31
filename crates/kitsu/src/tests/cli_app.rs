use super::*;

/// Card T3-S4's risk gate (R4): **↑ walks the same entries in both
/// renderers.**
///
/// Not vacuous, and the first three assertions are what make it so: the
/// agreement is asserted against a *named, non-empty* expected sequence,
/// so two renderers that both walked nothing, or both walked the list in
/// the wrong direction, fail. The entries are the ones rustyline accepted
/// through the very call `chat::run_chat` makes, so the list is also not an
/// invention of the test.
#[cfg(unix)]
#[test]
fn up_walks_the_same_entries_in_both_renderers() {
    use rustyline::history::History as _;

    // Built exactly as `chat::run_chat` builds it, so the history's
    // duplicate and size policy is production's.
    let config = rustyline::Config::builder()
        .bracketed_paste(true)
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut history = rustyline::history::DefaultHistory::with_config(config);
    // `chat::run_chat` adds the *trimmed* line, and only a non-empty one.
    for line in ["/status", "write the readme", "write the readme", "/quit"] {
        history.add(line).unwrap();
    }

    // Non-vacuity, part 1: there is something to agree about, and it is
    // the de-duplicated list -- the consecutive repeat collapsed.
    let snapshot = inline_history_snapshot(&history);
    assert_eq!(
        snapshot,
        vec![
            "/status".to_string(),
            "write the readme".to_string(),
            "/quit".to_string(),
        ],
        "the snapshot handed to the inline reader is rustyline's list, oldest first"
    );

    // Non-vacuity, part 2: the expected walk is spelled out, newest first,
    // then one step past the oldest (which must not move), then back down
    // through the list to the draft that was stashed.
    let expected = vec![
        "/quit".to_string(),
        "write the readme".to_string(),
        "/status".to_string(),
        "/status".to_string(),
        "write the readme".to_string(),
        "/quit".to_string(),
        "half-typed".to_string(),
    ];
    let classic = classic_history_walk(&history, "half-typed", 4, 3);
    let inline = inline_history_walk(&snapshot, "half-typed", 4, 3);

    assert_eq!(classic, expected, "the classic renderer's ↑/↓ walk");
    assert_eq!(inline, expected, "the inline renderer's ↑/↓ walk");
    assert_eq!(inline, classic, "R4: the same entries in both renderers");
}

/// Card T3-S4's acceptance: a round trip over a **fixture history file on
/// disk**, in rustyline's own format.
///
/// The fixture exercises every part of that format kitsu can produce: the
/// `#V2` header, an escaped line feed (reachable here -- a bracketed paste
/// at the inline prompt submits a multi-line draft), an escaped backslash,
/// and a blank line, which the loader skips.
#[cfg(unix)]
#[test]
fn the_history_file_round_trips_into_both_renderers() {
    use rustyline::history::History as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history");
    std::fs::write(
        &path,
        "#V2\n/status\nfirst line\\nsecond line\nC:\\\\temp\n\n/quit\n",
    )
    .unwrap();

    let config = rustyline::Config::builder()
        .bracketed_paste(true)
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut history = rustyline::history::DefaultHistory::with_config(config);
    history.load(&path).unwrap();

    // Read back: the escapes are undone and the blank line is gone.
    let loaded = inline_history_snapshot(&history);
    assert_eq!(
        loaded,
        vec![
            "/status".to_string(),
            "first line\nsecond line".to_string(),
            "C:\\temp".to_string(),
            "/quit".to_string(),
        ],
        "the fixture file's entries, as both renderers receive them"
    );

    // Both renderers walk those entries, the multi-line one included.
    // Non-vacuous: the expected first two Ups are named, and the list
    // above is already pinned non-empty.
    let expected = vec![
        "/quit".to_string(),
        "C:\\temp".to_string(),
        "first line\nsecond line".to_string(),
    ];
    // Three Ups and no more: each of these arrives on a draft that is
    // single-line at the time (empty, then "/quit", then "C:\\temp"), so
    // the classic model above is faithful. A fourth Up would arrive on the
    // recalled multi-line draft, where rustyline moves the cursor up
    // inside it instead of walking -- see `classic_history_walk`.
    assert_eq!(classic_history_walk(&history, "", 3, 0), expected);
    assert_eq!(inline_history_walk(&loaded, "", 3, 0), expected);

    // Write back: a line submitted at either prompt reaches the file, and
    // the file is still the format the fixture was written in. Pinned as
    // the whole text rather than a substring (F41).
    history.add("ship it \\ now").unwrap();
    history.save(&path).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "#V2\n/status\nfirst line\\nsecond line\nC:\\\\temp\n/quit\nship it \\\\ now\n",
        "the saved file is rustyline's V2 format, re-escaped"
    );

    // And it round-trips: a fresh load sees what was saved.
    let mut reloaded =
        rustyline::history::DefaultHistory::with_config(rustyline::Config::builder().build());
    reloaded.load(&path).unwrap();
    let reloaded = inline_history_snapshot(&reloaded);
    assert_eq!(
        reloaded,
        vec![
            "/status".to_string(),
            "first line\nsecond line".to_string(),
            "C:\\temp".to_string(),
            "/quit".to_string(),
            "ship it \\ now".to_string(),
        ],
        "the round trip adds the new entry and changes nothing else"
    );
    assert_eq!(
        inline_history_walk(&reloaded, "", 1, 0),
        vec!["ship it \\ now".to_string()],
        "the inline reader's first Up is the entry the save round-tripped"
    );
}

#[test]
fn a_broken_output_pipe_is_not_a_crash() {
    assert!(panic_payload_is_broken_pipe(
        "failed printing to stdout: Broken pipe (os error 32)"
    ));
    assert!(panic_payload_is_broken_pipe(
        "failed printing to stderr: Broken pipe (os error 32)"
    ));
    // A full disk is a real failure and must keep the normal panic report
    // (the JSONL sink relies on a typed nonzero exit for it).
    assert!(!panic_payload_is_broken_pipe(
        "failed printing to stdout: No space left on device (os error 28)"
    ));
    assert!(!panic_payload_is_broken_pipe("index out of bounds"));
    assert_eq!(EXIT_BROKEN_PIPE, 0);
}

#[test]
fn screen_reader_is_a_discoverable_global_cli_mode() {
    let cli = Cli::try_parse_from(["kitsu", "--screen-reader", "ask", "hello"]).unwrap();
    assert!(cli.screen_reader);
}

#[test]
fn effort_preset_raises_defaulted_budgets_but_not_explicit_ones() {
    // No --effort: byte-identical to today's defaults.
    let plain = parse_with_effort(&["kitsu", "ask", "hi"]);
    assert_eq!((plain.max_rounds, plain.timeout_secs), (10, 120));
    assert_eq!(plain.max_output_tokens, None);

    // --effort high lifts all three where they were defaulted.
    let high = parse_with_effort(&["kitsu", "--effort", "high", "ask", "hi"]);
    assert_eq!((high.max_rounds, high.timeout_secs), (40, 300));
    assert_eq!(high.max_output_tokens, Some(16000));

    // An explicit flag wins over the preset; the others still move.
    let mixed = parse_with_effort(&["kitsu", "--effort", "max", "--max-rounds", "7", "ask", "hi"]);
    assert_eq!(
        mixed.max_rounds, 7,
        "explicit --max-rounds must beat preset"
    );
    assert_eq!(mixed.timeout_secs, 600);
    assert_eq!(mixed.max_output_tokens, Some(32000));

    // --effort low is a deliberate no-op (equals historical defaults).
    let low = parse_with_effort(&["kitsu", "--effort", "low", "ask", "hi"]);
    assert_eq!((low.max_rounds, low.timeout_secs), (10, 120));
    assert_eq!(low.max_output_tokens, None);
}

#[test]
fn global_skill_flag_parses_after_the_subcommand() {
    let parsed = Cli::try_parse_from(["kitsu", "ask", "hello", "--skill", "review"])
        .expect("global flag after subcommand");
    assert_eq!(parsed.skill, vec!["review"]);
}

#[test]
fn common_global_options_compose_before_or_after_chat() {
    let before = Cli::try_parse_from([
        "kitsu",
        "--theme",
        "ocean",
        "--no-artifacts",
        "chat",
        "--no-session",
    ])
    .expect("common global options before the subcommand");
    assert_eq!(before.theme.as_deref(), Some("ocean"));
    assert!(before.no_artifacts);
    assert!(matches!(before.command, Some(Command::Chat { .. })));

    let after = Cli::try_parse_from([
        "kitsu",
        "chat",
        "--no-session",
        "--theme",
        "forest",
        "--no-artifacts",
    ])
    .expect("common global options after the subcommand");
    assert_eq!(after.theme.as_deref(), Some("forest"));
    assert!(after.no_artifacts);
    assert!(matches!(after.command, Some(Command::Chat { .. })));
}

#[test]
fn allow_network_parses_on_chat_and_defaults_off() {
    let on = Cli::try_parse_from(["kitsu", "chat", "--allow-network", "--no-session"])
        .expect("--allow-network parses on chat");
    let Some(Command::Chat { allow_network, .. }) = on.command else {
        panic!("expected a chat command");
    };
    assert!(allow_network, "the flag is set when passed");

    let off = Cli::try_parse_from(["kitsu", "chat", "--no-session"]).expect("chat parses");
    let Some(Command::Chat { allow_network, .. }) = off.command else {
        panic!("expected a chat command");
    };
    assert!(!allow_network, "network is denied by default");
}

#[test]
fn yolo_parses_as_the_single_dangerous_skip_permissions_argument() {
    for argv in [
        vec!["kitsu", "--dangerously-skip-permissions", "chat"],
        vec!["kitsu", "--yolo", "chat"],
        vec!["kitsu", "chat", "--yolo"],
    ] {
        let parsed = Cli::try_parse_from(argv.clone()).unwrap_or_else(|error| {
            panic!("dangerous bypass spelling must parse {argv:?}: {error}")
        });
        assert!(parsed.dangerously_skip_permissions, "argv: {argv:?}");
        assert!(matches!(parsed.command, Some(Command::Chat { .. })));
    }

    let error = Cli::try_parse_from(["kitsu", "--dangerously-skip-permissions", "--yolo", "chat"])
        .expect_err("repeating one boolean argument through both spellings is ambiguous");
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn yolo_inherits_the_canonical_dangerous_flag_conflicts() {
    for other in ["--yes", "--dangerously-allow-all-filesystem"] {
        let error = Cli::try_parse_from(["kitsu", "--yolo", other, "chat"])
            .expect_err("contradictory permission policies must fail closed");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}

#[test]
fn bare_cli_parses_as_implicit_chat() {
    let parsed = Cli::try_parse_from(["kitsu"]).expect("bare kitsu must parse");
    assert!(parsed.command.is_none());
    assert!(parsed.print_prompt.is_none());
}

#[test]
fn top_level_prompt_flag_parses_without_a_subcommand() {
    let parsed = Cli::try_parse_from(["kitsu", "-p", "explain this"])
        .expect("top-level prompt mode must parse");
    assert_eq!(parsed.print_prompt.as_deref(), Some("explain this"));
    assert!(parsed.command.is_none());
}

/// Regression: README documents "--resume implies --session", so passing
/// both explicitly must be a harmless no-op confirmation, not a clap
/// conflict error — in either argument order.
#[test]
fn resume_and_session_compose_in_either_order() {
    let resume_then_session =
        Cli::try_parse_from(["kitsu", "chat", "--resume", "abc", "--session"])
            .expect("--resume then --session must not conflict");
    let Some(Command::Chat {
        resume, session, ..
    }) = &resume_then_session.command
    else {
        panic!("expected Command::Chat");
    };
    assert_eq!(resume.as_deref(), Some("abc"));
    assert!(*session);

    let session_then_resume =
        Cli::try_parse_from(["kitsu", "chat", "--session", "--resume", "abc"])
            .expect("--session then --resume must not conflict");
    let Some(Command::Chat {
        resume, session, ..
    }) = &session_then_resume.command
    else {
        panic!("expected Command::Chat");
    };
    assert_eq!(resume.as_deref(), Some("abc"));
    assert!(*session);
}

/// `--session` still conflicts with `--no-session` — that combination is
/// genuinely contradictory, unlike `--resume` (which already implies
/// `--session`).
#[test]
fn session_still_conflicts_with_no_session() {
    let error = Cli::try_parse_from(["kitsu", "chat", "--session", "--no-session"])
        .expect_err("--session and --no-session remain a hard conflict");
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

/// Card T4-S4, the `p` key: every outcome of acknowledging the selected
/// agent is **distinguishable**, per T2-S5b. A human typing `p` into a
/// racy world must be able to tell "it worked" from "there was nothing
/// here" from "someone already collected this".
///
/// `p` is `AgentRegistry::acknowledge_result`, which T4-S3's peek already
/// calls; this pins its four answers on real samples rather than adding a
/// second path.
#[cfg(unix)]
#[test]
fn acknowledging_the_selected_agent_distinguishes_every_outcome() {
    use subagent::AgentHandoffAckOutcome;

    let registry = subagent::AgentRegistry::new();
    let done = registry.start("has a result".to_string()).unwrap();
    let running = registry.start("still running".to_string()).unwrap();
    registry.finish(done, Ok("collect me".to_string()));

    assert_eq!(
        registry.acknowledge_result(done),
        AgentHandoffAckOutcome::Acknowledged,
        "`p` on a done agent with an uncollected result collects it"
    );
    assert_eq!(
        registry.acknowledge_result(done),
        AgentHandoffAckOutcome::AlreadyAcknowledged,
        "`p` again says so rather than reporting a second success"
    );
    assert_eq!(
        registry.acknowledge_result(running),
        AgentHandoffAckOutcome::NotReady,
        "`p` on a running agent has nothing to acknowledge, and says that \
             rather than nothing"
    );
    assert_eq!(
        registry.acknowledge_result(9_999),
        AgentHandoffAckOutcome::NotFound,
        "`p` on an id the registry does not hold is its own answer"
    );

    // The four answers are four, not one with four spellings: a `p` whose
    // caller could not tell them apart would print the same line for all
    // of them.
    let answers = [
        AgentHandoffAckOutcome::Acknowledged,
        AgentHandoffAckOutcome::AlreadyAcknowledged,
        AgentHandoffAckOutcome::NotReady,
        AgentHandoffAckOutcome::NotFound,
    ];
    for (index, left) in answers.iter().enumerate() {
        for right in &answers[index + 1..] {
            assert_ne!(left, right);
        }
    }
}

/// Card T4-S4: renaming a terminal agent is allowed and **acknowledges
/// nothing**. Naming a row is a note to yourself about it, not a claim to
/// have read its result, so the exit guard must still hold it back.
///
/// The POSITIVE CONTROL is the second half: `p` on the same agent does
/// release it.
#[cfg(unix)]
#[test]
fn renaming_a_done_agent_is_allowed_and_does_not_collect_its_result() {
    let registry = subagent::AgentRegistry::new();
    let id = registry.start("will finish".to_string()).unwrap();
    registry.finish(id, Ok("the answer".to_string()));

    assert!(
        matches!(
            registry.rename(id, "the-one-that-failed"),
            subagent::AgentRenameOutcome::Renamed { .. }
        ),
        "a terminal agent can be named: it is the one worth labelling"
    );
    assert_eq!(
        registry.handoff_state(id),
        Some(subagent::thread::AgentHandoffState::Ready),
        "naming a row is not collecting its result"
    );
    assert_eq!(
        registry
            .take_unobserved_terminal_results()
            .iter()
            .map(|result| result.id)
            .collect::<Vec<_>>(),
        vec![id],
        "so the exit guard still holds it"
    );

    // POSITIVE CONTROL, on the same agent: `p` does release it.
    let registry = subagent::AgentRegistry::new();
    let id = registry.start("will finish".to_string()).unwrap();
    registry.finish(id, Ok("the answer".to_string()));
    let _ = registry.rename(id, "named-then-collected");
    assert_eq!(
        registry.acknowledge_result(id),
        subagent::AgentHandoffAckOutcome::Acknowledged
    );
    assert!(
        registry.take_unobserved_terminal_results().is_empty(),
        "POSITIVE CONTROL: the guard does let a collected result go, so \
             the assertion above is about the rename"
    );
}
