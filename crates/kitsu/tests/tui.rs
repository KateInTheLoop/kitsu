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

/// One scripted input: wait until `trigger` appears in everything read so far,
/// then write `send` to the master. Polling on a trigger rather than sleeping
/// is what keeps the harness from racing the child's prompt.
struct Step {
    trigger: &'static str,
    send: &'static str,
}

/// Everything a finished session left behind: the raw byte stream off the PTY
/// master and the emulator that consumed it.
struct Session {
    raw: Vec<u8>,
    parser: vt100::Parser,
    status: std::process::ExitStatus,
    /// `raw.len()` at the moment each scripted input was written, so a test
    /// can replay a *prefix* of the stream and see the screen as it stood
    /// mid-session rather than only after exit. `marks[1]` is the screen just
    /// before `/quit` went in, which is the one the F17 question is about.
    marks: Vec<usize>,
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

    /// The final screen, one `String` per row, trailing blanks trimmed per row
    /// by `vt100` itself.
    fn screen(&self) -> Vec<String> {
        self.parser.screen().rows(0, COLS).collect()
    }

    /// The screen as it stood when scripted input `step` was written: the
    /// first `marks[step]` bytes replayed into a fresh emulator.
    fn screen_at(&self, step: usize) -> Vec<String> {
        let upto = self.marks[step];
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        parser.process(&self.raw[..upto]);
        parser.screen().rows(0, COLS).collect()
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

/// Run a **styled** interactive chat on a PTY of exactly `ROWS`x`COLS` and
/// drive it with `script`.
///
/// The child gets all three descriptors on the PTY slave (one `dup` each, the
/// same discipline `tests/cli.rs` uses) and no `--plain`/`--no-stream`, so
/// styling and the viewport are live. `tui` is the value of `KITSU_TUI`:
/// `None` leaves it unset, which is classic mode.
fn run_styled_chat_pty(home: &Path, tui: Option<&str>, script: &[Step]) -> Session {
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
    let size = libc::winsize {
        ws_row: ROWS,
        ws_col: COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `master` is a live PTY master and `size` is a valid winsize.
    assert_eq!(
        unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &size) },
        0,
        "set the PTY window size"
    );

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
        .args(["--gateway", "example", "chat", "--no-session"]);
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
                terminal
                    .write_all(next.send.as_bytes())
                    .expect("write scripted input");
                terminal.flush().expect("flush scripted input");
                marks.push(raw.len());
                step += 1;
            }
        }

        if let Some(status) = child.try_wait().expect("poll styled carrier") {
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

    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    parser.process(&raw);
    Session {
        raw,
        parser,
        status,
        marks,
    }
}

/// The script both measurements run: submit `/help`, then `/quit`.
///
/// The triggers are what the binary prints unconditionally at startup (the
/// banner's `/help for commands`) and what `/help` itself prints (`commands:`),
/// so each input is sent only once the child has demonstrably reached the
/// prompt.
fn help_then_quit() -> Vec<Step> {
    vec![
        Step {
            trigger: "/help for commands",
            send: "/help\n",
        },
        Step {
            trigger: "/help <command> explains one command in detail",
            send: "/quit\n",
        },
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

/// Test 4. **A second finding, pinned as measured rather than as wanted.**
///
/// The session's closing summary line (`↳ completed in …`) is printed *after*
/// `BridgeGuard::drop` has released the region: the release and that line are
/// adjacent in the byte stream, release first. `ESC[r` homes the cursor (per
/// DECSTBM, resetting the margins moves the cursor to row 1 column 1), so
/// that line lands on **row 1** and overwrites the top of the transcript,
/// while the status text painted on row 24 is left on the screen because
/// `hide` releases the region without clearing the viewport rows.
///
/// Both are defects of the teardown order, not of this test; fixing them
/// belongs to a later card. Asserting the observed behaviour keeps the next
/// card honest about what it changed.
#[test]
#[serial]
fn inline_mode_teardown_leaves_the_last_line_at_the_top_and_a_stale_status_row() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let session = run_styled_chat_pty(home.path(), Some("inline"), &help_then_quit());
    let text = session.raw_text();

    let release = text.find("\x1b[r").expect("the guard releases the region");
    let tail = &text[release..];
    assert!(
        tail.contains("↳ completed in"),
        "the closing summary line is written after the region release; tail:\n{}",
        tail.escape_debug()
    );

    let screen = session.screen();
    assert!(
        screen[0].contains("↳ completed in"),
        "ESC[r homes the cursor, so that line lands on row 1; screen:\n{}",
        session.screen_dump()
    );
    assert_eq!(
        screen[(ROWS - 1) as usize].trim_end(),
        "kitsu",
        "and the painted status row is never cleared on the way out; screen:\n{}",
        session.screen_dump()
    );
}
