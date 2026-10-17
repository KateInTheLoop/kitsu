//! The agent state hook: a user-configured local command kitsu runs when a
//! background agent changes state (`[hooks] on_agent_state`, requirements H3
//! and N7).
//!
//! This is the push counterpart of the advisory state file. Where the file lets
//! an outside tool poll, the hook lets kitsu tell a notifier — a desktop
//! notification, a multiplexer status refresh, a herder — the moment an agent
//! appears, finishes, fails or is cancelled.
//!
//! Three properties shape everything here.
//!
//! **No shell.** The configured command is split by [`split_command`] and
//! executed directly, so a stray metacharacter in a path can never become
//! execution and behaviour does not depend on which shell is installed. No
//! variable expansion happens: the event's values arrive in the child's
//! environment, and a user who wants interpolation points this at a script or
//! writes `sh -c ...` themselves — their explicit choice, not kitsu's default.
//!
//! **Once per transition.** [`transitions`] is called at every turn boundary,
//! so firing is driven by a *change* between two observations, never by the
//! mere presence of an agent. A stable agent must not re-fire every turn.
//!
//! **Never costs a turn.** [`fire`] spawns with stdin, stdout and stderr all
//! null — a hook that wrote to stderr would splice into kitsu's live status row
//! and break the byte-identical plain/non-TTY/assistive output guarantee — does
//! not block the caller, does not panic, and drops every failure: a spawn
//! error, a nonzero exit and a timeout are all silent.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use crate::subagent::thread::safe_inline_label;
use crate::subagent::AgentProgressStatus;

/// Longest `waiting_for` value handed to a child, in characters. The value
/// derives from activity labels that model-driven work influences, so it is
/// bounded as well as sanitized before it crosses into a user's process.
const MAX_WAITING_FOR_CHARS: usize = 256;

/// How long a hook child is awaited before it is dropped (and killed). Long
/// enough for a notifier, short enough that nothing accumulates.
const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// Split a configured command string into argv: whitespace separates words,
/// and double- or single-quoted runs keep their spaces. There is no escape
/// character, no expansion and no operator — every other byte, `$HOME` and
/// `;|&&`*` included, is ordinary argument text.
///
/// Returns `None` for an empty or whitespace-only command, and for unbalanced
/// quotes: a half-written command is a misconfiguration, not an argv.
pub(crate) fn split_command(command: &str) -> Option<Vec<String>> {
    let mut argv: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for character in command.chars() {
        match quote {
            Some(open) => {
                if character == open {
                    quote = None;
                } else {
                    current.push(character);
                }
            }
            None => {
                if character == '"' || character == '\'' {
                    quote = Some(character);
                    started = true;
                } else if character.is_whitespace() {
                    if started {
                        argv.push(std::mem::take(&mut current));
                        started = false;
                    }
                } else {
                    current.push(character);
                    started = true;
                }
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        argv.push(current);
    }
    if argv.is_empty() {
        None
    } else {
        Some(argv)
    }
}

/// The word for an agent's state. Byte-identical to the vocabulary `/jobs`
/// prints, so one agent is never described two ways.
pub(crate) fn agent_state_label(status: AgentProgressStatus) -> &'static str {
    crate::agent_progress_status_label(status)
}

/// One agent as observed at a turn boundary: the input to [`transitions`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentObservation {
    pub(crate) id: u64,
    pub(crate) state: &'static str,
    /// The agent's last activity label, if any. Absent means "nothing to
    /// report", which becomes the empty string rather than any placeholder.
    pub(crate) waiting_for: Option<String>,
}

/// One state change worth firing the hook for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HookEvent {
    pub(crate) id: u64,
    pub(crate) state: String,
    pub(crate) waiting_for: String,
}

/// The changes between two observations, sorted by id.
///
/// A newly seen id fires with its current state and a changed state fires with
/// the new one. An unchanged state fires nothing — this is the re-fire guard,
/// and it is what makes calling this at every turn boundary free. A vanished
/// id fires nothing either: a pruned record is bookkeeping, not a state change.
pub(crate) fn transitions(
    previous: &[AgentObservation],
    current: &[AgentObservation],
) -> Vec<HookEvent> {
    let before: BTreeMap<u64, &str> = previous
        .iter()
        .map(|observation| (observation.id, observation.state))
        .collect();
    let mut events: Vec<HookEvent> = current
        .iter()
        .filter(|observation| before.get(&observation.id) != Some(&observation.state))
        .map(|observation| HookEvent {
            id: observation.id,
            state: observation.state.to_string(),
            // An absent label must not travel through the sanitizer: it would
            // come back as the word "unknown", exporting a name no agent is
            // actually waiting on.
            waiting_for: observation
                .waiting_for
                .as_deref()
                .map(|label| safe_inline_label(label, MAX_WAITING_FOR_CHARS))
                .unwrap_or_default(),
        })
        .collect();
    events.sort_by_key(|event| event.id);
    events
}

/// The exact environment a hook child receives, in order, added to the
/// inherited environment.
pub(crate) fn hook_env(event: &HookEvent) -> [(&'static str, String); 3] {
    // Already-empty stays empty: sanitizing "" yields "unknown".
    let waiting_for = if event.waiting_for.is_empty() {
        String::new()
    } else {
        safe_inline_label(&event.waiting_for, MAX_WAITING_FOR_CHARS)
    };
    [
        ("KITSU_AGENT_ID", event.id.to_string()),
        (
            "KITSU_AGENT_STATE",
            safe_inline_label(event.state.as_str(), 64),
        ),
        ("KITSU_AGENT_WAITING_FOR", waiting_for),
    ]
}

/// Run the hook for one event, best effort.
///
/// Returns as soon as the child is spawned: the wait happens on a detached
/// task, so the caller never blocks. Standard input, output and error are all
/// null, so nothing can splice into kitsu's own output. A spawn error, nonzero
/// exit or timeout is dropped, and `kill_on_drop` plus a bounded wait mean the
/// child is reaped rather than left a zombie.
pub(crate) fn fire(argv: &[String], event: &HookEvent) {
    let Some((program, arguments)) = argv.split_first() else {
        return;
    };
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for (name, value) in hook_env(event) {
        command.env(name, value);
    }
    let Ok(mut child) = command.spawn() else {
        return;
    };
    tokio::spawn(async move {
        let _ = tokio::time::timeout(HOOK_TIMEOUT, child.wait()).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::AgentProgressStatus;

    /// Every variant, so adding one breaks the label tests rather than
    /// silently going unlabelled.
    const ALL_STATUSES: [AgentProgressStatus; 4] = [
        AgentProgressStatus::Running,
        AgentProgressStatus::Done,
        AgentProgressStatus::Failed,
        AgentProgressStatus::Cancelled,
    ];

    fn observation(id: u64, state: &'static str) -> AgentObservation {
        AgentObservation {
            id,
            state,
            waiting_for: None,
        }
    }

    fn waiting(id: u64, state: &'static str, waiting_for: &str) -> AgentObservation {
        AgentObservation {
            id,
            state,
            waiting_for: Some(waiting_for.to_string()),
        }
    }

    // -----------------------------------------------------------------
    // 1. Splitting: whitespace plus quoted runs, and emphatically no shell.
    // -----------------------------------------------------------------

    #[test]
    fn split_command_handles_words_and_quoted_runs() {
        let cases: [(&str, Vec<&str>); 7] = [
            ("notify-send kitsu", vec!["notify-send", "kitsu"]),
            (
                "\"/opt/my tools/notify\" --now",
                vec!["/opt/my tools/notify", "--now"],
            ),
            (
                "notify '--title=an agent'",
                vec!["notify", "--title=an agent"],
            ),
            (
                "notify \"two words\" '--and more' tail",
                vec!["notify", "two words", "--and more", "tail"],
            ),
            ("notify \"a\"\"b\"", vec!["notify", "ab"]),
            ("notify 'a'\"b\"c", vec!["notify", "abc"]),
            ("   spaced    out   ", vec!["spaced", "out"]),
        ];
        for (input, expected) in cases {
            let split = split_command(input).unwrap_or_else(|| panic!("must split {input:?}"));
            assert_eq!(split, expected, "splitting {input:?}");
        }
    }

    #[test]
    fn split_command_rejects_empty_whitespace_only_and_unbalanced_quotes() {
        for input in ["", "   ", "\t\n  "] {
            assert!(
                split_command(input).is_none(),
                "an empty command must be None: {input:?}"
            );
        }
        for input in [
            "notify \"unterminated",
            "notify 'unterminated",
            "\"only-open",
            "notify \"a\" \"b",
        ] {
            assert!(
                split_command(input).is_none(),
                "unbalanced quotes must be None: {input:?}"
            );
        }
    }

    /// The observable half of the no-shell contract: nothing kitsu does to the
    /// command can turn `$HOME` into a path, and no metacharacter is special.
    #[test]
    fn split_command_performs_no_expansion_at_all() {
        let split = split_command("notify $HOME").expect("must split");
        assert_eq!(split, vec!["notify".to_string(), "$HOME".to_string()]);
        assert_eq!(split[1].len(), 5, "$HOME must survive as literal bytes");

        let split = split_command("notify \"$KITSU_AGENT_STATE\"").expect("must split");
        assert_eq!(split[1], "$KITSU_AGENT_STATE");

        let split = split_command("notify a;rm -rf b | c && d `e` $(f) *").expect("must split");
        assert_eq!(
            split,
            vec![
                "notify".to_string(),
                "a;rm".to_string(),
                "-rf".to_string(),
                "b".to_string(),
                "|".to_string(),
                "c".to_string(),
                "&&".to_string(),
                "d".to_string(),
                "`e`".to_string(),
                "$(f)".to_string(),
                "*".to_string(),
            ],
            "metacharacters are ordinary argument bytes, never operators"
        );
    }

    // -----------------------------------------------------------------
    // 2. Labels, shared with /jobs.
    // -----------------------------------------------------------------

    #[test]
    fn agent_state_label_names_every_status() {
        assert_eq!(agent_state_label(AgentProgressStatus::Running), "running");
        assert_eq!(agent_state_label(AgentProgressStatus::Done), "done");
        assert_eq!(agent_state_label(AgentProgressStatus::Failed), "failed");
        assert_eq!(
            agent_state_label(AgentProgressStatus::Cancelled),
            "cancelled"
        );
        for status in ALL_STATUSES {
            let label = agent_state_label(status);
            assert!(!label.is_empty(), "{status:?} must have a label");
            assert!(label.is_ascii(), "{status:?} label must be ascii: {label}");
        }
    }

    /// One agent must never be described by two vocabularies: the hook's word
    /// and the word `/jobs` prints are the same bytes for the same status.
    #[test]
    fn agent_state_label_matches_the_jobs_vocabulary_byte_for_byte() {
        for status in ALL_STATUSES {
            assert_eq!(
                agent_state_label(status),
                crate::agent_progress_status_label(status),
                "the hook and /jobs must agree about {status:?}"
            );
        }
    }

    // -----------------------------------------------------------------
    // 3-4. Transitions: once per change, never once per observation.
    // -----------------------------------------------------------------

    #[test]
    fn transitions_fire_once_for_a_newly_seen_id() {
        let events = transitions(&[], &[observation(7, "running")]);
        assert_eq!(
            events.len(),
            1,
            "a new agent is a transition into existence"
        );
        assert_eq!(events[0].id, 7);
        assert_eq!(events[0].state, "running");
        assert_eq!(events[0].waiting_for, "");
    }

    #[test]
    fn transitions_fire_once_carrying_the_new_state() {
        let events = transitions(&[observation(7, "running")], &[observation(7, "done")]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, 7);
        assert_eq!(
            events[0].state, "done",
            "the event must carry the state moved into, not the one left"
        );
    }

    #[test]
    fn transitions_fire_nothing_for_an_unchanged_state() {
        assert!(
            transitions(&[observation(7, "running")], &[observation(7, "running")]).is_empty(),
            "a stable agent must not fire"
        );
    }

    #[test]
    fn transitions_fire_nothing_for_an_id_that_vanished() {
        assert!(
            transitions(&[observation(7, "done")], &[]).is_empty(),
            "a pruned record is bookkeeping, not a state change"
        );
        let events = transitions(
            &[observation(1, "running"), observation(2, "running")],
            &[observation(2, "done")],
        );
        assert_eq!(events.len(), 1, "only the surviving change fires");
        assert_eq!(events[0].id, 2);
    }

    #[test]
    fn transitions_are_sorted_by_id_so_firing_order_is_deterministic() {
        let previous = [observation(30, "running"), observation(4, "running")];
        let current = [
            observation(30, "failed"),
            observation(200, "running"),
            observation(4, "done"),
            observation(9, "cancelled"),
        ];

        let events = transitions(&previous, &current);

        assert_eq!(
            events.iter().map(|event| event.id).collect::<Vec<_>>(),
            vec![4, 9, 30, 200]
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.state.as_str())
                .collect::<Vec<_>>(),
            vec!["done", "cancelled", "failed", "running"]
        );
    }

    #[test]
    fn transitions_of_two_empty_observations_fire_nothing() {
        assert!(transitions(&[], &[]).is_empty());
    }

    /// The re-fire guard, stated on its own: the same observation on both sides
    /// is exactly what a turn boundary looks like when nothing moved, and it
    /// must be silent however many agents are retained.
    #[test]
    fn transitions_are_silent_when_current_is_compared_with_itself() {
        let current = [
            observation(1, "running"),
            waiting(2, "running", "a tool"),
            observation(3, "done"),
            observation(4, "failed"),
            observation(5, "cancelled"),
        ];

        assert!(
            transitions(&current, &current).is_empty(),
            "a stable registry must not fire on every turn boundary"
        );

        // And the idempotence that follows: fire once, then never again.
        let first = transitions(&[], &current);
        assert_eq!(
            first.len(),
            5,
            "the first observation fires each agent once"
        );
        assert!(transitions(&current, &current).is_empty());
    }

    // -----------------------------------------------------------------
    // 5. waiting_for: bounded, sanitized, absent means empty.
    // -----------------------------------------------------------------

    #[test]
    fn waiting_for_is_bounded_and_sanitized_and_absent_becomes_empty() {
        let long = "x".repeat(4096);
        let events = transitions(&[], &[waiting(1, "running", &long)]);
        assert_eq!(events.len(), 1);
        let bounded = &events[0].waiting_for;
        assert!(
            bounded.chars().count() < 512,
            "a long activity label must be bounded, got {} chars",
            bounded.chars().count()
        );
        assert!(!bounded.is_empty());

        let events = transitions(
            &[],
            &[waiting(2, "running", "line one\nline two\r\ttab\u{7}bell")],
        );
        assert_eq!(events.len(), 1);
        let cleaned = &events[0].waiting_for;
        for forbidden in ['\n', '\r', '\t', '\u{7}', '\u{1b}', '\0'] {
            assert!(
                !cleaned.contains(forbidden),
                "{forbidden:?} must not survive into {cleaned:?}"
            );
        }
        assert!(
            !cleaned.chars().any(char::is_control),
            "no control character may survive: {cleaned:?}"
        );

        assert_eq!(
            transitions(&[], &[observation(3, "done")])[0].waiting_for,
            "",
            "an absent waiting_for must become the empty string"
        );
    }

    // -----------------------------------------------------------------
    // 6. The environment handed to the child.
    // -----------------------------------------------------------------

    #[test]
    fn hook_env_names_exactly_three_variables_in_order() {
        let event = HookEvent {
            id: 4321,
            state: "done".to_string(),
            waiting_for: "a tool".to_string(),
        };

        let env = hook_env(&event);

        assert_eq!(
            env.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec![
                "KITSU_AGENT_ID",
                "KITSU_AGENT_STATE",
                "KITSU_AGENT_WAITING_FOR"
            ]
        );
        assert_eq!(env[0].1, "4321", "the id is a decimal string");
        assert_eq!(env[1].1, "done");
        assert_eq!(env[2].1, "a tool");
    }

    #[test]
    fn hook_env_values_carry_no_control_characters_or_nul() {
        let event = HookEvent {
            id: u64::MAX,
            state: "running".to_string(),
            waiting_for: "tool\nnoise\u{1b}[31m\0and\ttabs".to_string(),
        };

        let env = hook_env(&event);

        assert_eq!(env[0].1, u64::MAX.to_string());
        for (name, value) in &env {
            assert!(
                !value.chars().any(|character| character.is_control()
                    || character == '\0'
                    || character == '\u{1b}'),
                "{name} must be free of control characters and NUL: {value:?}"
            );
            assert!(
                value.len() < 4096,
                "{name} must be bounded, got {} bytes",
                value.len()
            );
        }
    }

    // -----------------------------------------------------------------
    // 8-10. Firing. The only impure part, and the only place a child runs.
    // -----------------------------------------------------------------

    /// Polls for `path` until `deadline`, returning its contents. Polling
    /// rather than sleeping a fixed time keeps these tests off the clock: a
    /// slow machine waits longer, a fast one finishes at once.
    #[cfg(unix)]
    async fn await_file(path: &std::path::Path, timeout: std::time::Duration) -> Option<String> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Ok(body) = std::fs::read_to_string(path) {
                if !body.is_empty() {
                    return Some(body);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        None
    }

    /// The child writes its three lines one at a time, so a file that merely
    /// exists may hold only the first. Wait for the whole record.
    async fn await_file_lines(
        path: &std::path::Path,
        lines: usize,
        timeout: std::time::Duration,
    ) -> Option<String> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Ok(body) = std::fs::read_to_string(path) {
                if body.lines().count() >= lines {
                    return Some(body);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        None
    }

    /// The end-to-end proof, and deliberately two proofs at once: the three
    /// variables reach the child, and the shell in this argv is *the user's own
    /// command* — `sh` chosen by them — not something kitsu wrapped around it.
    #[cfg(unix)]
    #[tokio::test]
    async fn fire_runs_the_command_and_exports_the_three_variables() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("hook.out");
        let script = format!(
            "printf '%s' \"$KITSU_AGENT_ID|$KITSU_AGENT_STATE|$KITSU_AGENT_WAITING_FOR\" > {}",
            out.display()
        );
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script];
        let event = HookEvent {
            id: 77,
            state: "done".to_string(),
            waiting_for: "a tool".to_string(),
        };

        fire(&argv, &event);

        let body = await_file(&out, std::time::Duration::from_secs(10))
            .await
            .expect("the hook command must have run and written the file");
        assert_eq!(body, "77|done|a tool");
    }

    /// A bogus argv[0] is the common misconfiguration. It must cost nothing:
    /// no panic, no block, no propagated error.
    #[cfg(unix)]
    #[tokio::test]
    async fn fire_is_harmless_when_the_command_does_not_exist() {
        let argv = vec![
            "/nonexistent/kitsu-hook-does-not-exist".to_string(),
            "--whatever".to_string(),
        ];
        let event = HookEvent {
            id: 1,
            state: "failed".to_string(),
            waiting_for: String::new(),
        };

        let started = std::time::Instant::now();
        fire(&argv, &event);
        fire(&argv, &event);
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "fire must not block the caller, took {elapsed:?}"
        );
    }

    /// The nulled-stdio guarantee, asserted from inside the child rather than
    /// by capturing this process's output: a unit test cannot redirect the
    /// harness's own fds, but the child can report what its fds point at, and
    /// `/dev/null` on all three is exactly the claim.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn fire_gives_the_child_null_stdin_stdout_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("fds.out");
        let script = format!(
            "exec 7<&0 8>&1 9>&2; {{ readlink /proc/self/fd/7; readlink /proc/self/fd/8; readlink /proc/self/fd/9; }} > {}; echo noise; echo noise >&2",
            out.display()
        );
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script];
        let event = HookEvent {
            id: 2,
            state: "running".to_string(),
            waiting_for: String::new(),
        };

        fire(&argv, &event);

        let body = await_file_lines(&out, 3, std::time::Duration::from_secs(10))
            .await
            .expect("the hook command must have run");
        // Read through duplicates taken before the test's own redirect, so the
        // fds reported are the ones `fire` handed the child.
        let fds = body.lines().collect::<Vec<_>>();
        assert_eq!(
            fds,
            vec!["/dev/null", "/dev/null", "/dev/null"],
            "stdin, stdout and stderr must all be null: {body:?}"
        );
    }

    /// A long-running hook must neither block the caller nor be waited on
    /// unboundedly: the child is spawned, the caller returns at once, and the
    /// runtime drops with the child still reaped by the implementation's own
    /// bounded wait rather than left a zombie.
    #[cfg(unix)]
    #[tokio::test]
    async fn fire_returns_immediately_for_a_slow_command() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 30".to_string(),
        ];
        let event = HookEvent {
            id: 3,
            state: "running".to_string(),
            waiting_for: String::new(),
        };

        let started = std::time::Instant::now();
        fire(&argv, &event);
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "fire must not wait for the hook, took {elapsed:?}"
        );
    }
}
