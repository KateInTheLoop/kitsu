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

/// The model id both configs below declare, and therefore the leading segment
/// of requirement **R5**'s composed status row in every session in this file.
///
/// Named once rather than spelled per assertion: the status-row tests all
/// identify the pinned row by the segment R5's drop order keeps longest
/// (`compose_status_row` truncates the model before it discards it), so a
/// change to the fixture model must not be able to leave half of them
/// matching a stale literal.
const STATUS_ROW_MODEL: &str = "example-model-v1";

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
    /// Deliver `signal` to the child with `kill(2)` and **write no bytes**.
    ///
    /// Card T3-S8, requirement N5. Delivered with `kill` to the child's pid
    /// rather than by writing `\x03` to the master, and that is finding
    /// **F54**, not a stylistic choice: this PTY is nobody's controlling
    /// terminal (no session leader, no `tcsetpgrp`), so the line discipline
    /// generates no job-control signals at all -- a `\x03` byte would simply
    /// be read by the child as a byte, which is also exactly what inline
    /// mode's raw mode (`ISIG` cleared) makes of it. A test that wanted a
    /// *signal* and sent a byte would be measuring the editor's Ctrl+C
    /// keymap instead.
    Signal(libc::c_int),
    /// Write nothing and perform no action for `Duration`, then mark the
    /// stream and move on.
    ///
    /// The point is the **mark**: it separates "everything the child wrote in
    /// response to the previous step alone" from the next step's input, the
    /// same window the `Action::Resize` tests assert in, for a step whose
    /// effect emits nothing a later trigger could key on. Without it the only
    /// way to sequence a step after a keystroke whose output is not unique is
    /// a trigger that was already satisfied, which fires on the next 5 ms
    /// poll and can have its bytes coalesced into the same `read` as the
    /// keystroke.
    Pause(Duration),
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

/// A step that delivers `signal` to the child once `trigger` is seen.
fn signal(trigger: &'static str, signal: libc::c_int) -> Step {
    Step {
        trigger,
        action: Action::Signal(signal),
    }
}

/// A step that waits `millis` once `trigger` is seen, writing nothing.
fn pause(trigger: &'static str, millis: u64) -> Step {
    Step {
        trigger,
        action: Action::Pause(Duration::from_millis(millis)),
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
    /// The PTY's line settings read back **after** the session ended, through
    /// a descriptor on the slave the harness keeps for itself (the same
    /// `dup`-a-probe discipline `tests/cli.rs`'s
    /// `test_auth_login_hides_tty_input_and_restores_echo_on_sigint` uses).
    ///
    /// Card T3-S8: this is the half of a terminal restore that is *not* a
    /// byte in the stream. `RawStdinGuard::engage` clears `ICANON`, `ECHO`
    /// and `ISIG` with `tcsetattr`, and that state belongs to the pty, not to
    /// the process -- so it **survives the child's death**, which is what
    /// makes it a non-vacuous observation: a child killed by a signal with no
    /// restore leaves these three cleared here, exactly as it leaves a real
    /// user's terminal unusable.
    termios: libc::termios,
}

impl Session {
    /// Whether the pty was canonical again once the session ended: `ICANON`,
    /// `ECHO` and `ISIG` all back on, which is what `RawStdinGuard`'s restore
    /// puts back and nothing else in this binary touches.
    fn raw_mode_off(&self) -> bool {
        let flags = self.termios.c_lflag;
        flags & libc::ICANON != 0 && flags & libc::ECHO != 0 && flags & libc::ISIG != 0
    }

    /// The three line flags, named, for a failure message.
    fn termios_dump(&self) -> String {
        let flags = self.termios.c_lflag;
        format!(
            "ICANON={} ECHO={} ISIG={}",
            flags & libc::ICANON != 0,
            flags & libc::ECHO != 0,
            flags & libc::ISIG != 0
        )
    }

    /// Everything the child wrote from scripted action `step` onwards.
    fn raw_after(&self, step: usize) -> String {
        String::from_utf8_lossy(&self.raw[self.marks[step]..]).into_owned()
    }
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
    /// Set `KITSU_TUI=inline` in the child's environment anyway.
    ///
    /// Only the T3-S2 removal test sets this: it exists so one test can prove
    /// the retired variable has no effect, and every other session in this
    /// file keeps the variable removed.
    legacy_env_gate: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            no_session: true,
            resume_on_stop: false,
            legacy_env_gate: false,
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
/// styling and the viewport are live. `tui` is the value passed to the
/// `--tui <MODE>` flag: `None` passes no flag at all, which is classic mode
/// (plan row T3-S2 retired the `KITSU_TUI` gate).
///
/// `options` carries everything that is not the script; see [`Options`].
fn run_styled_chat_pty_with(
    home: &Path,
    tui: Option<&str>,
    script: &[Step],
    options: Options,
) -> Session {
    run_styled_chat_pty_core(home, tui, script, options, &[])
}

/// The one body every styled PTY session shares. `extra_args` is appended
/// after `chat`'s own flags, which is what lets the streaming harness below
/// add `--allow-path` without a second copy of the terminal plumbing.
fn run_styled_chat_pty_core(
    home: &Path,
    tui: Option<&str>,
    script: &[Step],
    options: Options,
    extra_args: &[String],
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
    // The harness's own descriptor on the slave, never handed to the child:
    // it keeps the pty alive after the child dies so its line settings can be
    // read back (see `Session::termios`).
    let probe_slave = unsafe { libc::dup(slave) };
    assert!(child_stdin >= 0 && child_stdout >= 0 && probe_slave >= 0);

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
    command.args(extra_args);
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
    // T3-S2: the renderer is selected by the `--tui <MODE>` flag. The
    // `KITSU_TUI` preview gate is gone, so it is actively REMOVED from the
    // child's environment here: were a stray `KITSU_TUI=inline` in the test
    // runner's shell still able to select the viewport, the classic-mode
    // measurements in this file would be silently measuring inline mode.
    command.env_remove("KITSU_TUI");
    if options.legacy_env_gate {
        command.env("KITSU_TUI", "inline");
    }
    if let Some(mode) = tui {
        command.args(["--tui", mode]);
    }

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
    // An `Action::Pause` in flight: when it is over. The step is not counted
    // as performed, and the stream is not marked, until then.
    let mut pausing: Option<Instant> = None;

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

        if let Some(until) = pausing {
            if Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
                if let Some(status) = poll_child(&mut child, options.resume_on_stop, &mut stops) {
                    break status;
                }
                continue;
            }
            // The mark goes here, at the end of the pause, so the window that
            // closes at it holds everything the child wrote in response to
            // the step before.
            pausing = None;
            marks.push(raw.len());
            step += 1;
        }

        if let Some(next) = script.get(step) {
            let seen = String::from_utf8_lossy(&raw);
            if seen.contains(next.trigger) {
                if let Action::Pause(span) = next.action {
                    pausing = Some(Instant::now() + span);
                    continue;
                }
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
                    Action::Signal(signal) => {
                        // SAFETY: signalling a child this harness owns, by
                        // pid. See `Action::Signal` on why not through the
                        // terminal.
                        assert_eq!(
                            unsafe { libc::kill(pid, signal) },
                            0,
                            "deliver signal {signal} to the styled carrier"
                        );
                    }
                    // Handled above: a pause never reaches here.
                    Action::Pause(_) => unreachable!(),
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

    // Read the line settings back before the probe descriptor is closed: the
    // pty outlives the child precisely because this descriptor is still open.
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `probe_slave` is a live descriptor on the pty slave and the
    // out-pointer is valid.
    assert_eq!(
        unsafe { libc::tcgetattr(probe_slave, termios.as_mut_ptr()) },
        0,
        "read the styled carrier's line settings back"
    );
    // SAFETY: `tcgetattr` returned 0, so the value is initialised.
    let termios = unsafe { termios.assume_init() };
    // SAFETY: the harness owns this descriptor and closes it exactly once.
    unsafe { libc::close(probe_slave) };

    Session {
        raw,
        status,
        stops,
        marks,
        resizes,
        termios,
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

/// Test 1. **The harness itself.** Classic mode, no `--tui` flag: the
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
    // T3-S6 (the wiring card): the pinned row is no longer the literal
    // `kitsu` placeholder but requirement R5's composed status row, so what
    // survives `/help` is *that*. The row is model-led by construction — the
    // model is the only segment R5 never drops outright at a 80-column
    // terminal, and the drop order truncates it before discarding it — so the
    // model name is what identifies the row as the status row rather than
    // debris. The meter is deliberately not pinned here: this session is at
    // the **idle** prompt with no turn and no usage recorded, so whether a
    // meter has a denominator at all is the config's business, not this
    // test's. `inline_mode_status_row_shows_r5_segments_during_a_streamed_turn`
    // pins the full segment set where a turn makes them all real.
    let row = screen[(ROWS - 1) as usize].trim_end();
    assert!(
        row.starts_with(STATUS_ROW_MODEL),
        "the pinned viewport row must show R5's composed status row, which \
         leads with the live model {STATUS_ROW_MODEL:?}, after /help printed; \
         row {ROWS} reads {row:?}; whole screen:\n{}",
        Session::dump(&screen)
    );
    assert!(
        row.chars().count() <= COLS as usize,
        "the status row must never exceed the terminal's {COLS} columns, but \
         row {ROWS} is {} columns of {row:?}; whole screen:\n{}",
        row.chars().count(),
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
    // it.
    //
    // **This assertion used to be `assert_ne!(.., "kitsu")`, and the wiring
    // card made that vacuous** — once the row carries R5's composed status row
    // instead of the placeholder, "not `kitsu`" holds even if the row were
    // left showing a frozen half-drawn status row, or arbitrary debris. Its
    // *intent* (the row is cleared on the way out) survives; its *expression*
    // did not. Finding **F41**: pin what the row IS.
    //
    // Measured, not guessed: `hide` clears row 24, releases the region and
    // parks on row 24, and the closing summary printed there is wider than
    // `COLS`, so it wraps and scrolls the screen up by one — leaving the
    // summary's two physical rows on 22 and 23 and **row 24 genuinely
    // empty**. That is the exact post-state, so that is what is asserted.
    assert_eq!(
        screen[(ROWS - 1) as usize].trim_end(),
        "",
        "the painted status row must be cleared on the way out, leaving row \
         {ROWS} empty; it reads {:?}; screen:\n{}",
        screen[(ROWS - 1) as usize],
        session.screen_dump()
    );
    // ...and that is not vacuous, because the row genuinely *was* the status
    // row until teardown: the same session's mid-run screen has R5's
    // model-led row on 24. Without this, an inline mode that never painted a
    // status row at all would satisfy the clear above.
    let mid = session.screen_at(1);
    assert!(
        mid[(ROWS - 1) as usize]
            .trim_end()
            .starts_with(STATUS_ROW_MODEL),
        "the clear is only meaningful if a status row was there to clear: \
         mid-run row {ROWS} must lead with {STATUS_ROW_MODEL:?}, but reads \
         {:?}; mid-run screen:\n{}",
        mid[(ROWS - 1) as usize],
        Session::dump(&mid)
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

// ---------------------------------------------------------------------------
// T0-S16: the slash palette above the composer, end to end
// ---------------------------------------------------------------------------

/// The palette script: type `/co` at the idle prompt and leave it unsubmitted,
/// then finish `/context` by hand and leave.
///
/// `/co` is a prefix of three registry commands, so the palette has entries to
/// show and is strictly taller than its minimal box. Both inputs trigger on
/// something that exists **today** -- `RAW_MODE_MARKER` for the first
/// keystroke and the draft's own echo for the second -- so a missing palette
/// fails this test on its assertion rather than on the driver's deadline.
fn type_slash_co_then_quit() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/co"),
        send("/co", "ntext\r"),
        send("context window source", "/quit\r"),
    ]
}

/// Test 13. **The composer, end to end.** With `/co` typed and nothing
/// submitted, the pinned viewport must be a real bordered palette with the
/// composer as its bottom row: the lid and its ` slash commands ` label, the
/// matching entries, the footer separator, and the draft on row 24.
///
/// This is the first end-to-end proof that the inline composer draws anything
/// above one row. The unit tests in `tui_bridge` pin the bytes; this one pins
/// that the palette is wired through the real reader, the real guard and a
/// real pseudo-terminal, and that its rows land on **pinned** rows rather than
/// inside the scroll region where the next transcript line would scroll them
/// away.
#[test]
#[serial]
fn inline_mode_shows_the_slash_palette_above_the_composer() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &type_slash_co_then_quit());

    let typed = session.screen_at(1);
    let composer = &typed[(ROWS - 1) as usize];
    assert!(
        composer.contains("/co"),
        "the bottom row must still be the composer, but it reads {composer:?}; \
         whole screen:\n{}",
        Session::dump(&typed)
    );

    // The palette sits directly above it: the footer separator on row 23 and
    // the lid above the entries, all of them inside the pinned region.
    let lid = typed
        .iter()
        .position(|row| row.contains("slash commands"))
        .unwrap_or_else(|| {
            panic!(
                "the palette's lid must be on screen; whole screen:\n{}",
                Session::dump(&typed)
            )
        });
    let footer = typed
        .iter()
        .position(|row| row.contains("type to filter"))
        .unwrap_or_else(|| {
            panic!(
                "the palette's footer separator must be on screen; whole screen:\n{}",
                Session::dump(&typed)
            )
        });
    assert_eq!(
        footer,
        (ROWS - 2) as usize,
        "the footer separator must sit directly above the composer; whole \
         screen:\n{}",
        Session::dump(&typed)
    );
    assert!(
        lid < footer,
        "the lid must be above the footer; whole screen:\n{}",
        Session::dump(&typed)
    );
    assert!(
        typed[lid + 1..footer]
            .iter()
            .any(|row| row.contains("/context")),
        "and the matching entries between them; whole screen:\n{}",
        Session::dump(&typed)
    );

    // The pinned rows really are pinned: the region the child set must be
    // tall enough for the whole box, not the startup one-row region.
    assert!(
        // The region's BOTTOM MARGIN, which is the screen height less the
        // viewport's own height -- not the viewport height itself, and not a
        // three-parameter cursor address the viewport never emits.
        session.raw_count(&format!(
            "\x1b[1;{}r",
            usize::from(ROWS) - (footer - lid + 2)
        )) > 0,
        "the child must latch a region that leaves the palette pinned; raw:\n{}",
        session.raw_text().escape_debug()
    );

    // And the palette is display-only: the draft was `/co`, so the three
    // characters sent next are what completes the command.
    let after = session.screen_at(2);
    assert!(
        after[..(ROWS - 1) as usize]
            .iter()
            .any(|row| row.contains("context window source")),
        "the palette must not have been submitted as text; whole screen:\n{}",
        Session::dump(&after)
    );
}

// ---------------------------------------------------------------------------
// T3-S1: the styled *streaming* harness.
//
// Everything above drives the binary with slash commands only, which is why
// `write_config` can point at `https://gateway.invalid`: no request is ever
// made. No PTY test has therefore ever driven a real turn, so nothing here
// has yet seen the viewport while model output is streaming into it or while
// a tool call interrupts it.
//
// The pieces this section adds:
//
// * [`SseMock`] -- a blocking, dependency-free HTTP/1.1 server on
//   `127.0.0.1:0` that answers each POST with a *chunked* `text/event-stream`
//   body, one HTTP chunk per scripted SSE event with a real gap between them.
//   The gap matters: a body written in one write arrives as one read and the
//   renderer never sees a partial answer, which is the state the viewport
//   assertions are about. `tests/cli.rs` serves SSE the same way at
//   `spawn_stalling_sse_server` and `test_jsonl_partial_stream_failure_*`;
//   this is that pattern without a tokio runtime, because the tests in this
//   file are plain `#[test]`.
// * [`Turn`] -- one request's worth of events. Request *n* is answered by turn
//   *n*, and the last turn answers every request after it, so a tool round
//   trip is just two turns.
// * [`run_styled_streaming_chat_pty`] -- the harness the plan names: the
//   session above, with the config pointed at the mock instead of
//   `gateway.invalid`, and the request bodies the child sent handed back so a
//   test can prove the tool result really went to the model.
// ---------------------------------------------------------------------------

/// The events one request is answered with, and the pause between them.
struct Turn {
    /// SSE event payloads, each written as its own HTTP chunk. `data: [DONE]`
    /// is appended by the mock, so a turn lists only its own events.
    events: Vec<String>,
    /// How long to wait between two chunks. Non-zero is what makes the stream
    /// observable *while in flight* rather than as one atomic delivery.
    gap: Duration,
}

/// One OpenAI-flavored content delta.
fn delta(text: &str) -> String {
    format!(
        r#"data: {{"choices":[{{"index":0,"delta":{{"content":{}}},"finish_reason":null}}]}}"#,
        serde_json_string(text)
    )
}

/// One OpenAI-flavored tool call, delivered whole in a single delta (which is
/// what every tool fixture in `tests/cli.rs` does).
fn tool_call(id: &str, name: &str, arguments: &str) -> String {
    format!(
        r#"data: {{"choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"id":"{id}","type":"function","function":{{"name":"{name}","arguments":{}}}}}]}},"finish_reason":null}}]}}"#,
        serde_json_string(arguments)
    )
}

/// `finish_reason` terminator for a turn.
fn finish(reason: &str) -> String {
    format!(r#"data: {{"choices":[{{"index":0,"delta":{{}},"finish_reason":"{reason}"}}]}}"#)
}

/// Minimal JSON string encoder, so the fixtures above can embed paths and
/// arbitrary text without a serde dependency in this file.
fn serde_json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A blocking streaming-SSE gateway on a loopback port.
///
/// Serves one response per connection and closes it (`connection: close`), so
/// the child's HTTP client cannot reuse a connection and request order is
/// connection order. Every request body is recorded.
struct SseMock {
    address: std::net::SocketAddr,
    bodies: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    joiner: Option<std::thread::JoinHandle<()>>,
}

impl SseMock {
    /// Bind, start serving `turns`, and return before the first request.
    fn start(turns: Vec<Turn>) -> Self {
        use std::io::{BufRead as _, Read as _, Write as _};
        use std::sync::atomic::Ordering;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind SSE mock");
        let address = listener.local_addr().expect("mock address");
        listener
            .set_nonblocking(true)
            .expect("nonblocking SSE listener");
        let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let thread_bodies = std::sync::Arc::clone(&bodies);
        let thread_stop = std::sync::Arc::clone(&stop);
        let joiner = std::thread::spawn(move || {
            let mut served = 0usize;
            while !thread_stop.load(Ordering::Relaxed) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => return,
                };
                stream.set_nonblocking(false).expect("blocking connection");
                let mut reader = std::io::BufReader::new(stream);

                // Request line + headers.
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => return,
                        Ok(_) => {}
                        Err(_) => return,
                    }
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some(value) = trimmed
                        .split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim())
                    {
                        length = value.parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0_u8; length];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                thread_bodies
                    .lock()
                    .expect("record request body")
                    .push(String::from_utf8_lossy(&body).into_owned());

                // The turn that answers this request: the last one repeats.
                let turn = turns.get(served).or_else(|| turns.last());
                served += 1;
                let Some(turn) = turn else { continue };

                let mut socket = reader.into_inner();
                if socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                          transfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                    )
                    .is_err()
                {
                    continue;
                }
                let mut chunks: Vec<String> = turn
                    .events
                    .iter()
                    .map(|event| format!("{event}\n\n"))
                    .collect();
                chunks.push("data: [DONE]\n\n".to_string());
                let mut failed = false;
                for (index, chunk) in chunks.iter().enumerate() {
                    if index > 0 && !turn.gap.is_zero() {
                        std::thread::sleep(turn.gap);
                    }
                    if socket
                        .write_all(format!("{:x}\r\n{chunk}\r\n", chunk.len()).as_bytes())
                        .and_then(|()| socket.flush())
                        .is_err()
                    {
                        failed = true;
                        break;
                    }
                }
                if !failed {
                    let _ = socket.write_all(b"0\r\n\r\n");
                    let _ = socket.flush();
                }
            }
        });

        Self {
            address,
            bodies,
            stop,
            joiner: Some(joiner),
        }
    }

    /// `http://127.0.0.1:<port>`, for the gateway's `base_url`.
    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Every request body the child sent, in order.
    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("read request bodies").clone()
    }
}

impl Drop for SseMock {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(joiner) = self.joiner.take() {
            let _ = joiner.join();
        }
    }
}

/// Write `~/.kitsu/config.toml` pointing the `example` gateway at `base_url`.
///
/// Identical to [`write_config`] except for the reachable address and a pinned
/// `context_window` (so `/context` and the status row have a number to show
/// without a `models_path` probe). [`write_config`] is left alone: the thirteen
/// sessions above assert on a run that makes no request at all.
fn write_streaming_config(home: &Path, base_url: &str) {
    let dir = home.join(".kitsu");
    fs::create_dir_all(&dir).expect("create ~/.kitsu");
    let config = format!(
        r#"default_gateway = "example"

[gateways.example]
flavor       = "openai"
base_url     = "{base_url}"
chat_path    = "{CHAT_PATH}"
models_path  = ""
model        = "example-model-v1"
context_window = 4096
auth         = {{ method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN", expires_env = "ANTHROPIC_TOKEN_EXPIRES", refresh_hint = "run `refresh-token` to refresh the gateway JWT" }}
"#
    );
    fs::write(dir.join("config.toml"), config).expect("write config.toml");
}

/// A finished streaming session: the [`Session`] plus what the mock saw.
struct StreamSession {
    session: Session,
    /// Request bodies in order, so a test can prove round two really carried
    /// the tool result rather than only that the final text rendered.
    bodies: Vec<String>,
}

impl std::ops::Deref for StreamSession {
    type Target = Session;

    fn deref(&self) -> &Session {
        &self.session
    }
}

/// **The T3-S1 harness.** Run a styled, streaming interactive chat on a
/// `ROWS`x`COLS` PTY against a local SSE mock serving `turns`, driven by
/// `script`.
///
/// Identical in every terminal respect to [`run_styled_chat_pty_with`] — same
/// PTY, same three descriptors on the slave, same trigger-driven driver, same
/// `vt100` replay — and different in exactly one: the gateway is reachable, so
/// submitting a line makes the child stream a real turn. `allow_path` passes
/// `--allow-path <home>` so a tool call runs without an approval prompt (the
/// same thing `tests/cli.rs`'s tool-round fixtures do).
fn run_styled_streaming_chat_pty(
    home: &Path,
    tui: Option<&str>,
    script: &[Step],
    turns: Vec<Turn>,
    allow_path: bool,
) -> StreamSession {
    let extra = if allow_path {
        vec![
            "--allow-path".to_string(),
            home.to_string_lossy().into_owned(),
        ]
    } else {
        Vec::new()
    };
    run_styled_streaming_chat_pty_with_args(home, tui, script, turns, &extra)
}

/// [`run_styled_streaming_chat_pty`] with the child's extra arguments spelled
/// out instead of reduced to `allow_path`.
///
/// T3-S7a needs two things the boolean cannot say: a run with **no** grant at
/// all (so a tool call reaches the approval gate) and `--enable-commands` (so
/// the *command* gate exists). The boolean form above is unchanged and
/// delegates here.
fn run_styled_streaming_chat_pty_with_args(
    home: &Path,
    tui: Option<&str>,
    script: &[Step],
    turns: Vec<Turn>,
    extra_args: &[String],
) -> StreamSession {
    let mock = SseMock::start(turns);
    write_streaming_config(home, &mock.base_url());
    let session = run_styled_chat_pty_core(home, tui, script, Options::default(), extra_args);
    let bodies = mock.bodies();
    StreamSession { session, bodies }
}

/// The trigger every streaming script uses for its *second* input: the live
/// summary line kitsu prints when a turn has finished (`↳ 0.1s · … (6%
/// used)`). Triggering on the end of the turn rather than on a fragment of
/// the answer keeps `/quit` out of the mid-turn queue, and triggering on
/// something the surface under test genuinely prints is finding **F46**.
const TURN_DONE_MARKER: &str = "% used)";

/// Submit `prompt` at the idle prompt, then leave once the turn is over.
///
/// Classic mode's idle prompt is rustyline in cooked mode, so the trigger is
/// the banner and the terminator is `\n`; inline mode's is the raw reader, so
/// it is [`RAW_MODE_MARKER`] and `\r` (the same split every script above
/// makes).
fn ask_then_quit_classic(prompt: &'static str) -> Vec<Step> {
    vec![
        send("/help for commands", prompt),
        send(TURN_DONE_MARKER, "/quit\n"),
    ]
}

fn ask_then_quit_inline(prompt: &'static str) -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, prompt),
        send(TURN_DONE_MARKER, "/quit\r"),
    ]
}

/// Test 14. **The harness itself, on a real turn (T3-S1).** Classic mode: a
/// three-fragment streamed answer must reach the screen in order, under the
/// echoed prompt, and the mock must have been asked exactly one question.
///
/// If this fails, nothing else in this section means anything: the streaming
/// half of the harness is wrong.
#[test]
#[serial]
fn classic_mode_streams_a_turn_into_the_transcript() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty(
        home.path(),
        None,
        &ask_then_quit_classic("say hello\n"),
        vec![Turn {
            events: vec![
                delta("STREAM_ALPHA\n"),
                delta("STREAM_BETA\n"),
                delta("STREAM_OMEGA\n"),
                finish("stop"),
            ],
            gap: Duration::from_millis(60),
        }],
        false,
    );

    assert!(
        session.status.success(),
        "the streaming classic session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );

    // The child really talked to the mock, and sent the submitted prompt.
    assert_eq!(
        session.bodies.len(),
        1,
        "one turn is one request; bodies: {:?}",
        session.bodies
    );
    assert!(
        session.bodies[0].contains(r#""content":"say hello""#),
        "the request must carry the submitted prompt; body:\n{}",
        session.bodies[0]
    );

    // The screen as it stood when the turn had finished and `/quit` had not
    // yet been submitted.
    let screen = session.screen_at(1);
    let rows = |needle: &str| {
        screen
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "{needle:?} must be on the screen after a streamed turn; \
                     screen:\n{}",
                    Session::dump(&screen)
                )
            })
    };
    let echo = rows("kitsu> say hello");
    let alpha = rows("STREAM_ALPHA");
    let beta = rows("STREAM_BETA");
    let omega = rows("STREAM_OMEGA");
    assert!(
        echo < alpha && alpha < beta && beta < omega,
        "the streamed fragments belong under the echoed prompt in order, but \
         they are on rows {}, {}, {}, {}; screen:\n{}",
        echo + 1,
        alpha + 1,
        beta + 1,
        omega + 1,
        Session::dump(&screen)
    );
    // Each fragment is its own transcript row, not a concatenation: the
    // newlines the model sent were honored.
    assert_eq!(
        screen[alpha].trim_end(),
        "STREAM_ALPHA",
        "screen:\n{}",
        Session::dump(&screen)
    );
}

/// Test 15. **The same turn in inline mode.** The streamed answer must land
/// *inside* the scroll region, the pinned status row must still read `kitsu`
/// after the turn, and the region must still be set exactly once — i.e. the
/// turn did not re-latch or lose the viewport.
#[test]
#[serial]
fn inline_mode_streams_a_turn_inside_the_region_with_the_status_row_intact() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty(
        home.path(),
        Some("inline"),
        &ask_then_quit_inline("say hello\r"),
        vec![Turn {
            events: vec![
                delta("STREAM_ALPHA\n"),
                delta("STREAM_BETA\n"),
                delta("STREAM_OMEGA\n"),
                finish("stop"),
            ],
            gap: Duration::from_millis(60),
        }],
        false,
    );

    assert!(
        session.status.success(),
        "the streaming inline session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
    assert_eq!(
        session.bodies.len(),
        1,
        "one turn is one request; bodies: {:?}",
        session.bodies
    );

    let screen = session.screen_at(1);
    let region = (ROWS - 1) as usize; // rows 1..=23, viewport_rows = 1
    for needle in ["STREAM_ALPHA", "STREAM_BETA", "STREAM_OMEGA"] {
        let row = screen
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "{needle:?} must be on the screen after a streamed turn in \
                     inline mode; screen:\n{}",
                    Session::dump(&screen)
                )
            });
        assert!(
            row < region,
            "streamed text belongs inside the scroll region (rows 1..={region}), \
             but {needle:?} is on row {}; screen:\n{}",
            row + 1,
            Session::dump(&screen)
        );
    }
    // The wiring card (T3-S5's deferred half) replaced the `"kitsu"`
    // placeholder this row used to hold with requirement R5's composed status
    // row, so what must survive a streamed turn is **that**. The intent is
    // unchanged and is the whole point of the assertion: the stream must not
    // reach row 24. Pinned as what the row IS (finding F41) rather than as a
    // literal — model-led, because the model is the segment R5's drop order
    // keeps longest, and within `COLS`, R5's standing no-wrap rule.
    let status = screen[region].trim_end();
    assert!(
        status.starts_with(STATUS_ROW_MODEL),
        "the pinned status row must survive a streamed turn, still showing \
         R5's composed row led by the live model {STATUS_ROW_MODEL:?}; row \
         {ROWS} reads {status:?}; screen:\n{}",
        Session::dump(&screen)
    );
    assert!(
        status.chars().count() <= COLS as usize,
        "the surviving status row must still fit the terminal's {COLS} \
         columns, but is {} columns of {status:?}; screen:\n{}",
        status.chars().count(),
        Session::dump(&screen)
    );
    assert_eq!(
        session.raw_count(&format!("\x1b[1;{}r", ROWS - 1)),
        1,
        "a streamed turn must not re-latch the region; raw:\n{}",
        session.raw_text().escape_debug()
    );
}

/// Test 16. **A turn that calls a tool.** Two mock turns: the first streams a
/// `fs_read_file` call, the second — which is the request carrying the tool
/// result — streams the final answer. Asserted on three things at once, so a
/// pass means the whole round trip happened rather than just that some text
/// rendered:
///
/// * the tool activity card and its receipt are on the screen;
/// * the final answer is below them;
/// * the *second request body* carries the file's contents in a `tool`
///   message, which is the only direct evidence that the result went back to
///   the model.
#[test]
#[serial]
fn streaming_turn_calls_a_tool_and_renders_its_result() {
    let home = TempDir::new().expect("temp home");
    let note = home.path().join("note.txt");
    fs::write(&note, "TOOL_MARKER_42\n").expect("write the note the model reads");
    let arguments = format!(r#"{{"path":"{}"}}"#, note.to_string_lossy());

    let session = run_styled_streaming_chat_pty(
        home.path(),
        None,
        &ask_then_quit_classic("read the note\n"),
        vec![
            Turn {
                events: vec![
                    tool_call("call_1", "fs_read_file", &arguments),
                    finish("tool_calls"),
                ],
                gap: Duration::from_millis(20),
            },
            Turn {
                events: vec![delta("the note says TOOL_MARKER_42\n"), finish("stop")],
                gap: Duration::from_millis(40),
            },
        ],
        true,
    );

    assert!(
        session.status.success(),
        "the tool-calling session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
    assert_eq!(
        session.bodies.len(),
        2,
        "a tool round trip is two requests; bodies: {:?}",
        session.bodies
    );
    assert!(
        !session.bodies[0].contains("TOOL_MARKER_42"),
        "the first request cannot know the file's contents; body:\n{}",
        session.bodies[0]
    );
    assert!(
        session.bodies[1].contains(r#""role":"tool""#)
            && session.bodies[1].contains("TOOL_MARKER_42"),
        "the second request must carry the tool result back to the model; \
         body:\n{}",
        session.bodies[1]
    );

    let screen = session.screen_at(1);
    let find = |needle: &str| {
        screen
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "{needle:?} must be on the screen after a tool round trip; \
                     screen:\n{}",
                    Session::dump(&screen)
                )
            })
    };
    // The activity card names the tool by its display verb, with the path.
    let card = find("◆ Read(");
    assert!(
        screen[card].contains("note.txt"),
        "the activity card must name the file it read ({:?}); screen:\n{}",
        screen[card],
        Session::dump(&screen)
    );
    let receipt = find("◇ 1 line");
    let answer = find("the note says TOOL_MARKER_42");
    assert!(
        card < receipt && receipt < answer,
        "the card, its receipt and the final answer belong in that order, but \
         they are on rows {}, {}, {}; screen:\n{}",
        card + 1,
        receipt + 1,
        answer + 1,
        Session::dump(&screen)
    );
}

/// Twelve numbered lines, each wider than the 80-column terminal, so the
/// stream both wraps and scrolls while it is in flight.
fn wide_stream_events() -> Vec<String> {
    let mut events: Vec<String> = (1..=12)
        .map(|n| delta(&format!("L{n:02}-{}-END{n:02}\n", "x".repeat(90))))
        .collect();
    events.push(finish("stop"));
    events
}

/// Test 17. **Width and scroll while the stream is in flight (inline mode).**
///
/// The screen is sampled *mid-stream*: the `/quit` step triggers on `L08`, so
/// `marks[1]` is a moment when four more lines are still to come. Three
/// things are asserted about that moment, which is the one the rest of
/// milestone T3 is about:
///
/// * the pinned status row is still the status row — a stream scrolling the
///   region does not reach row 24;
/// * a line wider than the terminal **wraps** rather than being truncated:
///   its `END` sentinel is on the row after its `L` sentinel;
/// * the region really scrolled: `L01` was streamed (it is in the raw bytes)
///   and is no longer on the screen, so the assertion above is not vacuous.
#[test]
#[serial]
fn inline_mode_wide_stream_wraps_and_scrolls_without_touching_the_status_row() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty(
        home.path(),
        Some("inline"),
        &[
            send(RAW_MODE_MARKER, "stream wide\r"),
            // Mid-stream: L08 of 12.
            send("L08-", "/quit\r"),
        ],
        vec![Turn {
            events: wide_stream_events(),
            gap: Duration::from_millis(40),
        }],
        false,
    );

    let region = (ROWS - 1) as usize;
    let screen = session.screen_at(1);
    // Same treatment, same reason as
    // `inline_mode_streams_a_turn_inside_the_region_with_the_status_row_intact`:
    // the row is R5's composed status row now, not the `"kitsu"` placeholder.
    // The claim being made is unchanged — a stream in flight must not reach
    // row 24 — and it is stronger stated as what the row is: the model-led
    // status row, intact and within `COLS`, mid-stream.
    let status = screen[region].trim_end();
    assert!(
        status.starts_with(STATUS_ROW_MODEL),
        "a stream in flight must not reach the pinned status row, which must \
         still be R5's composed row led by the live model \
         {STATUS_ROW_MODEL:?}; row {ROWS} reads {status:?}; screen:\n{}",
        Session::dump(&screen)
    );
    assert!(
        status.chars().count() <= COLS as usize,
        "the untouched status row must still fit the terminal's {COLS} \
         columns, but is {} columns of {status:?}; screen:\n{}",
        status.chars().count(),
        Session::dump(&screen)
    );

    // Wrapping, measured on a line that is certainly complete mid-stream.
    let head = screen
        .iter()
        .position(|row| row.contains("L05-"))
        .unwrap_or_else(|| {
            panic!(
                "L05 must be on the mid-stream screen; screen:\n{}",
                Session::dump(&screen)
            )
        });
    assert_eq!(
        screen[head].chars().count(),
        COLS as usize,
        "a line wider than the terminal must fill the row it starts on; \
         screen:\n{}",
        Session::dump(&screen)
    );
    assert!(
        head + 1 < region && screen[head + 1].contains("END05"),
        "the overflow of L05 belongs on the next row inside the region, but \
         row {} holds {:?}; screen:\n{}",
        head + 2,
        screen.get(head + 1).cloned().unwrap_or_default(),
        Session::dump(&screen)
    );

    // The region scrolled, and the check above is therefore about a screen
    // that really moved: L01 was streamed and has left the screen.
    assert!(
        session.raw_count("L01-") > 0,
        "L01 must have been streamed at all; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert!(
        !screen.iter().any(|row| row.contains("L01-")),
        "twelve wrapped lines must have scrolled L01 out of the region; \
         screen:\n{}",
        Session::dump(&screen)
    );
}

/// Test 18. **A product defect, left failing on purpose (T3-S1).**
///
/// A streamed answer whose last line is **not** newline-terminated loses that
/// line entirely in classic styled mode: it is written to the terminal and
/// then erased before the user can see it. `STREAM_ALPHA` and `STREAM_BETA`
/// arrive with newlines and survive; `STREAM_OMEGA` does not and is gone from
/// every screen, final and mid-session alike. A provider that does not end
/// its last content delta with `\n` — which is the common case — therefore
/// drops its answer's last line in the styled terminal. Measured with the
/// same harness, **inline mode keeps it**: an unterminated last fragment is
/// still on the screen there, which is what makes this classic-mode-specific
/// rather than a property of the stream.
///
/// What the raw bytes show, in stream order: the live row clears the row
/// (`\r\x1b[2K`), `finish_line`'s `flush_unstyled` writes the partial line
/// onto it, the live row repaints `kitsu> ` from column 0 of *that same row*,
/// a `\r\x1b[2K` erases it, and only then does the terminating newline
/// arrive.
///
/// *Hypothesis* for the cause, from `kitsu-render`: the live row paints and
/// clears relative to wherever the cursor happens to be — `render_region`'s
/// fast path writes `\r\x1b[2K{status}` and `clear_region_locked` writes
/// `\r\x1b[2K` with no absolute positioning (`live.rs:631-637`, `:709-713`) —
/// so it assumes stdout always leaves the cursor at column 0 of a fresh row.
/// `finish_line` breaks that assumption for one instant: it writes the
/// buffered partial line and its terminating `\n` as two separate writes
/// (`lib.rs:1611-1641`), so between them the cursor sits mid-row on a row
/// holding assistant text. Which of the two live-row writers lands in that
/// window, and why it is not covered by `write_block`'s suspension, is not
/// something this test establishes; that part is a hypothesis.
///
/// Known failure, T3-S1. Do not weaken this assertion: it is the finding.
#[test]
#[serial]
fn classic_mode_keeps_the_final_unterminated_line_of_a_streamed_answer() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty(
        home.path(),
        None,
        &ask_then_quit_classic("say hello\n"),
        vec![Turn {
            events: vec![
                delta("STREAM_ALPHA\n"),
                delta("STREAM_BETA\n"),
                // No trailing newline: what a provider's last delta usually
                // looks like.
                delta("STREAM_OMEGA"),
                finish("stop"),
            ],
            gap: Duration::from_millis(60),
        }],
        false,
    );

    let screen = session.screen_at(1);
    // Precondition: the newline-terminated fragments did render, so a failure
    // below is about the unterminated line and not about the turn.
    for needle in ["STREAM_ALPHA", "STREAM_BETA"] {
        assert!(
            screen.iter().any(|row| row.contains(needle)),
            "{needle:?} must render; screen:\n{}",
            Session::dump(&screen)
        );
    }
    // And the last line really was streamed: the mock sent it and the child
    // wrote it to the terminal.
    assert!(
        session.raw_count("STREAM_OMEGA") > 0,
        "the unterminated line must at least reach the terminal; raw:\n{}",
        session.raw_text().escape_debug()
    );

    assert!(
        screen.iter().any(|row| row.contains("STREAM_OMEGA")),
        "a streamed answer's last line must survive even without a trailing \
         newline, but it was written and then erased; screen:\n{}",
        Session::dump(&screen)
    );
}

// ---------------------------------------------------------------------------
// T3-S2: the environment gate is gone (plan row T3-S2, "env gate removed").
// FAILING-FIRST: `--tui` does not exist yet, so the flag halves of this test
// fail on an argument error, and `main.rs` still reads `KITSU_TUI`, so the
// env half fails by pinning the region it must no longer pin.
// ---------------------------------------------------------------------------

/// `KITSU_TUI` no longer selects anything, and `--tui` is the only thing that
/// does.
///
/// The variable was a T0 preview gate. Keeping it alongside `[tui] mode` and
/// `--tui` would be three ways to set one thing with no written precedence,
/// which is how the three drift apart (F45); it is removed instead, and this
/// test is what keeps it removed.
///
/// The negative is proved not to be vacuous inside the test itself: the same
/// script, same harness, same geometry, run once with the variable and once
/// with the flag. The flag run must contain the DECSTBM the variable run must
/// not, so an absence caused by a broken harness would fail the second half.
#[test]
#[serial]
fn kitsu_tui_environment_variable_no_longer_selects_the_viewport() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());

    let bottom = ROWS - 1; // rows - viewport_rows, with viewport_rows = 1
    let region = format!("\x1b[1;{bottom}r");

    // The variable alone: classic, i.e. no scroll region is ever set.
    let via_env = run_styled_chat_pty_with(
        home.path(),
        None,
        &help_then_quit(),
        Options {
            legacy_env_gate: true,
            ..Options::default()
        },
    );
    assert_eq!(
        via_env.raw_count(&region),
        0,
        "KITSU_TUI=inline must no longer set a scroll region; raw:\n{}",
        via_env.raw_text().escape_debug()
    );
    assert!(
        via_env.raw_text().contains("commands:"),
        "the session must still have run /help, or the absence above proves \
         nothing; raw:\n{}",
        via_env.raw_text().escape_debug()
    );

    // The flag alone: inline, i.e. the region this harness can observe.
    let via_flag = run_styled_chat_pty(home.path(), Some("inline"), &help_then_quit());
    assert_eq!(
        via_flag.raw_count(&region),
        1,
        "--tui inline must set the region exactly once, which is what makes \
         the assertion above a real absence; raw:\n{}",
        via_flag.raw_text().escape_debug()
    );

    // And the flag wins over the leftover variable, in the direction that
    // would be a silent surprise: an explicit `--tui classic` with
    // `KITSU_TUI=inline` set stays classic.
    let both = run_styled_chat_pty_with(
        home.path(),
        Some("classic"),
        &help_then_quit(),
        Options {
            legacy_env_gate: true,
            ..Options::default()
        },
    );
    assert_eq!(
        both.raw_count(&region),
        0,
        "--tui classic must stay classic with KITSU_TUI=inline set; raw:\n{}",
        both.raw_text().escape_debug()
    );
}

// ===========================================================================
// The wiring card: requirement **R5**'s composed status row, on screen.
//
// `kitsu_render::live::compose_status_row` and the `Surface::queue` seam
// shipped in T3-S5 with **no production caller**; the pinned row still
// painted the literal `"kitsu"` that `main.rs` passed to
// `tui_bridge::install` as a `status: &str`. These tests are the user-visible
// half: the row reaches row 24, carries R5's segments at the terminal's real
// width, and — the part a static string can never satisfy — **ticks**.
//
// # The repaint cadence these tests assume
//
// `main.rs`'s `run_live_typeahead` is the one loop that runs for the whole of
// a foreground turn: it is started by `TypeaheadCollector::start` whenever
// `typeahead_live_prompt` is `Some`, which is
// `!assistive_input && stdin_is_tty() && stderr_styled()` — true of every
// inline session, since inline mode *is* a styled tty. Its body polls stdin
// with `libc::poll(&mut descriptor, 1, 50)` and already refreshes the ambient
// agent tag once per tick through `sync_agent_counts`, which is the mechanism
// requirement A8's 250 ms budget is met by. A 50 ms tick is twenty times
// finer than the one-second granularity of R5's elapsed segment, so a row
// repainted on that tick is never more than one tick stale, and the cadence
// costs no new thread, no new timer and no new dependency.
//
// `read_line_inline` is deliberately *not* a cadence: it blocks in `read()`
// with no timeout (that is how `SIGWINCH` is delivered — see
// `install_sigwinch_handler`'s `SA_RESTART` warning), and between turns R5's
// elapsed segment is `None` anyway, so the idle prompt has nothing to tick.
//
// **Measured before these tests were written** (the pre-wiring baseline, so
// the assertions below are known not to pass for the wrong reason): over a
// five-event streamed turn with 700 ms gaps, row 24 is painted exactly three
// times, every one of them the string `"kitsu"`, and none of them during the
// turn.
// ===========================================================================

/// Strip CSI escape sequences from `text`, leaving the printable cells.
///
/// Hand-rolled rather than borrowed from `kitsu_render::sanitize_terminal_text`
/// on purpose: `kitsu` has no library target, so an integration test cannot
/// reach into the workspace's render crate, and adding a dev-dependency to
/// borrow one function is not worth it. The raw-byte tests below are the only
/// callers; everything else asserts on `vt100`'s already-parsed screen.
fn strip_csi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            // Consume parameter/intermediate bytes up to and including the
            // final byte, which is the first in 0x40..=0x7e.
            while let Some(&c) = chars.peek() {
                chars.next();
                if ('\x40'..='\x7e').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// Every row the inline viewport painted onto its pinned row, in stream
/// order, with escapes stripped.
///
/// `InlineSurface::paint` writes `ESC[<line>;1H ESC[2K <row>` for each pinned
/// row and then parks the cursor with `ESC[<bottom>;1H`, so one paint of row
/// `ROWS` is exactly the bytes between those two addresses. Reading the
/// *paint history* rather than two sampled screens is what makes the elapsed
/// test honest: it observes every repaint the child actually performed,
/// needs no second scripted mark, and contains no sleep.
fn pinned_row_paints(session: &Session) -> Vec<String> {
    let text = session.raw_text();
    let open = format!("\x1b[{};1H\x1b[2K", ROWS);
    let close = format!("\x1b[{};1H", ROWS - 1);
    let mut painted = Vec::new();
    let mut rest = text.as_str();
    while let Some(index) = rest.find(&open) {
        rest = &rest[index + open.len()..];
        let end = rest.find(&close).unwrap_or(rest.len());
        painted.push(strip_csi(&rest[..end]));
    }
    painted
}

/// Every pinned-row paint that is R5's status row, i.e. model-led.
///
/// The teardown clear is a paint of row `ROWS` too, and the closing summary
/// is written at the cursor it parks, so the raw window for the last paint
/// carries both. Selecting on the model keeps those out without banning
/// anything: a row that leads with the live model is the status row and
/// nothing else in this binary writes one.
fn status_row_paints(session: &Session) -> Vec<String> {
    pinned_row_paints(session)
        .into_iter()
        .filter(|row| row.trim_start().starts_with(STATUS_ROW_MODEL))
        .collect()
}

/// The elapsed segment of a composed status row, as whole seconds.
///
/// `kitsu_render::format_wait_elapsed` renders `{secs}s` under a minute and
/// `{mins}m{secs:02}s` at or above one, and R5 places elapsed after the model
/// and the meter, so the token is the row's last ` · `-joined content
/// segment. `None` when the row carries no elapsed segment at all, which is
/// a legitimate state (between turns, or at a width that dropped it) and so
/// is not an assertion failure here.
fn elapsed_secs(row: &str) -> Option<u64> {
    row.split(" · ")
        .map(str::trim)
        .filter_map(|segment| {
            let rest = segment.strip_suffix('s')?;
            match rest.split_once('m') {
                Some((mins, secs)) => {
                    Some(mins.parse::<u64>().ok()? * 60 + secs.parse::<u64>().ok()?)
                }
                None => rest.parse::<u64>().ok(),
            }
        })
        .last()
}

/// **The wiring acceptance test.** R5's composed status row is on the pinned
/// row *during a real streamed turn*, through the T3-S1 streaming harness so
/// the model name is the session's live one rather than a fixture's.
///
/// The screen is sampled at `marks[1]`, i.e. the moment `/quit` goes in,
/// which `TURN_DONE_MARKER` places just after the turn finished. Asserted:
///
/// * the row leads with the live model — the segment R5's drop order keeps
///   longest, and the thing that tells a status row from debris;
/// * it carries the context meter, which the streaming config makes real by
///   declaring `context_window = 4096`: R5's 8-cell meter plus a percentage,
///   so the row shows how full the window is without `/context`;
/// * it carries an elapsed segment, which is the whole reason the row needs
///   a cadence;
/// * it never wraps: at most `COLS` cells, R5's standing rule;
/// * nothing is queued and no agent is detached in this session, so the
///   `[N queued]` tag and the ambient tag are **absent** — and that absence
///   is not vacuous, because the three positive segments above are present
///   on the same row.
#[test]
#[serial]
fn inline_mode_status_row_shows_r5_segments_during_a_streamed_turn() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty(
        home.path(),
        Some("inline"),
        &ask_then_quit_inline("say hello\r"),
        vec![Turn {
            events: vec![
                delta("STREAM_ALPHA\n"),
                delta("STREAM_BETA\n"),
                finish("stop"),
            ],
            gap: Duration::from_millis(400),
        }],
        false,
    );

    assert!(
        session.status.success(),
        "the streaming inline session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );

    let screen = session.screen_at(1);
    let row = screen[(ROWS - 1) as usize].trim_end().to_string();

    assert!(
        row.starts_with(STATUS_ROW_MODEL),
        "the pinned row must be R5's status row, leading with the live model \
         {STATUS_ROW_MODEL:?}; row {ROWS} reads {row:?}; screen:\n{}",
        Session::dump(&screen)
    );
    // The meter: eight cells of `■`/`□` and a percentage, exactly what
    // `compose_status_row` builds from `used`/`effective`.
    let meter = row
        .split(" · ")
        .find(|segment| segment.contains('■') || segment.contains('□'))
        .unwrap_or_else(|| {
            panic!(
                "R5's context meter must be on the status row; row {ROWS} \
                 reads {row:?}; screen:\n{}",
                Session::dump(&screen)
            )
        });
    assert_eq!(
        meter.chars().filter(|&c| c == '■' || c == '□').count(),
        8,
        "the status row's meter is eight cells (half of /context's sixteen), \
         but {meter:?} has a different count; screen:\n{}",
        Session::dump(&screen)
    );
    assert!(
        meter.trim_end().ends_with('%'),
        "the meter carries its utilisation percentage, but reads {meter:?}; \
         screen:\n{}",
        Session::dump(&screen)
    );
    // The elapsed segment, present at all (its *advance* is the next test).
    //
    // Asserted on the paint **history** rather than this screen, because the
    // screen is sampled once the turn has finished and R5's elapsed is `None`
    // between turns -- a row with no elapsed segment there is correct, not a
    // regression. The history says the segment was on the row *while the turn
    // ran*, which is the claim.
    let painted = status_row_paints(&session);
    assert!(
        painted.iter().any(|row| elapsed_secs(row).is_some()),
        "R5's elapsed segment must be on the status row while the turn runs; \
         the status row was painted {} time(s): {painted:#?}",
        painted.len()
    );
    // It never wraps.
    assert!(
        row.chars().count() <= COLS as usize,
        "the status row must never exceed {COLS} columns, but row {ROWS} is \
         {} columns of {row:?}; screen:\n{}",
        row.chars().count(),
        Session::dump(&screen)
    );
    // Nothing queued and no detached agent, so neither optional tag renders.
    // Not a bare substring ban (F41): the three segments above pin what the
    // row IS, and these two say the row is *that and only that*.
    assert!(
        !row.contains("queued"),
        "nothing was queued in this session, so R5's `[N queued]` tag must \
         not render; row {ROWS} reads {row:?}; screen:\n{}",
        Session::dump(&screen)
    );
    assert!(
        !row.contains("agent"),
        "no agent was detached in this session, so R5's ambient tag must not \
         render; row {ROWS} reads {row:?}; screen:\n{}",
        Session::dump(&screen)
    );
}

/// **The cadence test.** R5's elapsed segment *advances* while a turn runs.
///
/// A status row that is composed once and painted once is indistinguishable
/// from the old static `"kitsu"` by every assertion in the test above, so
/// this is the one that pins the feature rather than its appearance. There is
/// no sleep anywhere in it: the turn's own 900 ms inter-chunk gaps are what
/// make real time pass inside the child, the script's two steps are
/// trigger-driven as always, and the observation is the **complete paint
/// history** recovered from the raw byte stream after the session ended —
/// every repaint the child actually performed, in order.
///
/// Asserted:
///
/// * at least two status-row paints carry an elapsed segment, so there is
///   something to compare (a single paint cannot show motion);
/// * the elapsed values never go backwards — a row recomposed per tick from a
///   turn-start `Instant` is monotone, and a decrease would mean the start
///   instant is being reset or the segments are coming from different turns;
/// * and at least one pair genuinely differs, which is the advance itself.
///   Three chunks at 900 ms span over two seconds, so a 50 ms cadence has
///   ~40 opportunities to observe the second tick over.
///
/// Pre-wiring baseline, measured: `status_row_paints` is **empty**, because
/// every one of the three pinned-row paints is the literal `"kitsu"`.
#[test]
#[serial]
fn inline_mode_status_row_elapsed_advances_while_a_turn_runs() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty(
        home.path(),
        Some("inline"),
        &ask_then_quit_inline("say hello slowly\r"),
        vec![Turn {
            events: vec![
                delta("TICK_ONE\n"),
                delta("TICK_TWO\n"),
                delta("TICK_THREE\n"),
                finish("stop"),
            ],
            // Real time inside the child, not in the test process: the mock
            // holds each chunk back, so the turn lasts ~2.7s of elapsed time
            // the row has to show.
            gap: Duration::from_millis(900),
        }],
        false,
    );

    let paints = status_row_paints(&session);
    let samples: Vec<u64> = paints.iter().filter_map(|row| elapsed_secs(row)).collect();

    assert!(
        samples.len() >= 2,
        "the status row must be repainted on the turn's cadence, so a turn \
         lasting ~2.7s must leave at least two paints carrying an elapsed \
         segment; got {} of {} pinned-row paint(s): {:#?}",
        samples.len(),
        pinned_row_paints(&session).len(),
        paints
    );
    assert!(
        samples.windows(2).all(|pair| pair[0] <= pair[1]),
        "elapsed is measured from one turn-start instant, so the painted \
         values can never go backwards; samples: {samples:?}; paints: {:#?}",
        paints
    );
    assert!(
        samples.first() < samples.last(),
        "the elapsed segment must genuinely advance over a ~2.7s turn — a \
         row frozen at {:?} is the static placeholder with extra steps; \
         samples: {samples:?}; paints: {:#?}",
        samples.first(),
        paints
    );
}

/// **The `[N queued]` tag, reusing R5's existing expectation.** A prompt
/// submitted *while a turn is running* is queued rather than run, and the
/// status row must say so.
///
/// The tag's text is `kitsu_render::live::queued_tag`'s — `[1 queued]` — the
/// same literal the classic live row has shown since F38 named it; nothing
/// new is invented here.
///
/// # The mid-turn trigger (finding **F46**)
///
/// **Measured first, because the obvious triggers do not work.** In inline
/// mode a streamed answer does not reach the terminal progressively: the
/// whole of `TICK_ONE`/`TWO`/`THREE` plus the turn summary arrives in one
/// burst *after* the turn, so triggering the mid-turn submission on a
/// streamed fragment (or on the `queued:` receipt that follows it) fires once
/// the turn is already over, and the prompt is then queued against the next
/// turn instead. The single thing the child emits at *turn start* is the
/// bracketed-paste toggle — the idle reader releasing raw mode
/// (`ESC[?2004l`) as the typeahead collector takes stdin — and it does not
/// occur before the submission, so it is a usable and honest trigger: a byte
/// the surface under test genuinely emits, at the moment the test needs.
///
/// With that trigger the receipt lands *before* the first streamed fragment
/// (measured), leaving the rest of the turn for the row to be repainted on
/// its cadence with the tag on it.
///
/// The observation is the paint history for the same reason the elapsed test
/// uses it: the tag's whole life is inside the turn, and the screen after it
/// has nothing left to show.
///
/// The second turn is what consumes the queued prompt, which is why the mock
/// is given two.
#[test]
#[serial]
fn inline_mode_status_row_shows_the_queued_tag_for_a_mid_turn_submission() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty(
        home.path(),
        Some("inline"),
        &[
            send(RAW_MODE_MARKER, "say hello\r"),
            // Turn start: the idle reader has just released raw mode.
            send("\x1b[?2004l", "queue this\r"),
            send(TURN_DONE_MARKER, "/quit\r"),
        ],
        vec![
            Turn {
                events: vec![
                    delta("TICK_ONE\n"),
                    delta("TICK_TWO\n"),
                    delta("TICK_THREE\n"),
                    finish("stop"),
                ],
                gap: Duration::from_millis(900),
            },
            Turn {
                events: vec![delta("SECOND_TURN\n"), finish("stop")],
                gap: Duration::from_millis(60),
            },
        ],
        false,
    );

    // The queue really accepted it: the dispatcher's own live receipt. If this
    // is missing the test below is measuring the wrong thing, so it is named
    // separately rather than folded into the row assertion.
    let text = session.raw_text();
    assert!(
        text.contains("queued: queue this"),
        "the mid-turn submission must reach the prompt queue; raw:\n{}",
        text.escape_debug()
    );

    let painted = status_row_paints(&session);
    let tagged: Vec<&String> = painted
        .iter()
        .filter(|row| row.trim_end().ends_with("[1 queued]"))
        .collect();
    assert!(
        !tagged.is_empty(),
        "one prompt queued mid-turn must render `queued_tag`'s own \
         `[1 queued]` at the end of R5's status row, after the content \
         segments; the status row was painted {} time(s): {painted:#?}",
        painted.len()
    );
    // ...and the tagged row is still a whole status row, not the tag alone:
    // it leads with the live model and it still fits the terminal.
    for row in tagged {
        let row = row.trim_end();
        assert!(
            row.starts_with(STATUS_ROW_MODEL),
            "a queued-tag paint must still be the model-led status row, but \
             reads {row:?}; paints: {painted:#?}"
        );
        assert!(
            row.chars().count() <= COLS as usize,
            "the status row must never exceed {COLS} columns with the queue \
             tag on it either, but {row:?} is {} columns; paints: {painted:#?}",
            row.chars().count()
        );
    }
}

// ===========================================================================
// T3-S7a: the approval modal RENDERS in the bottom pane.
//
// Requirement **R7**: an approval prompt in a TUI mode must render as a
// bottom-pane modal carrying the same `[y] allow once · [a] always (<dir>) ·
// [n] deny` option line (command approvals keeping their provenance rows),
// and the same `◉ permission —` text must be in the transcript receipt
// afterwards. The risk gate is the *receipt*: its sanitize must equal
// classic's.
//
// This card is the rendering half only. The `APPROVAL_INPUT_ACTIVE` protocol
// is deliberately untouched (card T3-S7b), so the sessions below are driven
// exactly as a user drives one today: the approval reader runs in **canonical
// mode with echo**, which `run_live_typeahead` restores the moment the claim
// is observed (`main.rs`, the `if APPROVAL_INPUT_ACTIVE.load(..) > 0` arm of
// its poll loop), so an answer is terminated by `\n` and not `\r` even in
// inline mode.
//
// # Triggers (finding F46)
//
// Every trigger here is a byte the child emits **today** *and* after the
// change: the header and the provenance rows are composed from the same
// strings whether they go to stderr in the scroll region (today) or onto the
// pinned rows (after). A test that triggered on a pane paint would time out
// before it could fail.
// ===========================================================================

/// Every `ESC[<line>;1H ESC[2K <row>` write in the stream, as `(line, row)`
/// with CSI escapes stripped from the row.
///
/// This is [`pinned_row_paints`] generalized from one row to any: the modal
/// is taller than the status row, so a paint of it addresses several
/// consecutive lines before parking the cursor. The write is the *only* way
/// a pinned row is ever drawn (`InlineSurface::paint`, pinned by
/// `paint_positions_clears_and_writes_each_row_then_reparks`), and a
/// transcript line is written with no addressing at all
/// (`transcript_line`: text plus CRLF, "no escape bytes whatever"), so a row
/// that appears here appeared in the pinned pane and nowhere else.
fn addressed_pane_writes(session: &Session) -> Vec<(u16, String)> {
    let text = session.raw_text();
    let bytes = text.as_bytes();
    // Every cursor address `ESC[<n>;1H`: (start, line, end-of-sequence).
    let mut addresses: Vec<(usize, u16, usize)> = Vec::new();
    let mut index = 0usize;
    while index + 2 < bytes.len() {
        if bytes[index] != 0x1b || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let mut cursor = index + 2;
        let digits = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor > digits && bytes[cursor..].starts_with(b";1H") {
            let line = text[digits..cursor].parse::<u16>().unwrap_or(0);
            addresses.push((index, line, cursor + 3));
            index = cursor + 3;
            continue;
        }
        index += 1;
    }
    let mut writes = Vec::new();
    for (position, (_, line, end)) in addresses.iter().copied().enumerate() {
        if !bytes[end..].starts_with(b"\x1b[2K") {
            continue;
        }
        let from = end + 4;
        let to = addresses
            .get(position + 1)
            .map(|(start, _, _)| *start)
            .unwrap_or(bytes.len());
        if to <= from {
            continue;
        }
        writes.push((line, strip_csi(&text[from..to])));
    }
    writes
}

/// Rows the inline viewport painted into the bottom pane, in stream order.
///
/// The pane is pinned to the bottom of the screen, so every row of it
/// addresses a line in `(ROWS - PANE_MAX)..=ROWS`. The bound exists so a
/// modal cannot satisfy R7 by being painted somewhere in the scroll region
/// that happens to carry a `ESC[2K`.
const PANE_MAX: u16 = 8;

fn pane_rows(session: &Session) -> Vec<String> {
    addressed_pane_writes(session)
        .into_iter()
        .filter(|(line, _)| *line > ROWS - PANE_MAX && *line <= ROWS)
        .map(|(_, row)| row)
        .collect()
}

/// Every `◉ permission — …` line in the stream, sanitized and trimmed.
///
/// `strip_csi` is this file's stand-in for `sanitize_terminal_text` (see its
/// docs: an integration test cannot link the render crate), so this is R7's
/// "the sanitize of the receipt" for a whole session's bytes. Split on both
/// terminators because the transcript's own line ending in inline mode is
/// CRLF and the classic prompt's is LF.
fn permission_receipts(session: &Session) -> Vec<String> {
    strip_csi(&session.raw_text())
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| line.starts_with("◉ permission —"))
        .map(str::to_string)
        .collect()
}

/// The same, over only the bytes the child wrote **after** scripted action
/// `step` -- R7's "afterwards".
fn permission_receipts_after(session: &Session, step: usize) -> Vec<String> {
    let tail = String::from_utf8_lossy(&session.raw[session.marks[step]..]).into_owned();
    strip_csi(&tail)
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| line.starts_with("◉ permission —"))
        .map(str::to_string)
        .collect()
}

/// A tool round that touches the filesystem gate: round one asks to read
/// `forbidden`, round two answers in prose.
fn gated_read_turns(forbidden: &Path) -> Vec<Turn> {
    vec![
        Turn {
            events: vec![
                tool_call(
                    "gated-read-1",
                    "fs_read_file",
                    &format!(
                        "{{\"path\":{}}}",
                        serde_json_string(&forbidden.to_string_lossy())
                    ),
                ),
                finish("tool_calls"),
            ],
            gap: Duration::from_millis(30),
        },
        Turn {
            events: vec![delta("GATE_ROUND_DONE\n"), finish("stop")],
            gap: Duration::from_millis(30),
        },
    ]
}

/// A tool round that touches the *command* gate. The answer is a denial, so
/// nothing is ever executed and the test does not depend on OS containment
/// being available at all -- only on the prompt being rendered.
fn gated_command_turns(cwd: &Path) -> Vec<Turn> {
    vec![
        Turn {
            events: vec![
                tool_call(
                    "gated-command-1",
                    "command_exec",
                    &format!(
                        "{{\"argv\":[\"/bin/true\"],\"cwd\":{}}}",
                        serde_json_string(&cwd.to_string_lossy())
                    ),
                ),
                finish("tool_calls"),
            ],
            gap: Duration::from_millis(30),
        },
        Turn {
            events: vec![delta("COMMAND_GATE_DONE\n"), finish("stop")],
            gap: Duration::from_millis(30),
        },
    ]
}

/// A gated read answered `y`, in one mode. `marks[1]` is the answer.
fn gated_read_session(tui: Option<&str>, forbidden: &Path, extra: &[String]) -> StreamSession {
    let home = TempDir::new().expect("temp home");
    let script: Vec<Step> = match tui {
        Some(_) => vec![
            send(RAW_MODE_MARKER, "read the file\r"),
            send("kitsu wants to read", "y\n"),
            send(TURN_DONE_MARKER, "/quit\r"),
        ],
        None => vec![
            send("/help for commands", "read the file\n"),
            send("kitsu wants to read", "y\n"),
            send(TURN_DONE_MARKER, "/quit\n"),
        ],
    };
    run_styled_streaming_chat_pty_with_args(
        home.path(),
        tui,
        &script,
        gated_read_turns(forbidden),
        extra,
    )
}

/// **T3-S7a, the acceptance round-trip.** A real PTY approval in inline mode:
/// the modal's header and its option line are painted into the pinned bottom
/// pane, and `y` from the same stdin the protocol already hands over carries
/// the turn through.
///
/// The gate is real: the child is given no `--allow-path`, its only read root
/// is its launch directory (`home`), and the model asks to read a file in a
/// different temporary directory.
///
/// The absence this test depends on is proved not vacuous by the positives
/// around it: the gate fired (the prompt's own text is in the stream), the
/// turn completed after the answer, and the pane was painted many times in
/// this session -- so "the option line is not among the pane's rows" is a
/// statement about the pane, not about an empty list.
#[test]
#[serial]
fn inline_mode_renders_the_approval_modal_in_the_bottom_pane() {
    let outside = TempDir::new().expect("a directory no read root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").expect("write the gated file");

    let session = gated_read_session(Some("inline"), &forbidden, &[]);
    let text = strip_csi(&session.raw_text());

    // The gate really fired, and the answer really carried the turn.
    assert!(
        text.contains("kitsu wants to read"),
        "the model's read of {} must reach the approval gate; raw:\n{}",
        forbidden.display(),
        session.raw_text().escape_debug()
    );
    assert!(
        text.contains("GATE_ROUND_DONE"),
        "answering `y` must let the turn finish; raw:\n{}",
        session.raw_text().escape_debug()
    );

    let rows = pane_rows(&session);
    assert!(
        !rows.is_empty(),
        "this session must paint the pinned pane at all, or the two \
         assertions below say nothing; raw:\n{}",
        session.raw_text().escape_debug()
    );

    let expected_header = format!("◉ permission — kitsu wants to read {}", forbidden.display());
    assert!(
        rows.iter().any(|row| row.trim() == expected_header),
        "R7: the approval header must be painted on a pinned pane row as \
         {expected_header:?}; the pane painted {} row(s): {rows:#?}",
        rows.len()
    );
    let expected_options = format!(
        "[y] allow once · [a] always ({}) · [n] deny",
        outside.path().display()
    );
    assert!(
        rows.iter().any(|row| row.trim() == expected_options),
        "R7: the option line must be painted on a pinned pane row as \
         {expected_options:?}; the pane painted {} row(s): {rows:#?}",
        rows.len()
    );
}

/// **T3-S7a, R7's risk gate.** The `◉ permission —` receipt in inline mode is
/// byte-identical, after sanitize, to the one classic leaves in the
/// transcript -- and it is there *afterwards*, i.e. in the bytes the child
/// wrote after the answer went in, because the modal itself is transient.
///
/// Both sessions are given the **same** forbidden path, so the two receipts
/// are comparable as bytes rather than as shapes.
#[test]
#[serial]
fn inline_mode_commits_the_same_permission_receipt_as_classic() {
    let outside = TempDir::new().expect("a directory no read root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").expect("write the gated file");

    let classic = gated_read_session(None, &forbidden, &[]);
    let classic_receipts = permission_receipts(&classic);
    assert_eq!(
        classic_receipts,
        vec![format!(
            "◉ permission — kitsu wants to read {}",
            forbidden.display()
        )],
        "the classic receipt is the baseline this test compares against; \
         raw:\n{}",
        classic.raw_text().escape_debug()
    );

    let inline = gated_read_session(Some("inline"), &forbidden, &[]);
    let after = permission_receipts_after(&inline, 1);
    assert_eq!(
        after,
        classic_receipts,
        "R7: after the modal closes, inline mode must leave classic's exact \
         sanitized receipt in the transcript; inline's whole-session \
         receipts were {:#?}; raw:\n{}",
        permission_receipts(&inline),
        inline.raw_text().escape_debug()
    );
}

/// **T3-S7a, R7's `command approvals keep their provenance rows`.** The
/// command modal carries all four provenance rows the classic command prompt
/// composes -- cwd, executable, timeout, authority -- on pinned pane rows,
/// with the command option line.
///
/// Answered `n`: the denial is what keeps this test independent of whether
/// OS containment is available on the machine running it. The prompt is
/// rendered before any execution either way.
#[test]
#[serial]
fn inline_mode_command_approval_modal_keeps_its_provenance_rows() {
    let home = TempDir::new().expect("temp home");
    // A command root that is **not** the launch directory: `HOME` is the
    // launch directory here, so `--enable-commands` alone would default the
    // root to it and be refused for intersecting `~/.kitsu` ("managed
    // credential namespace", measured).
    let work = TempDir::new().expect("temp command root");
    let session = run_styled_streaming_chat_pty_with_args(
        home.path(),
        Some("inline"),
        &[
            send(RAW_MODE_MARKER, "run the command\r"),
            send("authority:", "n\n"),
            send(TURN_DONE_MARKER, "/quit\r"),
        ],
        gated_command_turns(work.path()),
        &[
            "--command-root".to_string(),
            work.path().to_string_lossy().into_owned(),
        ],
    );

    let text = strip_csi(&session.raw_text());
    assert!(
        text.contains("authority:"),
        "the command gate must fire at all; raw:\n{}",
        session.raw_text().escape_debug()
    );

    let rows = pane_rows(&session);
    assert!(
        !rows.is_empty(),
        "this session must paint the pinned pane at all; raw:\n{}",
        session.raw_text().escape_debug()
    );
    for provenance in ["cwd:", "executable:", "timeout:", "authority:"] {
        assert!(
            rows.iter().any(|row| row.trim().starts_with(provenance)),
            "R7: the command modal must keep its {provenance:?} provenance \
             row on a pinned pane row; the pane painted {} row(s): {rows:#?}",
            rows.len()
        );
    }
    let expected_options = "[y] allow once · [a] allow this exact argv/cwd for session · [n] deny";
    assert!(
        rows.iter().any(|row| row.trim() == expected_options),
        "R7: the command modal's option line must be the shared composer's \
         own text {expected_options:?}; the pane painted {} row(s): \
         {rows:#?}",
        rows.len()
    );
}

/// **T3-S7a, the classic guard.** Classic mode's approval bytes do not change:
/// the prompt is the header, the option line and the `> ` cue in the scroll
/// region, and classic paints **no** pinned pane at all -- it installs no
/// viewport (`tui_bridge::install` returns `None` for `TuiMode::Classic`
/// before emitting a byte), so there is nothing for a modal to be painted
/// onto.
///
/// Passes today. It is the regression half of the card: the pane modal must
/// be added without any classic byte moving.
#[test]
#[serial]
fn classic_mode_approval_bytes_are_unchanged_by_the_pane_modal() {
    let outside = TempDir::new().expect("a directory no read root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").expect("write the gated file");

    let session = gated_read_session(None, &forbidden, &[]);
    let text = strip_csi(&session.raw_text());

    assert_eq!(
        permission_receipts(&session),
        vec![format!(
            "◉ permission — kitsu wants to read {}",
            forbidden.display()
        )],
        "classic's receipt text is fixed; raw:\n{}",
        session.raw_text().escape_debug()
    );
    let expected_options = format!(
        "  [y] allow once · [a] always ({}) · [n] deny",
        outside.path().display()
    );
    assert!(
        text.contains(&expected_options),
        "classic's option line is fixed at {expected_options:?}; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert!(
        text.contains("GATE_ROUND_DONE"),
        "the classic round-trip still completes; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert!(
        addressed_pane_writes(&session).is_empty(),
        "classic mode installs no viewport, so it must address and clear no \
         row at all; it wrote {:#?}",
        addressed_pane_writes(&session)
    );
}

// ===========================================================================
// T3-S7b: the approval answer is read IN-LOOP and leaves the transcript alone.
//
// T3-S7a shipped the paint and deliberately left the read alone: the approval
// reader runs in **canonical mode with echo** because `run_live_typeahead`'s
// `if APPROVAL_INPUT_ACTIVE.load(..) > 0` arm drops the `RawStdinGuard` and
// calls `live_input_end()` "for the approval reader", so the kernel echoes
// the typed answer wherever the cursor is parked -- which in inline mode is
// the scroll region, below a tidy modal. This section pins the fix.
//
// # What is being pinned, and what is deliberately not
//
// Nothing here says *how* the read moves in-loop, and nothing here mentions
// `APPROVAL_INPUT_ACTIVE`. The protocol also serves **classic** styled mode
// (`typeahead_live_prompt` is `Some` whenever `!assistive_input &&
// stdin_is_tty() && stderr_styled()`, with no condition on `[tui] mode`), so
// `classic_mode_still_echoes_the_approval_answer_in_the_transcript` below
// pins that classic's canonical, echoing, line-edited read is **untouched** --
// which it must be, since a raw single-key read would take line editing and
// backspace away from a mode whose approval read is deliberately canonical.
//
// # Triggers (finding F46)
//
// Every trigger is a byte the child emits today *and* after the change: the
// prompt's own header text (composed from the same strings either way),
// `TURN_DONE_MARKER`, and `RAW_MODE_MARKER`. No test here triggers on a pane
// paint or on the `> ` cue, because the cue is exactly what moves.
//
// # Bound (finding F46 again)
//
// Every session below runs under the harness's `DEADLINE` (20s): the driver
// kills the child and panics with the whole transcript attached, so a read
// that never resolves -- the deadlock this card's risk lives in -- **fails**
// with evidence rather than hanging the suite.
// ===========================================================================

/// The trigger the interrupt test waits on once the approval read has been
/// closed: the notice the failed turn prints, **discovered from a measured
/// transcript** rather than guessed (finding **F46**).
const INTERRUPTED_MARKER: &str = "authority input closed";

/// How the interrupt test closes the approval read.
///
/// Measured, not assumed (findings **F43**/**F47**): under this harness
/// **Ctrl+C does not interrupt an approval prompt at all**. At the approval
/// moment the read is canonical and `ISIG` is *off*, so `\x03` is echoed as
/// the two characters `^C` and left in the line buffer -- a probe that typed
/// `\x03` and then `/quit` had `read_approval_line` return the single line
/// `^C/quit` and answer "invalid permission response; denied". This is
/// pre-existing and mode-independent: classic and inline behave identically.
///
/// `\x04` is the reachable driver. In canonical mode `ICANON` turns it into
/// a zero-length read, which `read_approval_line` maps to `None` and the
/// prompter to `Decision::Interrupt`; after the move in-loop it is the byte
/// `kitsu_editor` already resolves to `EditorAction::Eof`. Either way it is
/// "the approval read was closed without a decision", which is exactly the
/// case T3-S7a's dismiss path exists for.
const APPROVAL_EOF: &str = "\x04";

/// Every cursor address `ESC[<n>;1H` in `text`, as `(start, line, end-of-sequence)`.
///
/// The same scan [`addressed_pane_writes`] performs inline, lifted to a bare
/// `&str` so the helpers below can read a *window* of the stream (between two
/// scripted marks) rather than only the whole session.
fn cursor_addresses(text: &str) -> Vec<(usize, u16, usize)> {
    let bytes = text.as_bytes();
    let mut addresses = Vec::new();
    let mut index = 0usize;
    while index + 2 < bytes.len() {
        if bytes[index] != 0x1b || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let mut cursor = index + 2;
        let digits = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor > digits && bytes[cursor..].starts_with(b";1H") {
            let line = text[digits..cursor].parse::<u16>().unwrap_or(0);
            addresses.push((index, line, cursor + 3));
            index = cursor + 3;
            continue;
        }
        index += 1;
    }
    addresses
}

/// The part of `text` that **is the transcript**: everything no pinned-row
/// write owns, with CSI escapes stripped.
///
/// This is the inverse of [`pane_rows`], and it rests on the same two facts
/// that file documents: `InlineSurface::paint` draws a pinned row as
/// `ESC[<line>;1H ESC[2K <row>`, and a transcript line is written with **no
/// addressing at all** (`transcript_line`: text plus CRLF, "no escape bytes
/// whatever"). So the stream splits at every cursor address: a segment
/// introduced by an address that is immediately followed by `ESC[2K` is a
/// pinned-row write and belongs to the pane; the stream's head, and the tail
/// after a paint's cursor park, are transcript.
///
/// In classic mode no address is ever emitted, so this is the identity on
/// classic's bytes -- which is what makes the classic guard below compare
/// like with like.
fn transcript_text(text: &str) -> String {
    let addresses = cursor_addresses(text);
    let mut out = String::new();
    let mut keep_from = Some(0usize);
    for (position, (start, _, end)) in addresses.iter().copied().enumerate() {
        if let Some(from) = keep_from.take() {
            out.push_str(&text[from..start]);
        }
        let row_write = text.as_bytes()[end..].starts_with(b"\x1b[2K");
        if !row_write {
            // A park, not a row write: what follows it is transcript, up to
            // the next address (or the end of the window).
            keep_from = Some(end);
            continue;
        }
        let _ = position;
    }
    if let Some(from) = keep_from {
        out.push_str(&text[from..]);
    }
    strip_csi(&out)
}

/// The transcript lines the child wrote **after** scripted action `step`,
/// trimmed, with blanks dropped. `marks[step]` is the stream position just
/// *before* that step's bytes went in, so `transcript_lines_after(s, 1)` of a
/// gated-read session is "everything the transcript gained from the moment
/// the answer was typed".
fn transcript_lines_after(session: &Session, step: usize) -> Vec<String> {
    let tail = String::from_utf8_lossy(&session.raw[session.marks[step]..]).into_owned();
    transcript_text(&tail)
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// The same, for the window between two scripted actions.
fn transcript_lines_between(session: &Session, from: usize, to: usize) -> Vec<String> {
    let window =
        String::from_utf8_lossy(&session.raw[session.marks[from]..session.marks[to]]).into_owned();
    transcript_text(&window)
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// [`pane_rows`] over a window of the stream: rows painted into the pinned
/// pane after scripted action `step`.
fn pane_rows_after(session: &Session, step: usize) -> Vec<String> {
    let tail = String::from_utf8_lossy(&session.raw[session.marks[step]..]).into_owned();
    let addresses = cursor_addresses(&tail);
    let bytes = tail.as_bytes();
    let mut rows = Vec::new();
    for (position, (_, line, end)) in addresses.iter().copied().enumerate() {
        if !bytes[end..].starts_with(b"\x1b[2K") {
            continue;
        }
        if line <= ROWS - PANE_MAX || line > ROWS {
            continue;
        }
        let from = end + 4;
        let to = addresses
            .get(position + 1)
            .map(|(start, _, _)| *start)
            .unwrap_or(bytes.len());
        if to <= from {
            continue;
        }
        rows.push(strip_csi(&tail[from..to]));
    }
    rows
}

/// A turn that asks to read **two** files, one gate round each, then answers
/// in prose. `a` on the first prompt must grant the directory and skip the
/// second gate; `y` must not.
fn gated_two_read_turns(first: &Path, second: &Path) -> Vec<Turn> {
    let mut turns = Vec::new();
    for (index, path) in [first, second].iter().enumerate() {
        turns.push(Turn {
            events: vec![
                tool_call(
                    &format!("gated-two-{index}"),
                    "fs_read_file",
                    &format!(
                        "{{\"path\":{}}}",
                        serde_json_string(&path.to_string_lossy())
                    ),
                ),
                finish("tool_calls"),
            ],
            gap: Duration::from_millis(30),
        });
    }
    turns.push(Turn {
        events: vec![delta("GATE_ROUND_DONE\n"), finish("stop")],
        gap: Duration::from_millis(30),
    });
    turns
}

/// The receipt line an approved read of `path` leaves in the transcript.
fn expected_receipt(path: &Path) -> String {
    format!("◉ permission — kitsu wants to read {}", path.display())
}

/// Every `◉ permission — …` line in the **transcript**, in stream order.
///
/// [`permission_receipts`] reads the whole byte stream, so in inline mode it
/// also matches the modal's own header *paint* -- a measured fact: a denied
/// session's whole-stream receipts are
/// `["◉ permission — … [y] allow once · … · [n] deny> n", "◉ permission — …"]`,
/// because the pane's rows are addressed rather than newline-separated and so
/// collapse into one "line". [`transcript_text`] drops every addressed row by
/// construction, which leaves exactly the committed receipts.
fn transcript_receipts(session: &Session) -> Vec<String> {
    transcript_text(&session.raw_text())
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| line.starts_with("◉ permission —"))
        .map(str::to_string)
        .collect()
}

/// **T3-S7b, the card's user-visible point.** In inline mode the typed answer
/// is **not** echoed into the scroll region: the first thing the transcript
/// gains from the moment the answer is typed is the `◉ permission —` receipt
/// itself.
///
/// Per finding **F41** this pins what the transcript *is* rather than banning
/// a substring: the assertion is an `assert_eq!` on the first transcript line
/// after the answer. Today that line is `y` -- the kernel's echo, printed
/// because the approval read is canonical with echo -- and after the change
/// it must be the receipt.
///
/// The negative is not vacuous three times over: the gate demonstrably fired,
/// the round-trip demonstrably **completed** (so a session that never
/// answered, or one that deadlocked waiting for a key, fails here rather than
/// passing by emitting nothing), and the receipt that must be first is
/// asserted to be present at all.
///
/// Bound: the harness `DEADLINE` (20s), after which the driver kills the
/// child and panics with the transcript.
#[test]
#[serial]
fn inline_mode_approval_answer_never_reaches_the_transcript() {
    let outside = TempDir::new().expect("a directory no read root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").expect("write the gated file");

    let session = gated_read_session(Some("inline"), &forbidden, &[]);
    let text = strip_csi(&session.raw_text());

    // Non-vacuity, part one: the gate fired and the answer carried the turn.
    assert!(
        text.contains("kitsu wants to read"),
        "the read of {} must reach the approval gate; raw:\n{}",
        forbidden.display(),
        session.raw_text().escape_debug()
    );
    assert!(
        text.contains("GATE_ROUND_DONE"),
        "the approval round-trip must COMPLETE -- this is what stops a \
         never-answered or deadlocked read from satisfying the absence \
         below; raw:\n{}",
        session.raw_text().escape_debug()
    );

    // Non-vacuity, part two: there IS a receipt in the window being read.
    let lines = transcript_lines_after(&session, 1);
    let receipt = expected_receipt(&forbidden);
    assert!(
        lines.contains(&receipt),
        "the receipt {receipt:?} must be in the transcript after the answer; \
         the transcript gained {lines:#?}; raw:\n{}",
        session.raw_text().escape_debug()
    );

    // The card: what the transcript gains first is the receipt, not the key.
    assert_eq!(
        lines.first().map(String::as_str),
        Some(receipt.as_str()),
        "T3-S7b: the first thing the transcript gains after the answer is \
         typed must be the receipt and not the echoed answer; it gained \
         {lines:#?}; raw:\n{}",
        session.raw_text().escape_debug()
    );
}

/// **T3-S7b, where the cue goes.** The `> ` cue belongs on a pinned pane row
/// with the rest of the modal, not in the scroll region.
///
/// The negative is pinned over the window from the prompt submission to the
/// answer, and it is not vacuous: the window is asserted to carry transcript
/// lines at all (the turn's own output), and the cue is demonstrably in that
/// window today -- it is the last thing `CliPrompter::ask` prints before the
/// read (`eprint!("> ")` in the pane-painted branch).
///
/// The positive assertion -- a pinned row ending in the cue -- is the one
/// place in this section that encodes a *design* choice: that the cue is
/// relocated rather than replaced by something with no `>` in it. If the
/// implementation renders the cue differently, this assertion is the one to
/// revisit; the negative above is the requirement.
///
/// Bound: the harness `DEADLINE` (20s).
#[test]
#[serial]
fn inline_mode_approval_cue_is_painted_on_a_pinned_row_not_in_the_transcript() {
    let outside = TempDir::new().expect("a directory no read root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").expect("write the gated file");

    let session = gated_read_session(Some("inline"), &forbidden, &[]);

    let window = transcript_lines_between(&session, 0, 1);
    assert!(
        !window.is_empty(),
        "the transcript must gain lines between the prompt and the answer, \
         or the absence below says nothing; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert!(
        !window.iter().any(|line| line == ">"),
        "T3-S7b: the `> ` cue must not be written into the scroll region; \
         the transcript gained {window:#?}; raw:\n{}",
        session.raw_text().escape_debug()
    );

    let rows = pane_rows(&session);
    assert!(
        !rows.is_empty(),
        "this session must paint the pinned pane at all; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert!(
        rows.iter().any(|row| row.trim_end().ends_with('>')),
        "T3-S7b: the cue the user types at belongs on a pinned pane row; the \
         pane painted {} row(s): {rows:#?}",
        rows.len()
    );
}

/// **T3-S7b, the classic guard.** Classic styled mode's approval read stays
/// canonical **with echo**: the cue is in the scroll region and the typed
/// answer is echoed there, exactly as today.
///
/// This is the test that catches an over-broad fix. The typeahead collector
/// and its `APPROVAL_INPUT_ACTIVE` protocol run in classic too
/// (`typeahead_live_prompt` has no condition on `[tui] mode`), and classic has
/// no pane to paint a modal into; a raw single-key read there would silently
/// take line editing and backspace away from a mode whose approval read is
/// deliberately canonical. Together with T3-S7a's
/// `classic_mode_approval_bytes_are_unchanged_by_the_pane_modal` (the option
/// line, the receipt and "no addressed rows at all") this is classic's
/// byte-identity net.
///
/// Passes today, by construction: it is the regression half of the card.
///
/// Bound: the harness `DEADLINE` (20s).
#[test]
#[serial]
fn classic_mode_still_echoes_the_approval_answer_in_the_transcript() {
    let outside = TempDir::new().expect("a directory no read root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").expect("write the gated file");

    let session = gated_read_session(None, &forbidden, &[]);

    let window = transcript_lines_between(&session, 0, 1);
    assert!(
        window.iter().any(|line| line == ">"),
        "classic's `> ` cue stays in the scroll region; the transcript \
         gained {window:#?}; raw:\n{}",
        session.raw_text().escape_debug()
    );
    let after = transcript_lines_after(&session, 1);
    assert_eq!(
        after.first().map(String::as_str),
        Some("y"),
        "classic's answer is still echoed by the kernel into the scroll \
         region, so the first line the transcript gains after the answer is \
         the echo; it gained {after:#?}; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert!(
        addressed_pane_writes(&session).is_empty(),
        "classic installs no viewport, so it addresses no row at all; it \
         wrote {:#?}",
        addressed_pane_writes(&session)
    );
}

/// **T3-S7b, `a` through the shared parser.** `a` is `AllowAlways`: the
/// directory is granted, so the second file in it is **not** gated and there
/// is exactly one receipt.
///
/// Not vacuous, and self-validating: the script answers **once**, so if `a`
/// were resolved as anything but `AllowAlways` the second round would gate
/// again, no answer would ever come, and the session would fail on its third
/// trigger at the harness deadline. Passing therefore requires both that the
/// second file was really read and that it was never gated.
///
/// Bound: the harness `DEADLINE` (20s).
#[test]
#[serial]
fn inline_mode_allow_always_grants_the_whole_directory() {
    let outside = TempDir::new().expect("a directory no read root covers");
    let first = outside.path().join("secret-one.txt");
    let second = outside.path().join("secret-two.txt");
    fs::write(&first, "one").expect("write the first gated file");
    fs::write(&second, "two").expect("write the second gated file");
    let home = TempDir::new().expect("temp home");

    let session = run_styled_streaming_chat_pty_with_args(
        home.path(),
        Some("inline"),
        &[
            send(RAW_MODE_MARKER, "read the files\r"),
            send("kitsu wants to read", "a\n"),
            send(TURN_DONE_MARKER, "/quit\r"),
        ],
        gated_two_read_turns(&first, &second),
        &[],
    );

    assert!(
        strip_csi(&session.raw_text()).contains("GATE_ROUND_DONE"),
        "the second read must run after the grant; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert!(
        strip_csi(&session.raw_text()).contains(&format!("{}", second.display())),
        "the second file must really have been read; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert_eq!(
        transcript_receipts(&session),
        vec![expected_receipt(&first)],
        "`a` grants the directory, so the second file is never gated and the \
         session leaves exactly one receipt; raw:\n{}",
        session.raw_text().escape_debug()
    );
    let lines = transcript_lines_after(&session, 1);
    assert_eq!(
        lines.first().map(String::as_str),
        Some(expected_receipt(&first).as_str()),
        "and `a` is not echoed into the transcript either; it gained \
         {lines:#?}; raw:\n{}",
        session.raw_text().escape_debug()
    );
}

/// **T3-S7b, `n` through the shared parser.** `n` is `Deny`: the tool call
/// fails with the policy denial, the turn still finishes, one receipt is
/// left, and the key is not echoed.
///
/// Bound: the harness `DEADLINE` (20s).
#[test]
#[serial]
fn inline_mode_deny_refuses_the_read_and_the_turn_finishes() {
    let outside = TempDir::new().expect("a directory no read root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").expect("write the gated file");
    let home = TempDir::new().expect("temp home");

    let session = run_styled_streaming_chat_pty_with_args(
        home.path(),
        Some("inline"),
        &[
            send(RAW_MODE_MARKER, "read the file\r"),
            send("kitsu wants to read", "n\n"),
            send(TURN_DONE_MARKER, "/quit\r"),
        ],
        gated_read_turns(&forbidden),
        &[],
    );

    let text = strip_csi(&session.raw_text());
    assert!(
        text.contains("GATE_ROUND_DONE"),
        "a denial must still let the turn finish; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert!(
        text.contains("user denied read access to"),
        "`n` must reach the gate as `Decision::Deny`, which is what makes \
         this a test of the answer and not only of the turn; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert_eq!(
        transcript_receipts(&session),
        vec![expected_receipt(&forbidden)],
        "a denial is a decision, so it leaves the same single receipt; \
         raw:\n{}",
        session.raw_text().escape_debug()
    );
    let lines = transcript_lines_after(&session, 1);
    assert_eq!(
        lines.first().map(String::as_str),
        Some(expected_receipt(&forbidden).as_str()),
        "and `n` is not echoed into the transcript; it gained {lines:#?}; \
         raw:\n{}",
        session.raw_text().escape_debug()
    );
}

/// **T3-S7b, the interrupt path T3-S7a added.** Closing the approval read
/// without answering ([`APPROVAL_EOF`]) is `Decision::Interrupt`: there is
/// no decision to record, so the modal is **dismissed** with no receipt, and the pane goes back to showing the
/// status row.
///
/// The absences are not vacuous: the modal is asserted to have been painted
/// in the first place (its header is on a pinned row), so there was something
/// to dismiss, and the status row coming back on a pinned row *after* the
/// interrupt is the positive proof the teardown ran rather than the pane
/// simply having gone quiet.
///
/// Bound: the harness `DEADLINE` (20s). An interrupt that leaves the reader
/// waiting fails on the third step's trigger, with the transcript attached.
#[test]
#[serial]
fn inline_mode_interrupting_the_approval_dismisses_the_modal_without_a_receipt() {
    let outside = TempDir::new().expect("a directory no read root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").expect("write the gated file");
    let home = TempDir::new().expect("temp home");

    let session = run_styled_streaming_chat_pty_with_args(
        home.path(),
        Some("inline"),
        &[
            send(RAW_MODE_MARKER, "read the file\r"),
            send("kitsu wants to read", APPROVAL_EOF),
            send(INTERRUPTED_MARKER, "/quit\r"),
        ],
        gated_read_turns(&forbidden),
        &[],
    );

    let rows = pane_rows(&session);
    let header = expected_receipt(&forbidden);
    assert!(
        rows.iter().any(|row| row.trim() == header),
        "the modal must have been painted before it can be dismissed; the \
         pane painted {} row(s): {rows:#?}",
        rows.len()
    );
    assert_eq!(
        permission_receipts_after(&session, 1),
        Vec::<String>::new(),
        "an interrupt records no decision, so it commits no receipt; raw:\n{}",
        session.raw_text().escape_debug()
    );
    let after = pane_rows_after(&session, 1);
    assert!(
        after
            .iter()
            .any(|row| row.trim_start().starts_with(STATUS_ROW_MODEL)),
        "after the dismissal the pane must go back to the status row, which \
         is what proves the modal was torn down; the pane painted {} row(s) \
         after the interrupt: {after:#?}",
        after.len()
    );
}

// ---------------------------------------------------------------------------
// T3-S8: `Ctrl+L`, resize without duplication, and terminal restore on every
// exit path (requirements R8 and N5)
// ---------------------------------------------------------------------------

/// The help footer, the one transcript line in this file's scripts that is
/// long, unique, and printed exactly once. Reused from Test 1 rather than
/// invented: the duplication tests below need a line whose *count* is already
/// established, so a change to the help text cannot leave them matching a
/// stale literal.
const HELP_FOOTER: &str = "/help <command> explains one command in detail";

/// Bracketed paste off, the sequence `RawStdinGuard::drop` writes.
const PASTE_OFF: &str = "\x1b[?2004l";

/// DECSTBM reset: the sequence `InlineSurface::hide` writes, and the one a
/// terminal is left unusable without.
const REGION_RESET: &str = "\x1b[r";

/// A draft that cannot occur in anything the binary prints by itself, so a
/// paint carrying it is this test's draft and nothing else.
const UNIQUE_DRAFT: &str = "xyzzy-draft";

/// Print `/help`, type [`UNIQUE_DRAFT`] without submitting it, then `signal`.
///
/// The signal is the last step: the restore under test is the one the child
/// performs while dying, so nothing may be written to it afterwards.
fn help_then_draft_then_signal(signal_number: libc::c_int) -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/help\r"),
        send("commands:", UNIQUE_DRAFT),
        signal(UNIQUE_DRAFT, signal_number),
    ]
}

/// Assert the restore every exit path owes (requirement N5), on a session
/// whose last step was a signal.
///
/// # Why this is not vacuous
///
/// Each of the three observations is false today, and would be false again if
/// the restore were removed:
///
/// * **`ESC[r`.** With no restore the child dies on the signal's default
///   disposition and writes nothing more, so the window after the signal is
///   empty and carries no region reset. The assertion that the region *was*
///   pinned first is what makes the release about something real.
/// * **`ESC[?2004l`.** Same: `RawStdinGuard::drop` writes it and `Drop` does
///   not run on a signal.
/// * **The line settings.** This one is not a byte in the stream at all. The
///   pty's termios is set by the child's `tcsetattr` and **outlives the
///   child** -- the harness holds its own descriptor on the slave, so the pty
///   is still there to be asked. A killed child leaves `ICANON`, `ECHO` and
///   `ISIG` cleared, which is precisely the terminal a user has to run
///   `reset` on.
///
/// The alternate screen and the window title are deliberately **not**
/// asserted **here**, and the reason changed in T5-S2.
///
/// Until this card the reason was that N5's other two clauses were vacuous:
/// `kitsu_tui::TuiRuntime` had no caller in this binary, so an assertion
/// about giving the title back would have passed by construction. That is no
/// longer so. The `/tui fullscreen` switch — the *switch*, not the renderer,
/// which is pure and touches no terminal — now calls `TuiRuntime::enter` and
/// `leave`, so the title is pushed with `ESC[22;2t` and popped with
/// `ESC[23;2t` on a real exit path, and the switch writes `ESC[?1049h` and
/// `ESC[?1049l` around the fullscreen segment itself. **Both clauses are met,
/// and both are asserted** — in the fullscreen transition tests at the bottom
/// of this file, where a session actually enters that mode.
///
/// Why not here as well: this script never leaves inline, so this session
/// takes neither the alternate screen nor the title. An assertion about
/// restoring them would still pass by construction in *this* session, which
/// is the F71 shape — an observation the test could make only of itself.
///
/// One correction while the comment is being rewritten: the old sentence
/// "nothing in this binary enters either" was already false before this card.
/// `run_agent_view_overlay` has written `ESC[?1049h` since T4-S2. The
/// parenthetical about `TuiRuntime` was the true half, and the general claim
/// was over-read from it.
fn assert_restored_after_signal(session: &Session, step: usize, label: &str) {
    let before = String::from_utf8_lossy(&session.raw[..session.marks[step]]).into_owned();
    let region = format!("\x1b[1;{}r", ROWS - 1);
    assert!(
        before.contains(&region),
        "{label}: the region must have been pinned before the signal, or there \
         is nothing to release; raw before the signal:\n{}",
        before.escape_debug()
    );

    let after = session.raw_after(step);
    assert!(
        after.contains(REGION_RESET),
        "{label}: a terminal left with a pinned DECSTBM region stays broken \
         until `reset`, so every exit path owes `ESC[r`; everything the child \
         wrote after the signal was:\n{}",
        after.escape_debug()
    );
    assert!(
        after.contains(PASTE_OFF),
        "{label}: bracketed paste was turned on by `RawStdinGuard::engage` and \
         must be turned off again; the child wrote:\n{}",
        after.escape_debug()
    );
    assert!(
        session.raw_mode_off(),
        "{label}: the pty must be canonical again once kitsu is gone, but reads \
         {}; everything written after the signal was:\n{}",
        session.termios_dump(),
        after.escape_debug()
    );
}

/// **The row's own risk gate: `kill -INT` restores (N5).**
///
/// `BridgeGuard::drop` restores correctly on a normal exit, but `Drop` does
/// not run on a signal, and the only signal this process handles is
/// `SIGWINCH`. So today a `SIGINT` at the inline idle prompt leaves the
/// scroll region pinned and stdin raw.
///
/// The signal is delivered with `kill` to the child's pid: see
/// [`Action::Signal`] and finding F54 for why `\x03` on the master is not a
/// `SIGINT` here (and, under inline mode's `ISIG`-cleared termios, would not
/// be one on a real terminal either).
///
/// The session must also *end*: the harness only returns once the child is
/// reaped, so a `SIGINT` that is swallowed entirely fails this test on the
/// driver's deadline with the whole transcript attached.
#[test]
#[serial]
fn inline_mode_sigint_restores_the_terminal() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("inline"),
        &help_then_draft_then_signal(libc::SIGINT),
    );
    assert_restored_after_signal(&session, 2, "SIGINT");
}

/// The same for `SIGTERM`, which N5 names beside `SIGINT`: `kill` with no
/// arguments is what a supervisor, a `timeout`, or a closing terminal sends,
/// and it is as unhandled today as `SIGINT` is.
#[test]
#[serial]
fn inline_mode_sigterm_restores_the_terminal() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("inline"),
        &help_then_draft_then_signal(libc::SIGTERM),
    );
    assert_restored_after_signal(&session, 2, "SIGTERM");
}

/// **Classic mode is unaffected.** A `SIGINT` to a classic session must not
/// start emitting viewport geometry, and the transcript it already printed
/// must not be re-emitted by whatever the restore does.
///
/// Classic's own raw-mode restore is *not* asserted here. Classic mode holds
/// raw mode only inside rustyline's own readline, which this card does not
/// touch; a claim about it would be this test smuggling in a second card.
#[test]
#[serial]
fn classic_mode_sigint_pins_no_region_and_reprints_nothing() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        None,
        &help_then_draft_then_signal(libc::SIGINT),
    );

    assert_eq!(
        session.raw_count(&format!("\x1b[1;{}r", ROWS - 1)),
        0,
        "classic mode emits no viewport geometry, signal or no signal; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert_eq!(
        session.raw_count(REGION_RESET),
        0,
        "and releases a region it never pinned; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert_eq!(
        session.raw_count(HELP_FOOTER),
        1,
        "the transcript is printed once and never re-emitted; raw:\n{}",
        session.raw_text().escape_debug()
    );
}

/// Print `/help`, type [`UNIQUE_DRAFT`], press `Ctrl+L`, wait, then clear the
/// draft and quit.
///
/// The [`Action::Pause`] is the load-bearing step. `Ctrl+L`'s repaint emits
/// nothing a later trigger could key on that is not already in the stream, so
/// without the pause the `/quit` step's trigger would be satisfied on the next
/// 5 ms poll and its bytes could reach the child in the *same* `read` as the
/// `\x0c` -- where the editor would have processed the whole chunk before the
/// repaint ran, and the row would be repainted from a draft that the same
/// chunk had already cleared. The pause makes `raw_between(2, 3)` exactly what
/// the child wrote in answer to `Ctrl+L` and nothing else, the same window the
/// resize tests assert in.
///
/// `\x15` (Ctrl+U) clears the draft before `/quit` so the submitted line is
/// `/quit` and not `xyzzy-draft/quit`.
fn help_then_draft_then_ctrl_l() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/help\r"),
        send("commands:", UNIQUE_DRAFT),
        send(UNIQUE_DRAFT, "\x0c"),
        pause(UNIQUE_DRAFT, 400),
        send(UNIQUE_DRAFT, "\x15/quit\r"),
    ]
}

/// **Requirement R8, the `Ctrl+L` half.** What "repaint" means here, stated
/// observably rather than as "something happened":
///
/// * the child answers the `\x0c` byte **alone** -- no other input is written
///   during the pause -- by painting the pinned row again: the row's own
///   address-and-clear (`ESC[24;1H ESC[2K`) followed by the draft;
/// * so the draft survives: it is painted at least twice in the session, once
///   while it was typed and once by the repaint, and `Ctrl+L` neither enters
///   the draft as a control character nor clears it;
/// * and the transcript is **not** re-emitted. A repaint that reprinted the
///   scrolled-back transcript would duplicate every line of `/help`; the
///   footer stays at exactly one occurrence in the whole byte stream, and at
///   one occurrence on the screen.
///
/// Today `0x0c` falls into the editor's "other control bytes are ignored"
/// arm, so the pause window is empty and the first assertion is what fails.
#[test]
#[serial]
fn inline_mode_ctrl_l_repaints_the_pinned_row_without_duplicating_the_transcript() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &help_then_draft_then_ctrl_l());

    let answer = session.raw_between(2, 3);
    let paint = format!("\x1b[{ROWS};1H\x1b[2K");
    assert!(
        answer.contains(&paint),
        "Ctrl+L alone must repaint the pinned row ({paint:?}); everything the \
         child wrote between the keystroke and the next input was:\n{}",
        answer.escape_debug()
    );
    assert!(
        answer.contains(UNIQUE_DRAFT),
        "and the repaint must carry the draft that was already typed; it \
         wrote:\n{}",
        answer.escape_debug()
    );

    let drafted = pinned_row_paints(&session)
        .into_iter()
        .filter(|row| row.contains(UNIQUE_DRAFT))
        .count();
    assert!(
        drafted >= 2,
        "the draft must be painted once as it was typed and once by the \
         repaint, but only {drafted} of the pinned-row paints carry it: {:?}",
        pinned_row_paints(&session)
    );

    assert_eq!(
        session.raw_count(HELP_FOOTER),
        1,
        "a repaint must not re-emit the transcript; raw:\n{}",
        session.raw_text().escape_debug()
    );
    let screen = session.screen_at(3);
    assert_eq!(
        screen
            .iter()
            .filter(|row| row.contains(HELP_FOOTER))
            .count(),
        1,
        "and must leave exactly one copy of it on the screen; screen:\n{}",
        Session::dump(&screen)
    );
    assert!(
        session.status.success(),
        "the session must still exit cleanly, but exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
}

/// **Classic mode is unaffected by `Ctrl+L`.** rustyline owns the keystroke
/// there; what this pins is that nothing in the classic path grew a viewport
/// and that the transcript is not duplicated.
#[test]
#[serial]
fn classic_mode_ctrl_l_pins_no_region_and_keeps_one_transcript() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), None, &help_then_draft_then_ctrl_l());

    assert_eq!(
        session.raw_count(&format!("\x1b[1;{}r", ROWS - 1)),
        0,
        "classic mode emits no viewport geometry on Ctrl+L; raw:\n{}",
        session.raw_text().escape_debug()
    );
    assert_eq!(
        session.raw_count(HELP_FOOTER),
        1,
        "and prints the transcript once; raw:\n{}",
        session.raw_text().escape_debug()
    );
}

/// Print `/help`, type a draft, grow the window, submit, then quit.
///
/// Built on the existing resize scripts' discipline: the resize writes no
/// bytes and the submit triggers on the *new* region, so the measurement is
/// of the signal alone.
fn help_then_draft_then_grow() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/help\r"),
        send("commands:", "/context"),
        resize("/context", TALL_ROWS, COLS),
        submit_on_tall_region(),
        send("context window source", "/quit\r"),
    ]
}

/// **Requirement R8, the resize half, over a screenful of transcript.**
///
/// Tests 10 to 12 already pin that the *pinned row* moves rather than being
/// duplicated. What they do not have above them is a transcript: their
/// sessions resize with nothing on the screen but a draft. This one prints
/// `/help` first, so the region is full of scrolled transcript when the
/// signal lands, and asserts the duplication R8 actually names -- a history
/// line appearing twice -- against the same `HELP_FOOTER` sentinel Test 1
/// counts.
#[test]
#[serial]
fn inline_mode_resize_does_not_duplicate_transcript_lines() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &help_then_draft_then_grow());

    let answer = session.raw_between(2, 3);
    assert!(
        answer.contains(&tall_region()),
        "the signal alone must re-latch at the new geometry; it wrote:\n{}",
        answer.escape_debug()
    );
    assert!(
        !answer.contains(HELP_FOOTER),
        "a reflow must not reprint transcript lines; between the resize and \
         the next input the child wrote:\n{}",
        answer.escape_debug()
    );
    assert_eq!(
        session.raw_count(HELP_FOOTER),
        1,
        "the transcript line is emitted exactly once in the whole session; \
         raw:\n{}",
        session.raw_text().escape_debug()
    );

    let screen = session.screen_at(3);
    assert_eq!(screen.len(), TALL_ROWS as usize);
    assert_eq!(
        screen
            .iter()
            .filter(|row| row.contains(HELP_FOOTER))
            .count(),
        1,
        "and appears on exactly one row of the reflowed screen; screen:\n{}",
        Session::dump(&screen)
    );
}

// ---------------------------------------------------------------------------
// Card T3-S9: `/tui classic|inline` as a runtime switch (requirement R2).
//
// `/tui` DOES NOT EXIST YET: it is not in `SLASH_COMMANDS` and there is no
// dispatch arm, so every session below currently answers the command with
// `unknown command: /tui - did you mean ...?` and never prints the switch
// receipt the scripts trigger on. Each of these tests therefore fails today,
// most of them by the harness deadline with the transcript attached (finding
// F46 names that shape deliberately: the view cannot print what the script
// waits for, because the feature is absent).
// ---------------------------------------------------------------------------

/// The transcript marker, and why it is `/theme <nonsense>` rather than
/// anything purpose-built.
///
/// "The transcript continues across a switch" needs content that is
/// (a) emitted by the binary itself, (b) **unique** in the whole byte stream,
/// so "exactly once" is countable rather than approximated, (c) exactly one
/// physical row at `COLS` = 80, because a 24-row screen with no scrollback in
/// the emulator is the whole budget, and (d) available with no network, no
/// tool and no turn. `/theme zz1` is all four: `kitsu_render::set_theme`
/// rejects the name with one line that quotes it back, the quoted token
/// appears nowhere else, and the line is 76 columns.
///
/// Pinned as whole lines rather than as substrings (finding F41): what is
/// asserted is what the row **is**.
const TUI_MARK_1: &str =
    "unknown theme 'zz1' — available: kitsune, sakura, ocean, forest, ember, mono";
const TUI_MARK_2: &str =
    "unknown theme 'zz2' — available: kitsune, sakura, ocean, forest, ember, mono";
const TUI_MARK_3: &str =
    "unknown theme 'zz3' — available: kitsune, sakura, ocean, forest, ember, mono";

/// The receipt a successful switch prints, per mode. **This is a decision
/// this card makes, not an observation**: a switch must be narrated, because
/// R2's gate is that the conversation survives and a silent switch leaves the
/// user unable to tell a working switch from a lost one.
const TUI_SWITCHED_TO_CLASSIC: &str =
    "~ switched to the classic renderer; the conversation is unchanged";
const TUI_SWITCHED_TO_INLINE: &str =
    "~ switched to the inline renderer; the conversation is unchanged";

/// Bare `/tui` **reports**, it does not print usage. Decision: the command's
/// argument is optional in the registry (`/tui [classic|inline|fullscreen]`),
/// the
/// question "which renderer am I in?" has no other answer in the product, and
/// `/theme` with no argument already answers the same question for themes.
/// The accepted vocabulary rides along, so the report is also the usage line.
/// `fullscreen` joined the vocabulary in T5-S2, so all three reports list
/// three modes. The report's shape is unchanged.
const TUI_REPORT_CLASSIC: &str = "tui mode: classic (accepted: classic, inline, fullscreen)";
const TUI_REPORT_INLINE: &str = "tui mode: inline (accepted: classic, inline, fullscreen)";
const TUI_REPORT_FULLSCREEN: &str = "tui mode: fullscreen (accepted: classic, inline, fullscreen)";

/// The third switch receipt, built from the same literal in `run_chat` as the
/// other two: `"~ switched to the {label} renderer; the conversation is
/// unchanged"`.
///
/// **This replaces `TUI_FULLSCREEN_REFUSED`**, which read "/tui fullscreen is
/// not available yet; accepted: classic, inline". T5-S2 retires that
/// boundary: the mode has a renderer, a switch arm and an alternate screen,
/// so the refusal it pinned no longer exists to be pinned.
const TUI_SWITCHED_TO_FULLSCREEN: &str =
    "~ switched to the fullscreen renderer; the conversation is unchanged";

/// The alternate screen, taken and given back.
///
/// **This is the F89 observable.** A `FullscreenView` that compiles proves
/// nothing; a `?1049h` in the byte stream proves the switch ran, and only a
/// transcript line repainted *after* that offset proves the reader read the
/// ring. The two are asserted separately and deliberately.
///
/// Attributable in these scripts, which is what keeps the count meaningful:
/// the binary's other `?1049h` writer is `run_agent_view_overlay`, reached
/// only by the agent-view chord or `/jobs view`, and no script below types
/// either. (So "nothing emits this today" is **not** the non-vacuity
/// argument — "nothing in *this session* can emit it but the switch" is.)
const ALT_SCREEN_ON: &str = "\x1b[?1049h";
const ALT_SCREEN_OFF: &str = "\x1b[?1049l";

/// N5's title clause, which stops being vacuous in this card: the switch
/// calls `TuiRuntime::enter`/`leave`, and these are the only two sequences in
/// the workspace that push and pop the terminal's title stack.
const TITLE_PUSH: &str = "\x1b[22;2t";
const TITLE_POP: &str = "\x1b[23;2t";

/// The DECSTBM region this geometry pins, and its release.
fn tui_region() -> String {
    format!("\x1b[1;{}r", ROWS - 1)
}

/// Inline → classic → inline in one session, with a marker printed in each of
/// the three segments.
fn switch_both_directions() -> Vec<Step> {
    vec![
        // Segment 1: inline.
        send(RAW_MODE_MARKER, "/theme zz1\r"),
        send(TUI_MARK_1, "/tui classic\r"),
        // Segment 2: classic.
        send(TUI_SWITCHED_TO_CLASSIC, "/theme zz2\r"),
        send(TUI_MARK_2, "/tui inline\r"),
        // Segment 3: inline again.
        send(TUI_SWITCHED_TO_INLINE, "/theme zz3\r"),
        send(TUI_MARK_3, "/quit\r"),
    ]
}

/// **The card's acceptance, and requirement R2's gate.**
///
/// What makes "the transcript continues" an assertion rather than a slogan:
/// each marker is counted **in the raw byte stream** (emitted exactly once —
/// a switch that reprinted the scrollback to rebuild it would count 2) and
/// **on the screen the user is left looking at** before `/quit` (present
/// exactly once — a switch that cleared the screen, took the alternate
/// screen, or scrolled the region away would count 0). Both halves are
/// needed: counting only the stream passes for a switch that erases the
/// screen, and counting only the screen passes for a switch that replays
/// history.
///
/// Were the gate violated, this test says which way: a dropped transcript
/// fails the screen count at 0 with the screen dumped, a replayed one fails
/// the raw count at 2 or more.
#[test]
#[serial]
fn tui_switch_both_directions_keeps_one_copy_of_the_transcript() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &switch_both_directions());

    let text = session.raw_text();
    for (label, mark) in [
        ("zz1", TUI_MARK_1),
        ("zz2", TUI_MARK_2),
        ("zz3", TUI_MARK_3),
    ] {
        assert_eq!(
            session.raw_count(mark),
            1,
            "the {label} transcript line must be written exactly once in the \
             whole session — a switch neither drops it nor replays it; raw:\n{}",
            text.escape_debug()
        );
    }

    // The screen with all three segments printed and `/quit` not yet sent.
    let screen = session.screen_at(5);
    for (label, mark) in [
        ("zz1", TUI_MARK_1),
        ("zz2", TUI_MARK_2),
        ("zz3", TUI_MARK_3),
    ] {
        assert_eq!(
            screen.iter().filter(|row| row.trim_end() == mark).count(),
            1,
            "the {label} line, printed {} the switch, must still be on screen \
             exactly once after both switches; screen:\n{}",
            if label == "zz3" { "after" } else { "before" },
            Session::dump(&screen)
        );
    }

    // Not vacuous in the other direction either: the three markers are in
    // transcript order, so the screen holds a *continuing* conversation and
    // not three unordered fragments.
    let positions: Vec<usize> = [TUI_MARK_1, TUI_MARK_2, TUI_MARK_3]
        .iter()
        .map(|mark| {
            screen
                .iter()
                .position(|row| row.trim_end() == *mark)
                .expect("each marker was just asserted to be on screen")
        })
        .collect();
    assert!(
        positions[0] < positions[1] && positions[1] < positions[2],
        "the three markers must read down the screen in the order they were \
         printed, but they are on rows {positions:?}; screen:\n{}",
        Session::dump(&screen)
    );

    assert!(
        session.status.success(),
        "the session must still exit cleanly, but exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
}

/// **Requirement 4 of the card: the region is released on the way out of
/// inline and re-pinned on the way back in.** T3-S8's lesson is the reason: a
/// leaked DECSTBM region leaves the user's terminal clamped to 23 rows with
/// no kitsu left to release it.
///
/// Counted, not merely present: this session enters inline twice and leaves
/// it twice (once by `/tui classic`, once by `/quit`), so the region is set
/// twice and released twice. A switch that forgot the teardown would set it
/// twice and release once; one that tore down without re-installing would set
/// it once.
#[test]
#[serial]
fn tui_switch_releases_the_region_and_re_pins_it() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &switch_both_directions());
    let text = session.raw_text();

    assert_eq!(
        session.raw_count(&tui_region()),
        2,
        "the region is pinned once per inline segment, and this session has \
         two; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count("\x1b[r"),
        2,
        "and released once per exit from inline: the switch away, and the \
         session's end; raw:\n{}",
        text.escape_debug()
    );

    // The classic segment is genuinely region-free: between the switch away
    // and the switch back the child pins nothing.
    let classic_segment = session.raw_between(1, 3);
    assert!(
        !classic_segment.contains(&tui_region()),
        "no region may be pinned while the session is in classic mode; \
         between the two switches the child wrote:\n{}",
        classic_segment.escape_debug()
    );
    assert!(
        classic_segment.contains("\x1b[r"),
        "and the release must happen *in* that window rather than only at \
         exit; between the two switches the child wrote:\n{}",
        classic_segment.escape_debug()
    );

    // ...and the re-install really re-pinned, so the count above is not two
    // copies of one install: the status row is painted again on the bottom
    // row of the second inline segment. R5's row, not the `kitsu` fallback —
    // a re-install that forgot to republish `StatusSource` would leave the
    // placeholder there, which is a live trap in `run_chat` (the source is
    // handed to the guard once, outside the loop).
    let screen = session.screen_at(5);
    let row = screen[(ROWS - 1) as usize].trim_end();
    assert!(
        row.starts_with(STATUS_ROW_MODEL),
        "after switching back to inline the pinned row must carry R5's \
         composed status row again, which leads with {STATUS_ROW_MODEL:?}; \
         row {ROWS} reads {row:?}; screen:\n{}",
        Session::dump(&screen)
    );
    assert!(
        session.raw_mode_off(),
        "the pty must be canonical again after the session: {}",
        session.termios_dump()
    );
}

/// Classic → inline, the other start mode, with the transcript gate applied
/// to the one switch this direction has.
fn switch_from_classic() -> Vec<Step> {
    vec![
        send("/help for commands", "/theme zz1\r"),
        send(TUI_MARK_1, "/tui inline\r"),
        send(TUI_SWITCHED_TO_INLINE, "/theme zz2\r"),
        send(TUI_MARK_2, "/quit\r"),
    ]
}

/// A session **started** in classic mode switches into inline and keeps what
/// it had already printed.
#[test]
#[serial]
fn tui_switch_from_classic_into_inline_keeps_the_transcript_and_pins_the_region() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), None, &switch_from_classic());
    let text = session.raw_text();

    assert_eq!(
        session.raw_count(&tui_region()),
        1,
        "the switch into inline pins the region exactly once; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count("\x1b[r"),
        1,
        "and the session releases it exactly once, at exit; raw:\n{}",
        text.escape_debug()
    );
    // The region is pinned *after* the classic-segment transcript, which is
    // what says the switch happened mid-session rather than at startup.
    let pinned_at = text.find(&tui_region()).unwrap_or_else(|| {
        panic!(
            "the region must be pinned at all; raw:\n{}",
            text.escape_debug()
        )
    });
    let mark_at = text.find(TUI_MARK_1).unwrap_or_else(|| {
        panic!(
            "the classic segment must print its marker; raw:\n{}",
            text.escape_debug()
        )
    });
    assert!(
        mark_at < pinned_at,
        "the classic transcript must precede the region being pinned, \
         otherwise this session started inline and proves nothing; raw:\n{}",
        text.escape_debug()
    );

    let screen = session.screen_at(3);
    for (label, mark) in [("zz1", TUI_MARK_1), ("zz2", TUI_MARK_2)] {
        assert_eq!(
            screen.iter().filter(|row| row.trim_end() == mark).count(),
            1,
            "the {label} line must be on screen exactly once after the \
             switch; screen:\n{}",
            Session::dump(&screen)
        );
        assert_eq!(
            session.raw_count(mark),
            1,
            "...and written exactly once; raw:\n{}",
            text.escape_debug()
        );
    }
}

// ---------------------------------------------------------------------------
// Card T5-S2: `/tui fullscreen` as a reachable renderer (A+B).
//
// `fullscreen` IS NOT REACHABLE YET. `main.rs`'s `/tui` arm answers it with
// `/tui fullscreen is not available yet`, `TuiRendererRequest` has two
// variants, and nothing in the binary constructs `FullscreenView` or calls
// `TuiRuntime::enter`. So every test below fails today, most of them at the
// harness deadline with the transcript attached: the script waits for a
// receipt or an `ESC[?1049h` the child cannot print (finding F46's shape).
//
// Four transitions are new here, and `classic -> fullscreen` is written
// first on purpose — see its own note. The other two directions
// (`classic <-> inline`) are already covered above and are untouched.
// ---------------------------------------------------------------------------

/// Classic -> fullscreen: a marker printed with no ring behind it, then the
/// switch.
fn switch_from_classic_into_fullscreen() -> Vec<Step> {
    vec![
        send("/help for commands", "/tui\r"),
        send(TUI_REPORT_CLASSIC, "/theme zz1\r"),
        send(TUI_MARK_1, "/tui fullscreen\r"),
        send(ALT_SCREEN_ON, "/quit\r"),
    ]
}

/// **Classic -> fullscreen, and the vacuity hazard in this card.**
///
/// This transition is written first because it is the one that can paint an
/// **empty screen and pass**. The cell ring is inline's scrollback: `install`
/// builds it for `TuiMode::Inline`, and a classic segment feeds nothing. So a
/// `/tui fullscreen` out of classic has an empty ring by construction, and a
/// view that painted nothing at all would satisfy any "the frame appeared"
/// assertion written here. That is T4-S8's hazard sitting inside a
/// transition, and the response is to **not** put the F89 repaint assertion
/// in this test — it belongs in the inline -> fullscreen one below, where
/// there is something in the ring that only a reader could have put on
/// screen.
///
/// What this test pins instead is everything that is *not* the ring: the
/// switch runs mid-session rather than at startup, it is narrated, it takes
/// and gives back the alternate screen, and the marker it had no ring to
/// repaint is written **exactly once** — the honest reading of an empty ring,
/// and the assertion that would catch a frame conjured from somewhere other
/// than the ring.
#[test]
#[serial]
fn tui_switch_from_classic_into_fullscreen_has_an_empty_ring_and_says_so() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), None, &switch_from_classic_into_fullscreen());
    let text = session.raw_text();

    // Intent kept from the retired refusal test: bare `/tui` reports the live
    // renderer and names the whole accepted vocabulary, which is now three
    // modes wide. Pinned in classic here and in inline and fullscreen in the
    // two tests below, so each of the three reports is asserted once.
    assert_eq!(
        session.raw_count(TUI_REPORT_CLASSIC),
        1,
        "bare /tui reports classic and the three accepted spellings, exactly \
         once; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(TUI_SWITCHED_TO_FULLSCREEN),
        1,
        "the switch into fullscreen is narrated exactly once, like the other \
         two: R2's gate is that a user can tell a working switch from a lost \
         one; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        1,
        "the switch takes the alternate screen exactly once; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_OFF),
        1,
        "...and gives it back exactly once, at exit — N5's clause, now real; \
         raw:\n{}",
        text.escape_debug()
    );

    // Mid-session, not at startup: this session started classic, so the
    // classic transcript must precede the alternate screen.
    let mark_at = text.find(TUI_MARK_1).unwrap_or_else(|| {
        panic!(
            "the classic segment must print its marker; raw:\n{}",
            text.escape_debug()
        )
    });
    let entered_at = text.find(ALT_SCREEN_ON).unwrap_or_else(|| {
        panic!(
            "the switch must enter the alternate screen; raw:\n{}",
            text.escape_debug()
        )
    });
    assert!(
        mark_at < entered_at,
        "the classic transcript must precede the alternate screen, otherwise \
         this session started in fullscreen and proves nothing about the \
         switch; raw:\n{}",
        text.escape_debug()
    );
    let left_at = text.find(ALT_SCREEN_OFF).unwrap_or_else(|| {
        panic!(
            "the alternate screen must be given back; raw:\n{}",
            text.escape_debug()
        )
    });
    assert!(
        entered_at < left_at,
        "and it must be entered before it is left; raw:\n{}",
        text.escape_debug()
    );

    // The empty-ring claim, stated as a count. Nothing fed a ring in classic,
    // so there is nothing for the view to repaint and the marker stays at one
    // occurrence. A second occurrence here would mean the frame was painted
    // from something other than this session's ring.
    assert_eq!(
        session.raw_count(TUI_MARK_1),
        1,
        "with an empty ring the classic marker is written once and never \
         repainted; raw:\n{}",
        text.escape_debug()
    );

    assert!(
        !text.contains("unknown command: /tui"),
        "/tui must stay a registered command on this path; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        session.status.success(),
        "the session exits cleanly out of fullscreen, but exited {:?}; \
         screen:\n{}",
        session.status,
        session.screen_dump()
    );
    assert!(
        session.raw_mode_off(),
        "the pty must be canonical again after a fullscreen session: {}",
        session.termios_dump()
    );
}

/// The marker this card's F89 assertion is carried by, and **why it is a
/// streamed answer rather than `/theme <nonsense>` like the three markers
/// above**.
///
/// This script needs a line that is in the **cell ring**, not merely on the
/// screen, and the two are not the same set. The ring is fed only by writers
/// that go through `kitsu_render`'s surface seam —
/// `InlineSurface::transcript_line` and `write_block`, reached from
/// `print_fragment`, `finish_line`, `tool_activity`, `tool_result_error`,
/// `milestone` and `render_error`. The binary's own `eprintln!` is a macro
/// (`main.rs`) that expands to `std::eprintln!` and writes straight to fd 2,
/// so **every diagnostic it prints bypasses the ring**: `/theme zz1`'s error
/// is one of those, and a session that printed only diagnostics hands the
/// switch an empty ring (measured: 0 retained lines).
///
/// The first version of this test used `TUI_MARK_1` and could not pass for
/// that reason. Routing the macro through the seam was considered and
/// **rejected** — it would re-route several hundred call sites, change inline
/// *and* classic byte streams and reorder diagnostics against `println!`, and
/// it is a decision about what the ring is a *record of* rather than a
/// wiring fix. What the ring records is the rendered conversation.
///
/// So the marker is a **streamed model fragment**, which `print_fragment`
/// hands to the seam as a block. It satisfies the same four properties
/// `TUI_MARK_1` was chosen for: emitted by the binary, unique in the whole
/// byte stream, one physical row at `COLS` = 80, and reachable with no real
/// network (the SSE mock supplies it).
///
/// # Why the session runs with `--no-tools`, which is not a detail
///
/// A fragment only reaches `print_fragment` when the round **did not offer
/// tools** (`orchestrator.rs`: "when tools are advertised this round, DON'T
/// stream text live — buffer it"). A buffered answer is printed by
/// `print_message`, which writes to `io::stdout()` **directly** and so
/// bypasses the seam exactly as `eprintln!` does — measured here too: with
/// tools advertised the ring held one empty line, pushed by `finish_line`'s
/// trailing newline, and nothing else.
///
/// Interactive chat advertises six tools by default, so `--no-tools` is what
/// makes this turn's text stream live and therefore land in the ring. Two
/// turns with a tool call in the first would also work — the final round
/// withholds tools — but that is a second mock turn and a `--allow-path`
/// grant to arrange the same one line.
const RING_MARK: &str = "RING_ALPHA";

/// Inline -> fullscreen: a **streamed** marker into the ring, a bare `/tui`
/// report, then the switch.
///
/// The turn is waited out on [`TURN_DONE_MARKER`] rather than on the fragment
/// itself, for the reason every streaming script here does: a `/tui` typed
/// mid-turn is refused rather than queued (`TUI_MID_TURN_REFUSAL`), so the
/// switch would never run.
///
/// And it is waited out with a [`pause`] rather than by triggering the `/tui`
/// on the summary line directly, which **was** tried and is not enough: the
/// summary is printed by the turn, and a line written the instant it appears
/// still reaches the mid-turn collector and is refused. `/quit` is not gated
/// that way, which is why the streaming scripts above can trigger on the
/// marker and this one cannot. The pause is the separation the marker alone
/// does not give; the step after it triggers on a condition already met, and
/// the pause is what keeps that from coalescing with the turn.
fn inline_then_fullscreen() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "say hello\r"),
        pause(TURN_DONE_MARKER, 500),
        send(RAW_MODE_MARKER, "/tui\r"),
        send(TUI_REPORT_INLINE, "/tui fullscreen\r"),
        send(ALT_SCREEN_ON, "/quit\r"),
    ]
}

/// **The reachability test, and the assertion finding F89 exists for.**
///
/// This is a rewrite of `tui_refuses_fullscreen_and_reports_the_mode_when_bare`
/// rather than a new test beside it: that test was the exact inverse of this
/// one — the same session shape, the same region-count machinery, the same
/// bare-`/tui` report — asserting that the mode was refused. Two tests, one
/// asserting a refusal and one an acceptance of the same input, cannot both
/// be right, so the refusal is retired and its skeleton reused (**F85**).
/// Its surviving intents, all still asserted below: the mode is answered **by
/// name** and never as an unknown command, bare `/tui` reports the renderer
/// it is actually in, nothing is pinned that should not be, and the session
/// keeps running and exits cleanly afterwards.
///
/// # How this proves the reader *read*
///
/// Two observations on the raw byte stream, and the distinction between them
/// is the whole of F89:
///
/// * **`ESC[?1049h` appears.** The switch ran. A capability that compiles is
///   not one that is reachable, and this is the byte that says a user typing
///   `/tui fullscreen` at a prompt got to the new code at all.
/// * **[`RING_MARK`] appears again *after* that offset.** The reader read.
///   The marker was streamed once already, into the inline transcript, and
///   therefore — because a streamed fragment goes through the surface seam —
///   into the cell ring `install` built for this session. Nothing else in the
///   process holds that line: the primary screen's copy is bytes already
///   past, and the alternate screen starts blank. So a second occurrence of
///   it after the alternate screen is entered can only have come from a
///   reader that pulled it out of the ring and painted it.
///
/// That second assertion is what T5-S1b was rejected for lacking. The feed
/// and a reader a user can reach land in one commit, or neither does.
#[test]
#[serial]
fn tui_switch_from_inline_into_fullscreen_repaints_the_ring() {
    let home = TempDir::new().expect("temp home");
    // The streaming harness writes its own config, pointed at the SSE mock
    // that supplies the one fragment this test needs in the ring.
    let session = run_styled_streaming_chat_pty_with_args(
        home.path(),
        Some("inline"),
        &inline_then_fullscreen(),
        vec![Turn {
            events: vec![delta(&format!("{RING_MARK}\n")), finish("stop")],
            gap: Duration::from_millis(60),
        }],
        &["--no-tools".to_string()],
    );
    let text = session.raw_text();

    assert_eq!(
        session.bodies.len(),
        1,
        "one turn is one request, so the fragment really came from the mock; \
         bodies: {:?}",
        session.bodies
    );

    // Intent kept from the retired test: bare `/tui` reports the live
    // renderer, and the vocabulary rides along.
    assert_eq!(
        session.raw_count(TUI_REPORT_INLINE),
        1,
        "bare /tui reports the current mode and all three accepted \
         spellings, exactly once; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(TUI_SWITCHED_TO_FULLSCREEN),
        1,
        "`fullscreen` is answered by name, with the switch receipt the other \
         two modes get; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        !text.contains("not available yet"),
        "and it is no longer refused: the renderer exists, so no surface may \
         still report it as unavailable; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        !text.contains("unknown command: /tui"),
        "/tui must be a registered command, so no spelling may fall through \
         to the unknown-command path; raw:\n{}",
        text.escape_debug()
    );

    // (a) The switch ran.
    let entered_at = text.find(ALT_SCREEN_ON).unwrap_or_else(|| {
        panic!(
            "the switch must enter the alternate screen, which is the byte \
             that says `/tui fullscreen` reached the new renderer at all; \
             raw:\n{}",
            text.escape_debug()
        )
    });
    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        1,
        "exactly once: the one switch in this session, and no agent view is \
         opened here; raw:\n{}",
        text.escape_debug()
    );

    // (b) The reader read. F89's assertion.
    let first_mark = text.find(RING_MARK).unwrap_or_else(|| {
        panic!(
            "the inline segment must stream its marker into the ring; raw:\n{}",
            text.escape_debug()
        )
    });
    assert!(
        first_mark < entered_at,
        "the marker must be written while inline, so that the ring is what \
         holds it by the time the alternate screen is entered; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        text[entered_at..].contains(RING_MARK),
        "the transcript line must be REPAINTED after the alternate screen is \
         entered: that is the only way a line written before the switch can \
         appear on a screen that starts blank, and it is what distinguishes a \
         reader that reads from a view that merely compiles (F89). Everything \
         from the alternate screen onwards:\n{}",
        text[entered_at..].escape_debug()
    );
    assert!(
        session.raw_count(RING_MARK) >= 2,
        "...which is also two occurrences of it in the stream, not one; \
         raw:\n{}",
        text.escape_debug()
    );

    // Nothing is left pinned: the inline segment's region is released on the
    // way into fullscreen or at exit, and either way the counts balance.
    assert_eq!(
        session.raw_count(&tui_region()),
        1,
        "the inline segment pins the region exactly once, and fullscreen does \
         not pin a second inline region; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(REGION_RESET),
        1,
        "and it is released exactly once; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_OFF),
        1,
        "the alternate screen is given back on the way out; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        session.status.success(),
        "`/quit` must work from fullscreen — a mode that cannot be left is a \
         trap — but the session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
    assert!(
        session.raw_mode_off(),
        "the pty must be canonical again after the session: {}",
        session.termios_dump()
    );
}

/// A session **started** in fullscreen (`--tui fullscreen`) that switches
/// into inline.
fn fullscreen_start_then_inline() -> Vec<Step> {
    vec![
        send(ALT_SCREEN_ON, "/tui\r"),
        send(TUI_REPORT_FULLSCREEN, "/tui inline\r"),
        send(TUI_SWITCHED_TO_INLINE, "/theme zz2\r"),
        send(TUI_MARK_2, "/quit\r"),
    ]
}

/// **Fullscreen -> inline, which is `tui_bridge::install`'s third call site.**
///
/// Finding **F87** recorded a two-site obligation: every field `install`
/// publishes has to be republished by every *re*-install, because a
/// re-install that skips one leaves `install`'s own fallback in place for the
/// rest of the session and nothing fails. This card makes it three sites, and
/// **two of the three are post-switch re-installs that fail silently** — so
/// the republish is asserted here exactly as `tui_switch_releases_the_region
/// _and_re_pins_it` asserts it for the inline -> classic -> inline path: the
/// pinned row must carry R5's composed status row, which leads with the
/// model name, and not the fixed `kitsu` placeholder.
///
/// It also pins the surface Decision 1 of this card is about: a mode a
/// session can switch to must be a mode it can **start** in, so this session
/// starts in fullscreen from the flag.
#[test]
#[serial]
fn tui_switch_from_fullscreen_into_inline_re_pins_and_republishes_the_status_row() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("fullscreen"),
        &fullscreen_start_then_inline(),
    );
    let text = session.raw_text();

    // Started in fullscreen: the alternate screen is taken before the first
    // scripted keystroke, which is what `--tui fullscreen` has to mean.
    let entered_at = text.find(ALT_SCREEN_ON).unwrap_or_else(|| {
        panic!(
            "`--tui fullscreen` must start the session in fullscreen; raw:\n{}",
            text.escape_debug()
        )
    });
    assert!(
        entered_at < session.marks[0],
        "the alternate screen must be taken at startup, before any input, or \
         the flag only changed a label; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(TUI_REPORT_FULLSCREEN),
        1,
        "and bare /tui reports the live renderer as fullscreen, exactly once; \
         raw:\n{}",
        text.escape_debug()
    );

    // The switch away leaves the alternate screen *before* the inline
    // segment writes to the real screen.
    let left_at = text.find(ALT_SCREEN_OFF).unwrap_or_else(|| {
        panic!(
            "the switch out of fullscreen must give the alternate screen \
             back; raw:\n{}",
            text.escape_debug()
        )
    });
    let mark_at = text.find(TUI_MARK_2).unwrap_or_else(|| {
        panic!(
            "the inline segment must print its marker; raw:\n{}",
            text.escape_debug()
        )
    });
    assert!(
        left_at < mark_at,
        "the alternate screen must be given back before the inline segment \
         writes, or the user's transcript lands on a screen that is about to \
         be discarded; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_OFF),
        1,
        "exactly once, at the switch and not again at exit; raw:\n{}",
        text.escape_debug()
    );

    // The third install site really installed.
    assert_eq!(
        session.raw_count(&tui_region()),
        1,
        "the switch into inline pins the region exactly once; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(REGION_RESET),
        1,
        "and releases it exactly once, at exit; raw:\n{}",
        text.escape_debug()
    );

    // F87's clause: the re-install republished R5's status source rather than
    // leaving `install`'s `"kitsu"` fallback on the pinned row.
    let screen = session.screen_at(3);
    let row = screen[(ROWS - 1) as usize].trim_end();
    assert!(
        row.starts_with(STATUS_ROW_MODEL),
        "after switching out of fullscreen the pinned row must carry R5's \
         composed status row, which leads with {STATUS_ROW_MODEL:?} — a \
         re-install that forgot to republish the status source leaves the \
         fixed `kitsu` placeholder there and nothing else fails; row {ROWS} \
         reads {row:?}; screen:\n{}",
        Session::dump(&screen)
    );
    assert!(
        session.raw_mode_off(),
        "the pty must be canonical again after the session: {}",
        session.termios_dump()
    );
}

/// A session started in fullscreen that drops all the way back to classic.
fn fullscreen_start_then_classic() -> Vec<Step> {
    vec![
        send(ALT_SCREEN_ON, "/tui classic\r"),
        send(TUI_SWITCHED_TO_CLASSIC, "/theme zz2\r"),
        send(TUI_MARK_2, "/quit\r"),
    ]
}

/// **Fullscreen -> classic: the terminal is given back whole.**
///
/// The fourth and last new transition, and the one with no renderer on the
/// other side to hide a half-restore: after it the session owns no pinned
/// row, no scroll region, no alternate screen and no title of its own. This
/// is where **N5's two formerly vacuous clauses** are asserted as a pair —
/// the alternate screen *and* the window title — because this is the only
/// script in the file with exactly one complete fullscreen segment, entered
/// and left before the process ends, so each sequence must appear exactly
/// once and the counts mean something.
#[test]
#[serial]
fn tui_switch_from_fullscreen_into_classic_restores_the_screen_and_the_title() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("fullscreen"),
        &fullscreen_start_then_classic(),
    );
    let text = session.raw_text();

    assert_eq!(
        session.raw_count(TUI_SWITCHED_TO_CLASSIC),
        1,
        "the switch is narrated; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        1,
        "one fullscreen segment takes the alternate screen once; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_OFF),
        1,
        "...and gives it back once, at the switch; raw:\n{}",
        text.escape_debug()
    );

    // N5's title clause. `TuiRuntime::enter`/`leave` are the only writers of
    // these two sequences in the workspace, so the pair is also the proof
    // that the switch went through the runtime rather than open-coding the
    // alternate screen beside it.
    assert_eq!(
        session.raw_count(TITLE_PUSH),
        1,
        "the fullscreen segment pushes the terminal's title once \
         (`TuiRuntime::enter`), which is N5's title clause becoming real; \
         raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(TITLE_POP),
        1,
        "and pops it once on the way out (`TuiRuntime::leave`); raw:\n{}",
        text.escape_debug()
    );
    let pushed_at = text.find(TITLE_PUSH).expect("the push was just counted");
    let popped_at = text.find(TITLE_POP).expect("the pop was just counted");
    assert!(
        pushed_at < popped_at,
        "pushed before popped, or the stack is unbalanced; raw:\n{}",
        text.escape_debug()
    );

    // The classic segment is genuinely bare: no region, no alternate screen,
    // and the marker it prints goes to the real screen.
    let classic_segment = session.raw_after(1);
    assert!(
        !classic_segment.contains(&tui_region()),
        "no region may be pinned after the drop to classic; after the switch \
         the child wrote:\n{}",
        classic_segment.escape_debug()
    );
    assert!(
        !classic_segment.contains(ALT_SCREEN_ON),
        "and the alternate screen must not be re-taken; after the switch the \
         child wrote:\n{}",
        classic_segment.escape_debug()
    );
    assert!(
        classic_segment.contains(TUI_MARK_2),
        "the classic segment keeps printing its transcript; after the switch \
         the child wrote:\n{}",
        classic_segment.escape_debug()
    );
    assert!(
        session.status.success(),
        "the session exits cleanly, but exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
    assert!(
        session.raw_mode_off(),
        "the pty must be canonical again after the session: {}",
        session.termios_dump()
    );
}

// ---------------------------------------------------------------------------
// Card T5-S1c: the fullscreen frame must FOLLOW the ring, not freeze at entry.
//
// Finding **F97**. `enter_fullscreen` paints one frame and returns: there is
// no repaint, no read loop and no output routing, so `/tui fullscreen` today
// means "take the alternate screen, print the ring's tail once, then carry on
// writing to that screen normally". The frame is a snapshot the conversation
// scrolls over.
//
// Every T5-S2 transition test above is satisfied by that single paint —
// including `tui_switch_from_inline_into_fullscreen_repaints_the_ring`, whose
// second occurrence of the marker proves the ring *was* read. **A test that
// proves the ring was read does not prove the ring is being read**, and the
// test below is the one that separates them: it makes the ring grow *while
// fullscreen is live* and asserts the retained line is painted again
// afterwards.
//
// The non-regression test after it pins the other half: inline and classic
// have no fullscreen frame, so neither may grow one.
// ---------------------------------------------------------------------------

/// The second streamed marker, for the turn that runs **inside** fullscreen.
///
/// Chosen for the four properties [`RING_MARK`]'s doc comment lists, and for
/// the same reason it is a streamed fragment rather than a diagnostic: only a
/// writer that goes through the surface seam can reach a ring at all, and
/// `eprintln!` bypasses it.
const RING_MARK_2: &str = "RING_BETA";

/// Inline -> fullscreen with the marker in the ring, then a **second streamed
/// turn while fullscreen holds the screen**, then out.
///
/// The second turn is what this script adds to [`inline_then_fullscreen`], and
/// it is submitted at fullscreen's own idle prompt. Both waits are [`pause`]s
/// on a trigger that is already satisfied, which is the separation this file's
/// streaming scripts use: a line written the instant a turn's output appears
/// still reaches the mid-turn collector, where `/tui` is refused and the
/// prompt would be queued instead of read.
fn fullscreen_then_a_second_streamed_turn() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "say hello\r"),
        pause(TURN_DONE_MARKER, 500),
        send(TURN_DONE_MARKER, "/tui fullscreen\r"),
        pause(TUI_SWITCHED_TO_FULLSCREEN, 500),
        send(TUI_SWITCHED_TO_FULLSCREEN, "say more\r"),
        pause(RING_MARK_2, 800),
        send(RING_MARK_2, "/quit\r"),
    ]
}

/// **F97's missing assertion: the frame follows the ring.**
///
/// The test above it proves a reader read the ring once. This one proves the
/// frame is still the ring's: the session streams [`RING_MARK`] while inline,
/// switches into fullscreen (which paints it), and then streams
/// [`RING_MARK_2`] with fullscreen holding the alternate screen.
///
/// # Why counting [`RING_MARK`] is the assertion
///
/// A single paint can only ever put that line on the stream **twice**: once
/// as the inline transcript wrote it, and once in the entry frame. Nothing
/// else in the process holds it — the primary screen's copy is bytes already
/// past, and the alternate screen starts blank. So a **third** occurrence can
/// only come from a frame painted again after entry, which is what a renderer
/// that follows its ring does when the ring grows and what a snapshot cannot
/// do at all.
///
/// # And why the order of the last two occurrences is the follow
///
/// The frame is painted oldest line first, so the retained line sits *above*
/// the new one. Asserting that the stream's **last** occurrence of
/// [`RING_MARK`] is inside the alternate screen and is followed by
/// [`RING_MARK_2`] pins both halves of transition 10 in
/// `kitsu_tui::fullscreen`'s table — the appended line entered the frame, and
/// the frame it entered is the one still showing the older line — without
/// duplicating the view's own unit coverage of it (the view is pure and
/// already pinned there; what is unpinned is whether production ever paints a
/// second frame).
///
/// This is deliberately silent about *how* the new line reaches the screen: a
/// renderer may write the line and then repaint, or route it through the ring
/// and let the repaint be its only appearance. Both satisfy the assertions
/// here, and choosing between them is the implementation's.
#[test]
#[serial]
fn tui_fullscreen_frame_follows_the_ring_while_it_is_live() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty_with_args(
        home.path(),
        Some("inline"),
        &fullscreen_then_a_second_streamed_turn(),
        vec![
            Turn {
                events: vec![delta(&format!("{RING_MARK}\n")), finish("stop")],
                gap: Duration::from_millis(60),
            },
            Turn {
                events: vec![delta(&format!("{RING_MARK_2}\n")), finish("stop")],
                gap: Duration::from_millis(60),
            },
        ],
        &["--no-tools".to_string()],
    );
    let text = session.raw_text();

    assert_eq!(
        session.bodies.len(),
        2,
        "two turns are two requests, so the second fragment really came from \
         the mock and was really streamed after the switch; bodies: {:?}",
        session.bodies
    );

    // The switch ran, exactly once, and this session is in fullscreen for the
    // whole of the second turn.
    let entered_at = text.find(ALT_SCREEN_ON).unwrap_or_else(|| {
        panic!(
            "the switch must enter the alternate screen; raw:\n{}",
            text.escape_debug()
        )
    });
    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        1,
        "exactly once: one switch, and no agent view is opened here; raw:\n{}",
        text.escape_debug()
    );
    let first_mark = text.find(RING_MARK).unwrap_or_else(|| {
        panic!(
            "the inline segment must stream its marker into the ring; raw:\n{}",
            text.escape_debug()
        )
    });
    assert!(
        first_mark < entered_at,
        "the first marker must be written while inline, so the ring is what \
         holds it once the alternate screen is entered; raw:\n{}",
        text.escape_debug()
    );
    let second_mark = text.find(RING_MARK_2).unwrap_or_else(|| {
        panic!(
            "the second turn must stream its marker; raw:\n{}",
            text.escape_debug()
        )
    });
    assert!(
        entered_at < second_mark,
        "and the second turn must run with fullscreen already holding the \
         screen, or this test measures the entry frame again; raw:\n{}",
        text.escape_debug()
    );

    // F97's assertion. Two occurrences is the single paint; three is a frame
    // painted again after the ring grew.
    assert!(
        session.raw_count(RING_MARK) >= 3,
        "the fullscreen frame must FOLLOW the ring: the retained line is \
         painted once by the inline transcript and once by the entry frame, \
         so a third occurrence is the repaint that a mode which paints one \
         frame and returns can never produce (F97). Counted {}; \
         everything from the alternate screen onwards:\n{}",
        session.raw_count(RING_MARK),
        text[entered_at..].escape_debug()
    );

    // ...and that repaint is a frame, with the appended line below the
    // retained one (`fullscreen`'s transition 10).
    let last_mark = text
        .rfind(RING_MARK)
        .expect("the marker was just counted at least three times");
    assert!(
        last_mark > entered_at,
        "the last paint of the retained line must be inside the alternate \
         screen, not back in the inline transcript; everything from the \
         alternate screen onwards:\n{}",
        text[entered_at..].escape_debug()
    );
    assert!(
        text[last_mark..].contains(RING_MARK_2),
        "and the appended line must be on screen *below* the retained one, \
         which is what makes the repaint a frame that follows rather than a \
         stray reprint of one line; everything from that paint onwards:\n{}",
        text[last_mark..].escape_debug()
    );

    assert_eq!(
        session.raw_count(ALT_SCREEN_OFF),
        1,
        "the alternate screen is still given back exactly once; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        session.status.success(),
        "`/quit` must still work after a turn taken in fullscreen, but the \
         session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
    assert!(
        session.raw_mode_off(),
        "the pty must be canonical again after the session: {}",
        session.termios_dump()
    );
}

/// Two streamed turns and no switch at all, for the renderer named by `tui`.
fn two_streamed_turns() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "say hello\r"),
        pause(TURN_DONE_MARKER, 500),
        send(TURN_DONE_MARKER, "say more\r"),
        pause(RING_MARK_2, 800),
        send(RING_MARK_2, "/quit\r"),
    ]
}

/// **The other half of T5-S1c: inline and classic have no frame to follow.**
///
/// A repaint wired at the wrong level — in the surface seam rather than in the
/// fullscreen segment — would reach these two renderers as well, and inline's
/// contract is that the transcript goes to the terminal's own scrollback
/// exactly once (R3) while classic's is that its byte stream is unchanged.
/// Both are asserted here as counts on the two streamed markers, and as the
/// absence of the alternate screen, which neither renderer may take.
///
/// Two sessions in one test because they assert the same three things about
/// two renderers; the fullscreen session that *must* repaint is the test
/// above.
#[test]
#[serial]
fn tui_two_streamed_turns_paint_each_line_once_without_fullscreen() {
    for mode in ["inline", "classic"] {
        let home = TempDir::new().expect("temp home");
        let session = run_styled_streaming_chat_pty_with_args(
            home.path(),
            Some(mode),
            &two_streamed_turns(),
            vec![
                Turn {
                    events: vec![delta(&format!("{RING_MARK}\n")), finish("stop")],
                    gap: Duration::from_millis(60),
                },
                Turn {
                    events: vec![delta(&format!("{RING_MARK_2}\n")), finish("stop")],
                    gap: Duration::from_millis(60),
                },
            ],
            &["--no-tools".to_string()],
        );
        let text = session.raw_text();
        assert_eq!(
            session.bodies.len(),
            2,
            "two turns are two requests in {mode} too; bodies: {:?}",
            session.bodies
        );
        assert_eq!(
            session.raw_count(ALT_SCREEN_ON),
            0,
            "the {mode} renderer must never take the alternate screen; \
             raw:\n{}",
            text.escape_debug()
        );
        assert_eq!(
            session.raw_count(RING_MARK),
            1,
            "and each streamed line reaches the terminal exactly once in \
             {mode}: there is no frame here to repaint it into; raw:\n{}",
            text.escape_debug()
        );
        assert_eq!(
            session.raw_count(RING_MARK_2),
            1,
            "...the second line included; raw:\n{}",
            text.escape_debug()
        );
        assert!(
            session.status.success(),
            "the {mode} session exits cleanly, but exited {:?}; screen:\n{}",
            session.status,
            session.screen_dump()
        );
    }
}

// ---------------------------------------------------------------------------
// Card T5-S1c-ii: fullscreen gets a composer with a key decoder.
//
// Finding **F97**, sharpened. T5-S1c-i made the frame follow the ring; the
// next rough edge is that fullscreen's idle prompt is **rustyline**.
// `read_idle_chat_line` reaches the inline composer only inside
// `if let Some(bridge) = bridge` and `install` returns `None` for
// `TuiMode::Fullscreen` (`tui_bridge.rs`'s `TuiMode::Classic |
// TuiMode::Fullscreen => return None`), so this surface has no `KeyFilter`,
// no `RawStdinGuard` and no composer of its own -- and rustyline's
// `ChatHelper` paints its prompt, its `\u{2570} model ... cwd ... \u{256f}`
// row and the whole `\u{256d} slash commands \u{256e}` palette straight onto
// the frame the repaint just wrote.
//
// **Measured on this pty before a line of it was written**, so the two tests
// below assert against the real failure rather than against a reading of it:
// with the ring 24 lines deep and the frame filling the screen, the switch
// leaves `FS_ROW_05 ..= FS_ROW_24` on screen and rustyline's deck row already
// on the bottom row; one `/c` keystroke then scrolls **eleven** of the
// frame's rows off the top for good (`FS_ROW_16 ..= FS_ROW_24` is all that is
// left), and taking the draft back out does not bring them back, because
// nothing on this surface owns the screen.
//
// So the pins are:
//
// * the deck row rustyline alone can write never reaches the alternate screen
//   (**F71**: the painted bytes, never a state field), and the frame is
//   **whole again** once a palette-opening draft has been taken back out --
//   which is the user-visible point of the card and is deliberately silent
//   about whether the composer repaints the frame or never scrolls it;
// * and a draft **grows by keystroke** on that surface, through two separate
//   reads, so the decoder is something production reaches twice rather than a
//   capability that merely compiles (**F89**, and **F97**'s own lesson: one
//   occurrence proves it happened, a second that it keeps happening).
//
// `\u{2570} model` is the discriminator between the two composers, and it is
// one by construction rather than by taste: `composer_status` (`main.rs`) has
// exactly one caller, `impl Hinter for ChatHelper::hint`, which appends it to
// **every** hint that helper returns, palette or no palette. The inline
// composer's `paint_composer` never writes it. So that row on the alternate
// screen is rustyline reading this surface's keystrokes, and its absence
// while a draft is nevertheless painted and edited is something else reading
// them.
//
// Not in scope here, deliberately: **F54**. Nothing below sends `0x03` or
// asserts anything about `ISIG`. Paging has its own section further down.
// ---------------------------------------------------------------------------

/// Rustyline's own composer deck, which only its `ChatHelper` can write.
///
/// `composer_status` builds `\u{2570}{ model <model> | cwd <dir> }\u{256f}`
/// and has one caller, that helper's `hint`, which appends it unconditionally.
const RUSTYLINE_DECK_ROW: &str = "\u{2570} model";

/// The streamed rows that fill the ring, so the frame fills the screen and
/// the rows rustyline scrolls away are countable.
///
/// Twenty-four lines on a 24-row pty: the entry frame is `rows - 1` of them,
/// so the frame is screen-sized by construction rather than by arithmetic a
/// later geometry change could falsify. The prefix is what the tests count,
/// and it is streamed rather than `eprintln!`ed because only a writer that
/// goes through the surface seam reaches a ring at all.
const FS_ROW_PREFIX: &str = "FS_ROW_";

/// One streamed turn that fills the ring with [`FS_ROW_PREFIX`] rows.
fn one_screenful_turn() -> Vec<Turn> {
    let mut body = String::new();
    for row in 1..=24 {
        body.push_str(&format!("{FS_ROW_PREFIX}{row:02}\n"));
    }
    vec![Turn {
        events: vec![delta(&body), finish("stop")],
        gap: Duration::from_millis(60),
    }]
}

/// How many of the frame's rows are on `screen`.
fn frame_rows_on(screen: &[String]) -> usize {
    screen
        .iter()
        .filter(|row| row.contains(FS_ROW_PREFIX))
        .count()
}

/// Inline -> fullscreen with a screenful in the ring, then **three separate
/// keystroke steps** at fullscreen's own idle prompt, then out.
///
/// The keystrokes are separate steps with a pause after each so they cannot
/// be coalesced into one `read`: the point of the second one is that the
/// surface decoded the first and is still decoding, and the point of the
/// third is that the draft can be taken back *out* again. Every trigger is
/// already satisfied, which is this file's discipline for a keystroke whose
/// response is not a unique string -- a composer that paints nothing fails on
/// an assertion rather than on the driver's deadline.
///
/// `/c` is a slash prefix on purpose: it is what makes a palette open, and
/// the palette is what rustyline scrolls the frame away for. The three
/// backspaces take the draft back to empty, which closes the palette and is
/// where the frame must be whole again.
fn fullscreen_then_a_drafted_and_undrafted_slash_prefix() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "say hello\r"),
        pause(TURN_DONE_MARKER, 500),
        send(TURN_DONE_MARKER, "/tui fullscreen\r"),
        pause(TUI_SWITCHED_TO_FULLSCREEN, 500),
        send(TUI_SWITCHED_TO_FULLSCREEN, "/c"),
        pause(TUI_SWITCHED_TO_FULLSCREEN, 400),
        send(TUI_SWITCHED_TO_FULLSCREEN, "o"),
        pause(TUI_SWITCHED_TO_FULLSCREEN, 400),
        send(TUI_SWITCHED_TO_FULLSCREEN, "\x7f\x7f\x7f"),
        pause(TUI_SWITCHED_TO_FULLSCREEN, 400),
        send(TUI_SWITCHED_TO_FULLSCREEN, "/quit\r"),
    ]
}

/// **The frame is not painted over, and rustyline is not what reads this
/// surface.**
///
/// Two assertions on the painted bytes, and neither is the other's
/// restatement: the deck row says *who* is reading the keystrokes, and the
/// frame-row count says what the reading *costs the user*. A composer that
/// painted nothing at all would satisfy the first and fail the second's
/// companion test below.
#[test]
#[serial]
fn tui_fullscreen_idle_prompt_keeps_rustylines_deck_off_the_frame() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty_with_args(
        home.path(),
        Some("inline"),
        &fullscreen_then_a_drafted_and_undrafted_slash_prefix(),
        one_screenful_turn(),
        &["--no-tools".to_string()],
    );
    let text = session.raw_text();

    // NON-VACUOUS: one real streamed turn, one switch, and a frame that
    // really does fill the screen before a key is pressed.
    assert_eq!(
        session.bodies.len(),
        1,
        "the screenful must have come from the mock and been streamed into \
         the ring; bodies: {:?}",
        session.bodies
    );
    let entered_at = text.find(ALT_SCREEN_ON).unwrap_or_else(|| {
        panic!(
            "the switch must enter the alternate screen; raw:\n{}",
            text.escape_debug()
        )
    });
    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        1,
        "exactly once: one switch and no overlay; raw:\n{}",
        text.escape_debug()
    );
    let at_switch = session.screen_at(3);
    let framed = frame_rows_on(&at_switch);
    assert!(
        framed >= 15,
        "NON-VACUOUS: the frame must fill the screen before anything is \
         typed, or there is nothing for a keystroke to cost; {framed} frame \
         row(s) at the switch:\n{}",
        Session::dump(&at_switch)
    );

    // F71. The deck row is rustyline's and only rustyline's, so a single
    // occurrence of it inside the alternate screen is this surface's prompt
    // being read by a cooked-mode line editor painting over the frame.
    let on_the_alternate_screen = &text[entered_at..];
    assert_eq!(
        on_the_alternate_screen.matches(RUSTYLINE_DECK_ROW).count(),
        0,
        "rustyline's `{RUSTYLINE_DECK_ROW} ... cwd ...` row is written by its \
         `ChatHelper` hint and by nothing else in this binary, so it must \
         never reach the alternate screen: while fullscreen holds the screen \
         the keystrokes belong to fullscreen's own composer. Everything from \
         the alternate screen onwards:\n{}",
        on_the_alternate_screen.escape_debug()
    );

    // ...and the frame survives being typed at. Asserted after the draft has
    // been taken back out, because a palette that is *open* may legitimately
    // cover frame rows -- what it may not do is cost them permanently.
    let dismissed = session.screen_at(9);
    assert!(
        frame_rows_on(&dismissed) >= framed,
        "the frame's rows must survive the idle prompt: {framed} were on \
         screen at the switch and {} after a palette-opening draft was typed \
         and taken back out. Rustyline scrolls them off the top of the \
         alternate screen and nothing puts them back (F97). Screen after the \
         draft was dismissed:\n{}",
        frame_rows_on(&dismissed),
        Session::dump(&dismissed)
    );

    // The ordinary exit path, with raw mode now new state on this surface:
    // every bracketed-paste enable is matched by a disable, and the pty is
    // canonical again once the session is gone.
    assert_eq!(
        session.raw_count("\x1b[?2004h"),
        session.raw_count("\x1b[?2004l"),
        "whatever engaged raw mode on this surface must have let it go: \
         bracketed paste is enabled by `RawStdinGuard::engage` and disabled \
         by its `Drop`, so an unbalanced count is a guard that outlived its \
         read; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        session.status.success(),
        "`/quit` must still work from fullscreen's own composer, but the \
         session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
    assert!(
        session.raw_mode_off(),
        "and the pty must be canonical again after a fullscreen session that \
         held raw mode: {}",
        session.termios_dump()
    );
}

/// **The decoder is reachable, and it keeps being reached.**
///
/// The companion to the test above, and the one that refuses the cheap fix to
/// it: suppressing rustyline's helper in fullscreen would leave the deck row
/// off the screen and the frame intact while leaving the user unable to type.
/// So this asserts the draft itself -- painted, grown by a second keystroke
/// in a second read, and erased again -- and asserts it **below** the frame,
/// which is what "the composer draws where it belongs" means on a surface
/// whose rows are all frame.
///
/// Three reads rather than one is **F97**'s shape applied to the decoder: one
/// draft proves a keystroke was decoded, the second proves the surface is
/// still decoding, and the third proves the decoding is editing rather than
/// echoing -- a cooked-mode byte stream cannot shrink.
#[test]
#[serial]
fn tui_fullscreen_decodes_each_keystroke_at_its_own_idle_prompt() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty_with_args(
        home.path(),
        Some("inline"),
        &fullscreen_then_a_drafted_and_undrafted_slash_prefix(),
        one_screenful_turn(),
        &["--no-tools".to_string()],
    );
    let text = session.raw_text();
    assert_eq!(
        session.bodies.len(),
        1,
        "one streamed turn fills the ring; bodies: {:?}",
        session.bodies
    );

    // NON-VACUOUS: the keystrokes were written after the alternate screen was
    // taken, so what answers them is fullscreen's prompt and not inline's.
    let entered_at = text
        .find(ALT_SCREEN_ON)
        .expect("the switch takes the screen");
    assert!(
        entered_at < session.marks[4],
        "the keystrokes must be typed while fullscreen holds the screen; \
         raw:\n{}",
        text.escape_debug()
    );

    // One keystroke, one decoded draft, painted under the frame.
    for (step, draft) in [(5usize, "/c"), (7, "/co")] {
        let screen = session.screen_at(step);
        let row = screen
            .iter()
            .position(|row| row.contains(draft))
            .unwrap_or_else(|| {
                panic!(
                    "fullscreen's own composer must paint the draft {draft:?} \
                     it decoded: this surface has no `KeyFilter` and no raw \
                     mode today, so the keystroke reaches a cooked-mode line \
                     editor instead (F97). Screen:\n{}",
                    Session::dump(&screen)
                )
            });
        let last_frame_row = screen
            .iter()
            .rposition(|row| row.contains(FS_ROW_PREFIX))
            .unwrap_or_else(|| {
                panic!(
                    "the frame must still be on screen while the draft is \
                     being typed; screen:\n{}",
                    Session::dump(&screen)
                )
            });
        assert!(
            row > last_frame_row,
            "and it must paint it *under* the frame rather than into it: the \
             draft {draft:?} is on row {} and the frame's last row is {}; \
             screen:\n{}",
            row + 1,
            last_frame_row + 1,
            Session::dump(&screen)
        );
    }

    // And the paint that answered each keystroke is **this surface's**, not a
    // cooked-mode line editor's: the window a keystroke opens carries the new
    // draft and none of rustyline's deck. Per keystroke rather than per
    // session, because what is being pinned here is the decode -- the draft
    // appearing in the window the keystroke opened is the composer having
    // read that byte (F89: reached, not merely present).
    for (from, to, draft) in [(4usize, 5usize, "/c"), (6, 7, "/co")] {
        let window = session.raw_between(from, to);
        assert!(
            window.contains(draft),
            "the keystroke must be answered by a paint of the draft {draft:?} \
             in the window it opened; the child wrote:\n{}",
            window.escape_debug()
        );
        assert!(
            !window.contains(RUSTYLINE_DECK_ROW),
            "...and that paint must not be rustyline's: `{RUSTYLINE_DECK_ROW} \
             ... cwd ...` in this window is its `ChatHelper` answering the \
             keystroke, which is a cooked-mode line editor reading a surface \
             that has no decoder of its own (F97). The child wrote:\n{}",
            window.escape_debug()
        );
    }

    // ...and the editing is the decoder's, not the terminal's: the draft is
    // gone from the composer once it has been backspaced out.
    let dismissed = session.screen_at(9);
    let composer = dismissed
        .iter()
        .rposition(|row| row.contains("kitsu>"))
        .unwrap_or_else(|| {
            panic!(
                "the composer's prompt must still be on screen with the draft \
                 taken back out; screen:\n{}",
                Session::dump(&dismissed)
            )
        });
    assert!(
        !dismissed[composer].contains("/co"),
        "the backspaces must have been decoded as edits: the composer row \
         still reads {:?}; screen:\n{}",
        dismissed[composer],
        Session::dump(&dismissed)
    );
}

// ---------------------------------------------------------------------------
// Card **T5-S1c-iii**: the four paging keys, end to end.
//
// The decoder's side is pinned in `tui_bridge`'s own tests (the press list,
// the `Home`/`End` collision, the split sequence, the inline negative) and
// the anchor's eleven transitions are pinned in `kitsu_tui::fullscreen`.
// Neither of those is **F89**: a decoder that compiles and a view that pages
// are not a key the user can press. What this section adds is the one thing
// only a pty can show -- that a `PgUp` typed at a live fullscreen prompt
// moves the frame on the screen.
// ---------------------------------------------------------------------------

/// `PgUp`, `PgDn`, `Ctrl+Home` and `Ctrl+End` as a terminal sends them.
///
/// `Ctrl+Home`/`Ctrl+End` are the **modified** forms of plain `Home`/`End`'s
/// finals, which is the whole of **F65**'s family here: the bytes differ only
/// in the parameters, and the plain forms must keep moving the caret.
const PG_UP_KEY: &str = "\x1b[5~";
const CTRL_END_KEY: &str = "\x1b[1;5F";

/// Inline -> fullscreen with a screenful in the ring, then `PgUp`, then
/// `Ctrl+End`, then out.
///
/// Each press is its own step with a pause after it, for the reason the
/// keystroke script above gives: the presses must not be coalesced into one
/// `read`, because a frame that moved once is not a frame the keys keep
/// moving. Every trigger is already satisfied, so a press that does nothing
/// fails on an assertion rather than on the driver's deadline.
fn fullscreen_then_pgup_then_ctrl_end() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "say hello\r"),
        pause(TURN_DONE_MARKER, 500),
        send(TURN_DONE_MARKER, "/tui fullscreen\r"),
        pause(TUI_SWITCHED_TO_FULLSCREEN, 500),
        send(TUI_SWITCHED_TO_FULLSCREEN, PG_UP_KEY),
        pause(TUI_SWITCHED_TO_FULLSCREEN, 500),
        send(TUI_SWITCHED_TO_FULLSCREEN, CTRL_END_KEY),
        pause(TUI_SWITCHED_TO_FULLSCREEN, 500),
        send(TUI_SWITCHED_TO_FULLSCREEN, "/quit\r"),
    ]
}

/// **The paging keys are reachable, and they move the frame.**
///
/// Finding **F89**: a capability that compiles is not one that is reachable.
/// The reader hands its bytes to `kitsu_editor::TypeaheadEditor`, whose
/// `dispatch_csi` has no `5`/`6` parameter for `~` at all -- measured: a
/// `PgUp` fed to it leaves `(buffer, cursor)` untouched and reports
/// `changed: false` with no action -- so today this surface answers a `PgUp`
/// with nothing whatsoever. Only the screen can show that.
///
/// **F71/F41**: asserted on the parsed screen, by *which* of the ring's lines
/// are on it. `FS_ROW_24` is the newest and `FS_ROW_01` is above the entry
/// window (the frame is `rows - 1` of a 24-line ring), so one press has to
/// swap which of the two is visible, and `Ctrl+End` has to swap it back.
#[test]
#[serial]
fn tui_fullscreen_pages_the_frame_with_pgup_and_re_latches_with_ctrl_end() {
    let home = TempDir::new().expect("temp home");
    let session = run_styled_streaming_chat_pty_with_args(
        home.path(),
        Some("inline"),
        &fullscreen_then_pgup_then_ctrl_end(),
        one_screenful_turn(),
        &["--no-tools".to_string()],
    );
    let text = session.raw_text();

    // NON-VACUOUS: a real streamed turn, one switch, and an entry frame that
    // fills the screen with the *newest* lines and keeps `FS_ROW_01` above it.
    assert_eq!(
        session.bodies.len(),
        1,
        "the screenful must have been streamed into the ring; bodies: {:?}",
        session.bodies
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        1,
        "exactly once: one switch and no overlay; raw:\n{}",
        text.escape_debug()
    );
    let at_switch = session.screen_at(3);
    assert!(
        frame_rows_on(&at_switch) >= 15,
        "NON-VACUOUS: the frame must fill the screen before a key is \
         pressed:\n{}",
        Session::dump(&at_switch)
    );
    let on_screen = |screen: &[String], row: &str| screen.iter().any(|r| r.contains(row));
    assert!(
        on_screen(&at_switch, "FS_ROW_24"),
        "NON-VACUOUS: the entry frame follows the newest line:\n{}",
        Session::dump(&at_switch)
    );
    assert!(
        !on_screen(&at_switch, "FS_ROW_01"),
        "NON-VACUOUS: and holds the oldest line *above* the window, or a \
         `PgUp` has nothing to reveal:\n{}",
        Session::dump(&at_switch)
    );

    // One `PgUp`: the frame moves back and the oldest retained line arrives.
    let paged = session.screen_at(6);
    assert!(
        on_screen(&paged, "FS_ROW_01"),
        "a `PgUp` at fullscreen's own idle prompt must page the frame back: \
         `ESC[5~` reaches `TypeaheadEditor`, which has no `5` parameter for \
         `~` and drops it, so this surface answers the key with nothing \
         today. Screen after the press:\n{}",
        Session::dump(&paged)
    );
    assert!(
        !on_screen(&paged, "FS_ROW_24"),
        "...and must move it by a window rather than by a line: the newest \
         line has to leave the screen, or the press scrolled instead of \
         paging. Screen after the press:\n{}",
        Session::dump(&paged)
    );
    assert!(
        session.raw_between(4, 6).contains("FS_ROW_01"),
        "and the repaint must be what *answers the press*, in the window the \
         press opened: the frame follows the ring only when the ring grows, \
         so a press that leaves the repaint to the next transcript line \
         leaves the user's key dead. The child wrote:\n{}",
        session.raw_between(4, 6).escape_debug()
    );

    // `Ctrl+End`: the re-latch, and the half of the `Home`/`End` collision
    // that has to page rather than move a caret. The draft is empty here, so
    // a leak to the editor's `move_end` would be invisible -- which is why
    // the caret half of this pair is pinned in `tui_bridge`'s unit tests and
    // the frame half is pinned here.
    let relatched = session.screen_at(8);
    assert!(
        on_screen(&relatched, "FS_ROW_24"),
        "`Ctrl+End` (`ESC[1;5F`) must bring the newest line back and resume \
         following (transition 9); screen:\n{}",
        Session::dump(&relatched)
    );

    // The ordinary exit path, unchanged by the new keys.
    assert_eq!(
        session.raw_count("\x1b[?2004h"),
        session.raw_count("\x1b[?2004l"),
        "every bracketed-paste enable is matched by a disable: raw:\n{}",
        text.escape_debug()
    );
    assert!(
        session.status.success(),
        "`/quit` must still work after a run of page presses, but the \
         session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
    assert!(
        session.raw_mode_off(),
        "and the pty must be canonical again: {}",
        session.termios_dump()
    );
}

/// Classic -> fullscreen, then a `SIGTERM` inside the segment.
///
/// The third way into fullscreen, and the one no test had taken: the two
/// signal scripts in the exit-path section below enter from **inline**
/// (whose `install` registers the dying handler) or start with
/// `--tui fullscreen` (whose startup registers it for the mode, card F96).
/// The `/tui` arm's `TuiRendererRequest::Fullscreen` branch calls
/// `enter_fullscreen` and `eprintln!` and nothing else -- no
/// `install_exit_restore` -- so a session that reaches fullscreen this way
/// holds the alternate screen on the **default** signal disposition.
///
/// That is an F94-family hole in its own right, and this card is where it
/// has to be pinned: raw termios is new state on this surface, so a dying
/// signal on this path now leaves the user's shell with no echo and no line
/// editing on top of leaving them on the alternate screen, which is exactly
/// what F96 recorded for the startup path.
fn classic_into_fullscreen_then_sigterm() -> Vec<Step> {
    vec![
        send("/help for commands", "/tui fullscreen\r"),
        pause(ALT_SCREEN_ON, SETTLE_MS),
        signal(ALT_SCREEN_ON, libc::SIGTERM),
        pause(ALT_SCREEN_ON, SETTLE_MS),
    ]
}

/// **The exit path this surface's new terminal state adds a suspect to.**
///
/// *When a surface takes new terminal state, every exit path is a suspect,
/// and exit paths are enumerable.* The enumeration for fullscreen's composer
/// is: a normal switch away, `/quit`, a dying signal, and a panic. The first
/// two are asserted on the scripts above and in T5-S2's transition tests; the
/// panic path runs the chained hook and then unwinds through
/// `RawStdinGuard::drop` and `FullscreenSegment::drop` (neither profile sets
/// `panic = "abort"`), and there is no in-process way to provoke one from a
/// script. **This is the third**, on the one route into fullscreen that
/// registers no handler at all.
#[test]
#[serial]
fn fullscreen_entered_from_classic_sigterm_restores_termios_and_the_screen() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), None, &classic_into_fullscreen_then_sigterm());
    assert_alt_screen_released_after_signal(&session, 2, "SIGTERM in fullscreen out of classic");
    assert!(
        session.raw_mode_off(),
        "and the pty must be canonical again: a fullscreen composer holds raw \
         termios, so a session signalled on this path owes the terminal its \
         line discipline back as well as its screen, but it reads {}",
        session.termios_dump()
    );
    assert!(
        !session.status.success(),
        "a signalled session does not exit successfully; it exited {:?}",
        session.status
    );
}

/// Switch into inline, then **switch again**, then take a `SIGTERM`.
fn switch_twice_then_sigterm() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/tui classic\r"),
        send(TUI_SWITCHED_TO_CLASSIC, "/tui inline\r"),
        send(TUI_SWITCHED_TO_INLINE, "/theme zz1\r"),
        signal(TUI_MARK_1, libc::SIGTERM),
        pause(TUI_MARK_1, 300),
    ]
}

/// **T3-S8's restore, after a second install.** `install_exit_restore` is
/// `Once`-gated, so the second install does not register anything: the
/// question this pins is whether the restore that was registered by the
/// *first* install still gives back what the *second* one took.
///
/// Read, not assumed (findings F43/F47): the handler reads only
/// `SELF_RAISED`, the `CANONICAL_TERMIOS` snapshot and a `const` byte string
/// — it holds no viewport handle at all — so `Once` is sound here by
/// construction and this test is the proof rather than the discovery. What it
/// *can* still catch is the re-install forgetting to re-publish the termios
/// snapshot, which would leave the pty raw after the signal.
#[test]
#[serial]
fn tui_switch_back_into_inline_still_restores_on_a_signal() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &switch_twice_then_sigterm());
    let text = session.raw_text();

    assert!(
        session.raw_mode_off(),
        "a SIGTERM taken in the *second* inline segment must still leave the \
         pty canonical, but it reads {}; raw:\n{}",
        session.termios_dump(),
        text.escape_debug()
    );
    // The dying handler's own byte string, which only the signal path writes.
    assert!(
        text.contains("\x1b[r\x1b[?25h\x1b[?2004l\x1b[23;2t"),
        "the dying-signal restore must run for the re-installed viewport; \
         raw:\n{}",
        text.escape_debug()
    );
    assert!(
        !session.status.success(),
        "a signalled session does not exit successfully; it exited {:?}",
        session.status
    );
}

/// **Requirement 7 of the card: a classic session that never switches is
/// unchanged.** The byte-level half of that claim is that nothing the switch
/// brought with it — a pinned region, a region release, or the dying-signal
/// restore the inline path installs — appears in a session that never asks
/// for one.
///
/// This is the one test in this block that **passes before the feature
/// exists**: it is the guard that says the implementation did not reach into
/// the classic path, and it can only ever fail afterwards.
#[test]
#[serial]
fn classic_session_that_never_switches_emits_no_viewport_bytes() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), None, &help_then_quit());
    let text = session.raw_text();

    assert_eq!(
        session.raw_count(&tui_region()),
        0,
        "no region; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count("\x1b[r"),
        0,
        "and no release, because there was nothing to release; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        !text.contains("\x1b[r\x1b[?25h\x1b[?2004l\x1b[23;2t"),
        "and no dying-signal restore sequence; raw:\n{}",
        text.escape_debug()
    );
    // Non-vacuous: this session really did run a transcript.
    assert_eq!(
        session.raw_count("/help <command> explains one command in detail"),
        1,
        "the session must have printed its transcript; raw:\n{}",
        text.escape_debug()
    );
}

// -------------------------------------------------------------------------
// Card **T3-S12**, the risk gate: a PTY session under `--reduced-motion`
// (notes **N3** and **N9**).
//
// What a PTY can and cannot measure here, said plainly (F43/F47): the status
// row's elapsed segment is `None` between turns, so an **idle** session has
// no age to refresh and the two tests below cannot measure N9's periodic age
// refresh at all. That half is pinned where it can be driven --
// `tui_bridge::tests::reduced_motion_suppresses_the_elapsed_tick_but_not_a_state_change`
// -- because driving it from a PTY would need a model turn, and this harness's
// gateway is `https://gateway.invalid` by design.
//
// What these two do measure is the rest of the gate: reduced motion still
// **enters** the viewport (N9: it keeps styled output by definition), the
// transcript is **never redrawn** (N3's third clause), an idle second costs
// **zero bytes**, and no synchronized-output mode is emitted.
// -------------------------------------------------------------------------

/// A styled session with `--reduced-motion` on the command line. The flag is
/// `global = true` in the CLI, so it rides along after `chat`'s own flags
/// exactly as `--allow-path` does for the streaming harness.
fn run_reduced_motion_chat_pty(home: &Path, tui: Option<&str>, script: &[Step]) -> Session {
    run_styled_chat_pty_core(
        home,
        tui,
        script,
        Options::default(),
        &["--reduced-motion".to_string()],
    )
}

/// `/help`, then two pauses, then `/quit`.
///
/// Two pauses rather than one, and that is the measurement rather than a
/// nicety: a mark is written after each step, so the bytes between the first
/// pause's mark and the second's are **exactly** what the child wrote during
/// the second pause, with no step's input or output mixed in. One pause would
/// only bound a window that still contained all of `/help`.
fn help_then_idle_then_quit() -> Vec<Step> {
    vec![
        send("/help for commands", "/help\n"),
        pause("/help <command> explains one command in detail", 300),
        pause("/help <command> explains one command in detail", 1_200),
        send("/help <command> explains one command in detail", "/quit\n"),
    ]
}

/// Notes **N3** and **N9** on a real terminal: inline mode under
/// `--reduced-motion` pins its viewport, prints its transcript once, writes
/// nothing at all while idle, and emits no synchronized-update mode.
///
/// Every assertion is derived from a measurement taken on this tree before the
/// test was written (a throwaway probe in this file): region `ESC[1;23r` once,
/// the `/help` footer once, **0 bytes** across a 1 200 ms idle pause, 8
/// occurrences of the status row's model, and zero `?2026`.
///
/// The idle-pause assertion is a negative one, so it carries its controls in
/// the same test: the session must have pinned a region, must have printed its
/// transcript, and must have painted the status row at least once -- otherwise
/// "wrote nothing" would be satisfied by a session that did nothing.
#[test]
#[serial]
fn inline_mode_under_reduced_motion_is_static_and_never_redraws_history() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session =
        run_reduced_motion_chat_pty(home.path(), Some("inline"), &help_then_idle_then_quit());

    assert!(
        session.status.success(),
        "a reduced-motion inline session exited {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
    // N9: reduced motion keeps styled output, so the viewport is entered.
    assert_eq!(
        session.raw_count(&tui_region()),
        1,
        "--reduced-motion may enter inline mode and must pin the region once; \
         raw:\n{}",
        session.raw_text().escape_debug()
    );
    // N3, third clause: history is written once and never redrawn.
    assert_eq!(
        session.raw_count("/help <command> explains one command in detail"),
        1,
        "the transcript must be emitted exactly once -- a redraw of history \
         would repeat it; raw:\n{}",
        session.raw_text().escape_debug()
    );
    // Control: the pinned row was really painted.
    assert!(
        session.raw_count(STATUS_ROW_MODEL) >= 1,
        "control: the status row must have been painted; raw:\n{}",
        session.raw_text().escape_debug()
    );
    // N3/N9: an idle second costs nothing. `marks[1]..marks[2]` is the second
    // pause's window; see `help_then_idle_then_quit`.
    let during_idle = &session.raw[session.marks[1]..session.marks[2]];
    assert_eq!(
        during_idle.len(),
        0,
        "1 200 ms of idle must emit no bytes at all, got {:?}",
        String::from_utf8_lossy(during_idle)
            .escape_debug()
            .to_string()
    );
    // The synchronized-output deferral, on the wire.
    for mode in ["\x1b[?2026h", "\x1b[?2026l"] {
        assert_eq!(
            session.raw_count(mode),
            0,
            "synchronized output is deferred until a capability probe exists, \
             so {:?} must not reach the terminal; raw:\n{}",
            mode.escape_debug().to_string(),
            session.raw_text().escape_debug()
        );
    }
}

/// And classic mode is unaffected by the card: `--reduced-motion` with no
/// `--tui` flag pins no region, releases none, emits no synchronized-update
/// mode, and still prints its transcript exactly once.
///
/// This is a guard rather than a measurement -- it passes before the feature
/// exists and can only ever fail afterwards, exactly like
/// `classic_session_that_never_switches_emits_no_viewport_bytes`, whose shape
/// it borrows.
#[test]
#[serial]
fn classic_mode_under_reduced_motion_emits_no_viewport_bytes() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_reduced_motion_chat_pty(home.path(), None, &help_then_quit());
    let text = session.raw_text();

    assert_eq!(
        session.raw_count(&tui_region()),
        0,
        "classic mode pins no region under --reduced-motion; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count("\x1b[r"),
        0,
        "and releases none, because there was nothing to release; raw:\n{}",
        text.escape_debug()
    );
    for mode in ["\x1b[?2026h", "\x1b[?2026l"] {
        assert_eq!(
            session.raw_count(mode),
            0,
            "and emits no synchronized-update mode {:?}; raw:\n{}",
            mode.escape_debug().to_string(),
            text.escape_debug()
        );
    }
    // Non-vacuous: this session really did run a transcript.
    assert_eq!(
        session.raw_count("/help <command> explains one command in detail"),
        1,
        "the session must have printed its transcript once; raw:\n{}",
        text.escape_debug()
    );
}

// ---------------------------------------------------------------------------
// T4-S2: the agent view overlay opens and closes (requirement A1)
// ---------------------------------------------------------------------------
//
// Card T4-S2's acceptance is a *failing* PTY open/close test, and these are
// it. Nothing below passes today: `[tui] prefix` and
// `[tui] left_arrow_opens_agents` are resolved at `main.rs:16840-16841` and
// consumed by nobody, and `kitsu-tui::agents_view` is a model with no
// renderer. The whole card is the wiring these measure.
//
// # What a test may say about chrome that does not exist yet
//
// Finding **F58** says to derive every expectation from a real sample, and
// there is no sample of an overlay nobody has drawn. So the overlay's chrome
// is pinned here by exactly **one** token, [`AGENT_VIEW_TITLE`], and the
// `←`-hint by two words — the minimum that distinguishes "the agent view is on
// screen" from "it is not" — and everything else is asserted *structurally*:
// which row the thing appears on, whether the screen changed, and whether it
// changed back. The two literals are this card's contract, not a transcription
// of something already shipped; see the report for the amendment they imply.
//
// # Where the overlay is, and why `closing restores` is checkable at all
//
// `[tui] alternate_screen` defaults to `auto` and these sessions run on a real
// pty, so an overlay takes the alternate screen and the restore on close is
// the *terminal's*, exact by construction. That is what lets the close half
// assert whole-row equality with the screen as it stood before the open
// rather than naming lines. The design's own `alternate_screen = never` path
// (overlays drawn inside the viewport area) is not exercised here; under it
// the restore is the cell ring's and the documented ring limitation applies.

/// The one token the overlay must print somewhere above the pinned row. The
/// design calls the surface "the agent view overlay" throughout (§3.2 A1,
/// §4.4) and no shipped output contains these two words today, which is what
/// makes it usable as the detector: `raw_count` of it is 0 in every session in
/// this file that never opens the view.
const AGENT_VIEW_TITLE: &str = "agent view";

/// What the **first** `←` on an empty composer must put in the hint slot: that
/// another press is needed, and what it would open. Two words rather than a
/// sentence, so the wording stays the implementer's while the affordance is
/// pinned — a hint that fails to say "again" leaves the user with a keystroke
/// that silently did nothing, which is the whole hazard of a two-press key.
const LEFT_HINT_WORDS: [&str; 2] = ["again", "agent"];

/// The heading `/jobs view`'s text deck prints (`main.rs:11592`, through
/// `deck_header`, so the styled tier prefixes a marker and the words survive).
/// In TUI modes the overlay replaces that deck; in classic it is unchanged.
const JOBS_VIEW_DECK_HEADING: &str = "agent deck by state";

/// `Ctrl+X`, the default `[tui] prefix` (`TuiConfig::DEFAULT_PREFIX`).
const CTRL_X: &str = "\x18";

/// `Ctrl+X a`: the chord itself, as the two bytes a terminal sends for it.
const CTRL_X_A: &str = "\x18a";

/// `Ctrl+X b`: the default prefix with a letter that is **not** `a`, used
/// where a test needs the prefix byte to be inert and the letter to be
/// observable in the draft.
const CTRL_X_B: &str = "\x18b";

/// `Ctrl+T a` under `[tui] prefix = "ctrl+t"`. `0x14` is unbound in the
/// editor's keymap today (it binds 0x01, 0x02, 0x04, 0x05, 0x06, 0x08, 0x09,
/// 0x0b, 0x0c, 0x12, 0x15, 0x17, 0x1a and 0x1c, and ignores the rest), so a
/// re-configured prefix cannot pass by colliding with an existing binding.
const CTRL_T_A: &str = "\x14a";

/// The left-arrow key as the terminal sends it (CSI D), which is also what
/// `TypeaheadEditor` reads as backward-char today.
const LEFT: &str = "\x1b[D";

/// Two left arrows in one write: the whole two-press affordance in a single
/// read, which is also the harder case for an implementation that keys the
/// state off reads rather than keystrokes.
const LEFT_TWICE: &str = "\x1b[D\x1b[D";

/// Two up arrows in one write, as the existing recall measurement sends them.
const UP_TWICE: &str = "\x1b[A\x1b[A";

/// The last step of every script below: leave.
///
/// The three `Ctrl+U`s are sacrificial and the reason is measurable in a
/// transcript. At the idle composer an `ESC` byte with no continuation stays
/// *pending* in the editor's escape-sequence state machine until another byte
/// arrives, and that byte is then consumed as part of the sequence - so a
/// script that sends `Esc` and later `/quit` submits `quit`, which is a
/// **prompt**, which reaches the gateway, which is unreachable. The session
/// then never exits and the measurement is reported as a 20-second timeout
/// instead of as the assertion that actually failed (finding **F46**).
///
/// `Ctrl+U` is the right sacrifice: it kills to the start of the line, so it
/// is a no-op on the empty composer whether it is swallowed by a pending
/// escape or read as itself, and it clears any draft a test left behind.
/// Once the overlay exists and consumes the `Esc` itself, nothing is pending
/// and all three are no-ops.
const QUIT_AFTER_CLOSE: &str = "\x15\x15\x15/quit\r";

/// How long a step waits for the child to finish reacting before the stream is
/// marked. Generous, because every assertion below is taken on a *mark* rather
/// than on a trigger: a step that waits for bytes the surface cannot print yet
/// would time the session out at [`DEADLINE`] and report nothing about what is
/// missing, which is finding **F46**.
const SETTLE_MS: u64 = 500;

/// Write `~/.kitsu/config.toml` with one unreachable gateway **and** a `[tui]`
/// table. `write_config`'s body is reused verbatim and the table appended, so
/// the gateway the other sessions run under cannot drift from this one.
fn write_config_with_tui(home: &Path, tui_table: &str) {
    write_config(home);
    let path = home.join(".kitsu").join("config.toml");
    let base = fs::read_to_string(&path).expect("read the config just written");
    fs::write(&path, format!("{base}\n[tui]\n{tui_table}\n")).expect("append the [tui] table");
}

/// Whether the agent view is on screen: the title on any row **inside** the
/// scroll region, i.e. anywhere but the pinned bottom row.
///
/// Excluding the pinned row is load-bearing rather than tidy: the `←` hint
/// names the view it would open, so a detector that read the whole screen
/// would call a session with the hint showing "open" and the two-press
/// assertion below would be vacuous in the one direction that matters.
fn agent_view_open(screen: &[String]) -> bool {
    screen[..(ROWS - 1) as usize]
        .iter()
        .any(|row| row.to_lowercase().contains(AGENT_VIEW_TITLE))
}

/// The pinned bottom row: the composer, its hint slot and the status.
fn pinned_row(screen: &[String]) -> String {
    screen[(ROWS - 1) as usize].to_lowercase()
}

/// Assert the `←` hint is showing on the pinned row, and say what it reads
/// instead when it is not.
fn assert_left_hint(screen: &[String], context: &str) {
    let row = pinned_row(screen);
    for word in LEFT_HINT_WORDS {
        assert!(
            row.contains(word),
            "{context}: the first \u{2190} must hint that another press opens the \
             agent view, but the pinned row reads {row:?} (missing {word:?}); \
             whole screen:\n{}",
            Session::dump(screen)
        );
    }
}

/// Assert the `←` hint is **not** showing. Paired with a positive observation
/// at every call site, never alone.
fn assert_no_left_hint(screen: &[String], context: &str) {
    let row = pinned_row(screen);
    assert!(
        !row.contains(LEFT_HINT_WORDS[0]),
        "{context}: nothing may arm the two-press \u{2190} affordance here, but \
         the pinned row reads {row:?}; whole screen:\n{}",
        Session::dump(screen)
    );
}

/// The closing half, shared by every open/close script: the screen with the
/// overlay up must differ from the screen before it opened, and the screen
/// after `Esc` must be that pre-open screen again, row for row inside the
/// region.
///
/// The inequality is what keeps the equality honest: an overlay that painted
/// nothing at all would "restore" perfectly.
fn assert_opened_and_restored(session: &Session, before: usize, open: usize, closed: usize) {
    let (before_screen, open_screen, closed_screen) = (
        session.screen_at(before),
        session.screen_at(open),
        session.screen_at(closed),
    );
    let region = |screen: &[String]| screen[..(ROWS - 1) as usize].to_vec();
    assert!(
        agent_view_open(&open_screen),
        "the agent view must be on screen at mark {open}; whole screen:\n{}",
        Session::dump(&open_screen)
    );
    assert_ne!(
        region(&open_screen),
        region(&before_screen),
        "an overlay that changes nothing inside the region has not opened; \
         screen before:\n{}\nscreen with the view up:\n{}",
        Session::dump(&before_screen),
        Session::dump(&open_screen)
    );
    assert!(
        !agent_view_open(&closed_screen),
        "Esc must close the agent view; whole screen:\n{}",
        Session::dump(&closed_screen)
    );
    assert_eq!(
        region(&closed_screen),
        region(&before_screen),
        "closing must restore what was on screen; screen before the open:\n{}\n\
         screen after the close:\n{}",
        Session::dump(&before_screen),
        Session::dump(&closed_screen)
    );
}

/// Test T4-S3a. **`Space` never reaches the composer underneath.**
///
/// A GUARD: green today, because the overlay's byte loop ignores every key it
/// does not bind, and required to stay green once `Space` is bound to peek. An
/// implementer who forwards the unhandled-key case to the editor, or who makes
/// `Space` close the view, breaks it.
///
/// What `Space` *shows* is pinned at unit level in `main.rs` (a fixture PTY
/// session has no agents in it, so there is no row to peek and no handoff to
/// acknowledge here). What only a PTY can answer is the routing: the byte is
/// read by the overlay, so the draft on the pinned row underneath is
/// untouched, and `Space` is not an exit either.
///
/// The POSITIVE CONTROL is the last step: after the view is closed, the same
/// single space typed at the idle composer *does* change the pinned row. So
/// the equality asserted while the view was up is a claim about the overlay
/// consuming the key and not about a space being invisible.
#[test]
#[serial]
fn inline_mode_agent_view_answers_space_without_typing_into_the_composer() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        send(RAW_MODE_MARKER, CTRL_X_A),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, " "),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, " "),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let (before, opened, peeked, typed) = (
        session.screen_at(0),
        session.screen_at(1),
        session.screen_at(3),
        session.screen_at(7),
    );
    assert!(
        agent_view_open(&opened),
        "the view must be open before `Space` is pressed; whole screen:\n{}",
        Session::dump(&opened)
    );
    assert!(
        agent_view_open(&peeked),
        "`Space` is peek, not an exit: the agent view is still the surface on \
         screen; whole screen:\n{}",
        Session::dump(&peeked)
    );
    assert_eq!(
        pinned_row(&peeked),
        pinned_row(&opened),
        "`Space` belongs to the view: nothing may be typed into the composer \
         underneath it; screen with the view up:\n{}\nscreen after `Space`:\n{}",
        Session::dump(&opened),
        Session::dump(&peeked)
    );
    assert_ne!(
        pinned_row(&typed),
        pinned_row(&before),
        "POSITIVE CONTROL: the same single space typed at the idle composer \
         does change the pinned row, so the equality above is a real claim; \
         screen before the open:\n{}\nscreen after typing a space:\n{}",
        Session::dump(&before),
        Session::dump(&typed)
    );
    assert_opened_and_restored(&session, 0, 1, 5);
}

/// Test T4-S2a. **`Ctrl+X a` opens the agent view, `Esc` closes it.**
///
/// The default `[tui] prefix` is `ctrl+x`, so this is the chord A1 names. Four
/// things are measured: the view appears inside the region, the region really
/// changed, `Esc` takes it away, and what was on screen comes back.
///
/// That the chord was read as a *chord* and not as two inert keystrokes is
/// pinned by the exit: `0x18` is ignored by the editor's control catch-all
/// today, so an unimplemented chord leaves `a` in the draft, `/quit` arrives
/// as `a/quit`, and the session cannot exit at all. A clean exit is therefore
/// positive evidence that the `a` was consumed by the binding.
#[test]
#[serial]
fn inline_mode_opens_the_agent_view_on_the_default_prefix_chord_and_closes_on_escape() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        send(RAW_MODE_MARKER, CTRL_X_A),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    assert_opened_and_restored(&session, 0, 1, 3);
    assert!(
        session.status.success(),
        "the chord must consume its `a`, so `/quit` submits and the session \
         exits cleanly; exit was {:?}; screen:\n{}",
        session.status,
        session.screen_dump()
    );
}

/// Test T4-S2b. **The chord is the *configured* prefix, not a literal.**
///
/// `[tui] prefix = "ctrl+t"` and the session must answer `Ctrl+T a` and ignore
/// `Ctrl+X a`. The negative half is not vacuous twice over: the `b` typed
/// after the `Ctrl+X` lands in the draft and is asserted on the pinned row —
/// so the keystrokes demonstrably arrived and the prefix byte was inert — and
/// the configured chord then opens the very surface the first chord failed to.
#[test]
#[serial]
fn inline_mode_opens_the_agent_view_on_the_chord_the_tui_prefix_configures() {
    let home = TempDir::new().expect("temp home");
    write_config_with_tui(home.path(), "prefix = \"ctrl+t\"");
    let script = vec![
        send(RAW_MODE_MARKER, CTRL_X_B),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, CTRL_T_A),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let unbound = session.screen_at(1);
    assert!(
        !agent_view_open(&unbound),
        "`{}` is not the configured prefix and must not open the view; whole \
         screen:\n{}",
        CTRL_X.escape_debug(),
        Session::dump(&unbound)
    );
    assert!(
        pinned_row(&unbound).contains('b'),
        "non-vacuous: the keystrokes arrived and the unbound prefix byte was \
         inert, so its `b` is in the draft; pinned row {:?}; whole screen:\n{}",
        pinned_row(&unbound),
        Session::dump(&unbound)
    );
    assert_opened_and_restored(&session, 1, 3, 5);
}

/// Test T4-S2c. **`←` twice on an empty composer opens it; once only hints.**
///
/// A1's two-press rule, with the hint as the first press's entire observable
/// effect. The first-press assertion is non-vacuous because it is *positive*:
/// the hint has to be on the pinned row, which is only possible if the
/// keystroke was seen at all.
#[test]
#[serial]
fn inline_mode_opens_the_agent_view_on_a_second_left_arrow_and_hints_on_the_first() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        send(RAW_MODE_MARKER, LEFT),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, LEFT),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let hinted = session.screen_at(1);
    assert!(
        !agent_view_open(&hinted),
        "one \u{2190} must not open the view; whole screen:\n{}",
        Session::dump(&hinted)
    );
    assert_left_hint(&hinted, "one \u{2190} on an empty composer");
    assert_opened_and_restored(&session, 1, 3, 5);
}

/// Test T4-S2d. **`←` never consumes a draft** — and the cursor really moved.
///
/// The draft is `/contextXY`; two `←`s then two forward deletes remove the
/// `XY` and the submitted line is `/context`, whose output is asserted inside
/// the region. That is the positive control the negative assertion needs: the
/// command could only have run if the cursor sat two characters from the end,
/// which is what `←` does and what opening an overlay instead would not.
///
/// The same session then presses `←` twice on the *emptied* composer and the
/// view must open, so "the view did not open on a draft" is measured against a
/// detector proven to fire in the same session, at the same geometry.
#[test]
#[serial]
fn inline_mode_left_arrow_moves_the_cursor_in_a_draft_and_never_opens_the_agent_view() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        send(RAW_MODE_MARKER, "/contextXY"),
        send("/contextXY", LEFT_TWICE),
        pause("/contextXY", SETTLE_MS),
        // Delete forward twice: with the cursor moved left twice this eats the
        // `XY` and leaves `/context`, which the carriage return then submits.
        send("/contextXY", "\x1b[3~\x1b[3~\r"),
        send("context window source", LEFT_TWICE),
        pause("context window source", SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let drafting = session.screen_at(2);
    assert!(
        !agent_view_open(&drafting),
        "\u{2190} on a NON-empty composer is cursor movement and must not open \
         the view; whole screen:\n{}",
        Session::dump(&drafting)
    );
    assert!(
        pinned_row(&drafting).contains("/contextxy"),
        "\u{2190} must not consume the draft: the pinned row still reads the \
         whole of it, but it reads {:?}; whole screen:\n{}",
        pinned_row(&drafting),
        Session::dump(&drafting)
    );
    assert_no_left_hint(&drafting, "\u{2190} on a non-empty composer");

    // The cursor moved: only a line whose `XY` was deleted from in front of
    // the cursor can run as `/context`.
    let submitted = session.screen_at(4);
    assert!(
        submitted[..(ROWS - 1) as usize]
            .iter()
            .any(|row| row.contains("context window source")),
        "the two \u{2190}s must have moved the cursor, so the forward deletes \
         left `/context` to submit; whole screen:\n{}",
        Session::dump(&submitted)
    );

    // And the detector above is live: on the now-empty composer the same two
    // keystrokes do open the view.
    assert_opened_and_restored(&session, 4, 5, 7);
}

/// Test T4-S2e. **`left_arrow_opens_agents = false` disables the `←` route
/// and nothing else.**
///
/// The chord in the same session is the positive control: with the config off
/// `←←` must do nothing and `Ctrl+X a` must still open the view, so the
/// setting is a route switch rather than a feature switch.
#[test]
#[serial]
fn inline_mode_left_arrow_route_is_disabled_by_config_while_the_chord_still_opens() {
    let home = TempDir::new().expect("temp home");
    write_config_with_tui(home.path(), "left_arrow_opens_agents = false");
    let script = vec![
        send(RAW_MODE_MARKER, LEFT_TWICE),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, CTRL_X_A),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let disabled = session.screen_at(1);
    assert!(
        !agent_view_open(&disabled),
        "`left_arrow_opens_agents = false` must disable the \u{2190} route; \
         whole screen:\n{}",
        Session::dump(&disabled)
    );
    assert_no_left_hint(
        &disabled,
        "a disabled \u{2190} route hints nothing either (the hint is the \
         affordance, so showing it would promise a key that does nothing)",
    );
    assert_opened_and_restored(&session, 1, 3, 5);
}

/// Test T4-S2f. **A stale first `←` must not open the view later.**
///
/// Press `←` once (armed), type, delete back to empty, press `←` once: that
/// press is the *first* of a new pair and must only hint. The second then
/// opens, which is the in-test positive control.
///
/// This pins the reset rule the card asks for: the armed state belongs to an
/// uninterrupted run of `←`s on an untouched empty composer, and **any other
/// keystroke that reaches the composer clears it**. The alternative — arming
/// until the view opens — is a keystroke whose meaning depends on something
/// the user did minutes ago and can no longer see, which is the bug this test
/// exists to forbid.
#[test]
#[serial]
fn inline_mode_a_stale_first_left_press_does_not_open_the_agent_view() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        send(RAW_MODE_MARKER, LEFT),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "zz"),
        send("zz", "\x7f\x7f"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, LEFT),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, LEFT),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let restarted = session.screen_at(6);
    assert!(
        !agent_view_open(&restarted),
        "typing and clearing between the two \u{2190}s must reset the \
         affordance, so this press cannot be the second one; whole screen:\n{}",
        Session::dump(&restarted)
    );
    assert_left_hint(
        &restarted,
        "the press after the draft was touched is a FIRST press",
    );
    assert_opened_and_restored(&session, 6, 8, 10);
}

/// Test T4-S2g. **`↑` recall is untouched by the `←` binding.**
///
/// The existing behaviour: at the idle prompt the editor emits
/// `EditorAction::RecallRequest` on `↑` over an empty, walk-free draft
/// (`kitsu-editor/src/lib.rs:703`) and the inline reader **declines** it,
/// calling `history_prev_forced` so the press walks history instead
/// (`tui_bridge.rs`'s `RecallRequest` arm, measured by
/// `inline_mode_recalls_history_persisted_by_an_earlier_session` and by the
/// reader's own test 10). A `←` binding that shared that empty-draft state, or
/// that consumed the next key, would break history recall on the first press —
/// which is the regression a user notices immediately.
///
/// So: arm the `←` affordance, then walk two entries back, and the recalled
/// line must land on the pinned row exactly as it does with no `←` pressed.
/// The run then clears the draft and opens the view with `←←`, so the recall
/// assertion is not quietly measuring a session where the binding is absent.
#[test]
#[serial]
fn inline_mode_up_arrow_recall_is_unchanged_with_the_left_arrow_binding_armed() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session_options = Options {
        no_session: false,
        ..Options::default()
    };

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

    let script = vec![
        send(RAW_MODE_MARKER, LEFT),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, UP_TWICE),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        // Ctrl+C abandons the recalled draft, which also resets the `←` state.
        send(RAW_MODE_MARKER, "\x03"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, LEFT_TWICE),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let second = run_styled_chat_pty_with(home.path(), Some("inline"), &script, session_options);

    let recalled = second.screen_at(3);
    assert!(
        pinned_row(&recalled).contains("/context"),
        "two \u{2191}s must still walk history onto the pinned row with the \
         \u{2190} affordance armed, but it reads {:?}; whole screen:\n{}",
        pinned_row(&recalled),
        Session::dump(&recalled)
    );
    assert!(
        !agent_view_open(&recalled),
        "\u{2191} is not the \u{2190} route's second press; whole screen:\n{}",
        Session::dump(&recalled)
    );
    assert_no_left_hint(
        &recalled,
        "\u{2191} touched the draft, so the \u{2190} affordance is disarmed",
    );
    assert_opened_and_restored(&second, 5, 7, 9);
}

/// Test T4-S2h. **`/jobs view` opens the overlay in a TUI mode and keeps its
/// text deck in classic.**
///
/// Both halves run here so each is the other's control: the classic session
/// must print `agent deck by state` and never the overlay's title, and the
/// inline session must open the overlay and never print the deck heading *at
/// all*. The deck half of the inline assertion is taken on the raw byte stream
/// rather than the screen, because an overlay on the alternate screen would
/// hide a deck that had been printed underneath it.
#[test]
#[serial]
fn jobs_view_opens_the_overlay_in_inline_mode_and_prints_the_deck_in_classic() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());

    let classic = run_styled_chat_pty(
        home.path(),
        None,
        &[
            send(RAW_MODE_MARKER, "/jobs view\r"),
            pause(RAW_MODE_MARKER, SETTLE_MS),
            send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
        ],
    );
    assert!(
        classic.raw_count(JOBS_VIEW_DECK_HEADING) >= 1,
        "classic mode keeps the text deck; raw:\n{}",
        classic.raw_text().escape_debug()
    );
    assert_eq!(
        classic.raw_count(AGENT_VIEW_TITLE),
        0,
        "and classic mode opens no overlay; raw:\n{}",
        classic.raw_text().escape_debug()
    );

    let inline = run_styled_chat_pty(
        home.path(),
        Some("inline"),
        &[
            send(RAW_MODE_MARKER, "/jobs view\r"),
            pause(RAW_MODE_MARKER, SETTLE_MS),
            send(RAW_MODE_MARKER, "\x1b"),
            pause(RAW_MODE_MARKER, SETTLE_MS),
            send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
        ],
    );
    assert_opened_and_restored(&inline, 0, 1, 3);
    assert_eq!(
        inline.raw_count(JOBS_VIEW_DECK_HEADING),
        0,
        "in a TUI mode `/jobs view` is the overlay, not the deck as well; \
         raw:\n{}",
        inline.raw_text().escape_debug()
    );
}

/// Test T4-S2i. **The `/agents` alias reaches the overlay too.**
///
/// A1 widens "the `/agents` alias arm, which today forwards only an empty or
/// numeric argument". It does not: the arm at `main.rs:20259` already forwards
/// any other argument verbatim to `handle_jobs_command`, and two unit tests
/// pin that it advertises `view`. What is missing is only the overlay at the
/// far end, so this measures the alias end to end rather than the forwarding.
#[test]
#[serial]
fn agents_view_alias_opens_the_overlay_in_inline_mode() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("inline"),
        &[
            send(RAW_MODE_MARKER, "/agents view\r"),
            pause(RAW_MODE_MARKER, SETTLE_MS),
            send(RAW_MODE_MARKER, "\x1b"),
            pause(RAW_MODE_MARKER, SETTLE_MS),
            send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
        ],
    );
    assert_opened_and_restored(&session, 0, 1, 3);
}

/// Test T4-S2j. **`?` shows the key help without closing the view, and `Esc`
/// still gets out.**
///
/// A3 gives `?` to help and `Esc` to close; what the help *says* is T4-S10's
/// (the single bindings table feeding a help overlay and `/help keys`), so
/// this pins only what T4-S2 owes: the key is answered — the screen changes —
/// the view is still the surface on screen afterwards, and the session can get
/// back out of both.
///
/// Two `Esc`s are sent deliberately. Whether the first returns from the help
/// to the list or closes the view outright is the implementer's to choose;
/// what is asserted is that after them the view is gone and what was on screen
/// before the open is back. A spare `Esc` at the idle composer is inert on an
/// empty draft (`resolve_lone_escape` clears a draft and there is none), so
/// neither choice makes the script wrong.
#[test]
#[serial]
fn inline_mode_agent_view_answers_the_help_key_and_still_closes() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        send(RAW_MODE_MARKER, CTRL_X_A),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "?"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let (opened, helped) = (session.screen_at(1), session.screen_at(3));
    let region = |screen: &[String]| screen[..(ROWS - 1) as usize].to_vec();
    assert!(
        agent_view_open(&opened),
        "the view must be open before `?` is pressed; whole screen:\n{}",
        Session::dump(&opened)
    );
    assert_ne!(
        region(&helped),
        region(&opened),
        "`?` must show the key help, so the screen cannot be unchanged; \
         screen with the view up:\n{}\nscreen after `?`:\n{}",
        Session::dump(&opened),
        Session::dump(&helped)
    );
    assert!(
        agent_view_open(&helped),
        "`?` is help, not an exit: the agent view is still the surface on \
         screen; whole screen:\n{}",
        Session::dump(&helped)
    );
    // Non-vacuous in the other direction too: `?` did not type itself into the
    // composer underneath, which is what an unhandled key would do.
    assert!(
        !pinned_row(&helped).contains('?'),
        "`?` belongs to the view, not to the composer; pinned row {:?}; whole \
         screen:\n{}",
        pinned_row(&helped),
        Session::dump(&helped)
    );
    assert_opened_and_restored(&session, 0, 1, 7);
}

/// Test T4-S4a. **`↑/↓` belong to the agent view.**
///
/// What the arrows *select* is pinned at unit level in `kitsu-tui`
/// (`AgentSelection`) and in `main.rs` (the walk over a real registry's rows):
/// a fixture session has zero agents, so there is no row to move over here.
/// What only a PTY can answer is the routing, and the routing is the half an
/// implementer is most likely to get wrong, because an arrow key is **not one
/// byte**. `↑` arrives as `ESC [ A`, and the overlay's byte loop reads `ESC`
/// as close today — so a naive binding closes the view on the first arrow and
/// then feeds `[A` to whatever is underneath.
///
/// Three things are asserted, and the third is what makes the first two a
/// claim rather than an observation about a dead loop:
///
/// 1. the view is still on screen after each arrow — the arrow is not an exit;
/// 2. the pinned composer row is unchanged — no byte of the sequence reached
///    the editor underneath;
/// 3. the POSITIVE CONTROL: a `?` pressed *after* both arrows still brings up
///    the help, so the overlay was reading keys the whole time. A loop that
///    had exited, or that had desynchronised on the escape sequence, cannot
///    answer `?`.
///
/// `Esc` must still close afterwards, which is what distinguishes "the overlay
/// consumed the arrow" from "the overlay now swallows `Esc` too".
#[test]
#[serial]
fn inline_mode_agent_view_answers_the_arrow_keys_without_typing_into_the_composer() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        send(RAW_MODE_MARKER, CTRL_X_A),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b[B"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b[A"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "?"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let (opened, downed, upped, helped) = (
        session.screen_at(1),
        session.screen_at(3),
        session.screen_at(5),
        session.screen_at(7),
    );
    let region = |screen: &[String]| screen[..(ROWS - 1) as usize].to_vec();
    assert!(
        agent_view_open(&opened),
        "the view must be open before the arrows are pressed; whole screen:\n{}",
        Session::dump(&opened)
    );
    for (name, screen) in [("\u{2193}", &downed), ("\u{2191}", &upped)] {
        assert!(
            agent_view_open(screen),
            "`{name}` is move, not an exit: the agent view must still be the \
             surface on screen; whole screen:\n{}",
            Session::dump(screen)
        );
        assert_eq!(
            pinned_row(screen),
            pinned_row(&opened),
            "`{name}` belongs to the view: no byte of its escape sequence may \
             reach the composer underneath it; screen with the view up:\n{}\n\
             screen after `{name}`:\n{}",
            Session::dump(&opened),
            Session::dump(screen)
        );
    }
    assert_ne!(
        region(&helped),
        region(&opened),
        "POSITIVE CONTROL: `?` pressed after both arrows still brings up the \
         key help, so the overlay was reading keys throughout and the two \
         assertions above are not about a loop that had already exited; \
         screen with the view up:\n{}\nscreen after `?`:\n{}",
        Session::dump(&opened),
        Session::dump(&helped)
    );
    assert!(
        agent_view_open(&helped),
        "whole screen:\n{}",
        Session::dump(&helped)
    );
    assert_opened_and_restored(&session, 0, 1, 11);
}

/// Test T4-S4b. **None of the action keys reaches the composer underneath,
/// and none of them is an exit.**
///
/// A GUARD in the same shape as T4-S3a's `Space` test, and for the same
/// reason: a fixture PTY session has **zero agents**, so every one of `r`,
/// `s`, `x`, `n` and `p` meets the no-selection case here and there is
/// nothing for it to act on. What the keys *do* is pinned at unit level in
/// `main.rs` against a real `AgentRegistry`. What only a PTY can answer is
/// the routing, and it is the half that regresses: an implementer who
/// forwards an unhandled key to the editor, or who lets `n` fall through to
/// the draft, breaks this.
///
/// `s` is the one to watch. It opens a line of text when there *is* a
/// selection, and a sub-mode that read bytes before checking the selection
/// would start echoing here, with no agent to steer and no obvious way back.
///
/// The POSITIVE CONTROL is the `?` after all five: the overlay still answers
/// its own key, so the five equalities above are about keys being consumed
/// and not about a loop that had already exited or desynchronised.
#[test]
#[serial]
fn inline_mode_agent_view_answers_the_action_keys_without_typing_into_the_composer() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        send(RAW_MODE_MARKER, CTRL_X_A),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "r"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "s"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "x"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "n"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "p"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "?"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let opened = session.screen_at(1);
    assert!(
        agent_view_open(&opened),
        "the view must be open before the action keys are pressed; whole \
         screen:\n{}",
        Session::dump(&opened)
    );
    for (key, mark) in [("r", 3), ("s", 5), ("x", 7), ("n", 9), ("p", 11)] {
        let screen = session.screen_at(mark);
        assert!(
            agent_view_open(&screen),
            "`{key}` acts on the selected agent; it is not an exit, so the \
             agent view must still be the surface on screen; whole screen:\n{}",
            Session::dump(&screen)
        );
        assert_eq!(
            pinned_row(&screen),
            pinned_row(&opened),
            "`{key}` belongs to the view: nothing may be typed into the \
             composer underneath it; screen with the view up:\n{}\nscreen \
             after `{key}`:\n{}",
            Session::dump(&opened),
            Session::dump(&screen)
        );
    }

    let region = |screen: &[String]| screen[..(ROWS - 1) as usize].to_vec();
    let helped = session.screen_at(13);
    assert_ne!(
        region(&helped),
        region(&opened),
        "POSITIVE CONTROL: `?` pressed after all five action keys still \
         brings up the key help, so the overlay was reading keys throughout \
         and the assertions above are not about a loop that had already \
         exited; screen with the view up:\n{}\nscreen after `?`:\n{}",
        Session::dump(&opened),
        Session::dump(&helped)
    );
    assert!(
        agent_view_open(&helped),
        "whole screen:\n{}",
        Session::dump(&helped)
    );
    assert_opened_and_restored(&session, 0, 1, 17);
}

/// Finding **F68**, the routing half: **a bracketed paste is not a string of
/// overlay keys.**
///
/// The registry half — that a paste answers no permission and stops no agent
/// — is pinned at unit level in `main.rs` against a real `AgentRegistry`, for
/// T4-S3's and T4-S4a's reason: a fixture PTY session has **zero agents**, so
/// no key here can act on one. What only a PTY can answer is whether the
/// overlay is still the surface on screen afterwards and whether any pasted
/// byte reached the composer underneath, and both regress the moment a paste
/// is decoded as keys: the payload below carries `q`, which **closes the
/// view**, and a newline, which submits whatever prompt is open.
///
/// The paste is sent **bracketed** (`ESC[200~` … `ESC[201~`) on purpose. It is
/// what a terminal with paste reporting sends, and it is the only spelling
/// that is immune to the pty splitting the write: the delimiters carry the
/// state, so the assertion does not depend on how the bytes arrive.
///
/// The POSITIVE CONTROL is the `?` afterwards: the overlay still answers its
/// own key, so this is not a loop that had already exited, and the `Esc`
/// closes it, so it is not one that has stopped answering `Esc` either.
#[test]
#[serial]
fn inline_mode_agent_view_treats_a_bracketed_paste_as_text_and_not_as_keys() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        send(RAW_MODE_MARKER, CTRL_X_A),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(
            RAW_MODE_MARKER,
            "\x1b[200~retry the exxtract\nand quit the bug\x1b[201~",
        ),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "?"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let opened = session.screen_at(1);
    assert!(
        agent_view_open(&opened),
        "the view must be open before the paste; whole screen:\n{}",
        Session::dump(&opened)
    );
    let pasted = session.screen_at(3);
    assert!(
        agent_view_open(&pasted),
        "a paste is text, not keys: its `q` must not close the view; whole \
         screen:\n{}",
        Session::dump(&pasted)
    );
    assert_eq!(
        pinned_row(&pasted),
        pinned_row(&opened),
        "and no pasted byte reaches the composer underneath; screen with the \
         view up:\n{}\nscreen after the paste:\n{}",
        Session::dump(&opened),
        Session::dump(&pasted)
    );

    let region = |screen: &[String]| screen[..(ROWS - 1) as usize].to_vec();
    let helped = session.screen_at(5);
    assert_ne!(
        region(&helped),
        region(&opened),
        "POSITIVE CONTROL: `?` pressed after the paste still brings up the \
         key help, so the overlay was reading bytes throughout and the \
         assertions above are not about a loop that had already exited; \
         screen with the view up:\n{}\nscreen after `?`:\n{}",
        Session::dump(&opened),
        Session::dump(&helped)
    );
    assert_opened_and_restored(&session, 0, 1, 9);
}

// ----------------------------------------------------------------------
// Card T4-S6a: attach/detach, the routing half the PTY can measure.
//
// **What this harness can and cannot reach.** A fixture PTY session has
// **zero agents** and no way to spawn one (the gateway is unreachable by
// construction), so nothing here can attach: `Enter`/`→`, the `⛩ attached
// to agent N` milestone and requirement A7's custody are pinned at unit
// level against a real `AgentRegistry` in `main.rs`, and the `←`-detaches
// rule against the reader in `tui_bridge.rs`. What is left for the PTY is
// what the PTY is for (T4-S2's division): that the keys are routed and
// that no byte of them reaches the draft.
// ----------------------------------------------------------------------

/// `Ctrl+X d`: detach, as the two bytes a terminal sends for it
/// (design §4.7). `d` is not bound in the editor's keymap, so with the
/// prefix stripped it would otherwise be typed into the draft.
const CTRL_X_D: &str = "\x18d";

/// The refusal a `/detach` typed with nothing attached gets.
///
/// **The division this card settles between the chord and the command**:
/// `Ctrl+X d` with nothing attached is **inert and silent**, exactly as
/// `Ctrl+X b` is (T4-S2: "any other letter is inert"), because a chord
/// pressed in the wrong state is a slip and a transcript line is a worse
/// answer than none. `/detach` is a line the user typed and submitted, so
/// it is answered — requirement T2-S5b's rule applied to the surface that
/// has somewhere to put the answer.
const DETACH_WITHOUT_ATTACHMENT: &str = "not attached to a background agent";

/// Card T4-S6a. **`Ctrl+X d` is a chord, not a `d`; `/detach` answers.**
///
/// Four observations in one session, so the detector that finds the
/// refusal is proven live at the same geometry (**F41**/the non-vacuity
/// rule): a typed `d` really does land in the draft, the chord's `d` does
/// not, a known command's output really does reach the region, and
/// `/detach` is answered rather than reported as an unknown command.
#[test]
#[serial]
fn inline_mode_routes_the_detach_chord_and_answers_detach_without_an_attachment() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let script = vec![
        // 0..1 A typed `d` lands in the draft: the detector is live.
        send(RAW_MODE_MARKER, "zd"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        // 2..4 The same letter behind the chord prefix does not.
        send(RAW_MODE_MARKER, "\x15"),
        send(RAW_MODE_MARKER, "z"),
        send(RAW_MODE_MARKER, CTRL_X_D),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        // 6..7 A known command's output reaches the region.
        send(RAW_MODE_MARKER, "\x15"),
        send(RAW_MODE_MARKER, "/context\r"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        // 9..10 And `/detach` is answered.
        send(RAW_MODE_MARKER, "/detach\r"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, QUIT_AFTER_CLOSE),
    ];
    let session = run_styled_chat_pty(home.path(), Some("inline"), &script);

    let typed = session.screen_at(1);
    assert!(
        pinned_row(&typed).contains("zd"),
        "NON-VACUOUS: a typed `d` must reach the draft, but the pinned row \
         reads {:?}; whole screen:\n{}",
        pinned_row(&typed),
        Session::dump(&typed)
    );

    let chorded = session.screen_at(5);
    assert!(
        pinned_row(&chorded).contains('z') && !pinned_row(&chorded).contains("zd"),
        "`Ctrl+X d` is one keystroke and its `d` must not reach the draft, \
         which still reads {:?}; whole screen:\n{}",
        pinned_row(&chorded),
        Session::dump(&chorded)
    );
    assert!(
        !agent_view_open(&chorded),
        "and it is not the agent view's chord either; whole screen:\n{}",
        Session::dump(&chorded)
    );

    let command = session.screen_at(8);
    assert!(
        command[..(ROWS - 1) as usize]
            .iter()
            .any(|row| row.contains("context window source")),
        "NON-VACUOUS: a known command's output must reach the region, so the \
         refusal below is measured by a detector that fires; whole screen:\n{}",
        Session::dump(&command)
    );

    let detached = session.screen_at(10);
    assert!(
        detached[..(ROWS - 1) as usize]
            .iter()
            .any(|row| row.contains(DETACH_WITHOUT_ATTACHMENT)),
        "`/detach` with nothing attached must say so (A5 names it beside \
         `\u{2190}`), rather than being reported as an unknown command; whole \
         screen:\n{}",
        Session::dump(&detached)
    );
}

// ---------------------------------------------------------------------------
// Card F94: a dying signal must give the alternate screen back.
//
// `SIGNAL_RESTORE` (`tui_bridge.rs`) is `ESC[r ESC[?25h ESC[?2004l
// ESC[23;2t` -- region reset, show cursor, bracketed paste off, title pop --
// and **no `ESC[?1049l`**. Two surfaces in this binary take the alternate
// screen: the agent-view overlay (`main.rs`, written straight-line, restored
// only on the normal path) and `FullscreenSegment` (a guard, restored only by
// `Drop`). `Drop` does not run on a signal and the handler writes no
// `?1049l`, so a `SIGINT` or a `SIGTERM` taken while either surface is up
// leaves the user looking at kitsu's alternate screen with their transcript
// stranded on the primary one. That is requirement N5's signal clause, open
// for the overlay since T4-S2 and for fullscreen since T5-S2.
//
// Nothing caught it because no test in this file had ever killed the process
// while a surface held the alternate screen: every `Action::Signal` script
// above signals an inline session, which takes no alternate screen at all.
// The absence was the bug; these four tests are the thing that was missing.
//
// The two restores below are deliberately *counted*, not merely found. The
// restore has to be emitted when the screen is held and **not** when it is
// not -- a handler that wrote `?1049l` unconditionally would move the cursor
// to a stale DECSC slot on every `SIGTERM` of a session that never left
// inline -- so the no-regression and once-only tests are as load-bearing as
// the two that fail today, and they are what distinguishes a gated restore
// from an unconditional one.
// ---------------------------------------------------------------------------

/// Open the agent view with `/jobs view`, let it paint, then kill the child
/// while it is up.
///
/// `/jobs view` rather than the `Ctrl+X a` chord because it is one line with
/// a receipt-free, already-satisfied trigger and the overlay it reaches is
/// the same one (`jobs_view_opens_the_overlay_in_inline_mode_and_prints_the_deck_in_classic`
/// pins that equivalence). The pause is the mark: it separates the overlay's
/// paint from everything the child writes *after* the signal, which is the
/// only window in which the restore can be observed.
fn jobs_view_then_signal(signal_number: libc::c_int) -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/jobs view\r"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        signal(RAW_MODE_MARKER, signal_number),
        pause(RAW_MODE_MARKER, SETTLE_MS),
    ]
}

/// Assert that a session which was holding the alternate screen when it was
/// signalled gave it back.
///
/// Both halves are needed and neither is the other's restatement: the screen
/// must have been *taken* before the signal, or there is nothing owed, and
/// the leave must appear in the window **after** the signal, or it is the
/// normal path's leave being read twice.
fn assert_alt_screen_released_after_signal(session: &Session, step: usize, label: &str) {
    let before = String::from_utf8_lossy(&session.raw[..session.marks[step]]).into_owned();
    assert!(
        before.contains(ALT_SCREEN_ON),
        "{label}: the alternate screen must have been taken before the signal, \
         or there is nothing to give back; raw before the signal:\n{}",
        before.escape_debug()
    );
    assert!(
        !before.contains(ALT_SCREEN_OFF),
        "{label}: and it must still have been held at the signal -- a session \
         that had already left it cannot measure the dying restore; raw before \
         the signal:\n{}",
        before.escape_debug()
    );

    let after = session.raw_after(step);
    assert!(
        after.contains(ALT_SCREEN_OFF),
        "{label}: a terminal left on kitsu's alternate screen hides the user's \
         own transcript until they run `reset`, so every exit path owes \
         `ESC[?1049l` (N5); everything the child wrote after the signal \
         was:\n{}",
        after.escape_debug()
    );
}

/// **The overlay's hole, five cards old: `SIGINT` with the agent view up.**
///
/// Fails today in the one way that matters: the window after the signal
/// carries the handler's `ESC[r ESC[?25h ESC[?2004l ESC[23;2t` and nothing
/// else, so the assertion prints a restore that gave back the title and kept
/// the screen.
#[test]
#[serial]
fn agent_view_sigint_gives_the_alternate_screen_back() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("inline"),
        &jobs_view_then_signal(libc::SIGINT),
    );
    assert_alt_screen_released_after_signal(&session, 2, "SIGINT with the agent view up");
    // The rest of N5's signal clause still holds for this session, so a fix
    // that reached the alternate screen by rewriting the handler's one
    // `write(2)` into something that drops the other sequences fails here
    // rather than passing above.
    let after = session.raw_after(2);
    assert!(
        after.contains(REGION_RESET) && after.contains(PASTE_OFF),
        "the region reset and the paste-off the handler already wrote must \
         survive whatever gives the screen back; after the signal the child \
         wrote:\n{}",
        after.escape_debug()
    );
    assert!(
        session.raw_mode_off(),
        "and the pty must be canonical again, but it reads {}",
        session.termios_dump()
    );
}

/// Script: switch into fullscreen from inline, then take a `SIGTERM` inside
/// the fullscreen segment.
///
/// Entered by a switch rather than by starting the session in fullscreen, and
/// that is a measurement rather than a preference: `install` is inert in
/// fullscreen, so a session *started* there never reaches
/// `install_exit_restore` and has no dying-signal handler at all. A script
/// that started in fullscreen would fail this test for that second, larger
/// reason and say nothing about the alternate screen. See the report.
fn switch_into_fullscreen_then_sigterm() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/tui fullscreen\r"),
        signal(ALT_SCREEN_ON, libc::SIGTERM),
        pause(ALT_SCREEN_ON, SETTLE_MS),
    ]
}

/// **Fullscreen's inherited hole: `SIGTERM` inside the segment.**
///
/// The same assertion as the overlay's, on the other surface, and it is a
/// separate test because the two takers are separate code with separate
/// restores -- a fix that gates on a flag only one of them sets passes one of
/// these and fails the other, which is the enumeration this card exists to
/// force.
#[test]
#[serial]
fn fullscreen_segment_sigterm_gives_the_alternate_screen_back() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("inline"),
        &switch_into_fullscreen_then_sigterm(),
    );
    assert_alt_screen_released_after_signal(&session, 1, "SIGTERM in fullscreen");
    assert!(
        !session.status.success(),
        "a signalled session does not exit successfully; it exited {:?}",
        session.status
    );
}

/// **A GUARD, green today, and the one that says the restore must be gated.**
///
/// An inline session takes no alternate screen, so it is owed none back. The
/// cheapest fix for the two tests above is to append `ESC[?1049l` to the
/// handler's `const` unconditionally, and this is the test that refuses it:
/// `?1049l` is not inert on the primary screen -- xterm's `rmcup` restores
/// the cursor from the DECSC slot, so an unconditional leave on a session
/// that never took the screen can park the shell's next prompt wherever that
/// slot happens to point.
///
/// Counted over the whole session rather than the post-signal window: this
/// script never opens a surface, so *any* `?1049l` in it came from the dying
/// handler.
#[test]
#[serial]
fn inline_sigterm_gives_back_no_alternate_screen_it_never_took() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("inline"),
        &help_then_draft_then_signal(libc::SIGTERM),
    );
    let text = session.raw_text();
    // NON-VACUOUS: the handler did run in this session, so the count below is
    // a statement about what it wrote and not about whether it fired.
    assert!(
        session.raw_after(2).contains(REGION_RESET),
        "NON-VACUOUS: the dying restore must have run, or this session proves \
         nothing about what it writes; after the signal the child wrote:\n{}",
        session.raw_after(2).escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        0,
        "NON-VACUOUS: no surface in this script takes the alternate screen, \
         so any leave below is the handler's; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_OFF),
        0,
        "a dying signal must not give back an alternate screen the session \
         never took; raw:\n{}",
        text.escape_debug()
    );
}

/// Open the agent view, close it with `Esc`, **then** signal.
fn jobs_view_closed_then_sigint() -> Vec<Step> {
    vec![
        send(RAW_MODE_MARKER, "/jobs view\r"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        send(RAW_MODE_MARKER, "\x1b"),
        pause(RAW_MODE_MARKER, SETTLE_MS),
        signal(RAW_MODE_MARKER, libc::SIGINT),
        pause(RAW_MODE_MARKER, SETTLE_MS),
    ]
}

/// **A GUARD, green today: the restore is given once, not twice.**
///
/// The overlay's normal close already wrote `ESC[?1049l`; a signal arriving
/// afterwards is on the primary screen again and owes nothing. So the whole
/// session holds exactly one leave for exactly one take, and whatever tracks
/// "the screen is held" has to be cleared on the way out as well as set on
/// the way in -- the half of a gated fix that is easy to forget, and the half
/// that this test is the only reader of.
#[test]
#[serial]
fn agent_view_closed_before_a_signal_gives_the_screen_back_exactly_once() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &jobs_view_closed_then_sigint());
    let text = session.raw_text();

    // NON-VACUOUS, both ways: the overlay really opened and really closed, so
    // the count is about the restore's discipline and not about a view that
    // never appeared.
    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        1,
        "NON-VACUOUS: the overlay must have taken the alternate screen once; \
         raw:\n{}",
        text.escape_debug()
    );
    assert!(
        session.raw_after(4).contains(REGION_RESET),
        "NON-VACUOUS: and the dying restore must have run after the close; \
         after the signal the child wrote:\n{}",
        session.raw_after(4).escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_OFF),
        1,
        "one take, one leave: the overlay's own close gave the screen back, so \
         the dying handler must not write a second leave; raw:\n{}",
        text.escape_debug()
    );
}

// ---------------------------------------------------------------------------
// Card F96: a session *started* in fullscreen installs no dying-signal
// handler at all, which is strictly worse than F94 and shipped one card ago.
//
// `install_exit_restore` was called only inside `if installed.is_some()`
// (`main.rs`), and `install` returns `None` for `Classic | Fullscreen` --
// fullscreen pins no region, which is right. The conjunction was the defect:
// `--tui fullscreen` left `SIGINT` and `SIGTERM` on the **default
// disposition**, so the process died before anything ran. No scroll-region
// reset, no title pop, no alternate-screen release, and -- the serious half
// -- no `tcsetattr` back to canonical termios, which is a shell with no echo
// and no line editing until the user types `reset` blind.
//
// Nothing covered it because the four transition tests above all *switch*
// into fullscreen, so the inline segment they came from had already installed
// the handler; and the two scripts that start in fullscreen exit normally,
// which is the one exit path that does not need it. Starting in fullscreen
// and then dying is the uncovered intersection, and this is the test for it.
// ---------------------------------------------------------------------------

/// Start in fullscreen, wait until the session owns raw mode, then `SIGTERM`.
///
/// Triggered on [`RAW_MODE_MARKER`] rather than on the alternate screen, and
/// that is the point of the script rather than a detail: the termios half is
/// the serious half, so the signal has to land while raw mode is actually
/// engaged. The trailing pause is the window the dying restore is read in.
fn fullscreen_start_then_sigterm() -> Vec<Step> {
    vec![
        signal(RAW_MODE_MARKER, libc::SIGTERM),
        pause(RAW_MODE_MARKER, SETTLE_MS),
    ]
}

/// **A session started in fullscreen gets its terminal back on a signal.**
///
/// The three observations are one requirement (N5) in three places, and each
/// was false before this card:
///
/// * **Termios.** The pty's line settings outlive the child -- the harness
///   holds its own descriptor on the slave -- so `ICANON`/`ECHO`/`ISIG` are
///   the state a real user's shell would be left in. This is the assertion
///   that fails on a missing *handler* rather than on missing bytes, and it
///   is why this test is not a restatement of F94's: a flag that tells the
///   handler to write more does nothing when there is no handler.
/// * **The alternate screen.** Taken by `enter_fullscreen` at startup and
///   owed back by whatever ends the process.
/// * **The title.** Pushed by the same call, popped by the handler's stream.
#[test]
#[serial]
fn fullscreen_start_sigterm_restores_termios_and_the_screen() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("fullscreen"),
        &fullscreen_start_then_sigterm(),
    );
    let text = session.raw_text();
    let before = String::from_utf8_lossy(&session.raw[..session.marks[0]]).into_owned();

    // NON-VACUOUS, both halves: the session really was in fullscreen holding
    // the alternate screen when the signal landed, so the restores below are
    // about a terminal that was really taken.
    assert!(
        before.contains(ALT_SCREEN_ON),
        "NON-VACUOUS: `--tui fullscreen` must have taken the alternate screen \
         before the signal; raw before the signal:\n{}",
        before.escape_debug()
    );
    assert!(
        !before.contains(ALT_SCREEN_OFF),
        "NON-VACUOUS: and must still have been holding it -- a session that \
         had already left cannot measure the dying restore; raw before the \
         signal:\n{}",
        before.escape_debug()
    );

    // The termios half, and the one that says the handler exists at all: a
    // child killed on the default disposition leaves these three cleared.
    assert!(
        session.raw_mode_off(),
        "a session started in fullscreen must install the dying-signal \
         handler too: without it the signal's default disposition kills the \
         process with the terminal still in raw mode, and the user's shell \
         has no echo and no line editing until they type `reset` blind. The \
         pty reads {} and the child wrote this after the signal:\n{}",
        session.termios_dump(),
        session.raw_after(0).escape_debug()
    );

    let after = session.raw_after(0);
    assert!(
        after.contains(REGION_RESET) && after.contains(PASTE_OFF),
        "and the handler's own stream must have run on this exit path like \
         any other; after the signal the child wrote:\n{}",
        after.escape_debug()
    );
    assert!(
        after.contains(ALT_SCREEN_OFF),
        "the alternate screen the startup took must be given back, or the \
         user's own transcript stays hidden under kitsu's screen (N5); after \
         the signal the child wrote:\n{}",
        after.escape_debug()
    );
    assert!(
        after.contains(TITLE_POP),
        "and so must the title the same call pushed; after the signal the \
         child wrote:\n{}",
        after.escape_debug()
    );
    assert!(
        !session.status.success(),
        "a signalled session does not exit successfully; it exited {:?}, \
         raw:\n{}",
        session.status,
        text.escape_debug()
    );
}

// ---------------------------------------------------------------------------
// Card H2-title: the OSC terminal title, and `[tui] title` gating it.
//
// Requirement **H2**: *"Kitsu MUST set the terminal title (OSC 0/2) to
// `kitsu · working|blocked|idle|N agents` when `tui.title = true` (default on
// TTY), so herdr/tmux can classify by title."* **F139** measured the half that
// exists: the title *stack* is pushed and popped in production without
// consulting the key, and nothing writes the OSC 2 string at all.
//
// FAILING-FIRST against this commit: no session writes `ESC]2;`, and
// `title = false` pushes the stack anyway.
// ---------------------------------------------------------------------------

/// The OSC 2 set-window-title introducer, and its ST terminator.
///
/// Attributable in these sessions for the reason [`ALT_SCREEN_ON`] is: the
/// only other writer of this sequence in the workspace is
/// `kitsu_tui::TuiRuntime::enter`, which has no non-test caller (**F139**),
/// and no script below reaches one.
const OSC_TITLE_OPEN: &str = "\x1b]2;";

/// The title a styled session with no background agents must be carrying: the
/// state word is `agent_state_file::process_state`'s own, and the count is
/// always present.
const IDLE_TITLE: &str = "\x1b]2;kitsu · idle · 0 agents";

/// **The title is actually set, in a real session, with the default key.**
///
/// The end-to-end half: a unit test over `state_title` pins the string, and
/// this pins that a session *writes* it. One styled fullscreen session that
/// reaches the idle prompt must carry the idle title at least once.
#[test]
#[serial]
fn tui_title_sets_the_osc_state_title_in_a_styled_session() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(
        home.path(),
        Some("fullscreen"),
        &fullscreen_start_then_classic(),
    );
    let text = session.raw_text();

    assert!(
        text.contains(IDLE_TITLE),
        "requirement H2: a styled session with `[tui] title` at its default \
         must set the terminal title to {IDLE_TITLE:?}; raw:\n{}",
        text.escape_debug()
    );
    // NON-VACUOUS: the session genuinely reached the fullscreen segment and
    // genuinely exited, so the title above was written by a live session and
    // not by a startup probe that then failed.
    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        1,
        "one fullscreen segment; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        session.status.success(),
        "the session must still exit cleanly, but exited {:?}; raw:\n{}",
        session.status,
        text.escape_debug()
    );
}

/// **`[tui] title = false` sets no title AND pushes no title stack.**
///
/// **F139**'s half-built mechanism, closed from the user's side: the key must
/// gate both clauses. The alternate-screen counts are the non-vacuity anchor
/// — the segment still runs, takes the screen and gives it back, so the three
/// absent title sequences are the key's doing and not a session that never
/// started.
#[test]
#[serial]
fn tui_title_false_sets_no_title_and_pushes_no_title_stack() {
    let home = TempDir::new().expect("temp home");
    write_config_with_tui(home.path(), "title = false");
    let session = run_styled_chat_pty(
        home.path(),
        Some("fullscreen"),
        &fullscreen_start_then_classic(),
    );
    let text = session.raw_text();

    assert_eq!(
        session.raw_count(ALT_SCREEN_ON),
        1,
        "NON-VACUOUS: the fullscreen segment still takes the alternate \
         screen; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(ALT_SCREEN_OFF),
        1,
        "NON-VACUOUS: ...and gives it back; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(OSC_TITLE_OPEN),
        0,
        "`title = false` must set no terminal title; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(TITLE_PUSH),
        0,
        "`title = false` must not push the title stack either — the half \
         F139 measured as ungated; raw:\n{}",
        text.escape_debug()
    );
    assert_eq!(
        session.raw_count(TITLE_POP),
        0,
        "...and must not pop a stack it never pushed; raw:\n{}",
        text.escape_debug()
    );
    assert!(
        session.status.success(),
        "the session must still exit cleanly, but exited {:?}; raw:\n{}",
        session.status,
        text.escape_debug()
    );
}

/// **Requirement N1: the title's control bytes reach neither the plain nor the
/// assistive surface, and today's bytes are otherwise untouched.**
///
/// The title is control bytes on **stderr**, so N1 applies per descriptor
/// (F120/F122): `--plain`, `NO_COLOR` and `--screen-reader` all fold into
/// `kitsu_render::plain_mode`, and that surface must stay byte-identical to
/// today.
///
/// Which makes the second half of each case the load-bearing one: the title
/// **stack** is pushed and popped on these surfaces **today**, with the key at
/// its default, so suppressing those two sequences here would be the N1
/// violation rather than the fix. The negative is the OSC 2 string alone.
#[test]
#[serial]
fn tui_title_is_not_set_on_the_plain_or_assistive_surfaces() {
    for flag in ["--plain", "--screen-reader"] {
        let home = TempDir::new().expect("temp home");
        write_config(home.path());
        let session = run_styled_chat_pty_core(
            home.path(),
            Some("fullscreen"),
            &fullscreen_start_then_classic(),
            Options::default(),
            &[flag.to_string()],
        );
        let text = session.raw_text();

        assert_eq!(
            session.raw_count(OSC_TITLE_OPEN),
            0,
            "{flag} must receive no OSC title (N1, per descriptor); raw:\n{}",
            text.escape_debug()
        );
        assert_eq!(
            session.raw_count(TITLE_PUSH),
            1,
            "NON-VACUOUS, and N1 in the other direction: {flag} pushes the \
             title stack today with the key at its default, and must keep \
             doing so; raw:\n{}",
            text.escape_debug()
        );
        assert_eq!(
            session.raw_count(TITLE_POP),
            1,
            "...and pops it, exactly as today; raw:\n{}",
            text.escape_debug()
        );
    }
}
