use super::*;

/// Card T3-S3, the acceptance test's `@file` half -- the user-visible
/// part, since the inline composer has never had path completion (R4,
/// finding F20).
///
/// The launch directory is passed in rather than set process-wide, so the
/// test is pure and parallel-safe.
#[cfg(unix)]
#[test]
fn both_hosts_offer_the_same_at_file_candidates_for_fi() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file_one.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.path().join("fish.txt"), "x\n").unwrap();
    std::fs::create_dir(root.path().join("figures")).unwrap();
    std::fs::write(root.path().join("other.md"), "x\n").unwrap();
    let permissions = Arc::new(
        Permissions::for_roots(&[root.path().to_string_lossy().to_string()], &[]).unwrap(),
    );
    let base = root.path().to_path_buf();
    let helper = chat_helper_completing_in(permissions.clone(), base.clone());
    let line = "look at @fi";
    let pos = line.len();

    let shared = helper.completion.candidates(line, pos);
    let classic = classic_candidates(&helper, line, pos);
    let inline = inline_candidates(permissions, base, line, pos);

    assert_eq!(classic, inline, "the two hosts must agree on `@fi`");
    assert_eq!(classic, shared, "the classic host adds nothing of its own");
    assert_eq!(inline, shared, "the inline host adds nothing of its own");

    // Non-vacuity, and the replacement range: `@fi` starts at byte 8.
    assert_eq!(shared.start, 8);
    assert_eq!(
        shared
            .candidates
            .iter()
            .map(|candidate| (candidate.display.as_str(), candidate.replacement.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("@figures/", "@figures/"),
            ("@file_one.rs", "@file_one.rs "),
            ("@fish.txt", "@fish.txt "),
        ],
        "sorted, directories keep their slash, files gain a trailing space, \
             and `other.md` is not a `fi` match"
    );
}

/// The anti-fork test: for **every** prefix of **every** registered
/// command, each host's answer is exactly the shared source's answer.
///
/// A fork cannot survive by agreeing on the two sampled inputs above; it
/// would have to reproduce the registry traversal over its whole domain,
/// which is the duplication this card exists to prevent.
#[test]
fn both_hosts_are_exactly_the_shared_source_for_every_slash_prefix() {
    let permissions = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
    let base = std::env::current_dir().unwrap();
    let helper = chat_helper_completing_in(permissions.clone(), base.clone());

    let mut checked = 0usize;
    for spec in SLASH_COMMANDS.iter() {
        for end in 0..=spec.name.len() {
            if !spec.name.is_char_boundary(end) {
                continue;
            }
            let line = format!("/{}", &spec.name[..end]);
            let pos = line.len();
            let shared = helper.completion.candidates(&line, pos);
            assert_eq!(
                classic_candidates(&helper, &line, pos),
                shared,
                "classic host forked at {line:?}"
            );
            assert_eq!(
                inline_candidates(permissions.clone(), base.clone(), &line, pos),
                shared,
                "inline host forked at {line:?}"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= SLASH_COMMANDS.len(),
        "the sweep must visit at least one prefix per command, visited {checked}"
    );
}

/// The re-hosted highlighter: one styling function, two hosts. Rustyline
/// computes the hint's display width from the unstyled text, so a second
/// styling pass that differed would corrupt the classic row -- which is
/// why this is pinned rather than left to agree by accident.
#[test]
fn both_hosts_style_the_composer_hint_through_one_function() {
    use kitsu_tui::composer::ComposerDecoration as _;
    use rustyline::highlight::Highlighter as _;

    let permissions = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
    let base = std::env::current_dir().unwrap();
    let helper = chat_helper_completing_in(permissions.clone(), base.clone());
    let hint = "deck row\nmodel: test-model";

    let shared = highlight_composer_hint(hint, true);
    let inline =
        tui_bridge::InlineComposerHost::new(ComposerCompletion::new(permissions, base), true)
            .highlight_hint(hint);

    assert_eq!(helper.highlight_hint(hint).as_ref(), shared.as_str());
    assert_eq!(inline, shared);
    assert_ne!(
        shared, hint,
        "a rich terminal really is styled, so the equalities above are not \
             two pass-throughs agreeing on nothing"
    );
}

/// The inline row's hint is the row suffix, not the classic multi-row
/// deck -- F20's second correction. Pinned so "parity" is not read as
/// "the hosts return the same hint string", which would be wrong.
#[test]
fn the_inline_rows_hint_is_the_shared_row_hint() {
    use kitsu_tui::composer::ComposerDecoration as _;

    let permissions = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
    let base = std::env::current_dir().unwrap();
    let host =
        tui_bridge::InlineComposerHost::new(ComposerCompletion::new(permissions, base), true);

    for line in ["/st", "/", "", "hello", "/steer now"] {
        assert_eq!(
            host.hint(line),
            idle_slash_hint(line),
            "the inline hint for {line:?} is the shared row hint"
        );
    }
}

#[test]
fn typeahead_slash_hint_prefers_mid_turn_actionable_commands() {
    // While a turn runs, /s must suggest steer (usable now), not save
    // (registry order), and /st must beat status.
    assert_eq!(typeahead_slash_hint("/s"), "teer");
    assert_eq!(typeahead_slash_hint("/st"), "eer");
    assert_eq!(typeahead_slash_hint("/q"), "ueue");
    assert_eq!(typeahead_slash_hint("/j"), "obs");
    // Outside the priority set, registry order still decides.
    assert_eq!(typeahead_slash_hint("/sa"), "ve");
    // Exact names, non-slash text, arguments, and no-match: no hint.
    assert_eq!(typeahead_slash_hint("/steer"), "");
    assert_eq!(typeahead_slash_hint("steer"), "");
    assert_eq!(typeahead_slash_hint("/queue add x"), "");
    assert_eq!(typeahead_slash_hint("/zzz"), "");
    assert_eq!(typeahead_slash_hint("/"), "");
}

/// Card T0-S17, finding F30. At the idle prompt no turn is running, so
/// the mid-turn priority list has no claim: `/s` must complete `save`,
/// the first registry match, not `steer`.
///
/// The `/st` expectation is derived from `matching_slash_commands` rather
/// than hardcoded, so adding a command cannot silently void the test.
#[test]
fn idle_slash_hint_follows_registry_order() {
    assert_eq!(idle_slash_hint("/s"), "ave");
    let lead_st = matching_slash_commands("st")
        .first()
        .map(|spec| spec.name)
        .expect("/st matches at least one command");
    assert_eq!(
        idle_slash_hint("/st"),
        lead_st["st".len()..],
        "the idle hint must complete registry order's lead for /st ({lead_st})"
    );
    // The non-slash, argument, exact-name and no-match rules are unchanged.
    assert_eq!(idle_slash_hint("/save"), "");
    assert_eq!(idle_slash_hint("save"), "");
    assert_eq!(idle_slash_hint("/queue add x"), "");
    assert_eq!(idle_slash_hint("/zzz"), "");
    assert_eq!(idle_slash_hint("/"), "");
}

/// Card T0-S17. With the idle hint fixed, `slash_palette_adds_to_the_row`
/// loses its third clause: a hint that disagrees with the lead entry no
/// longer suppresses a correct palette. The two surviving clauses -- not a
/// bare single-token slash prefix, and the prefix already being a
/// command's whole name -- still hold.
#[test]
fn slash_palette_adds_to_the_row_drops_the_disagreeing_hint_clause() {
    // The case the clause was papering over: /s now hints `ave` and gets
    // a palette.
    assert!(slash_palette_adds_to_the_row("/s", &idle_slash_hint("/s")));
    // And the clause is gone, not merely unreachable: even a hint that
    // does not complete the lead entry no longer stands the palette down.
    assert!(
        slash_palette_adds_to_the_row("/s", "teer"),
        "the third stand-down clause must be gone, not just unreachable"
    );
    // Clause one: not a bare single-token slash prefix.
    assert!(!slash_palette_adds_to_the_row("/co ", ""));
    assert!(!slash_palette_adds_to_the_row("hello /co", ""));
    assert!(!slash_palette_adds_to_the_row("/co\nx", ""));
    // Clause two: the prefix already is a command's whole name.
    assert!(!slash_palette_adds_to_the_row("/context", ""));
    // Unchanged: a prefix matching nothing still gets its own box.
    assert!(slash_palette_adds_to_the_row("/zzz", ""));
}

#[test]
fn mid_turn_hint_priority_names_exist_in_the_registry() {
    for name in MID_TURN_HINT_PRIORITY {
        assert!(
            SLASH_COMMANDS.iter().any(|spec| spec.name == name),
            "priority hint '{name}' is not a registered command"
        );
    }
}

#[test]
fn multiline_highlighting_preserves_buffer_geometry() {
    use rustyline::highlight::Highlighter;
    let h = chat_helper();
    assert_eq!(h.highlight("just one line", 0), "just one line");
    assert!(!h.highlight_char("just one line", 0, false));
    let pasted = "first line here\nsecond\nthird";
    assert_eq!(h.highlight(pasted, 0), pasted);
    assert!(!h.highlight_char(pasted, 0, false));
}

#[test]
fn assistive_input_disables_rustyline_bracketed_paste_sequences() {
    assert!(bracketed_paste_enabled(false));
    assert!(!bracketed_paste_enabled(true));
}

#[test]
fn slash_completion_discovers_commands_and_inserts_argument_space() {
    use rustyline::completion::Completer;
    let history = rustyline::history::DefaultHistory::new();
    let context = rustyline::Context::new(&history);

    let helper = chat_helper();
    let (start, candidates) = helper.complete("/mod", 4, &context).unwrap();
    assert_eq!(start, 0);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.replacement.as_str())
            .collect::<Vec<_>>(),
        vec!["/model ", "/models "]
    );
    assert!(candidates[0].display.contains("switch model"));

    let (_, candidates) = helper.complete("/status", 7, &context).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].replacement, "/status");

    let (_, candidates) = helper.complete("/model qwen", 12, &context).unwrap();
    assert!(candidates.is_empty(), "arguments are not command names");
}

#[test]
fn slash_palette_opens_filters_and_never_becomes_insertable_text() {
    use rustyline::hint::{Hint, Hinter};
    let history = rustyline::history::DefaultHistory::new();
    let context = rustyline::Context::new(&history);
    let helper = chat_helper();

    let all = helper.hint("/", 1, &context).expect("slash opens palette");
    assert!(all.display().starts_with("\n╭ slash commands "));
    assert!(all.display().contains("more — keep typing"));
    assert!(all.display().contains("Tab to complete"));
    assert!(all.display().contains("model test-model"));
    assert!(all.display().contains("cwd ~/project"));
    assert_eq!(
        all.completion(),
        None,
        "right-arrow must not insert the deck"
    );
    let expected_width = composer_surface_inner_width(current_terminal_columns()) + 2;
    assert!(
        all.display()
            .lines()
            .filter(|line| !line.is_empty())
            .all(|line| unicode_width::UnicodeWidthStr::width(line) == expected_width),
        "palette rows must share one stable boundary width: {}",
        all.display()
    );

    let filtered = helper
        .hint("/mod", 4, &context)
        .expect("prefix keeps palette open");
    assert!(filtered.display().contains("/model"));
    assert!(filtered.display().contains("/models"));
    assert!(!filtered.display().contains("/status"));

    let argument = helper
        .hint("/model ", 7, &context)
        .expect("status remains after command arguments begin");
    assert!(!argument.display().contains("slash commands"));
    assert!(argument.display().contains("model test-model"));

    let ordinary = helper
        .hint("ordinary prompt", 15, &context)
        .expect("status is persistent");
    assert!(!ordinary.display().contains("slash commands"));
    assert!(ordinary.display().contains("model test-model"));
    assert!(ordinary.display().contains("cwd ~/project"));

    helper
        .live_model_window
        .lock()
        .expect("live model/window")
        .0 = "changed-model".to_string();
    let changed = helper
        .hint("", 0, &context)
        .expect("status tracks live model changes");
    assert!(changed.display().contains("model changed-model"));
    assert!(!changed.display().contains("model test-model"));
}

#[test]
fn slash_palette_has_colored_unicode_and_no_color_ascii_variants() {
    use rustyline::highlight::Highlighter;
    let rich = slash_palette("mod", true, COMPOSER_SURFACE_DEFAULT_INNER_WIDTH);
    assert!(rich.contains('╭') && rich.contains('┤'));
    let helper = chat_helper();
    let history = rustyline::history::DefaultHistory::new();
    let context = rustyline::Context::new(&history);
    let rich_with_status = rustyline::hint::Hinter::hint(&helper, "/mod", 4, &context)
        .expect("rich palette and status")
        .0;
    assert!(rich_with_status.contains('╭') && rich_with_status.contains('╯'));
    let highlighted = helper.highlight_hint(&rich_with_status).into_owned();
    assert!(highlighted.contains("\x1b[1;96m"));
    assert!(highlighted.contains("\x1b[2;36m"));
    assert_eq!(
        kitsu_render::sanitize_terminal_text(&highlighted),
        rich_with_status
    );

    let plain = slash_palette("mod", false, COMPOSER_SURFACE_DEFAULT_INNER_WIDTH);
    assert!(plain.contains("+ slash commands "));
    assert!(!plain.contains('\u{1b}'));
    assert!(!plain.contains('╭'));
    assert!(plain.contains("/model"));
}

#[test]
fn composer_surfaces_fit_terminal_columns_and_unicode_cell_widths() {
    use unicode_width::UnicodeWidthStr as _;

    assert_eq!(composer_surface_inner_width(None), 68);
    assert_eq!(composer_surface_inner_width(Some(50)), 48);
    assert_eq!(composer_surface_inner_width(Some(200)), 68);

    for columns in [32, 50, 70, 120] {
        let inner_width = composer_surface_inner_width(Some(columns));
        for rich in [true, false] {
            let rendered = format!(
                "{}{}",
                slash_palette("", rich, inner_width),
                composer_status("模型-狐", "~/仕事/計画", rich, inner_width)
            );
            let expected_width = columns.min(COMPOSER_SURFACE_MAX_WIDTH);
            assert!(
                rendered
                    .lines()
                    .filter(|line| !line.is_empty())
                    .all(|line| line.width() == expected_width),
                "surface exceeded {columns} columns (rich={rich}):\n{rendered}"
            );
        }
    }

    let compact = slash_palette("mod", true, 30);
    assert!(compact.contains(" /model <name>"));
    assert!(!compact.contains("switch model while keeping history"));
}

/// The remaining registry-fed sites, in one test, because one entry is
/// what feeds them all: `/help`'s grouped listing, `/help tui`, the
/// palette, and the shared completion source both hosts use (T3-S3).
///
/// This is the top-level answer to finding F38: a top-level command has
/// **no unchecked string literal of its own** on these four surfaces —
/// they are all registry traversals. The literals a top-level command
/// does own are its dispatch arm's match pattern and its own messages,
/// and for the dispatch arm there is no registry-drift guard at all: an
/// entry with no arm prints `unknown command: /tui` and nothing in this
/// suite would notice. That is what the PTY tests in `tests/tui.rs` are
/// for.
#[test]
fn every_registry_fed_surface_shows_the_tui_switch() {
    let help = slash_help("test-model", false);
    // T5-S2 re-quotes both literals, mechanically. This test holds no
    // opinion about the mode vocabulary — its subject is registry drift,
    // "no unchecked string literal of its own on these four surfaces" —
    // so when `SlashCommandSpec::args` gains a mode, the string these
    // traversals must equal gains it too. The claim is unchanged; the
    // quotation of the registry's own output is what moved.
    assert!(
        help.contains("/tui [classic|inline|fullscreen]"),
        "/help lists the command: {help}"
    );

    let detail = slash_command_help("tui");
    assert!(
        detail.starts_with("/tui [classic|inline|fullscreen]"),
        "/help tui leads with the syntax: {detail}"
    );

    let palette = slash_palette("tu", false, 80);
    assert!(
        palette.contains("/tui") && palette.contains("report or switch the live renderer"),
        "the palette offers it for the prefix `tu`: {palette}"
    );

    let permissions = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
    let base = std::env::current_dir().unwrap();
    let helper = chat_helper_completing_in(permissions, base);
    let candidates = helper.completion.candidates("/tu", 3);
    assert!(
        candidates
            .candidates
            .iter()
            .any(|candidate| candidate.replacement.trim_end() == "/tui"),
        "the shared completion source completes `/tu` to `/tui`: {:?}",
        candidates
            .candidates
            .iter()
            .map(|candidate| candidate.replacement.as_str())
            .collect::<Vec<_>>()
    );
}
