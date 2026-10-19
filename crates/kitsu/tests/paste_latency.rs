//! Requirement **N2**, measured: a 10 KiB bracketed paste into the composer,
//! timed through a real PTY, in both renderers.
//!
//! > **N2** — Idle input latency MUST not regress: paste of 10 KiB into the
//! > composer completes in under 100 ms (the rustyline `ExternalPrinter`
//! > lesson). *Measured by a benchmark, not a CI gate.*
//!
//! Plan row **T3-S13**. This file is the benchmark, and it is deliberately
//! **reported, not gating** — see "What gates and what only prints" below.
//!
//! # What is timed, exactly
//!
//! `tui_bridge`'s test 20 already times the same payload *in process*: it
//! hands `read_line_inline` a canned `Chunks` reader and measures the reader
//! alone. That is a good pathology detector and it is not N2. N2 is about
//! **idle input latency** — what a user who hits Ctrl+V at a settled prompt
//! waits for — and everything test 20 cannot see lives in between: the pty
//! line discipline, the child's `read` loop boundaries, the composer's
//! windowing of a 10 KiB draft, and the escape bytes the renderer writes back
//! for every repaint.
//!
//! So this measures the round trip, with these two points:
//!
//! * **Start (`t0`)** — the instant *before* the first byte of the burst is
//!   written to the pty master, with the child already at a settled idle
//!   prompt: `ESC[?2004h` has been seen (the raw reader is engaged, so the
//!   line discipline is no longer echoing and the child is blocked in `read`
//!   on the slave) and the stream has then been quiet for [`SETTLE`].
//!
//! * **End (`t1`)** — the instant the harness has read back the composer row
//!   containing [`SENTINEL`], the last 24 bytes of the payload.
//!
//! The burst is exactly what a terminal delivers for a paste:
//! `ESC[200~` + 10 KiB + `ESC[201~`, written as one `write` call per accepted
//! chunk and nothing else. **No `\r` is sent**: submitting would start a turn,
//! and N2 is about the composer accepting the paste, not about a turn.
//!
//! ## Why the sentinel is the end point
//!
//! "Completes" is the word doing the work in N2, and this is the definition
//! taken: the paste has completed when the composer is on screen showing the
//! finished draft. `fit_draft_window` windows a draft that overflows the row
//! around the **cursor**, and a paste leaves the cursor at the end, so the
//! composer row for this payload is `…` followed by the draft's tail — which
//! ends in `SENTINEL`. The sentinel reaching the master therefore means three
//! things at once, and they are the three the requirement is about: the child
//! consumed the whole burst (the tail is the last of it), it kept it as one
//! draft (a split at a newline would have submitted the first line and the
//! tail would never be a composer row), and it has painted.
//!
//! The alternative end points were rejected as measuring the wrong thing: the
//! last byte the harness *writes* measures the pty buffer, not the child; the
//! child's next read of a later keystroke measures that keystroke's latency
//! too; and quiescence of the stream measures a timeout.
//!
//! Non-vacuity, since the whole measurement is one `contains`: the harness
//! asserts the sentinel is **absent** from everything read before `t0`, so a
//! sentinel that was going to be found anyway fails the benchmark instead of
//! reporting 0 ms. That check is what makes this a measurement rather than a
//! tautology.
//!
//! It was also falsified on purpose once, which is the other half of the same
//! worry: with the burst suppressed and nothing else changed, the inline run
//! read **zero** bytes off the master for the whole 20 s deadline and failed.
//! An idle inline composer emits nothing at all, so every byte counted in
//! `response_bytes`, and the whole of `total`, is the paste's.
//!
//! ## What is inside the number, honestly
//!
//! `t1 - t0` includes two costs that are the harness's and not the binary's,
//! and both are reported separately rather than subtracted:
//!
//! * the kernel pty transit, and the harness's own `write`/`read` loop, which
//!   spins without sleeping precisely so this stays small;
//! * the drain, which is *required*: the master is `O_NONBLOCK` and the
//!   payload is larger than a pty buffer, so the writer must keep reading
//!   while it writes or the child blocks writing its repaints and the two
//!   processes deadlock. A real terminal does the same thing.
//!
//! `write_elapsed` in the report is when the last byte of the burst was
//! accepted by the pty; the remainder is the child.
//!
//! # What gates and what only prints
//!
//! The row says *reported, not gating*, so the **time never fails this
//! test**. Nothing is `#[ignore]`d either: an ignored benchmark is a benchmark
//! nobody reruns, and the measurement is cheap (a second per renderer). The
//! split is:
//!
//! * **Gating** — the correctness of the paste: the sentinel was absent
//!   before the burst and present after, within the harness deadline. That is
//!   the "one draft, intact, painted" assertion above, and it is the same
//!   property test 20 pins, now through the real carrier.
//! * **Reported** — the duration, printed on one `N2` line per renderer.
//!
//! One opt-in makes the 100 ms a hard assertion, for a run where the number
//! is the point:
//!
//! ```text
//! cargo test --locked -p kitsu --test paste_latency -- --nocapture
//! KITSU_N2_STRICT=1 cargo test --locked -p kitsu --test paste_latency -- --nocapture
//! ```
//!
//! `--nocapture` is what surfaces the numbers; without it the test still runs
//! and still gates the paste's correctness, it just prints nowhere.

#![cfg(unix)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use serial_test::serial;
use tempfile::TempDir;

/// The window size the benchmark runs under, set with `TIOCSWINSZ` before the
/// spawn so the composer's windowing arithmetic is this geometry and not the
/// developer's terminal. The same 24x80 `tests/tui.rs` pins.
const ROWS: u16 = 24;
const COLS: u16 = 80;

/// N2's payload size.
const PAYLOAD_LEN: usize = 10 * 1024;

/// N2's bound. Reported against, not asserted, unless `KITSU_N2_STRICT` is
/// set in the environment.
const N2_BUDGET: Duration = Duration::from_millis(100);

/// The payload's last bytes, and the benchmark's end point. Long enough to be
/// impossible by accident, short enough to survive the composer's windowing
/// of the row (24 columns of a row that has ~70 to spare after the prompt).
const SENTINEL: &str = "KITSU-N2-PASTE-END-9x7q";

/// `ESC[?2004h`, the bracketed-paste enable `RawStdinGuard::engage` writes
/// straight after its `tcsetattr`. Seeing it is how the benchmark knows the
/// raw reader is engaged, which is the same marker `tests/tui.rs` adopted for
/// the same race: input sent before it can reach a terminal that is still
/// canonical, where the tty would echo the payload back and the sentinel
/// would be found without the child having done anything.
const RAW_MODE_MARKER: &str = "\x1b[?2004h";

/// How long the stream must be quiet, after the raw-mode marker, before the
/// clock starts. Startup prints a banner through direct writers; a paste
/// timed into the middle of that would be timing the banner.
const SETTLE: Duration = Duration::from_millis(400);

/// How long the whole measurement may take before the harness gives up and
/// fails with the transcript. Twenty seconds is `tests/tui.rs`'s deadline: it
/// bounds a hang, and it is two orders of magnitude above the thing being
/// measured, so it can never turn a slow machine into a failure.
const DEADLINE: Duration = Duration::from_secs(20);

/// Write `~/.kitsu/config.toml` with one unreachable gateway.
///
/// No request is ever made here — the benchmark never submits — so the config
/// exists only so startup finds a gateway and a model name and reaches the
/// prompt.
fn write_config(home: &Path) {
    let dir = home.join(".kitsu");
    fs::create_dir_all(&dir).expect("create ~/.kitsu");
    let config = r#"default_gateway = "example"

[gateways.example]
flavor       = "openai"
base_url     = "https://gateway.invalid"
chat_path    = "/v1/chat/completions"
models_path  = ""
model        = "example-model-v1"
auth         = { method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN", expires_env = "ANTHROPIC_TOKEN_EXPIRES", refresh_hint = "run `refresh-token` to refresh the gateway JWT" }
"#;
    fs::write(dir.join("config.toml"), config).expect("write config.toml");
}

/// Exactly `PAYLOAD_LEN` bytes of plausible pasted text ending in
/// [`SENTINEL`].
///
/// Tabs and newlines are in it on purpose: they are what makes the payload a
/// *paste* rather than a long line. Inside `ESC[200~`…`ESC[201~` the editor
/// must keep both as content, and the composer renders them as `⏎`/`⇥`
/// placeholders, so a draft that was split at a newline could not produce a
/// row ending in the sentinel. All bytes are ASCII, so the truncation below
/// cannot land mid-codepoint.
fn paste_payload() -> String {
    let body_len = PAYLOAD_LEN - SENTINEL.len();
    let mut payload = String::with_capacity(PAYLOAD_LEN);
    let mut line = 0usize;
    while payload.len() < body_len {
        payload.push_str(&format!("line {line:04}\tcolumn two\tcolumn three\n"));
        line += 1;
    }
    payload.truncate(body_len);
    payload.push_str(SENTINEL);
    assert_eq!(payload.len(), PAYLOAD_LEN);
    assert!(payload.is_ascii());
    assert!(payload.contains('\n') && payload.contains('\t'));
    payload
}

/// Set the window size on a pty master.
fn set_window_size(master: libc::c_int, rows: u16, cols: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `master` is a live pty master and `size` is a valid winsize.
    assert_eq!(
        unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &size) },
        0,
        "set the pty window size to {rows}x{cols}"
    );
}

/// An escape-stripping tail of the child's output.
///
/// The sentinel is looked for in what a *user would see*, not in the raw
/// bytes, because a renderer is free to put styling anywhere in the row and a
/// literal search would then silently never match — which, in a benchmark
/// whose end point is that search, would be reported as a timeout rather than
/// as a wrong search. Stripping also makes the two renderers comparable:
/// rustyline and the inline composer do not emit the same escapes.
///
/// Only a short tail is kept. The point is that the per-poll cost is constant:
/// this runs inside the timed loop, and a search over a growing buffer would
/// put the harness's own quadratic into the number being reported.
#[derive(Default)]
struct VisibleTail {
    /// Printable bytes seen, truncated from the front to [`Self::KEEP`].
    tail: Vec<u8>,
    /// Mid-escape-sequence: dropping bytes until the sequence ends.
    in_escape: bool,
}

impl VisibleTail {
    /// Enough for the sentinel plus slack, and far less than a row.
    const KEEP: usize = 512;

    /// Feed raw bytes; printable ones join the tail, escape sequences are
    /// dropped.
    ///
    /// A deliberately crude CSI/OSC skipper: `ESC` opens, and a byte in
    /// `0x40..=0x7e` (CSI final) or `0x07`/`ESC \` (OSC terminator) closes.
    /// It can only ever *drop* text, never invent it, so the worst a
    /// mis-parse can do is make the search fail loudly at the deadline rather
    /// than report a wrong time.
    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.in_escape {
                // `[`, `?`, `;`, digits and the other parameter/intermediate
                // bytes stay inside the sequence; the first final byte ends
                // it. `0x07` ends an OSC.
                if byte == 0x07 || (0x40..=0x7e).contains(&byte) && byte != b'[' {
                    self.in_escape = false;
                }
                continue;
            }
            match byte {
                0x1b => self.in_escape = true,
                // Keep printable ASCII and anything non-ASCII (the `…`/`⏎`
                // placeholders are multi-byte UTF-8 and must not break the
                // run of bytes the sentinel sits in).
                b' '..=b'~' => self.tail.push(byte),
                b if b >= 0x80 => self.tail.push(b),
                // `\r`, `\n`, `\t` and the rest are row boundaries: drop
                // them. The sentinel never contains one.
                _ => {}
            }
        }
        if self.tail.len() > Self::KEEP {
            let cut = self.tail.len() - Self::KEEP;
            self.tail.drain(..cut);
        }
    }

    /// Whether `needle` is in the visible tail.
    fn contains(&self, needle: &str) -> bool {
        let needle = needle.as_bytes();
        self.tail.len() >= needle.len() && self.tail.windows(needle.len()).any(|w| w == needle)
    }

    /// Forget everything seen so far, so the search that follows is about the
    /// bytes that follow.
    fn clear(&mut self) {
        self.tail.clear();
    }
}

/// One renderer's measurement.
struct Measurement {
    /// How the child was launched: `None` is classic, `Some("inline")` the
    /// inline viewport.
    mode: Option<&'static str>,
    /// `t1 - t0`: the whole round trip, and the number N2 is about.
    total: Duration,
    /// When the last byte of the burst was accepted by the pty, measured from
    /// `t0`. The remainder of `total` is the child.
    write_done: Duration,
    /// How many `write` calls the burst took, i.e. how many times the pty
    /// buffer filled. Reported because it is the shape of the delivery, and a
    /// paste delivered in one `write` and one in forty are not the same
    /// measurement.
    writes: usize,
    /// How many bytes the child wrote back between `t0` and `t1`: the cost of
    /// repainting a 10 KiB draft, in bytes on the wire.
    response_bytes: usize,
}

impl Measurement {
    /// The one line the row's deliverable is made of.
    fn report(&self) -> String {
        let renderer = match self.mode {
            None => "classic",
            Some(mode) => mode,
        };
        format!(
            "N2 [{renderer}] 10 KiB bracketed paste: total {:.1} ms \
             (burst accepted by the pty at {:.1} ms, in {} writes; \
             {} bytes repainted back) — budget {} ms: {}",
            self.total.as_secs_f64() * 1000.0,
            self.write_done.as_secs_f64() * 1000.0,
            self.writes,
            self.response_bytes,
            N2_BUDGET.as_millis(),
            if self.total < N2_BUDGET {
                "HOLDS"
            } else {
                "OVER"
            },
        )
    }
}

/// Spawn a styled interactive chat on a 24x80 pty, wait for a settled idle
/// prompt, then time one 10 KiB bracketed paste into it.
///
/// `mode` is the `--tui <MODE>` value; `None` passes no flag, which is classic
/// mode. `KITSU_TUI` is removed from the child's environment for the reason
/// `tests/tui.rs` removes it: a stray value in the runner's shell must not be
/// able to make the classic measurement a second inline one.
fn measure_paste(home: &Path, mode: Option<&'static str>) -> Measurement {
    use std::os::fd::FromRawFd as _;
    use std::process::Stdio;

    let payload = paste_payload();
    let mut burst = Vec::with_capacity(PAYLOAD_LEN + 16);
    burst.extend_from_slice(b"\x1b[200~");
    burst.extend_from_slice(payload.as_bytes());
    burst.extend_from_slice(b"\x1b[201~");

    let mut master = -1;
    let mut slave = -1;
    // SAFETY: both out-pointers are valid; null termios/winsize request the
    // platform defaults, which `TIOCSWINSZ` then overrides.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0,
        "allocate the N2 benchmark's pty"
    );
    set_window_size(master, ROWS, COLS);

    // Non-blocking, so the loop below can interleave writing the burst with
    // draining the repaints instead of deadlocking against the child.
    // SAFETY: `master` is a live descriptor owned here.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );

    // SAFETY: each duplicated slave descriptor transfers to exactly one Stdio
    // owner, and the original transfers as stderr, so each is closed once.
    let child_stdin = unsafe { libc::dup(slave) };
    let child_stdout = unsafe { libc::dup(slave) };
    assert!(child_stdin >= 0 && child_stdout >= 0);

    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
    command
        .env("HOME", home)
        .env("ANTHROPIC_AUTH_TOKEN", "test-token")
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .env_remove("KITSU_GATEWAY")
        .env_remove("KITSU_MODEL")
        .env_remove("KITSU_TUI")
        .current_dir(home)
        // SAFETY: all three live descriptors transfer exactly once.
        .stdin(unsafe { Stdio::from_raw_fd(child_stdin) })
        .stdout(unsafe { Stdio::from_raw_fd(child_stdout) })
        .stderr(unsafe { Stdio::from_raw_fd(slave) })
        .args(["--gateway", "example", "chat", "--no-session"]);
    if let Some(mode) = mode {
        command.args(["--tui", mode]);
    }

    let mut child = command.spawn().expect("spawn the N2 benchmark's chat");
    // SAFETY: ownership of the remaining master descriptor transfers once.
    let mut terminal = unsafe { fs::File::from_raw_fd(master) };

    let give_up = Instant::now() + DEADLINE;
    let mut transcript: Vec<u8> = Vec::new();
    let mut visible = VisibleTail::default();
    let mut buffer = [0_u8; 8192];

    // ---- Phase 1: reach a settled idle prompt. -------------------------
    //
    // The raw-mode marker first, then `SETTLE` of quiet. Quiet rather than a
    // fixed sleep from the marker because the banner is written by direct
    // writers that are not synchronised with the reader engaging.
    let mut engaged = false;
    let mut quiet_since = Instant::now();
    loop {
        let read = match terminal.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => 0,
            Err(error) if error.raw_os_error() == Some(libc::EIO) => 0,
            Err(error) => panic!("read the N2 pty: {error}"),
        };
        if read > 0 {
            transcript.extend_from_slice(&buffer[..read]);
            visible.feed(&buffer[..read]);
            quiet_since = Instant::now();
            if !engaged && String::from_utf8_lossy(&transcript).contains(RAW_MODE_MARKER) {
                engaged = true;
            }
        }
        if engaged && quiet_since.elapsed() >= SETTLE {
            break;
        }
        if let Some(status) = child.try_wait().expect("poll the N2 child") {
            panic!(
                "the child exited {status:?} before the prompt settled; transcript:\n{}",
                String::from_utf8_lossy(&transcript)
            );
        }
        if Instant::now() >= give_up {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "no settled idle prompt within {DEADLINE:?} (raw-mode marker \
                 seen: {engaged}); transcript:\n{}",
                String::from_utf8_lossy(&transcript)
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // The non-vacuity check the whole measurement rests on: if the sentinel
    // were already on the wire, `contains` would be true at `t0` and the
    // benchmark would report a latency of nothing at all.
    assert!(
        !visible.contains(SENTINEL),
        "the sentinel {SENTINEL:?} is on the wire BEFORE the paste, so finding \
         it afterwards would prove nothing; transcript:\n{}",
        String::from_utf8_lossy(&transcript)
    );
    // ...and the search after `t0` is about the bytes after `t0`.
    visible.clear();
    let prelude = transcript.len();

    // ---- Phase 2: the measurement. -------------------------------------
    //
    // `t0` is here: the next statement is the first `write` of the burst.
    // The loop never sleeps, so the harness contributes a spin and not a
    // poll interval to the number.
    let mut written = 0usize;
    let mut writes = 0usize;
    let mut write_done: Option<Duration> = None;
    let t0 = Instant::now();
    let total = loop {
        if written < burst.len() {
            match terminal.write(&burst[written..]) {
                Ok(0) => {}
                Ok(count) => {
                    written += count;
                    writes += 1;
                    if written == burst.len() {
                        write_done = Some(t0.elapsed());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("write the paste burst: {error}"),
            }
        }

        match terminal.read(&mut buffer) {
            Ok(0) => {}
            Ok(read) => {
                transcript.extend_from_slice(&buffer[..read]);
                visible.feed(&buffer[..read]);
                if visible.contains(SENTINEL) {
                    break t0.elapsed();
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => panic!("read the N2 pty: {error}"),
        }

        if let Some(status) = child.try_wait().expect("poll the N2 child") {
            panic!(
                "the child exited {status:?} mid-paste after {written} of {} \
                 burst bytes; transcript:\n{}",
                burst.len(),
                String::from_utf8_lossy(&transcript)
            );
        }
        if Instant::now() >= give_up {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the composer never showed the pasted draft's tail within \
                 {DEADLINE:?} ({written} of {} burst bytes accepted by the \
                 pty); everything the child wrote since the paste:\n{}",
                burst.len(),
                String::from_utf8_lossy(&transcript[prelude..])
            );
        }
    };
    let response_bytes = transcript.len() - prelude;

    // The draft is deliberately left unsubmitted (see the module docs), so
    // there is no clean way to ask this child to exit that would not be a
    // second, unmeasured interaction. It is killed instead: the measurement
    // is already taken, and nothing here asserts on the shutdown path —
    // `tests/tui.rs` owns that.
    let _ = child.kill();
    let _ = child.wait();

    Measurement {
        mode,
        total,
        write_done: write_done.expect("the whole burst was accepted before the sentinel"),
        writes,
        response_bytes,
    }
}

/// Time the paste, print the number, and gate only on the paste's
/// correctness — unless `KITSU_N2_STRICT` asks for the budget to be enforced.
fn benchmark(mode: Option<&'static str>) {
    let home = TempDir::new().expect("temp home");
    write_config(home.path());
    let measurement = measure_paste(home.path(), mode);
    let report = measurement.report();
    // Both, so the number is findable whether the reader has `--nocapture` on
    // stdout or is reading a captured-failure dump.
    println!("{report}");
    eprintln!("{report}");

    if std::env::var_os("KITSU_N2_STRICT").is_some() {
        assert!(measurement.total < N2_BUDGET, "KITSU_N2_STRICT: {report}");
    }
}

/// N2 in the inline viewport: the composer this requirement is named after.
#[test]
#[serial]
fn n2_ten_kib_paste_into_the_inline_composer() {
    benchmark(Some("inline"));
}

/// N2 in classic mode, for comparison.
///
/// Not decoration: N2's parenthesis is "the rustyline `ExternalPrinter`
/// lesson", and classic mode *is* the rustyline path. A regression that only
/// one renderer has is a different finding from one both have, and only two
/// numbers can tell them apart.
#[test]
#[serial]
fn n2_ten_kib_paste_into_the_classic_prompt() {
    benchmark(None);
}

/// The payload and the sentinel, checked without a child process.
///
/// Small, but it is the thing the measurement's end point is built on: a
/// sentinel that was not at the very end of the payload would make the
/// composer's tail-window search for it fail at the deadline, and a payload
/// that was not 10 KiB would make the benchmark measure a different
/// requirement.
#[test]
fn the_payload_is_ten_kib_and_ends_in_the_sentinel() {
    let payload = paste_payload();
    assert_eq!(payload.len(), PAYLOAD_LEN);
    assert!(payload.ends_with(SENTINEL));
    assert_eq!(
        payload.matches(SENTINEL).count(),
        1,
        "the sentinel must occur once, at the end, or the end point is ambiguous"
    );
    assert!(
        !payload.contains('\x1b'),
        "the payload must contain no ESC: one would close the bracketed paste"
    );
}

/// The escape stripper, on bytes a renderer really emits.
///
/// [`VisibleTail`] is the only thing standing between "the composer painted
/// the draft" and "the benchmark timed out", so it gets its own test rather
/// than being trusted inside the timed loop.
#[test]
fn the_visible_tail_finds_text_through_the_escapes_a_renderer_emits() {
    let mut tail = VisibleTail::default();
    // A row painted the way the inline viewport paints one: absolute cursor
    // address, erase to end of line, SGR reset, then the windowed draft.
    tail.feed(b"\x1b[24;1H\x1b[K\x1b[0mkitsu> \xe2\x80\xa6two\xe2\x8f\x8e");
    tail.feed(SENTINEL.as_bytes());
    assert!(tail.contains(SENTINEL));
    assert!(tail.contains("kitsu> "));
    // Nothing of the escapes survives.
    assert!(!tail.contains("24;1H"));
    assert!(!tail.contains("[K"));

    // Split across reads, which is how it arrives off a pty.
    let mut split = VisibleTail::default();
    let (head, rest) = SENTINEL.split_at(7);
    split.feed(b"\x1b[0m");
    split.feed(head.as_bytes());
    assert!(!split.contains(SENTINEL));
    split.feed(rest.as_bytes());
    assert!(split.contains(SENTINEL));

    // `clear` really does make the next search about the next bytes.
    split.clear();
    assert!(!split.contains(SENTINEL));

    // The tail is bounded, and bounding it does not lose the newest bytes.
    let mut bounded = VisibleTail::default();
    bounded.feed(&vec![b'x'; VisibleTail::KEEP * 3]);
    bounded.feed(SENTINEL.as_bytes());
    assert!(bounded.contains(SENTINEL));
    assert!(bounded.tail.len() <= VisibleTail::KEEP);
}
