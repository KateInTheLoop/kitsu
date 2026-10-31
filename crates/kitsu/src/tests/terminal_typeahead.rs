use super::*;

#[cfg(unix)]
#[test]
fn action_resolution_dispatches_trimmed_submissions_and_touches_the_queue() {
    let mut editor = kitsu_editor::TypeaheadEditor::new(Vec::new());
    let mut queue = prompt_queue::PromptQueue::new(4);
    let mut dispatched = Vec::new();
    let (effects, result) = resolve(&mut editor, b"  hello  \r\r", &mut queue, &mut dispatched);
    // Trimmed once, empty second Enter skipped entirely.
    assert_eq!(dispatched, vec!["hello"]);
    assert!(effects.queue_touched);
    assert!(effects.needs_redraw);
    assert!(!result.eof);
    assert_eq!(effects.raise_signal, None);
}

#[cfg(unix)]
#[test]
fn action_resolution_recall_pops_newest_and_falls_back_to_history() {
    let mut editor = kitsu_editor::TypeaheadEditor::new(vec!["from history".to_string()]);
    let mut queue = prompt_queue::PromptQueue::new(4);
    queue.enqueue("older").unwrap();
    queue.enqueue("newest").unwrap();
    let mut dispatched = Vec::new();
    let (effects, _) = resolve(&mut editor, b"\x1b[A", &mut queue, &mut dispatched);
    assert_eq!(editor.buffer_string(), "newest");
    assert_eq!(effects.recalled_remaining, Some(1));
    assert!(effects.queue_touched && effects.needs_redraw);
    assert_eq!(queue.status().queued, 1);
    // Empty queue: the same ↑ falls back to the plain history walk.
    let mut editor = kitsu_editor::TypeaheadEditor::new(vec!["from history".to_string()]);
    let mut queue = prompt_queue::PromptQueue::new(4);
    let (effects, _) = resolve(&mut editor, b"\x1b[A", &mut queue, &mut dispatched);
    assert_eq!(editor.buffer_string(), "from history");
    assert_eq!(effects.recalled_remaining, None);
    assert!(!effects.queue_touched);
}

#[cfg(unix)]
#[test]
fn action_resolution_maps_control_actions_to_signals_and_eof() {
    let mut editor = kitsu_editor::TypeaheadEditor::new(Vec::new());
    let mut queue = prompt_queue::PromptQueue::new(4);
    let mut dispatched = Vec::new();
    // Ctrl+C on empty draft → SIGINT effect (raise happens in the caller).
    let (effects, _) = resolve(&mut editor, &[0x03], &mut queue, &mut dispatched);
    assert_eq!(effects.raise_signal, Some(libc::SIGINT));
    // Ctrl+Z → SIGTSTP (a swapped mapping must fail this).
    let (effects, _) = resolve(&mut editor, &[0x1a], &mut queue, &mut dispatched);
    assert_eq!(effects.raise_signal, Some(libc::SIGTSTP));
    // ^C then ^\ in one batch: the LAST signal wins.
    let (effects, _) = resolve(&mut editor, &[0x03, 0x1c], &mut queue, &mut dispatched);
    assert_eq!(effects.raise_signal, Some(libc::SIGQUIT));
    // Ctrl+D on empty draft → EOF, no signal.
    let (effects, result) = resolve(&mut editor, &[0x04], &mut queue, &mut dispatched);
    assert!(result.eof);
    assert_eq!(effects.raise_signal, None);
}

#[cfg(unix)]
#[test]
fn action_resolution_tab_accepts_the_slash_hint() {
    let mut editor = kitsu_editor::TypeaheadEditor::new(Vec::new());
    let mut queue = prompt_queue::PromptQueue::new(4);
    let mut dispatched = Vec::new();
    let (effects, _) = resolve(&mut editor, b"/ste\t", &mut queue, &mut dispatched);
    assert_eq!(editor.buffer_string(), "/steer");
    assert!(effects.needs_redraw);
    // Tab with no matching hint leaves the draft alone.
    let mut editor = kitsu_editor::TypeaheadEditor::new(Vec::new());
    let (_, _) = resolve(&mut editor, b"/zzz\t", &mut queue, &mut dispatched);
    assert_eq!(editor.buffer_string(), "/zzz");
}

#[test]
fn pregrant_spec_accepts_both_shapes_and_rejects_ambiguity() {
    let launch = std::path::Path::new("/launch");
    let resolve = |encoded: &str| {
        PregrantedCommandSpec::parse(encoded)
            .and_then(|spec| spec.resolve(launch))
            .map_err(|error| error.message)
    };

    let legacy = resolve("[\"pwd\"]").expect("legacy array form still parses");
    assert_eq!(legacy.cwd, launch);
    assert_eq!(legacy.cwd_source, CwdSource::LaunchDirectory);

    let explicit = resolve("{\"argv\":[\"pwd\"],\"cwd\":\"/proj\"}").expect("object form parses");
    assert_eq!(explicit.cwd, std::path::Path::new("/proj"));
    assert_eq!(explicit.cwd_source, CwdSource::Explicit);

    // A misspelled key must be a startup error, never a silent launch-cwd grant.
    let unknown = resolve("{\"argv\":[\"pwd\"],\"cd\":\"/proj\"}").unwrap_err();
    assert!(unknown.contains("invalid --allow-command value"));

    let relative = resolve("{\"argv\":[\"pwd\"],\"cwd\":\"proj\"}").unwrap_err();
    assert!(relative.contains("must be an absolute path"));

    let empty = resolve("[]").unwrap_err();
    assert!(empty.contains("at least one program"));
}

#[test]
#[serial]
fn command_env_spec_bare_passthrough_resolves_from_process_environment() {
    let _guard = EnvVarGuard::set("KITSU_TEST_COMMAND_ENV_PROBE", "probe-value");
    let (name, value) =
        expect_ok(expect_ok(CommandEnvSpec::parse("KITSU_TEST_COMMAND_ENV_PROBE")).resolve());
    assert_eq!(name, "KITSU_TEST_COMMAND_ENV_PROBE");
    assert_eq!(value, "probe-value");
}

#[test]
fn command_env_spec_bare_passthrough_of_a_secret_shaped_name_is_refused() {
    // SAFETY-adjacent note: deliberately do NOT set this var — the point
    // is that the name alone is enough to refuse the bare form, whether
    // or not it happens to be set in this process.
    let error = expect_ok(CommandEnvSpec::parse("MY_API_KEY"))
        .resolve()
        .expect_err("secret-shaped bare passthrough must be refused");
    let message = error.to_string();
    assert!(message.contains("MY_API_KEY"), "{message}");
    assert!(message.contains("--command-env MY_API_KEY="), "{message}");
}

#[test]
fn command_env_spec_explicit_form_allows_a_secret_shaped_name() {
    // The explicit form is always allowed: the caller is typing the
    // literal value directly rather than asking kitsu to forward an
    // ambient one from its own environment.
    let (name, value) =
        expect_ok(expect_ok(CommandEnvSpec::parse("MY_API_KEY=sk-explicit-value")).resolve());
    assert_eq!(name, "MY_API_KEY");
    assert_eq!(value, "sk-explicit-value");
}

#[test]
fn command_env_spec_bare_passthrough_of_an_unset_var_is_a_clear_error() {
    let name = "KITSU_TEST_COMMAND_ENV_DEFINITELY_UNSET";
    assert!(std::env::var(name).is_err(), "test precondition");
    let error = expect_ok(CommandEnvSpec::parse(name))
        .resolve()
        .expect_err("unset bare passthrough must error, not silently drop the flag");
    assert!(error.to_string().contains(name));
}

// --- site 2: the mid-turn dispatcher (the plan's named acceptance) ---

/// T2-S5's acceptance verbatim: "reply during a running turn releases the
/// agent". The mid-turn dispatcher is the only `/jobs` route reachable
/// while a turn is in flight, so this is where that sentence is pinned.
#[cfg(unix)]
#[tokio::test]
async fn mid_turn_reply_releases_a_blocked_agent_with_the_decision_typed() {
    for (letter, expected) in [
        ("y", Decision::AllowOnce),
        ("a", Decision::AllowAlways),
        ("n", Decision::Deny),
    ] {
        let (registry, id, answer) = registry_with_one_blocked_agent().await;
        let reported = handle_active_jobs_command(
            &registry,
            &tokio::runtime::Handle::current(),
            &format!("reply {id} {letter}"),
        )
        .expect("replying to a blocked agent is not an error");
        assert!(
            reported.contains(&format!("#{id}")),
            "the receipt names the agent it released: {reported}"
        );
        assert_eq!(
            answer.await.expect("the gate's oneshot is resolved"),
            expected,
            "the gate sees exactly the decision `{letter}` spells"
        );
        // Released, not terminated: the agent goes back to running.
        let status = registry
            .progress_snapshot()
            .agents
            .into_iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent.status);
        assert_eq!(
            status,
            Some(subagent::AgentProgressStatus::Running),
            "an answered agent resumes rather than ending"
        );
    }
}

/// The same three decisions under their long spellings, which the
/// foreground approval prompt already accepts (`parse_permission_answer`:
/// `y|yes`, `a|always`, `n|no`, case-insensitively). `/jobs reply` reads
/// its key the same way, so the two prompts never disagree.
#[cfg(unix)]
#[tokio::test]
async fn mid_turn_reply_accepts_the_same_keys_the_foreground_prompt_accepts() {
    for (word, expected) in [
        ("yes", Decision::AllowOnce),
        ("ALWAYS", Decision::AllowAlways),
        ("No", Decision::Deny),
    ] {
        let (_registry, id, answer) = registry_with_one_blocked_agent().await;
        handle_active_jobs_command(
            &_registry,
            &tokio::runtime::Handle::current(),
            &format!("reply {id} {word}"),
        )
        .expect("a long-form key is accepted");
        assert_eq!(
            answer.await.expect("the gate's oneshot is resolved"),
            expected,
            "`{word}` means the same thing at both prompts"
        );
    }
}

/// The three outcomes are distinguishable in what the user reads. A human
/// typing an id into a racy world must be able to tell "nothing happened"
/// from "it worked".
#[cfg(unix)]
#[tokio::test]
async fn mid_turn_reply_distinguishes_answered_from_not_waiting_and_unknown() {
    let (registry, blocked, _answer) = registry_with_one_blocked_agent().await;
    let running = registry.start("busy, not parked".to_string()).unwrap();

    let answered = handle_active_jobs_command(
        &registry,
        &tokio::runtime::Handle::current(),
        &format!("reply {blocked} y"),
    )
    .expect("the blocked agent is answered");

    let not_blocked = handle_active_jobs_command(
        &registry,
        &tokio::runtime::Handle::current(),
        &format!("reply {running} y"),
    );
    let unknown = handle_active_jobs_command(
        &registry,
        &tokio::runtime::Handle::current(),
        "reply 9999 y",
    );

    assert!(
        not_blocked
            .as_ref()
            .err()
            .or(not_blocked.as_ref().ok())
            .expect("some message")
            .contains("not waiting on an answer"),
        "a running agent is reported as not waiting: {not_blocked:?}"
    );
    assert_eq!(
        unknown,
        Err("no background job #9999".to_string()),
        "an unknown id reuses the one wording every other subcommand uses"
    );
    assert_ne!(
        Ok(answered.clone()),
        not_blocked,
        "answered and not-waiting must not read alike"
    );
    assert_ne!(
        Ok(answered),
        unknown,
        "answered and unknown must not read alike"
    );
}

/// Bad input answers nothing. A missing id, a non-numeric id, a missing
/// key and an unknown key are all usage errors, and the parked agent is
/// still parked afterwards.
#[cfg(unix)]
#[tokio::test]
async fn mid_turn_reply_rejects_bad_input_and_answers_nothing() {
    let (registry, id, mut answer) = registry_with_one_blocked_agent().await;
    for argument in [
        "reply".to_string(),
        "reply y".to_string(),
        format!("reply {id}"),
        "reply notanumber y".to_string(),
        format!("reply {id} maybe"),
        format!("reply {id} y extra"),
    ] {
        let error =
            handle_active_jobs_command(&registry, &tokio::runtime::Handle::current(), &argument)
                .expect_err(&format!("`{argument}` must be rejected"));
        assert!(
            error.contains("usage: /jobs reply <id> <y|a|n>"),
            "`{argument}` must print the reply usage line, not {error}"
        );
        assert!(
            matches!(
                answer.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "`{argument}` must not resolve the parked gate"
        );
    }
    let status = registry
        .progress_snapshot()
        .agents
        .into_iter()
        .find(|agent| agent.id == id)
        .map(|agent| agent.status);
    assert_eq!(
        status,
        Some(subagent::AgentProgressStatus::Blocked),
        "the agent is still parked after every rejection"
    );
}

/// Site 7: the usage line an unknown subcommand prints is the other half
/// of discoverability, and it is a separate literal from the `args:` row.
/// The mid-turn dispatcher returns its own line, so it is asserted here;
/// the idle dispatcher prints to stderr and is covered by the scripted
/// `test_script_jobs_reply_*` tests in `tests/cli.rs`.
#[cfg(unix)]
#[tokio::test]
async fn the_mid_turn_usage_line_lists_reply() {
    let registry = subagent::AgentRegistry::new();
    let usage = handle_active_jobs_command(
        &registry,
        &tokio::runtime::Handle::current(),
        "definitely-not-a-subcommand",
    )
    .expect_err("an unknown subcommand is a usage error");
    assert!(
        usage.contains("reply <id> <y|a|n>"),
        "the active-turn controls line must advertise reply: {usage}"
    );
}

/// **Mid-turn, the decision pinned.** A `/tui` submitted as type-ahead is
/// refused by name and, critically, is **not** enqueued as a prompt: the
/// assertion that matters is the empty queue, because the pre-feature
/// behaviour is a silent enqueue that would later be sent to the model.
#[cfg(unix)]
#[tokio::test]
async fn mid_turn_tui_is_refused_rather_than_queued_as_a_prompt() {
    for line in ["/tui", "/tui inline", "/tui classic", "/tui fullscreen"] {
        let queue = Arc::new(std::sync::Mutex::new(prompt_queue::PromptQueue::new(8)));
        let dispatcher = TypeaheadDispatcher {
            queue: queue.clone(),
            steering: prompt_queue::SteeringChannel::new(8).expect("steering channel"),
            registry: subagent::AgentRegistry::new(),
            runtime: tokio::runtime::Handle::current(),
            live: false,
        };
        let mut result = TypeaheadCaptureResult::default();
        dispatcher.dispatch(line, &mut result);

        assert_eq!(
            queue.lock().unwrap().iter().len(),
            0,
            "{line:?} must never become a future prompt for the model"
        );
        assert_eq!(result.captured, 0, "{line:?} is not captured type-ahead");
        let [outcome] = &result.outcomes[..] else {
            panic!(
                "{line:?} must produce exactly one narrated outcome, got {}",
                result.outcomes.len()
            );
        };
        match outcome {
            TypeaheadOutcome::Notice(notice) => assert_eq!(
                notice, TUI_MID_TURN_REFUSAL,
                "the refusal says when /tui is accepted"
            ),
            TypeaheadOutcome::SteeringError(error) | TypeaheadOutcome::QueueError(error) => {
                panic!(
                    "{line:?} is neither a steering nor a queue failure; it is a \
                         notice, so it is not narrated as `queue refused:`: {error}"
                )
            }
        }
    }

    // Non-vacuity: the same dispatcher still queues an ordinary line, so
    // the empty queue above is a refusal and not a broken fixture.
    let queue = Arc::new(std::sync::Mutex::new(prompt_queue::PromptQueue::new(8)));
    let dispatcher = TypeaheadDispatcher {
        queue: queue.clone(),
        steering: prompt_queue::SteeringChannel::new(8).expect("steering channel"),
        registry: subagent::AgentRegistry::new(),
        runtime: tokio::runtime::Handle::current(),
        live: false,
    };
    let mut result = TypeaheadCaptureResult::default();
    dispatcher.dispatch("an ordinary follow-up", &mut result);
    assert_eq!(queue.lock().unwrap().iter().len(), 1);
    assert_eq!(result.captured, 1);
}

/// Card **F57-reach**, the mid-turn half: the registry's *own* entries,
/// every one of them, driven through `TypeaheadDispatcher::dispatch`.
///
/// The subjects are enumerated from `SLASH_COMMANDS`; no command name is
/// typed here, so an entry added without thought about the mid-turn path
/// is a subject of this test the moment it is registered.
///
/// What it pins, and why this is the honest claim rather than "every
/// command is handled mid-turn": the mid-turn path handles a small set
/// immediately **by design** and queues the rest, and a queued line is
/// not a dead end — the idle loop dequeues it and, when it starts with
/// `/`, dispatches it through `handle_slash` (the `strip_prefix('/')`
/// route at the queue drain). So mid-turn reachability is the composition
/// of two properties: the leading slash survives the queue (here), and
/// the name has a dispatch arm (the CLI sweep
/// `every_registered_slash_command_reaches_a_dispatch_arm`). Either half
/// alone lets **F57**'s second failure mode through — a registered
/// command with no arm, silently banked as a future prompt.
///
/// Deliberately **not** covered: arguments and subcommands (the subject
/// is the top-level word, which is what F57's instance was), and *which*
/// of the two legal shapes a given command takes — moving a command
/// between "handled now" and "queued for the turn boundary" is a design
/// decision each command's own test owns.
#[cfg(unix)]
#[tokio::test]
async fn every_registered_command_typed_mid_turn_is_handled_or_queued_verbatim() {
    let mut checked = 0usize;
    for spec in SLASH_COMMANDS {
        let line = format!("/{}", spec.name);
        let queue = Arc::new(std::sync::Mutex::new(prompt_queue::PromptQueue::new(4)));
        let dispatcher = TypeaheadDispatcher {
            queue: queue.clone(),
            steering: prompt_queue::SteeringChannel::new(8).expect("steering channel"),
            registry: subagent::AgentRegistry::new(),
            runtime: tokio::runtime::Handle::current(),
            live: false,
        };
        let mut result = TypeaheadCaptureResult::default();
        dispatcher.dispatch(&line, &mut result);

        let queued: Vec<String> = queue
            .lock()
            .expect("prompt queue")
            .iter()
            .map(str::to_string)
            .collect();
        assert_eq!(
            result.captured,
            queued.len(),
            "{line:?}: the capture count and the queue must agree"
        );
        match queued.len() {
            // Handled on the spot. Then it must have said so: a mid-turn
            // command that acts and narrates nothing is **F71** (a
            // message only the test can see) in the other direction.
            0 => assert!(
                !result.outcomes.is_empty(),
                "{line:?} was neither queued nor narrated, so a user who \
                     typed it mid-turn got silence"
            ),
            // Queued for the turn boundary. The leading slash is what
            // makes the drain dispatch it as a command instead of sending
            // its literal text to the model, so the queued entry must be
            // the submitted line byte for byte.
            1 => assert_eq!(
                queued[0], line,
                "{line:?} must reach the queue verbatim, slash intact, or \
                     the drain will not route it to the slash dispatcher"
            ),
            _ => panic!("{line:?} produced {} queue entries", queued.len()),
        }
        checked += 1;
    }
    assert_eq!(
        checked,
        SLASH_COMMANDS.len(),
        "every registry entry is a subject of this sweep"
    );
    assert!(checked > 0, "the registry is not empty");
}

/// Card **T4-S8**, requirement **A8**, during-turn half: the ambient
/// tick stays inside the 250 ms budget.
///
/// Finding **F50**: A8 was met by construction — a 50 ms `libc::poll`
/// timeout, twenty times finer than the clock it carries — and asserted
/// **nowhere**, so raising the interval to a second would have broken a
/// numbered requirement silently. This is the loud failure. It is a
/// structural assertion on the interval the loop actually polls with, not
/// a measurement: nothing sleeps, so it cannot join this tree's four open
/// timing flakes.
///
/// The guard is only as strong as the constant being the one the loop
/// uses, which is why `AMBIENT_TICK_POLL_MS` is referenced by
/// `run_live_typeahead`'s poll and by nothing else.
#[cfg(unix)]
#[test]
fn the_during_turn_ambient_tick_stays_inside_a8s_budget() {
    let tick = std::time::Duration::from_millis(
        u64::try_from(AMBIENT_TICK_POLL_MS).expect("a positive poll timeout"),
    );
    assert!(
        tick <= tui_bridge::AMBIENT_LATENCY_BUDGET,
        "A8 gives the ambient background-agent counter {:?} from a registry change; \
             `run_live_typeahead` re-pulls the counts once per {tick:?} poll tick",
        tui_bridge::AMBIENT_LATENCY_BUDGET,
    );
    assert!(
        tick > std::time::Duration::ZERO,
        "a zero timeout is a busy loop, not a tick"
    );
}
