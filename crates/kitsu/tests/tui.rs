//! A **styled** PTY harness for the binary, and the first measurements taken
//! with it (T0 proof of concept, question Q3).
//!
//! `tests/cli.rs`'s `run_plain_interactive_chat_pty` hardcodes `--plain`,
//! which turns styling and the inline viewport off entirely, so it cannot see
//! anything the viewport does. This harness spawns the same binary with
//! styling live — no `--plain`, no `--no-stream` — on a PTY whose window size
//! it sets itself, and feeds everything the child wrote into a `vt100`
//! emulator of exactly that geometry. Tests can then assert on the parsed
//! *screen* (what a user would see) and on the *raw bytes* (which escape
//! sequences were emitted).
//!
//! # Finding F17: what this harness exists to measure
//!
//! The bridge's unit tests prove the `Surface` seam routes to injected sinks.
//! They cannot prove *exclusivity*: in the real process `main.rs` holds ~392
//! direct `eprintln!`/`println!` calls that bypass the seam and write straight
//! to the descriptors. In inline mode those writes land somewhere relative to
//! the DECSTBM region and the pinned viewport row, and only a real terminal
//! can say where. See `inline_mode_*` below for what was actually observed.
//!
//! Every read loop here has a deadline and fails with the transcript attached
//! rather than blocking the suite.
//!
//! # Resize, now tested here (findings F16 and F24)
//!
//! The module used to record that a resize test was out of reach: [`Step`] had
//! no action that sends no bytes, [`Session`]'s emulator was fixed at `ROWS` x
//! `COLS`, and the one thing only a real terminal exercises -- that a
//! `SIGWINCH` arriving while the reader blocks in `read()` surfaces as `EINTR`
//! rather than being restarted under `SA_RESTART` -- was judged untestable
//! outside a human's terminal.
//!
//! Both gaps are closed below ([`Action::Resize`], and a parser that is
//! `set_size`d at the same offset in the byte stream where the child was
//! resized), and **the judgement was wrong**: this harness signals a separate
//! process that is genuinely blocked in a real `read` on a real tty, so with
//! `SA_RESTART` set the child would not wake, would emit no new geometry, and
//! the resize tests below would fail on their triggers. See
//! `inline_mode_resize_*`.
//!
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use serial_test::serial;
use tempfile::TempDir;

const CHAT_PATH: &str = "/v1/chat/completions";
const TOKEN: &str = "test-token";

/// The window size every test in this file runs under. Set explicitly with
/// `TIOCSWINSZ` before the spawn so the geometry under test is this and not
/// whatever the developer's or CI runner's terminal happens to be.
const ROWS: u16 = 24;
const COLS: u16 = 80;

/// How long a session may take before the harness kills the child and fails.
const DEADLINE: Duration = Duration::from_secs(20);

/// How long the driver waits for a resize to show up in the stream before
/// re-delivering `SIGWINCH`. See the `rearm` binding in
/// [`run_styled_chat_pty_with`] for why re-delivery is both necessary and
/// harmless.
const REARM: Duration = Duration::from_millis(100);

/// Write `~/.kitsu/config.toml` under `home` with one unreachable gateway.
///
/// The scripts below only ever submit slash commands, so no request is made;
/// the config exists purely so startup finds a gateway and a model name.
fn write_config(home: &Path) {
    let dir = home.join(".kitsu");
    fs::create_dir_all(&dir).expect("create ~/.kitsu");
    let config = format!(
        r#"default_gateway = "example"

[gateways.example]
flavor       = "openai"
base_url     = "https://gateway.invalid"
chat_path    = "{CHAT_PATH}"
models_path  = ""
model        = "example-model-v1"
auth         = {{ method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN", expires_env = "ANTHROPIC_TOKEN_EXPIRES", refresh_hint = "run `refresh-token` to refresh the gateway JWT" }}
"#
    );
    fs::write(dir.join("config.toml"), config).expect("write config.toml");
}

/// What a [`Step`] does once its trigger fires.
#[derive(Clone, Copy)]
enum Action {
    /// Write these bytes to the master: a keystroke, a line, a control byte.
    Send(&'static str),
    /// Resize the window to `rows` x `cols` and **send no input at all**:
    /// `TIOCSWINSZ` on the master, then `SIGWINCH` to the child.
    ///
    /// This is the action the resize tests are built on, and sending no bytes
    /// is the whole point of it -- any keystroke would wake the child's
    /// blocking `read` by itself and the measurement would say nothing about
    /// how the signal handler was installed. See
    /// [`run_styled_chat_pty_with`]'s driver for the re-arm, and the
    /// `inline_mode_resize_*` tests for why.
    Resize { rows: u16, cols: u16 },
}

/// One scripted action: wait until `trigger` appears in everything read so far,
/// then perform `action`. Polling on a trigger rather than sleeping is what
/// keeps the harness from racing the child's prompt.
struct Step {
    trigger: &'static str,
    action: Action,
}

/// A step that writes `bytes` to the master once `trigger` is seen.
fn send(trigger: &'static str, bytes: &'static str) -> Step {
    Step {
        trigger,
        action: Action::Send(bytes),
    }
}

/// A step that resizes the window to `rows` x `cols` once `trigger` is seen,
/// writing nothing to the master.
fn resize(trigger: &'static str, rows: u16, cols: u16) -> Step {
    Step {
        trigger,
        action: Action::Resize { rows, cols },
    }
}

/// Set the window size on a PTY master.
fn set_window_size(master: libc::c_int, rows: u16, cols: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `master` is a live PTY master and `size` is a valid winsize.
    assert_eq!(
        unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &size) },
        0,
        "set the PTY window size to {rows}x{cols}"
    );
}

/// Where in the byte stream the window was resized, and to what.
///
/// The emulator has to be `set_size`d at exactly this offset: resize it any
/// earlier or later and every absolute row address on the far side of the
/// resize is modelled against the wrong geometry.
#[derive(Clone, Copy)]
struct Resized {
    at: usize,
    rows: u16,
    cols: u16,
}

/// Everything a finished session left behind: the raw byte stream off the PTY
/// master and the emulator that consumed it.
struct Session {
    raw: Vec<u8>,
    status: std::process::ExitStatus,
    /// How many times the driver observed the child **stopped** (`WIFSTOPPED`)
    /// and resumed it with `SIGCONT`. Always zero unless
    /// [`Options::resume_on_stop`] was set, because the default poll cannot
    /// see a stopped child at all (see `poll_child`).
    stops: usize,
    /// `raw.len()` at the moment each scripted action was performed, so a test
    /// can replay a *prefix* of the stream and see the screen as it stood
    /// mid-session rather than only after exit. `marks[1]` is the screen just
    /// before `/quit` went in, which is the one the F17 question is about.
    ///
    /// A [`Action::Resize`] step marks the stream like any other, which is what
    /// lets a resize test assert on the screen *after* the signal was
    /// delivered and *before* the next byte of input went in.
    marks: Vec<usize>,
    /// Every window resize the script performed, in stream order. Empty for
    /// every script that never resizes, which is all nine original ones.
    resizes: Vec<Resized>,
}

impl Session {
    /// The raw bytes as lossy text, for asserting on escape sequences.
    fn raw_text(&self) -> String {
        String::from_utf8_lossy(&self.raw).into_owned()
    }

    /// How many times `needle` occurs in the raw byte stream.
    fn raw_count(&self, needle: &str) -> usize {
        let (hay, needle) = (self.raw.as_slice(), needle.as_bytes());
        if needle.is_empty() || hay.len() < needle.len() {
            return 0;
        }
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    /// Replay the first `upto` bytes into a fresh emulator, applying every
    /// resize the script performed at the offset it was performed at.
    ///
    /// Splitting the stream at the resize offsets is not a nicety: `vt100`'s
    /// screen is a fixed grid and the child addresses rows absolutely, so a
    /// parser left at `ROWS` would model every post-resize `ESC[30;1H` against
    /// a 24-row screen and silently discard it.
    fn parse(&self, upto: usize) -> vt100::Parser {
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        let mut pos = 0usize;
        for resized in &self.resizes {
            if resized.at >= upto {
                break;
            }
            parser.process(&self.raw[pos..resized.at]);
            parser.set_size(resized.rows, resized.cols);
            pos = resized.at;
        }
        parser.process(&self.raw[pos..upto]);
        parser
    }

    /// One parsed screen, one `String` per row, trailing blanks trimmed per
    /// row by `vt100` itself, at whatever geometry was in force by then.
    fn rows_of(parser: &vt100::Parser) -> Vec<String> {
        let (_rows, cols) = parser.screen().size();
        parser.screen().rows(0, cols).collect()
    }

    /// The final screen.
    fn screen(&self) -> Vec<String> {
        Self::rows_of(&self.parse(self.raw.len()))
    }

    /// The screen as it stood when scripted action `step` was performed: the
    /// first `marks[step]` bytes replayed into a fresh emulator.
    fn screen_at(&self, step: usize) -> Vec<String> {
        Self::rows_of(&self.parse(self.marks[step]))
    }

    /// The bytes the child wrote between two scripted actions, as lossy text.
    ///
    /// For a resize test this is the interesting window: everything the child
    /// emitted *in response to the signal alone*, with no input in it.
    fn raw_between(&self, from: usize, to: usize) -> String {
        String::from_utf8_lossy(&self.raw[self.marks[from]..self.marks[to]]).into_owned()
    }

    /// A numbered dump of any screen, for failure messages.
    fn dump(rows: &[String]) -> String {
        rows.iter()
            .enumerate()
            .map(|(i, r)| format!("{:>2}: {r:?}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The whole final screen rendered with row numbers, for failure messages.
    fn screen_dump(&self) -> String {
        Self::dump(&self.screen())
    }
}

/// What a session needs from the harness beyond its script.
///
/// The six measurements above all want the same thing — a throwaway session on
/// a 24x80 PTY — and said so by passing nothing. Two later measurements need
/// more: cross-session history cannot use `--no-session`, because loading and
/// saving `~/.kitsu/history` is gated on `!no_session`; and the end-to-end
/// suspend cycle needs a poll that can *see* a stopped child. Both are
/// expressed here rather than by duplicating the ~120 lines of terminal setup,
/// and `Default` is exactly what the six already-passing tests assume.
#[derive(Clone, Copy)]
struct Options {
    /// Pass `--no-session` (the default). Clearing it turns session state on,
    /// which is what makes `rl.load_history`/`rl.save_history` run.
    no_session: bool,
    /// Poll with `WUNTRACED` and send `SIGCONT` when the child is found
    /// stopped, counting each stop into [`Session::stops`].
    ///
    /// Off by default because the default poll is `Child::try_wait`, i.e.
    /// `waitpid(WNOHANG)` with no `WUNTRACED`: it reports a stopped child as
    /// still running, so a session that suspends itself would simply poll to
    /// the deadline.
    resume_on_stop: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            no_session: true,
            resume_on_stop: false,
        }
    }
}

/// The six original measurements' entry point: default [`Options`].
fn run_styled_chat_pty(home: &Path, tui: Option<&str>, script: &[Step]) -> Session {
    run_styled_chat_pty_with(home, tui, script, Options::default())
}

/// Run a **styled** interactive chat on a PTY of exactly `ROWS`x`COLS` and
/// drive it with `script`.
///
/// The child gets all three descriptors on the PTY slave (one `dup` each, the
/// same discipline `tests/cli.rs` uses) and no `--plain`/`--no-stream`, so
/// styling and the viewport are live. `tui` is the value of `KITSU_TUI`:
/// `None` leaves it unset, which is classic mode.
///
/// `options` carries everything that is not the script; see [`Options`].
fn run_styled_chat_pty_with(
    home: &Path,
    tui: Option<&str>,
    script: &[Step],
    options: Options,
) -> Session {
    use std::io::{Read as _, Write as _};
    use std::os::fd::FromRawFd as _;
    use std::process::Stdio;

    let mut master = -1;
    let mut slave = -1;
    // SAFETY: both output pointers are valid; null termios/winsize request the
    // platform defaults, which `TIOCSWINSZ` below then overrides.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0,
        "allocate styled carrier PTY"
    );

    // Pin the geometry *before* the spawn, so the child's own `TIOCGWINSZ`
    // probe (`crossterm::terminal::size`) reports this and the viewport's
    // arithmetic is deterministic rather than inherited from the test runner's
    // terminal (or absent, falling back to 80x24 by accident).
    set_window_size(master, ROWS, COLS);

    // SAFETY: the master descriptor stays owned here; non-blocking reads let
    // the driver poll for triggers instead of hanging.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );

    // SAFETY: each duplicated slave descriptor transfers to one Stdio owner,
    // and the original `slave` transfers as stderr, so all three are closed
    // exactly once by the child-spawning machinery.
    let child_stdin = unsafe { libc::dup(slave) };
    let child_stdout = unsafe { libc::dup(slave) };
    assert!(child_stdin >= 0 && child_stdout >= 0);

    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
    command
        .env("HOME", home)
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .env_remove("KITSU_GATEWAY")
        .env_remove("KITSU_MODEL")
        .current_dir(home)
        // SAFETY: all three live descriptors transfer exactly once.
        .stdin(unsafe { Stdio::from_raw_fd(child_stdin) })
        .stdout(unsafe { Stdio::from_raw_fd(child_stdout) })
        .stderr(unsafe { Stdio::from_raw_fd(slave) })
        .args(["--gateway", "example", "chat"]);
    if options.no_session {
        command.arg("--no-session");
    }
    if options.resume_on_stop {
        // Put the child in its own process group.
        //
        // This is what makes the suspend measurement independent of how the
        // test runner was started, and it is not a nicety: POSIX says a
        // process in an **orphaned** process group discards `SIGTSTP`
        // entirely. Inherit the runner's group and whether Ctrl+Z stops the
        // child depends on whether that group happens to be orphaned —
        // measured here, the same test passed 5/5 under one shell invocation
        // and failed 5/5 (never stopping, `stops == 0`) under a command
        // substitution. With the child as its own group leader its parent is
        // in a different group in the same session, so the group is never
        // orphaned and the stop is deterministic.
        //
        // It cannot earn a `SIGTTIN` instead: this PTY is nobody's controlling
        // terminal (the child is not a session leader and no `tcsetpgrp` is
        // ever done), so the kernel applies no job-control access checks to
        // it.
        //
        // SAFETY: `setpgid` is async-signal-safe and touches nothing in the
        // forked child's address space, which is the whole constraint on
        // `pre_exec`.
        unsafe {
            use std::os::unix::process::CommandExt as _;
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // ...and restore `SIGTSTP`'s default disposition, which is the
                // other half of making this deterministic and was the half
                // that actually bit. A `SIG_IGN` disposition survives both
                // `fork` and `exec`, and a non-interactive shell ignores
                // SIGTSTP/SIGTTIN/SIGTTOU in a forked subshell: measured on
                // this machine, `/proc/self/status` reports `SigIgn`
                // `0x0000000000001000` under a plain redirect but
                // `0x0000000000381000` (bits 20/21/22 = SIGTSTP/TTIN/TTOU)
                // inside a command substitution. The child inherited that, so
                // `raise(SIGTSTP)` was a no-op and the test passed or failed
                // according to how the suite happened to be invoked. Resetting
                // it here gives the child the disposition a real user's
                // terminal session would give it.
                if libc::signal(libc::SIGTSTP, libc::SIG_DFL) == libc::SIG_ERR {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    match tui {
        Some(mode) => command.env("KITSU_TUI", mode),
        None => command.env_remove("KITSU_TUI"),
    };

    let mut child = command.spawn().expect("spawn styled carrier chat");
    // SAFETY: ownership of the remaining master descriptor transfers once.
    let mut terminal = unsafe { fs::File::from_raw_fd(master) };

    let deadline = Instant::now() + DEADLINE;
    let mut raw: Vec<u8> = Vec::new();
    let mut step = 0usize;
    let mut marks: Vec<usize> = Vec::new();
    let mut resizes: Vec<Resized> = Vec::new();
    let mut stops = 0usize;
    let pid = child.id() as libc::pid_t;
    // The re-arm, for [`Action::Resize`] only: which step's trigger we are
    // waiting on, and when the next `SIGWINCH` is due.
    //
    // One signal is almost always enough, but "almost" would be a flake. The
    // trigger that fires a resize step is the child's own output, so the child
    // may still be inside its paint rather than back in `read()` when the
    // signal lands; the flag latches, the read then blocks, and nothing
    // further would ever wake it. Re-delivering is safe -- the second resize
    // is to the *same* dimensions, which `plan_resize` answers `Unchanged`,
    // i.e. zero bytes -- and it is still **only a signal**: no input byte is
    // ever written here, so a reader that only noticed the flag on the next
    // keystroke would still hang to the deadline, which is exactly the
    // failure the tests want.
    let mut rearm: Option<(usize, Instant)> = None;

    let status = loop {
        let mut buffer = [0_u8; 8192];
        match terminal.read(&mut buffer) {
            Ok(0) => {}
            Ok(read) => raw.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            // Linux PTY masters report EIO once the last slave closes.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => panic!("read styled carrier PTY: {error}"),
        }

        if let Some(next) = script.get(step) {
            let seen = String::from_utf8_lossy(&raw);
            if seen.contains(next.trigger) {
                marks.push(raw.len());
                match next.action {
                    Action::Send(bytes) => {
                        terminal
                            .write_all(bytes.as_bytes())
                            .expect("write scripted input");
                        terminal.flush().expect("flush scripted input");
                    }
                    Action::Resize { rows, cols } => {
                        set_window_size(master, rows, cols);
                        resizes.push(Resized {
                            at: raw.len(),
                            rows,
                            cols,
                        });
                        // SAFETY: signalling a child this harness owns.
                        unsafe { libc::kill(pid, libc::SIGWINCH) };
                        // Re-arm only while a *later* step is still waiting:
                        // a script whose last action is a resize has nothing
                        // left to observe the signal with, so nudging it
                        // forever would just be noise.
                        rearm =
                            (step + 1 < script.len()).then(|| (step + 1, Instant::now() + REARM));
                    }
                }
                step += 1;
            }
        }

        // Still waiting on the trigger that follows a resize: nudge it again.
        if let Some((waiting_on, due)) = rearm {
            if step != waiting_on {
                rearm = None;
            } else if Instant::now() >= due {
                // SAFETY: signalling a child this harness owns.
                unsafe { libc::kill(pid, libc::SIGWINCH) };
                rearm = Some((waiting_on, Instant::now() + REARM));
            }
        }

        if let Some(status) = poll_child(&mut child, options.resume_on_stop, &mut stops) {
            // Drain whatever the child wrote between the last read and exit.
            loop {
                match terminal.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => raw.extend_from_slice(&buffer[..read]),
                    Err(_) => break,
                }
            }
            break status;
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "styled carrier timed out after {DEADLINE:?} at script step {step} \
                 (of {}); transcript so far:\n{}",
                script.len(),
                String::from_utf8_lossy(&raw)
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    Session {
        raw,
        status,
        stops,
        marks,
        resizes,
    }
}

/// Poll the child once. `None` means still running.
///
/// With `resume_on_stop` clear this is plain `Child::try_wait`, which is what
/// the original harness did and what the six measurements above run under.
///
/// With it set the poll becomes `waitpid(WNOHANG | WUNTRACED)`: `try_wait`
/// omits `WUNTRACED`, so a child that has stopped itself with `SIGTSTP` is
/// indistinguishable from one that is merely busy, and the driver would poll
/// to its deadline. Here a stop is detected, counted, and answered with
/// `SIGCONT` — which is the half of a suspend cycle the shell would normally
/// perform — and the session continues.
fn poll_child(
    child: &mut std::process::Child,
    resume_on_stop: bool,
    stops: &mut usize,
) -> Option<std::process::ExitStatus> {
    use std::os::unix::process::ExitStatusExt as _;

    if !resume_on_stop {
        return child.try_wait().expect("poll styled carrier");
    }

    let pid = child.id() as libc::pid_t;
    let mut raw_status: libc::c_int = 0;
    // SAFETY: `pid` is this harness's own child and `raw_status` is a valid
    // out-pointer. Reaping here instead of through `Child::try_wait` is why
    // the status is rebuilt with `ExitStatus::from_raw` below.
    let waited = unsafe { libc::waitpid(pid, &mut raw_status, libc::WNOHANG | libc::WUNTRACED) };
    if waited != pid {
        // 0: nothing to report. -1: already reaped or gone; let the deadline
        // or a later poll deal with it rather than panicking mid-session.
        return None;
    }
    if libc::WIFSTOPPED(raw_status) {
        *stops += 1;
        // SAFETY: signalling a child this harness owns.
        unsafe { libc::kill(pid, libc::SIGCONT) };
        return None;
    }
    Some(std::process::ExitStatus::from_raw(raw_status))
}

/// Every script that types at the *idle* prompt triggers its first input on
/// `ESC[?2004h`, the bracketed-paste enable that `RawStdinGuard::engage`
/// writes immediately after its `tcsetattr`. The banner is printed long before
/// the reader engages, so a keystroke sent on it can reach a terminal that is
/// still canonical with `ECHO`/`ISIG` on — where the tty echoes the draft
/// itself (which is exactly what tests 5 and 6 assert must not happen) and
/// swallows `0x1a` as its `VSUSP` character. Measured on the full suite, that
/// race cost about 2 runs in 30 before this marker was adopted.
const RAW_MODE_MARKER: &str = "\x1b[?2004h";

/// The script both measurements run: submit `/help`, then `/quit`.
///
/// The triggers are what the binary prints unconditionally at startup (the
/// banner's `/help for commands`) and what `/help` itself prints (`commands:`),
/// so each input is sent only once the child has demonstrably reached the
/// prompt.
fn help_then_quit() -> Vec<Step> {
    vec![
        send("/help for commands", "/help\n"),
        send("/help <command> explains one command in detail", "/quit\n"),
    ]
}

/// Test 1. **The harness itself.** Classic mode, `KITSU_TUI` unset: the
/// session must start, run `/help`, print the command list, and exit on
/// `/quit`. If this fails the harness is wrong and nothing after it means
/// anything.
#[test]
#[serial]
fn classic_mode_styled_pty_runs_help_and_quits() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), None, &help_then_quit());

    assert!(
        session.status.success(),
        "classic session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
    let text = session.raw_text();
    assert!(
        text.contains("commands:"),
        "/help must print the command list; raw:\n{text}"
    );
    assert!(
        text.contains("/help <command> explains one command in detail"),
        "/help must print its footer; raw:\n{text}"
    );
    // Classic mode installs no viewport, so no DECSTBM region is ever set.
    assert_eq!(
        session.raw_count(&format!("\x1b[1;{};1H", ROWS - 1)),
        0,
        "classic mode emits no viewport geometry"
    );
}

/// Test 2. **The measurement (finding F17, question Q3).** Inline mode, same
/// script. The viewport contract says the region is set for rows `1..=23`
/// with the cursor parked on 23, and that the bridge's painted status text on
/// row 24 survives whatever the transcript does.
///
/// The second half is the open question: `/help` prints ~40 lines through
/// direct `println!`/`eprintln!` calls that bypass the `Surface` seam
/// entirely. Whether they land inside the DECSTBM region or on top of the
/// pinned row is what is being measured here.
///
/// **Observed: they flow into the region harmlessly.** The screen is sampled
/// at the moment `/quit` is submitted, i.e. once the whole of `/help`
/// (footer included) has printed, and row 24 still reads `kitsu`. A direct
/// writer inherits the cursor, the cursor is parked inside the region, so a
/// bare `println!` scrolls within the region exactly as a seam write would.
#[test]
#[serial]
fn inline_mode_pins_the_region_and_direct_writes_stay_inside_it() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &help_then_quit());

    let bottom = ROWS - 1; // rows - viewport_rows, with viewport_rows = 1
    let region = format!("\x1b[1;{bottom}r");
    let park = format!("\x1b[{bottom};1H");

    let text = session.raw_text();
    assert!(
        text.contains(&region),
        "the region set for a {ROWS}-row terminal with a 1-row viewport must \
         appear in the raw bytes; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        text.contains(&park),
        "the cursor park must appear in the raw bytes; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        text.contains("commands:"),
        "/help must still print under the viewport; raw:\n{}",
        text.escape_debug()
    );

    // The screen as it stood with all of `/help` printed and `/quit` not yet
    // processed.
    let screen = session.screen_at(1);
    assert_eq!(
        screen[(ROWS - 1) as usize].trim_end(),
        "kitsu",
        "the pinned viewport row must still show the bridge's status text \
         after /help printed; whole screen:\n{}",
        Session::dump(&screen)
    );
    // And the transcript really did reach the bottom of the region rather than
    // stopping short of it, so the assertion above is not vacuous: /help's
    // output (plus the composer) fills the region up to its last row.
    assert!(
        screen[..(ROWS - 1) as usize]
            .iter()
            .filter(|row| !row.trim().is_empty())
            .count()
            >= 20,
        "the region should be full of /help output; screen:\n{}",
        Session::dump(&screen)
    );
}

/// Test 3. The region is set exactly once and released exactly once.
///
/// Re-emitting DECSTBM would home the cursor and undo the park, and failing
/// to release it would leave the user's terminal pinned after kitsu exits, so
/// both counts are worth pinning now that the harness makes them free.
#[test]
#[serial]
fn inline_mode_sets_the_region_once_and_releases_it_once() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &help_then_quit());

    assert_eq!(
        session.raw_count(&format!("\x1b[1;{}r", ROWS - 1)),
        1,
        "the scroll region is set exactly once, by `show`"
    );
    assert_eq!(
        session.raw_count("\x1b[r"),
        1,
        "and released exactly once, by the guard's `Drop`; raw:\n{}",
        session.raw_text().escape_debug()
    );
}

/// Test 4. **Finding F18, now asserted as fixed rather than as measured.**
///
/// The session's closing summary line (`↳ completed in …`) is printed *after*
/// `BridgeGuard::drop` has released the region: the release and that line are
/// adjacent in the byte stream, release first. That ordering is not changed
/// here — the summary is emitted from `main` after `run_chat` returns, while
/// the guard is a `run_chat` local, so making the guard outlive it would mean
/// hoisting terminal ownership across process finalisation.
///
/// The fix is contained in `hide` instead: it clears its own viewport rows
/// while the geometry is still latched, releases the region, and then parks
/// the cursor on the first (now blank) viewport row, immediately below the
/// transcript. `ESC[r` homes the cursor, so without that park the summary
/// lands on row 1 and overwrites the top of the transcript; with it, a later
/// direct write lands below the transcript by construction.
///
/// Asserted on the **final** screen, not `screen_at(1)`: the whole point of
/// the fix is that the screen a user is left looking at after exit is correct,
/// so that is the screen under test.
#[test]
#[serial]
fn inline_mode_teardown_parks_below_the_transcript_and_clears_the_status_row() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &help_then_quit());
    let text = session.raw_text();

    // Unchanged: the summary still follows the release in the byte stream.
    // The fix is in where the cursor is left, not in the ordering.
    let release = text.find("\x1b[r").expect("the guard releases the region");
    let tail = &text[release..];
    assert!(
        tail.contains("↳ completed in"),
        "the closing summary line is written after the region release; tail:\n{}",
        tail.escape_debug()
    );

    let bottom = ROWS - 1; // rows - viewport_rows, with viewport_rows = 1

    let screen = session.screen();
    assert!(
        !screen[0].contains("↳ completed in"),
        "the summary must not land on row 1 any more; screen:\n{}",
        session.screen_dump()
    );

    // The top of the transcript is intact: the last of `/help`'s output is
    // still where the transcript left it, with nothing scribbled over it and
    // no debris from a half-overwritten row. Pre-fix the summary took row 1
    // and left the tail of the displaced row showing beneath it.
    let footer = screen
        .iter()
        .position(|row| row.contains("/help <command> explains one command in detail"))
        .unwrap_or_else(|| {
            panic!(
                "the transcript's own footer must survive teardown; screen:\n{}",
                session.screen_dump()
            )
        });

    // The summary lands immediately below the transcript, on the row `hide`
    // parked the cursor on, and not over it.
    let summary = screen
        .iter()
        .position(|row| row.contains("↳ completed in"))
        .unwrap_or_else(|| {
            panic!(
                "the closing summary must be visible on the exit screen; screen:\n{}",
                session.screen_dump()
            )
        });
    assert!(
        summary > footer,
        "the summary belongs below the transcript, not above its footer on \
         row {}; screen:\n{}",
        footer + 1,
        session.screen_dump()
    );
    // ...and it is the *last* thing on the screen: nothing of the transcript
    // follows it, so it was appended below rather than dropped into the middle
    // of what was already there. (It is wider than `COLS` and wraps onto one
    // continuation row, hence `summary + 1`.)
    let last = screen
        .iter()
        .rposition(|row| !row.trim().is_empty())
        .expect("the exit screen is not blank");
    assert!(
        last == summary || last == summary + 1,
        "the summary is the last content on the exit screen; it ends on row \
         {}, but row {} still holds {:?}; screen:\n{}",
        summary + 2,
        last + 1,
        screen[last],
        session.screen_dump()
    );

    // And the viewport row no longer carries stale status text: `hide` cleared
    // it. Whatever occupies row 24 is the summary the cursor was parked for,
    // never the painted `kitsu` status.
    assert_ne!(
        screen[(ROWS - 1) as usize].trim_end(),
        "kitsu",
        "the painted status row must be cleared on the way out; screen:\n{}",
        session.screen_dump()
    );

    // Finally the raw bytes, so a regression in the sequence itself is named
    // as such rather than only showing up as a wrong screen.
    assert_eq!(
        session.raw_count(&format!("\x1b[{ROWS};1H\x1b[2K\x1b[r\x1b[{ROWS};1H")),
        1,
        "teardown clears row {ROWS}, releases the region, then parks on row \
         {ROWS}, which is bottom + 1 = {}; raw:\n{}",
        bottom + 1,
        text.escape_debug()
    );
}

/// The script the two T0-S5b measurements run: type `/context` at the idle
/// prompt **without** submitting, submit it with a bare carriage return (what
/// a raw-mode terminal actually sends for Enter), then leave.
///
/// `/context` rather than `/help` because its name does not occur anywhere in
/// the startup output — the banner says `/help for commands` and the splash
/// says `25 enabled (use /tools)` — so the first appearance of the string
/// `/context` in the stream is the draft being echoed back, which makes it a
/// usable trigger for the submit step. Its output is also a dozen plain
/// left-aligned lines, which is what the staircase check needs.
fn type_context_then_quit() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/context"),
        send("/context", "\r"),
        send("context window source", "/quit\r"),
    ]
}

/// Test 5. **The wiring (T0-S5b).** Inline mode's idle prompt is the inline
/// reader: the draft is echoed into the pinned viewport row by the paint
/// callback, not by the terminal and not by rustyline's own repaints, and a
/// carriage return submits it.
///
/// The screen is sampled at the moment the carriage return is written, i.e.
/// with `/context` fully typed and nothing submitted yet. Two halves:
///
/// * row 24 (the viewport row) shows the draft, so something painted it there;
/// * no row **inside** the scroll region shows it, so the echo is the
///   viewport's and not the terminal's or rustyline's.
///
/// Today the second half is what fails: `ECHO` is off only during a turn, the
/// idle prompt is still rustyline's, and rustyline repaints `kitsu> /context`
/// inside the region while row 24 keeps the static `kitsu` status.
#[test]
#[serial]
fn inline_mode_echoes_the_idle_draft_in_the_viewport_row() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &type_context_then_quit());

    let typed = session.screen_at(1);
    let viewport = &typed[(ROWS - 1) as usize];
    assert!(
        viewport.contains("/context"),
        "the unsubmitted draft belongs on the pinned viewport row; whole screen:\n{}",
        Session::dump(&typed)
    );
    let stray = typed[..(ROWS - 1) as usize]
        .iter()
        .position(|row| row.contains("/context"));
    assert!(
        stray.is_none(),
        "the draft must be echoed only by the viewport, but row {} inside the \
         scroll region shows it too ({:?}); whole screen:\n{}",
        stray.unwrap_or_default() + 1,
        stray.map(|row| typed[row].clone()).unwrap_or_default(),
        Session::dump(&typed)
    );

    // And the carriage return really submitted: the command ran and its output
    // flowed into the scroll region, with the region still pinned exactly once.
    let after = session.screen_at(2);
    assert!(
        after[..(ROWS - 1) as usize]
            .iter()
            .any(|row| row.contains("context window source")),
        "the submitted command's output belongs inside the scroll region; \
         whole screen:\n{}",
        Session::dump(&after)
    );
    assert_eq!(
        session.raw_count(&format!("\x1b[1;{}r", ROWS - 1)),
        1,
        "the viewport survives the idle read: the region is still set once only"
    );
}

/// Test 6. **The staircase question, re-asked for raw input mode.**
///
/// Test 2 measured that direct `println!` writers flow harmlessly into the
/// scroll region, but only in cooked mode, where the terminal supplies the
/// carriage return. T0-S5b holds raw input mode across the *idle* prompt, so
/// the question is open again: if the line discipline's output post-processing
/// were off, every `\n` would be a bare line feed and successive lines would
/// march rightwards from where the previous one ended.
///
/// It should not, because `RawStdinGuard` touches only `c_lflag` (`ICANON`,
/// `ECHO`, `ISIG`) and leaves `c_oflag` — hence `OPOST`/`ONLCR` — alone. That
/// is the whole reason the ~392 direct writers survive raw mode today. This
/// asserts it instead of assuming it, on the output of a command submitted
/// through the raw reader: every line of `/context`'s report must begin at
/// column 1, and no transcript line may be indented beyond what the report
/// itself indents (its table rows use two spaces).
#[test]
#[serial]
fn inline_mode_raw_idle_input_does_not_staircase_command_output() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &type_context_then_quit());

    let screen = session.screen_at(2);
    // Precondition, so the measurement below is taken on the path it is about:
    // in inline mode the idle prompt is the inline reader, so rustyline's
    // composer must not have drawn its own prompt inside the scroll region. If
    // this fires, the alignment assertions that follow measured the cooked
    // rustyline path all over again and say nothing new.
    let composer = screen[..(ROWS - 1) as usize]
        .iter()
        .position(|row| row.starts_with("kitsu>"));
    assert!(
        composer.is_none(),
        "inline mode must not run rustyline's composer at the idle prompt, but \
         row {} is its prompt ({:?}); whole screen:\n{}",
        composer.unwrap_or_default() + 1,
        composer.map(|row| screen[row].clone()).unwrap_or_default(),
        Session::dump(&screen)
    );

    // The report's own left-aligned lines, in the order it prints them. Each
    // must *start* the row it lands on: a staircase shows up as leading
    // whitespace that grows line by line.
    for needle in [
        "context window source:",
        "context window cache:",
        "context window discovery:",
        "compaction:",
    ] {
        let row = screen
            .iter()
            .find(|row| row.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "{needle:?} must be on the screen after submitting /context \
                     through the raw reader; whole screen:\n{}",
                    Session::dump(&screen)
                )
            });
        assert!(
            row.starts_with(needle),
            "output printed under the raw idle reader must be left-aligned, but \
             {needle:?} starts at column {} ({:?}); whole screen:\n{}",
            row.find(needle).unwrap_or_default() + 1,
            row,
            Session::dump(&screen)
        );
    }

    // And nothing anywhere in the region has drifted: `/context`'s deepest
    // deliberate indent is four spaces, so a bound of six catches a staircase
    // long before it reaches the right margin while leaving the report's own
    // shape alone.
    for (index, row) in screen[..(ROWS - 1) as usize].iter().enumerate() {
        if row.trim().is_empty() {
            continue;
        }
        let indent = row.len() - row.trim_start().len();
        assert!(
            indent <= 6,
            "row {} is indented {indent} columns ({:?}), which is the shape of a \
             staircase; whole screen:\n{}",
            index + 1,
            row,
            Session::dump(&screen)
        );
    }
}

/// Session 1 of the cross-session history measurement: submit `/context` at the
/// idle prompt, then leave. `/context` is reused from the scripts above for the
/// same reason — its name occurs nowhere in the startup output, so the first
/// appearance of the string `/context` in session 2's stream can only be the
/// recalled history entry being painted.
fn submit_context_then_quit() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/context\r"),
        send("context window source", "/quit\r"),
    ]
}

/// Session 2: walk back through history at the idle prompt, then abandon the
/// recalled draft (Ctrl+C clears it) and leave.
///
/// **Two** Ups, not one: `add_history_entry` is on the common path, so session
/// 1's `/quit` is itself the newest history entry and the first Up recalls it.
/// The second reaches `/context`. (This is how the measurement was first taken:
/// a single Up painted `/quit` onto the viewport row — already proof that the
/// walk is reading session 1's saved file, since a fresh history would have
/// nothing to paint at all.)
fn recall_with_up_then_quit() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "\x1b[A\x1b[A"),
        send("/context", "\x03/quit\r"),
    ]
}

/// Test 39. **Finding F20, corrected: inline mode does _not_ lose history that
/// persists across sessions.**
///
/// F20 recorded, and `README.md`, `CHANGELOG.md`, `docs/ux-design.md` and the
/// T0 go/no-go record repeated, that the inline idle prompt loses cross-session
/// history. This runs the real binary twice under one `HOME` and shows the
/// first session's submitted line coming back on Up at the second session's
/// inline prompt.
///
/// Why it works, which the three call sites make unsurprising once they are put
/// together: `main.rs` loads `~/.kitsu/history` into the **rustyline** editor at
/// startup and saves it at exit (both gated on `!no_session`);
/// `read_idle_chat_line` seeds the inline reader with
/// `editor.history().iter().cloned().collect()`, i.e. that same loaded list; and
/// `rl.add_history_entry` sits on the common path *after* the read, outside the
/// match on which reader produced the line, so a line submitted through the
/// inline reader is recorded like any other. Inline mode therefore borrows
/// rustyline's persistence wholesale while never running its composer.
///
/// This is the one test here that cannot use `--no-session`: both the load and
/// the save are gated on it, hence [`Options::no_session`].
#[test]
#[serial]
fn inline_mode_recalls_history_persisted_by_an_earlier_session() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session_options = Options {
        no_session: false,
        ..Options::default()
    };

    // Session 1: submit `/context` through the inline reader and exit cleanly,
    // so the exit-time `save_history` runs.
    let first = run_styled_chat_pty_with(
        home.path(),
        Some("inline"),
        &submit_context_then_quit(),
        session_options,
    );
    assert!(
        first.status.success(),
        "the first session must exit cleanly so its history is saved; screen:\n{}",
        first.screen_dump()
    );

    // Direct evidence that the save happened, independently of what session 2
    // then does with it: the file exists and holds the submitted line.
    let history_path = home.path().join(".kitsu").join("history");
    let persisted = fs::read_to_string(&history_path).unwrap_or_else(|error| {
        panic!(
            "the first session must write {}: {error}; screen:\n{}",
            history_path.display(),
            first.screen_dump()
        )
    });
    assert!(
        persisted.contains("/context"),
        "the submitted line belongs in the history file, which instead holds \
         {persisted:?}"
    );

    // Session 2: same `HOME`, so the same history file. One Up at the inline
    // idle prompt must bring the line back.
    let second = run_styled_chat_pty_with(
        home.path(),
        Some("inline"),
        &recall_with_up_then_quit(),
        session_options,
    );
    assert!(
        second.status.success(),
        "the second session exited {:?}; screen:\n{}",
        second.status,
        second.screen_dump()
    );

    // The screen as it stood with Up pressed and nothing submitted: the
    // recalled entry is painted on the pinned viewport row, which is the row
    // the inline reader owns.
    let recalled = second.screen_at(1);
    let viewport = &recalled[(ROWS - 1) as usize];
    assert!(
        viewport.contains("/context"),
        "Up at the inline idle prompt must recall the previous session's line \
         onto the pinned viewport row; whole screen:\n{}",
        Session::dump(&recalled)
    );
    // The walk is the inline reader's, not rustyline's: nothing inside the
    // scroll region echoed the recalled line, and rustyline's composer never
    // drew a prompt there.
    let stray = recalled[..(ROWS - 1) as usize]
        .iter()
        .position(|row| row.starts_with("kitsu>"));
    assert!(
        stray.is_none(),
        "the recall must be the inline reader's, but row {} inside the region \
         is rustyline's composer ({:?}); whole screen:\n{}",
        stray.unwrap_or_default() + 1,
        stray.map(|row| recalled[row].clone()).unwrap_or_default(),
        Session::dump(&recalled)
    );
}

/// The F22 script: type `/context` at the inline idle prompt, suspend with
/// Ctrl+Z, submit the replayed draft once the session is back, then leave.
///
/// The trigger for the post-resume step is the **region release** (`ESC[r`),
/// which `release_for_suspend` emits before the process stops and which
/// nothing else emits until teardown. Writing to the PTY master while the
/// child is stopped is safe: the bytes sit in the terminal's input queue and
/// are read once `SIGCONT` lands, which is exactly how a real user's typing
/// into a stopped job behaves.
///
/// The *first* step triggers on the bracketed-paste enable (`ESC[?2004h`)
/// rather than on the banner the other scripts use, and that choice is
/// load-bearing. The banner is printed long before the idle reader engages, so
/// a Ctrl+Z sent on it can reach a terminal that is still **canonical** —
/// where `0x1a` is the line discipline's `VSUSP` character, swallowed by the
/// tty instead of delivered to the editor. Triggering on the banner made this
/// test fail about one run in four, always by never reaching the suspend at
/// all. `RawStdinGuard::engage` writes `ESC[?2004h` immediately *after* its
/// `tcsetattr`, so that sequence appearing in the stream is proof that
/// `ICANON`/`ISIG` are already cleared and `0x1a` will arrive as a byte.
/// (The reader's own first paint is not usable as the marker: with an empty
/// draft the viewport row shows the bare `kitsu` status, which the bridge
/// already painted at startup.)
fn type_context_suspend_then_quit() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/context\x1a"),
        send("\x1b[r", "\r"),
        send("context window source", "/quit\r"),
    ]
}

/// Test 40. **Finding F22: the suspend cycle, end to end.**
///
/// The unit tests pin the *order* of the five suspend effects behind the
/// `SuspendEffects` seam, because the real `stop_process` blocks inside
/// `raise(SIGTSTP)` and cannot be observed from inside the process. This runs
/// the real binary on a real PTY and observes the cycle from outside:
///
/// * the child really stops — the driver sees `WIFSTOPPED` and answers with
///   `SIGCONT`, exactly once ([`Options::resume_on_stop`]);
/// * the scroll region is released before the stop and set again after it, so
///   the shell would have inherited an unpinned terminal;
/// * the draft survives: the `/context` typed before the Ctrl+Z is replayed
///   into the resumed read, and the bare carriage return written while the
///   child was stopped submits it, so the command's output appears.
///
/// This was deferred on the previous card for want of `WUNTRACED` in the
/// harness's poll; `poll_child` supplies it.
#[test]
#[serial]
fn inline_mode_suspends_and_resumes_with_the_draft_intact() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty_with(
        home.path(),
        Some("inline"),
        &type_context_suspend_then_quit(),
        Options {
            resume_on_stop: true,
            ..Options::default()
        },
    );

    assert_eq!(
        session.stops,
        1,
        "the child must stop itself exactly once on Ctrl+Z; screen:\n{}",
        session.screen_dump()
    );
    assert!(
        session.status.success(),
        "the resumed session must still exit cleanly on /quit, but exited {:?}; \
         screen:\n{}",
        session.status,
        session.screen_dump()
    );

    // Released before the stop and re-set after it: two sets, two releases
    // (the second release being teardown's).
    assert_eq!(
        session.raw_count(&format!("\x1b[1;{}r", ROWS - 1)),
        2,
        "the region is set once at startup and once more on resume; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert_eq!(
        session.raw_count("\x1b[r"),
        2,
        "the region is released once for the suspend and once at teardown; raw:\n{}",
        session.raw_text().escape_debug()
    );

    // The draft survived: `/context` was typed before the Ctrl+Z and never
    // retyped, and the carriage return written during the stop submitted it.
    let screen = session.screen();
    assert!(
        screen
            .iter()
            .any(|row| row.contains("context window source")),
        "the draft typed before the suspend must be replayed and submitted \
         after it; whole screen:\n{}",
        session.screen_dump()
    );
}

/// The hint script: type the slash prefix `/cont` at the idle prompt and
/// leave it unsubmitted, then finish the command by hand and leave.
///
/// `/cont` is a prefix of exactly one registry command, `/context`, so the
/// completion hint for it is the remainder `ext`. Both inputs trigger on
/// something that exists today -- `RAW_MODE_MARKER` for the first keystroke
/// (output proves the program is alive, not that it has taken the terminal)
/// and the draft's own echo for the second -- so the measurement fails on its
/// assertion rather than on the driver's deadline.
fn type_slash_prefix_then_quit() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/cont"),
        send("/cont", "ext\r"),
        send("context window source", "/quit\r"),
    ]
}

/// Test 9. **The hint slot, end to end.** With `/cont` typed and nothing
/// submitted, the pinned viewport row must read `kitsu> /context`: the five
/// typed characters plus the dimmed completion remainder `ext`. The draft
/// itself is still only `/cont`, which the submission half proves -- the
/// three characters sent next are `ext`, and that is what makes the command
/// run.
///
/// Dimness is not asserted here (the screen model carries text, not
/// attributes); the unit tests pin the SGR bytes. What this adds is that the
/// hint is actually wired through the real reader, the real guard and the
/// real viewport row, and lands on the pinned row rather than inside the
/// scroll region.
#[test]
#[serial]
fn inline_mode_shows_the_slash_completion_hint_on_the_viewport_row() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &type_slash_prefix_then_quit());

    let typed = session.screen_at(1);
    let viewport = &typed[(ROWS - 1) as usize];
    assert!(
        viewport.contains("/context"),
        "the pinned row must show the draft plus its completion hint, but it \
         reads {viewport:?}; whole screen:\n{}",
        Session::dump(&typed)
    );

    // The hint is display-only: the three characters sent after it are what
    // completes the command, so the submitted line was `/cont` + `ext`.
    let after = session.screen_at(2);
    assert!(
        after[..(ROWS - 1) as usize]
            .iter()
            .any(|row| row.contains("context window source")),
        "the hint must not have been submitted as text; whole screen:\n{}",
        Session::dump(&after)
    );
}

// ---------------------------------------------------------------------------
// Resize (findings F16 and F24)
// ---------------------------------------------------------------------------

/// The taller window the resize tests grow to, and the region it implies.
///
/// 30 rather than 25 so the new bottom margin cannot be confused with the old
/// one by an off-by-one, and so the new pinned row is a long way from where
/// the old one was: a child that never re-latched would leave the draft on row
/// 24 of a 30-row screen, which the assertions below would see as a bare row
/// 30.
const TALL_ROWS: u16 = 30;

/// Two rows, which is *below* finding F19's `rows >= viewport_rows + 2` bound
/// for the single pinned row inline mode installs, so a resize to it must
/// release the viewport rather than pin one it cannot draw.
const TOO_SHORT_ROWS: u16 = 2;

/// The DECSTBM the child must emit once the window is [`TALL_ROWS`] high:
/// `bottom = rows - viewport_rows` with `viewport_rows = 1`.
fn tall_region() -> String {
    format!("\x1b[1;{}r", TALL_ROWS - 1)
}

/// Type `/context` at the inline idle prompt, grow the window, then leave.
///
/// **The third step's trigger is the load-bearing part of this script.** It is
/// the *new* scroll region, which only the child's resize path emits, so the
/// next byte of input is written strictly after the resize has already taken
/// effect -- and the draft is submitted by that step rather than retyped. No
/// byte of input exists between the signal and the geometry: if the pending
/// flag were only noticed on the next keystroke there would be no next
/// keystroke, the trigger would never fire, and the driver would fail on its
/// deadline with the transcript attached.
fn type_context_grow_then_quit() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/context"),
        resize("/context", TALL_ROWS, COLS),
        submit_on_tall_region(),
        send("context window source", "/quit\r"),
    ]
}

/// Submit the carried-over draft, triggered on the taller window's region.
///
/// Shared by the two scripts that end at [`TALL_ROWS`], and a function because
/// the trigger is a formatted string the `&'static str` field cannot hold
/// inline. A `OnceLock` leak-free `&'static str` is the cheapest way to give
/// it one.
fn submit_on_tall_region() -> Step {
    static TRIGGER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let trigger: &'static String = TRIGGER.get_or_init(tall_region);
    send(trigger.as_str(), "\r")
}

/// Test 10. **Finding F24: the `SIGWINCH` really wakes a blocked `read`.**
///
/// The handler in `tui_bridge::install_sigwinch_handler` is installed with
/// `sigaction` and `sa_flags = 0` *specifically* so that `SIGWINCH` arriving
/// while the inline reader blocks in `read()` returns `EINTR` instead of being
/// restarted. On Linux/glibc the one-line `libc::signal` implies
/// **`SA_RESTART`**, and under that flag the handler would still set the
/// pending flag but the read would never return, so the viewport would
/// silently never resize -- while every unit test in the suite still passed,
/// because raising a signal on the thread that is not blocked proves nothing
/// about a blocked one.
///
/// F24 recorded that as something only a human at a real terminal could catch.
/// It is not: this test signals a **separate process** that is genuinely
/// blocked in a real `read` on a real tty, and sends it **no input whatever**
/// afterwards. Under `SA_RESTART` the child would not wake, would emit no new
/// geometry, and the script's third step -- which triggers on that geometry --
/// would never fire, failing the test on the driver's deadline.
///
/// **Do not delete this as redundant with the `plan_resize` property test or
/// the `tui_bridge` resize tests.** Those pin the decision and the bytes. This
/// is the only test of the delivery mechanism.
#[test]
#[serial]
fn inline_mode_resizes_the_pinned_row_on_a_signal_alone() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &type_context_grow_then_quit());

    // What the child emitted between the resize and the next byte of input:
    // the signal's doing, alone.
    let answer = session.raw_between(1, 2);
    assert!(
        answer.contains(&tall_region()),
        "the signal alone must make the child re-latch at the new geometry; \
         everything it wrote between the resize and the next input was:\n{}",
        answer.escape_debug()
    );
    assert!(
        answer.contains(&format!("\x1b[{TALL_ROWS};1H")),
        "and repaint the draft on the new bottom row {TALL_ROWS}; it wrote:\n{}",
        answer.escape_debug()
    );

    // The screen as it stood after the resize and before `/quit` went in.
    let after = session.screen_at(2);
    assert_eq!(
        after.len(),
        TALL_ROWS as usize,
        "the emulator must have been resized with the child"
    );
    let viewport = &after[(TALL_ROWS - 1) as usize];
    assert!(
        viewport.contains("/context"),
        "the pinned row must have moved to the new bottom row {TALL_ROWS} and \
         still show the draft, but it reads {viewport:?}; whole screen:\n{}",
        Session::dump(&after)
    );
    // ...and it moved rather than being duplicated: the old pinned row is not
    // still carrying the draft 6 rows up the screen.
    let stray = after[..(TALL_ROWS - 1) as usize]
        .iter()
        .position(|row| row.contains("/context"));
    assert!(
        stray.is_none(),
        "the old pinned row must be cleared by the re-latch, but row {} still \
         shows the draft ({:?}); whole screen:\n{}",
        stray.unwrap_or_default() + 1,
        stray.map(|row| after[row].clone()).unwrap_or_default(),
        Session::dump(&after)
    );

    // The draft was never retyped, so the session still submits `/context`
    // after the resize: the editor's buffer survived the re-latch.
    assert!(
        session.status.success(),
        "the resized session must still exit cleanly, but exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
}

/// Type `/context`, shrink the window below F19's bound, then leave.
///
/// The submit triggers on the **region release** (`ESC[r`), which nothing
/// else emits until teardown -- so, as in the script above, the input is
/// written strictly after the signal has already been acted on, and no
/// keystroke exists that could have woken the read instead.
fn type_context_shrink_then_quit() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/context"),
        resize("/context", TOO_SHORT_ROWS, COLS),
        send("\x1b[r", "\r"),
        send("context window source", "/quit\r"),
    ]
}

/// Test 11. **The release case.** A window shrunk below finding F19's
/// `rows >= viewport_rows + 2` bound cannot carry a pinned row that is drawn
/// correctly, so the resize must release the region and pin nothing -- not
/// set a region whose margins exceed the screen, which is the corruption F19
/// was about.
///
/// Observed on the signal alone, with no input between the resize and the
/// assertion, so this is also a second instance of the F24 measurement above.
#[test]
#[serial]
fn inline_mode_resize_below_the_minimum_height_releases_the_region() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("inline"),
        &type_context_shrink_then_quit(),
    );

    let answer = session.raw_between(1, 2);
    assert!(
        answer.contains("\x1b[r"),
        "a resize below the bound must release the scroll region; everything \
         the child wrote between the resize and the next input was:\n{}",
        answer.escape_debug()
    );
    assert!(
        !answer.contains("\x1b[1;"),
        "and must pin no new region -- a DECSTBM whose bottom margin exceeds a \
         {TOO_SHORT_ROWS}-row screen is exactly what F19 forbids -- but it \
         wrote:\n{}",
        answer.escape_debug()
    );

    // Nothing is pinned afterwards either: the only region this session ever
    // set is the startup one, so no later paint re-latched behind the plan's
    // back.
    assert_eq!(
        session.raw_count(&format!("\x1b[1;{}r", ROWS - 1)),
        1,
        "the startup region is the only one ever set; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert_eq!(
        session.raw_count(&tall_region()),
        0,
        "and certainly not the taller one this script never resizes to"
    );

    // And no pinned row survives on the screen: with nothing latched, `paint`
    // is inert, so the draft is not drawn anywhere.
    let after = session.screen_at(2);
    assert_eq!(
        after.len(),
        TOO_SHORT_ROWS as usize,
        "the emulator must have been resized with the child"
    );
    let pinned = after.iter().position(|row| row.contains("/context"));
    assert!(
        pinned.is_none(),
        "with the viewport released nothing may be pinned, but row {} shows \
         {:?}; whole screen:\n{}",
        pinned.unwrap_or_default() + 1,
        pinned.map(|row| after[row].clone()).unwrap_or_default(),
        Session::dump(&after)
    );
}

/// The round trip: type `/context`, shrink below the bound, grow back above
/// it, then leave.
///
/// Both resizes send nothing, and each one's effect is the next step's
/// trigger: the release (`ESC[r`) gates the second resize, and the taller
/// window's region gates the submit. So the whole trip runs on two signals
/// and no input at all.
fn type_context_shrink_grow_then_quit() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/context"),
        resize("/context", TOO_SHORT_ROWS, COLS),
        resize("\x1b[r", TALL_ROWS, COLS),
        submit_on_tall_region(),
        send("context window source", "/quit\r"),
    ]
}

/// Test 12. **The round trip.** A window made too short and then tall again
/// ends up correctly pinned, with the draft intact -- the second half of
/// `ResizePlan::Install`, through the real signal path rather than the plan.
///
/// The draft is the point: it was typed once, before the first resize, and
/// never retyped. It survives both re-latches because the editor's buffer is
/// the reader's, not the surface's, and the reader repaints from it on every
/// wake.
#[test]
#[serial]
fn inline_mode_resize_round_trip_ends_correctly_pinned() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("inline"),
        &type_context_shrink_grow_then_quit(),
    );

    // The release came first, on the first signal...
    let released = session.raw_between(1, 2);
    assert!(
        released.contains("\x1b[r"),
        "the shrink must release the region first; it wrote:\n{}",
        released.escape_debug()
    );
    // ...and the re-install came on the second, still with no input in between.
    let installed = session.raw_between(2, 3);
    assert!(
        installed.contains(&tall_region()),
        "growing back above the bound must pin a viewport again; everything \
         the child wrote between the second resize and the next input was:\n{}",
        installed.escape_debug()
    );

    let after = session.screen_at(3);
    assert_eq!(after.len(), TALL_ROWS as usize);
    let viewport = &after[(TALL_ROWS - 1) as usize];
    assert!(
        viewport.contains("/context"),
        "the round trip must end with the draft pinned on row {TALL_ROWS}, but \
         it reads {viewport:?}; whole screen:\n{}",
        Session::dump(&after)
    );
    assert!(
        session.status.success(),
        "the session must still exit cleanly, but exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
}
