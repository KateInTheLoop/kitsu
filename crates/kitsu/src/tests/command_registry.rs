use super::*;

/// Card T3-S3, the acceptance test the plan names, slash half: the same
/// candidates in the same order from both hosts for `/st`.
///
/// The third assertion is what keeps the first two from being vacuous:
/// the shared list is also the live registry's own answer, so an empty or
/// invented list fails even if both hosts produced it.
#[test]
fn both_hosts_offer_the_same_slash_candidates_for_st() {
    let permissions = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
    let base = std::env::current_dir().unwrap();
    let helper = chat_helper_completing_in(permissions.clone(), base.clone());

    let shared = helper.completion.candidates("/st", 3);
    let classic = classic_candidates(&helper, "/st", 3);
    let inline = inline_candidates(permissions, base, "/st", 3);

    assert_eq!(classic, inline, "the two hosts must agree on `/st`");
    assert_eq!(classic, shared, "the classic host adds nothing of its own");
    assert_eq!(inline, shared, "the inline host adds nothing of its own");

    let expected_replacements: Vec<String> = matching_slash_commands("st")
        .into_iter()
        .map(|spec| {
            format!(
                "/{}{}",
                spec.name,
                if spec.args.is_empty() { "" } else { " " }
            )
        })
        .collect();
    assert!(
        !expected_replacements.is_empty(),
        "the registry must have at least one `st` command for this test to mean anything"
    );
    assert_eq!(shared.start, 0, "a slash candidate replaces the whole line");
    assert_eq!(
        shared
            .candidates
            .iter()
            .map(|candidate| candidate.replacement.clone())
            .collect::<Vec<_>>(),
        expected_replacements,
        "the shared source is the registry's answer, in registry order"
    );
}

/// Card T0-S17. The invariant that makes
/// `slash_palette_adds_to_the_row`'s third stand-down clause dead code:
/// at the idle prompt, for *any* slash prefix with at least one match,
/// the hint completes the palette's **lead** entry.
#[test]
fn idle_slash_hint_always_completes_the_lead_palette_entry() {
    let mut prefixes: Vec<String> = Vec::new();
    for spec in SLASH_COMMANDS {
        for end in 1..=spec.name.len() {
            let prefix = spec.name[..end].to_string();
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
    }
    assert!(
        prefixes.len() > 50,
        "the property needs a decent range of prefixes"
    );
    for prefix in prefixes {
        let buffer = format!("/{prefix}");
        let lead = matching_slash_commands(&prefix)
            .first()
            .map(|spec| spec.name)
            .expect("a prefix taken from a command name matches that command");
        let hint = idle_slash_hint(&buffer);
        assert_eq!(
            format!("{prefix}{hint}"),
            lead,
            "idle hint for {buffer:?} must complete the lead palette entry \
                 {lead:?}, not {:?}",
            format!("{prefix}{hint}")
        );
    }
}

#[test]
fn the_command_registry_advertises_jobs_view_on_jobs_and_its_agents_alias() {
    let jobs = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "jobs")
        .expect("/jobs is registered");
    assert!(jobs.args.contains("view"), "{}", jobs.args);

    let agents = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "agents")
        .expect("/agents is registered");
    assert_eq!(agents.alias_of, Some("jobs"));
    assert!(agents.args.contains("view"), "{}", agents.args);
    assert!(agents.args.contains("id"), "{}", agents.args);
}

#[test]
fn the_command_registry_advertises_jobs_explain() {
    let jobs = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "jobs")
        .expect("/jobs is registered");
    assert!(jobs.args.contains("explain"), "{}", jobs.args);

    // The alias needs no row change: it already advertises, and forwards,
    // every /jobs subcommand.
    let agents = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "agents")
        .expect("/agents is registered");
    assert_eq!(agents.alias_of, Some("jobs"));
    assert!(
        agents.args.contains("any /jobs subcommand"),
        "{}",
        agents.args
    );
}

#[test]
fn slash_registry_is_sorted_unique_and_drives_help() {
    let names: Vec<_> = SLASH_COMMANDS.iter().map(|spec| spec.name).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(names, sorted, "registry must remain sorted and unique");

    let help = slash_help("test-model", false);
    for spec in SLASH_COMMANDS {
        match spec.alias_of {
            // Aliases fold into their parent row instead of having one.
            Some(parent) => {
                assert!(help.contains(&format!("(alias: /{})", spec.name)));
                assert!(
                    !help.lines().any(|line| {
                        let row = line.trim_start();
                        row == spec.syntax()
                            || row
                                .strip_prefix(&spec.syntax())
                                .is_some_and(|rest| rest.starts_with("  "))
                    }),
                    "alias {} has own row",
                    spec.name
                );
                assert!(SLASH_COMMANDS
                    .iter()
                    .any(|candidate| candidate.name == parent && candidate.alias_of.is_none()));
            }
            None => {
                assert!(help.contains(&spec.syntax()));
                assert!(help.contains(spec.description));
            }
        }
    }
    assert!(help
        .ends_with("current model: test-model — /help <command> explains one command in detail"));
}

#[test]
fn usage_registry_exposes_session_default_and_cost_compatibility_alias() {
    let usage = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "usage")
        .expect("/usage is registered");
    assert_eq!(usage.args, "[session]");
    assert_eq!(usage.syntax(), "/usage [session]");
    assert!(!usage.description.contains("daily"));
    assert!(!usage.description.contains("weekly"));

    let cost = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "cost")
        .expect("/cost remains registered for compatibility");
    assert_eq!(cost.args, "");
    assert_eq!(cost.syntax(), "/cost");
    assert_eq!(cost.alias_of, Some("usage"));
    assert!(cost.description.contains("compatibility"));

    let help = slash_help("test-model", false);
    assert!(help.contains("/usage [session]"));
    assert!(help.contains("(alias: /cost)"));
    assert!(!help
        .lines()
        .any(|line| line.trim_start().starts_with("/cost ")));

    let cost_help = slash_command_help("cost");
    assert!(cost_help.starts_with("/usage [session]"));
    assert!(cost_help.contains("/cost is an alias of /usage"));
}

#[test]
fn context_registry_exposes_only_bare_window_sources_and_refresh_forms() {
    let context = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "context")
        .expect("/context is registered");
    assert_eq!(context.args, "[window|sources|refresh]");
    assert_eq!(context.syntax(), "/context [window|sources|refresh]");
    assert!(context.details.contains("Bare `/context` preserves"));
    assert!(context.details.contains("`sources`"));
    assert!(context.details.contains("`refresh`"));
    assert!(!context.details.contains("daily"));
    assert!(!context.details.contains("account"));
}

#[test]
fn slash_help_groups_commands_without_forking_the_registry() {
    let help = slash_help("test-model", false);
    assert!(
        !help.contains('\u{1b}'),
        "unstyled help must carry zero escape bytes"
    );
    // Every group heading appears exactly once, as a standalone line.
    let mut headings = 0;
    for group in CommandGroup::ORDER {
        assert!(
            help.lines().any(|line| line == group.heading()),
            "missing heading {:?}",
            group.heading()
        );
        headings += 1;
    }
    assert!(headings <= 6);
    // Styled mode differs only by heading emphasis: stripping terminal
    // sequences must restore the exact unstyled text.
    let styled = slash_help("test-model", true);
    assert!(styled.contains('\u{1b}'));
    assert_eq!(kitsu_render::sanitize_terminal_text(&styled), help);
}

/// Cycle-9 item 5, thickened: the syntax column is capped, every
/// two-column row starts its description at the same column, and a
/// long-arg command wraps to its own line with the description indented
/// beneath it instead of blowing the column out.
#[test]
fn slash_help_caps_the_syntax_column_and_wraps_long_commands() {
    const CAP: usize = 50;
    let long_syntaxes: Vec<String> = SLASH_COMMANDS
        .iter()
        .filter(|spec| spec.alias_of.is_none())
        .map(|spec| spec.syntax())
        .filter(|syntax| syntax.len() > CAP)
        .collect();
    assert!(
        !long_syntaxes.is_empty(),
        "fixture rot: no command exercises the wrapping branch"
    );

    let help = slash_help("test-model", false);
    let lines: Vec<&str> = help.lines().collect();
    for syntax in &long_syntaxes {
        let position = lines
            .iter()
            .position(|line| *line == format!("  {syntax}"))
            .unwrap_or_else(|| panic!("long command '{syntax}' must wrap onto its own line"));
        let continuation = lines[position + 1];
        assert!(
            continuation.starts_with("    ") && !continuation.starts_with("     "),
            "wrapped description must be indented exactly four spaces: {continuation:?}"
        );
    }

    // With the wrapping branch exercised, the column width is the cap, so
    // every two-column row must start its description at exactly column
    // 2 + CAP + 2 — never at a column dictated by the longest command.
    let mut two_column_rows = 0;
    for line in &lines {
        if !line.starts_with("  /") {
            continue; // headings, wrapped continuations, the model footer
        }
        if long_syntaxes
            .iter()
            .any(|syntax| *line == format!("  {syntax}"))
        {
            continue; // wrapped syntax-only line, checked above
        }
        two_column_rows += 1;
        let chars: Vec<char> = line.chars().collect();
        assert!(
            chars.len() > 2 + CAP + 2,
            "two-column row lost its description: {line:?}"
        );
        assert_eq!(
            (chars[2 + CAP], chars[2 + CAP + 1]),
            (' ', ' '),
            "syntax overflowed its capped column: {line:?}"
        );
        assert_ne!(
            chars[2 + CAP + 2],
            ' ',
            "description must start exactly at the shared column: {line:?}"
        );
    }
    assert!(two_column_rows > 10, "fixture rot: {two_column_rows} rows");
}

/// The advertised syntax for `/jobs`, for the `/agent` spelling that
/// forwards to the same dispatcher, and for the `/agents` alias.
#[test]
fn the_command_registry_advertises_jobs_reply_on_every_spelling() {
    let jobs = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "jobs")
        .expect("/jobs is registered");
    assert!(jobs.args.contains("reply"), "{}", jobs.args);
    assert!(
        jobs.args.contains("reply <id> <y|a|n>"),
        "the advertised form names the keys: {}",
        jobs.args
    );

    // `/agent` is not a cosmetic row: it forwards its argument verbatim to
    // `handle_jobs_command`, so anything it fails to advertise is a
    // subcommand that works and cannot be found.
    let agent = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "agent")
        .expect("/agent is registered");
    assert!(agent.args.contains("reply"), "{}", agent.args);

    let agents = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "agents")
        .expect("/agents is registered");
    assert_eq!(agents.alias_of, Some("jobs"));
    assert!(
        agents.args.contains("any /jobs subcommand"),
        "{}",
        agents.args
    );
}

/// Pre-existing drift from T1-S3, in the same breath: `/agent` reaches the
/// identical dispatcher as `/jobs`, so it must not advertise a smaller
/// vocabulary than the one it serves.
#[test]
fn the_agent_spelling_advertises_everything_it_forwards() {
    let agent = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "agent")
        .expect("/agent is registered");
    for subcommand in ["view", "explain", "stop-all", "prune", "reply"] {
        assert!(
            agent.args.contains(subcommand),
            "/agent forwards `{subcommand}` but does not advertise it: {}",
            agent.args
        );
    }
}

/// Site 1 of the top-level command's site list: the registry entry, which
/// is the one source `/help`, `/help <command>`, the palette and both
/// completion hosts read.
#[test]
fn the_command_registry_advertises_the_tui_switch() {
    let tui = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "tui")
        .expect("/tui is registered");
    assert_eq!(tui.args, "[classic|inline|fullscreen]");
    assert_eq!(tui.syntax(), "/tui [classic|inline|fullscreen]");
    assert_eq!(
        tui.alias_of, None,
        "/tui is a command in its own right, not an alias"
    );
    assert_eq!(
        tui.group,
        CommandGroup::Session,
        "the renderer is session state, which is where /theme lives too"
    );
    // The advertised vocabulary is exactly `[tui] mode`'s, and **T5-S2
    // inverts this clause**: until this card the assertion here was
    // `!tui.args.contains("fullscreen")`, on the ground that "the args row
    // must not advertise a mode that is refused". `fullscreen` is no
    // longer refused — it has its renderer, its switch arm and its
    // alternate screen — so the same rule now obliges the opposite. The
    // rule did not change; its subject did.
    assert!(
        tui.args.contains("fullscreen"),
        "the args row must advertise every mode the switch accepts, and \
             `fullscreen` is now one of them: {}",
        tui.args
    );
    assert!(
        tui.details.contains("fullscreen"),
        "the details must still describe `fullscreen`, because the \
             design's grammar has it: {}",
        tui.details
    );
    // ...and must no longer describe it as absent. This is the assertion
    // that catches the half-done card: a renderer that ships behind a
    // help string still telling the user it has not shipped.
    assert!(
        !tui.details.contains("not available yet"),
        "the details must not still announce `fullscreen` as unavailable \
             once the renderer is reachable: {}",
        tui.details
    );
    assert!(
        tui.details.contains("conversation"),
        "and must state R2's gate — a switch keeps the conversation: {}",
        tui.details
    );
}

/// **T5-S1c-iii.** The four paging keys' only advertisement.
///
/// Design §4.7 keeps a computed-label chord out of `keys::BINDINGS`
/// because it never reaches the swept dispatch, and these keys add a
/// third reason: they are live **only while fullscreen holds the
/// screen**, so a static row would advertise them at the inline and
/// classic composers, where they do nothing. There is therefore no row
/// and no second table, and the honest place left is `/tui`'s own
/// `details` -- the string requirement R2 already pins across four
/// spellings and `the_command_registry_advertises_the_tui_switch`
/// already asserts.
///
/// A capability a user cannot find is not one they have, so this asserts
/// the sentence is there **and** that it says what the keys do: the key
/// names, and the fact that they scroll the frame rather than the caret.
#[test]
fn the_tui_details_advertise_the_fullscreen_paging_keys() {
    let tui = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "tui")
        .expect("/tui is registered");

    for key in ["PgUp", "PgDn", "Ctrl+Home", "Ctrl+End"] {
        assert!(
            tui.details.contains(key),
            "the details are where the paging keys are advertised, and \
                 `{key}` is one of the four: there is no `keys::BINDINGS` row \
                 for them, so a user who is not told here is not told at all \
                 (F89: a capability that is unreachable is not one). The \
                 details read: {}",
            tui.details
        );
    }
    assert!(
        tui.details.contains("frame"),
        "...and the sentence must say what they move. `Ctrl+Home` is \
             also plain `Home`'s modified form, and plain `Home` still moves \
             the caret, so naming the keys without naming the frame \
             advertises an ambiguity rather than a capability: {}",
        tui.details
    );
}

/// **R6-a.** `Ctrl+O`'s only advertisement.
///
/// The transcript overlay's own footer advertises the keys that are live
/// **inside** it; nothing in it can tell a user that the overlay exists
/// or how to open it. And `Ctrl+O` cannot go in `keys::BINDINGS` for the
/// four paging keys' reason above: that table is the agent view's,
/// guarded by a sweep over *that* decoder's key space (**F82**), and this
/// key is read by the inline reader on the composer's surfaces. So its
/// home is the same one T5-S1c-iii gave the paging keys, and this test is
/// that test's shape deliberately rather than a second one.
///
/// **F82** is also why this test exists at all: its sweep found `q`,
/// `Enter` and `\u{2192}` dispatched and advertised nowhere, and the
/// lesson recorded was that a rule applied per-change is not a rule
/// enforced. An unadvertised key is that finding verbatim.
///
/// The second half is the footer's guard restated on this string: the
/// details must not promise the R6 clauses that are **not** built, for
/// exactly the reason the footer may not name them (**F75**: a key that
/// appears to work is worse than one that is missing).
#[test]
fn the_tui_details_advertise_the_transcript_overlay_key() {
    let tui = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "tui")
        .expect("/tui is registered");

    assert!(
        tui.details.contains("Ctrl+O"),
        "the details are where `Ctrl+O` is advertised: there is no \
             `keys::BINDINGS` row for it and the overlay's own footer is only \
             visible once it is already open, so a user who is not told here \
             is not told at all. The details read: {}",
        tui.details
    );
    assert!(
        tui.details.contains("overlay"),
        "...and the sentence must say what the key opens, not merely \
             that the key exists: {}",
        tui.details
    );
    let lowered = tui.details.to_ascii_lowercase();
    for unbuilt in ["search", "dump", "editor", "scrollback", "jump"] {
        assert!(
            !lowered.contains(unbuilt),
            "the details must not promise {unbuilt:?}: it is a later \
                 piece of R6 and nothing dispatches it, which is the same \
                 rule the overlay's footer is guarded by: {}",
            tui.details
        );
    }
}

/// Card **T4-S6-ordinal**: `Ctrl+X 1..9`'s only advertisement.
///
/// Deliberately the shape of the transcript overlay's test above, and for
/// its reasons. The chord cannot go in `keys::BINDINGS` -- that table is
/// the agent view's, swept over *that* decoder's key space, and this key
/// is read by the inline reader on the composer's surfaces -- and the
/// view's own footer is only visible once the view is open, which is the
/// one place the digits are **not** keys. So the details are the whole of
/// what a user is ever told, and **F106** is what happens when they are
/// not told there: for `Ctrl+O` every occurrence in the tree was a
/// comment or a test string. The rule that applied to it applies here
/// (**F82**: a rule applied per-change is not a rule enforced).
///
/// The three clauses asserted are the three a reader gets wrong: that the
/// digit is a **position** and not an agent number, that **nine** digits
/// reach the **first nine** rows and the rest are the view's arrows, and
/// that the inert cases are inert while the **draft survives** them.
///
/// The second half is the overlay test's negative guard: the details must
/// not promise what the chord cannot do. Multi-digit entry and wrapping
/// are both decided against, and a user who believed in either would
/// address the wrong agent in silence (**F75**: a key that appears to
/// work is worse than one that is missing).
#[test]
fn the_tui_details_advertise_the_ordinal_attach_chord() {
    let tui = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "tui")
        .expect("/tui is registered");

    assert!(
        tui.details.contains("Ctrl+X 1..9"),
        "the details are where `Ctrl+X 1..9` is advertised: there is no \
             `keys::BINDINGS` row for it and the agent view's own footer is \
             only visible once the view is open, which is the one place the \
             digits are not keys, so a user who is not told here is not told \
             at all. The details read: {}",
        tui.details
    );
    assert!(
        tui.details.contains("attach"),
        "...and the sentence must say what the chord does, not merely \
             that the keys exist: {}",
        tui.details
    );
    assert!(
        tui.details.contains("whatever its `#id`"),
        "...and it must say the digit is a **position**: the one thing a \
             reader gets wrong is reading `Ctrl+X 3` as agent #3, which is \
             the agent the chord deliberately does not attach to: {}",
        tui.details
    );
    assert!(
        tui.details.contains("first nine"),
        "...and it must say how far the nine digits reach, or a user \
             with twelve agents is left to discover the tenth row's silence: \
             {}",
        tui.details
    );
    assert!(
        tui.details.contains("Ctrl+X 0"),
        "...and it must say that `Ctrl+X 0` does nothing, because it is \
             the one digit that looks like it should be the first row: {}",
        tui.details
    );
    assert!(
        tui.details.contains("kept"),
        "...and it must say the typed line survives the chord (**F76**): \
             a user who believes a chord costs them their draft will not use \
             it with a line typed, which is the case it exists for: {}",
        tui.details
    );
    let lowered = tui.details.to_ascii_lowercase();
    for unbuilt in ["wrap", "ctrl+x 10", "two digits", "modulo"] {
        assert!(
            !lowered.contains(unbuilt),
            "the details must not promise {unbuilt:?}: multi-digit entry \
                 and wrapping are both decided against, and a user who \
                 believed in either would address the wrong agent in \
                 silence: {}",
            tui.details
        );
    }
}

// ------------------------------------------------------------------
// Card T4-S6a: `/detach`, the command requirement A5 names beside `←`.
//
// `/detach` is NOT a command yet — `SLASH_COMMANDS` has no `detach`
// entry and the dispatcher has no arm — so both tests below fail, and
// the PTY session in `tests/tui.rs` measures what a user gets instead
// (`unknown command: /detach`). The registry is the one source `/help`,
// `/help <command>`, the palette and both completion hosts read, so a
// command with no entry is a command nothing can discover (**F38**).
// ------------------------------------------------------------------

/// The registry entry, and what it must say.
///
/// The one clause worth pinning beyond the syntax is in the details:
/// detaching **stops nothing** (design §4.5 — stopping is `x` in the
/// view or `/jobs stop`). A user who reads this row as "cancel the
/// agent" would use it for the opposite of what it does, and that
/// sentence is the only place the command surface can say so.
#[test]
fn the_command_registry_advertises_the_detach_command() {
    let detach = SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == "detach")
        .expect("/detach is registered");
    assert_eq!(
        detach.args, "",
        "/detach takes no argument: there is one attachment"
    );
    assert_eq!(detach.syntax(), "/detach");
    assert_eq!(
        detach.alias_of, None,
        "/detach is a command in its own right, not an alias"
    );
    assert_eq!(
        detach.group,
        CommandGroup::AgentsJobs,
        "attachment is about a background agent, which is where /steer lives too"
    );
    assert!(
        detach.details.contains("stop"),
        "the details must say that detaching stops nothing, because a user \
             who reads this command as `cancel` uses it for the opposite of \
             what it does: {}",
        detach.details
    );

    // The registry-fed surfaces, which are traversals of the entry above
    // and so cannot drift from it.
    let help = slash_help("test-model", false);
    assert!(help.contains("/detach"), "/help lists it: {help}");
    assert!(
        help.contains(detach.description),
        "with its description: {help}"
    );
    let detail = slash_command_help("detach");
    assert!(
        detail.starts_with("/detach"),
        "/help detach leads with the syntax: {detail}"
    );
    let palette = slash_palette("deta", false, 80);
    assert!(
        palette.contains("/detach"),
        "the palette offers it for the prefix `deta`: {palette}"
    );
}
