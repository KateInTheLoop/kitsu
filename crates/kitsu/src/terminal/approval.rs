//! Terminal permission prompts and stdin ownership.

use kitsu_tools::{
    CommandApprovalRequest, CommandDecision, CommandPrompter, Decision, PermissionRequest, Prompter,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::presentation::terminal_inline;
use crate::terminal::typeahead::FOREGROUND_CANCELLED;
#[cfg(unix)]
use crate::tui_bridge;

// Count pending and active approval readers. A claimant increments before it
// waits for the stdin mutex so type-ahead cannot repeatedly reacquire the lock
// and starve the authority prompt. A count also avoids a false inactive window
// when more than one prompt is waiting.
pub(crate) static APPROVAL_INPUT_ACTIVE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static APPROVAL_INPUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Card **T3-S7b**: the in-loop approval read's hand-off.
///
/// When a pinned pane is published the prompter does **not** claim stdin
/// (`APPROVAL_INPUT_ACTIVE` stays at zero, so the collector never drops its
/// `RawStdinGuard` and the kernel never echoes a thing). It parks the sending
/// half of a one-shot channel here and blocks on the receiving half; the live
/// collector takes the sender on its next tick, reads one key with raw mode
/// still engaged, and sends it back. `None` is "the read was closed without a
/// decision" -- EOF, a dead reader, or a cancelled turn.
///
/// Process-global for `MODAL_PANE`'s reason: the prompter runs on the tool
/// executor and the reader is the type-ahead collector's thread.
pub(crate) static APPROVAL_KEY_REQUEST: std::sync::Mutex<
    Option<std::sync::mpsc::Sender<Option<char>>>,
> = std::sync::Mutex::new(None);
/// Whether a live raw-mode collector is running and able to serve
/// [`APPROVAL_KEY_REQUEST`]. Published by `run_live_typeahead` while it holds
/// its `RawStdinGuard`, and cleared when it lets the guard go for any reason
/// (an approval claim, a job-control signal, the end of the turn). A prompter
/// only bypasses the claim protocol while this is true, and a waiting
/// prompter gives up the moment it goes false -- which is what makes "the
/// collector is gone" an interrupt instead of a hang.
pub(crate) static APPROVAL_KEY_READER: AtomicBool = AtomicBool::new(false);
/// Serializes in-loop approval reads with each other.
///
/// Deliberately **not** `APPROVAL_INPUT_LOCK`: that mutex is `try_lock`ed by
/// the collector on every 50 ms tick, and `std::sync::Mutex` is not fair, so
/// a thread that blocks on it without first publishing a claim is starved
/// indefinitely -- the very failure `ApprovalInputGuard::enter`'s comment
/// describes, measured here as a 20 s approval that never painted. The in-loop
/// path publishes no claim (publishing one is what makes the collector hand
/// the terminal back, which is the echo this card removes), so it needs a
/// mutex the collector never touches.
pub(crate) static APPROVAL_PANE_READ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Prints a permission prompt to stderr and reads the user's choice from stdin.
/// Used only in interactive `Mode::Ask`; the prompt fires mid-turn while
/// rustyline is idle. Bracketed paste is disabled around the read so a stray
/// paste can't inject `ESC[200~` markers. Exactly one line is considered:
/// Invalid input denies. EOF or an input error interrupts the current run
/// without letting a later pasted line become an authorization decision.
pub(crate) struct CliPrompter;

pub(crate) struct CliCommandPrompter;

pub(crate) struct ApprovalInputGuard {
    pub(crate) _owner: std::sync::MutexGuard<'static, ()>,
}

impl ApprovalInputGuard {
    pub(crate) fn enter() -> Self {
        // Publish the claim before waiting for the mutex. Otherwise the
        // type-ahead loop can observe `false`, release its short poll lock, and
        // reacquire it indefinitely while this prompt is already waiting.
        APPROVAL_INPUT_ACTIVE.fetch_add(1, Ordering::AcqRel);
        let owner = APPROVAL_INPUT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Take the live type-ahead draft row off the terminal before the
        // approval prompt prints; the collector re-shows it (draft intact)
        // once the approval releases stdin.
        kitsu_render::live::live_input_end();
        Self { _owner: owner }
    }
}

impl Drop for ApprovalInputGuard {
    fn drop(&mut self) {
        let claims = APPROVAL_INPUT_ACTIVE.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(claims > 0, "approval-input claim underflow");
    }
}

#[cfg(unix)]
pub(crate) fn read_approval_line() -> Option<String> {
    loop {
        if FOREGROUND_CANCELLED.load(Ordering::Acquire) {
            return None;
        }
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `descriptor` is one initialized pollfd and stdin remains
        // process-owned for the duration of the call.
        let ready = unsafe { libc::poll(&mut descriptor, 1, 50) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return None;
        }
        if ready == 0 || descriptor.revents & (libc::POLLIN | libc::POLLHUP) == 0 {
            continue;
        }
        let mut line = String::new();
        return match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        };
    }
}

#[cfg(not(unix))]
pub(crate) fn read_approval_line() -> Option<String> {
    if FOREGROUND_CANCELLED.load(Ordering::Acquire) {
        return None;
    }
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

/// Requirement **R7**, the paint half: put an approval prompt on the pinned
/// bottom rows when a TUI pane is installed, and say whether it went there.
///
/// A thin, portable wrapper over [`tui_bridge::paint_approval_modal`]: the
/// bridge is `#![cfg(unix)]`, and on any other platform there is no pane to
/// paint on, so this answers `false` and the caller prints the classic stderr
/// prompt. **Classic mode answers `false` too** -- `tui_bridge::install`
/// returns `None` for `TuiMode::Classic` before emitting a byte, so no pane is
/// ever published and classic's bytes are untouched.
pub(crate) fn approval_modal_paint(
    header: &str,
    details: &[String],
    options: &str,
    cue: Option<&str>,
) -> bool {
    #[cfg(unix)]
    {
        tui_bridge::paint_approval_modal(header, details, options, cue)
    }
    #[cfg(not(unix))]
    {
        let _ = (header, details, options, cue);
        false
    }
}

/// Take the modal down with no receipt: the read was interrupted, so there is
/// no decision for a receipt to record. Inert when nothing was painted.
pub(crate) fn approval_modal_dismiss(header: Option<&str>) {
    if header.is_none() {
        return;
    }
    #[cfg(unix)]
    tui_bridge::clear_approval_modal();
}

/// Requirement **R7**, the receipt half: once the decision has been read, take
/// the modal down and leave the `◉ permission —` header in the transcript.
///
/// `header` is `Some` exactly when [`approval_modal_paint`] painted, so this
/// is inert in classic mode and in plain mode -- in classic the prompt *is*
/// the transcript line (it is printed into the scroll region and stays there),
/// which is why only the pane renderer has a receipt to commit. The receipt is
/// the modal's own header string, emitted through the renderer's surface seam,
/// so its sanitize is classic's prompt line byte for byte.
pub(crate) fn approval_modal_commit(header: Option<&str>) {
    let Some(header) = header else {
        return;
    };
    // The receipt first, the teardown second. The receipt is a transcript
    // line -- text plus CRLF, no addressing at all -- so it lands wherever the
    // cursor is parked, and that is immediately after the newline the kernel
    // echoed with the answer. Clearing the modal first would repark the cursor
    // with a cursor address instead, putting the receipt on a line the byte
    // stream never terminated.
    kitsu_render::surface::active_surface()
        .transcript_line(kitsu_render::surface::Fd::Stderr, header);
    #[cfg(unix)]
    tui_bridge::clear_approval_modal();
}

/// Card **T3-S7b**: the row the user types at, painted at the bottom of the
/// modal instead of written into the scroll region. The classic prompt's cue
/// text, so the two spellings cannot drift.
pub(crate) const APPROVAL_CUE: &str = "> ";

/// Whether this approval can be read **in-loop**: a pinned pane is published
/// *and* a live raw-mode collector is running to read the key.
///
/// Both halves are required. The pane is what gives the prompt somewhere to
/// live; the live reader is what makes the key readable without the claim
/// protocol. Classic mode publishes no pane, and a session whose collector is
/// the canonical line-at-a-time loop (piped stdin, `--plain`, assistive
/// input, a terminal that refused raw mode) has no in-loop reader -- both
/// answer `false` here and keep today's claim/handoff path.
pub(crate) fn approval_read_in_loop_available() -> bool {
    #[cfg(unix)]
    {
        !kitsu_render::plain_mode()
            && APPROVAL_KEY_READER.load(Ordering::Acquire)
            && tui_bridge::approval_pane_published()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Card **T3-S7b**, the prompter's half, step one: park a request for the
/// live collector and keep the receiving end.
///
/// Published **before** the modal is painted, which is what closes the only
/// window where the collector could have read the user's key as draft text:
/// the collector re-checks for a parked request after its poll and before its
/// read, so by the time a key can exist -- the user has not seen a prompt yet
/// -- the request is already parked and the key is served, not typed.
pub(crate) fn publish_approval_key_request() -> std::sync::mpsc::Receiver<Option<char>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    *APPROVAL_KEY_REQUEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
    receiver
}

/// Take back a request that will not be read after all (no pane painted).
pub(crate) fn withdraw_approval_key_request() {
    *APPROVAL_KEY_REQUEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Card **T3-S7b**, the prompter's half, step two: block until the collector
/// replies with one key.
///
/// The answer comes back as **the string a line read would have produced**,
/// for [`parse_permission_answer`] -- the one parser -- to resolve. `None` is
/// "closed without a decision", and it is returned promptly rather than
/// waited out when the turn is cancelled or the collector stops offering to
/// read, which is what makes this impossible to hang on.
pub(crate) fn await_approval_key(
    receiver: &std::sync::mpsc::Receiver<Option<char>>,
) -> Option<String> {
    let answer = loop {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(key) => break key.map(|key| key.to_string()),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if FOREGROUND_CANCELLED.load(Ordering::Acquire)
                    || !APPROVAL_KEY_READER.load(Ordering::Acquire)
                {
                    break None;
                }
            }
        }
    };
    // Withdraw an unserved request. Only one prompter is ever in here at a
    // time -- they serialize on `APPROVAL_PANE_READ_LOCK` -- so this can only
    // ever clear our own.
    withdraw_approval_key_request();
    answer
}

/// Printed when `[u]` turns command auto mode on.
pub(crate) const AUTO_MODE_ON_NOTICE: &str =
    "~ auto mode on: remaining commands run without asking \
     (containment still applies; /auto off to stop)";

/// `u`/`auto` at a command prompt: auto mode for the rest of the session.
pub(crate) fn is_auto_mode_answer(answer: &str) -> bool {
    let answer = answer
        .trim()
        .trim_start_matches("\u{1b}[200~")
        .trim_end_matches("\u{1b}[201~")
        .trim()
        .to_ascii_lowercase();
    matches!(answer.as_str(), "u" | "auto")
}

pub(crate) fn parse_permission_answer(answer: &str) -> Option<Decision> {
    let answer = answer
        .trim()
        .trim_start_matches("\u{1b}[200~")
        .trim_end_matches("\u{1b}[201~")
        .trim()
        .to_ascii_lowercase();
    match answer.as_str() {
        "y" | "yes" => Some(Decision::AllowOnce),
        "a" | "always" => Some(Decision::AllowAlways),
        "n" | "no" => Some(Decision::Deny),
        _ => None,
    }
}

impl Prompter for CliPrompter {
    fn ask(&self, req: &PermissionRequest) -> Decision {
        use std::io::Write;
        // The approval moment carries visual weight: one shared header
        // design for file and command prompts (see CliCommandPrompter).
        // Composed once, for whichever path takes this prompt: classic prints
        // these strings and the modal paints them, so they cannot drift.
        let styled = (!kitsu_render::plain_mode()).then(|| {
            (
                kitsu_render::approval_header(&format!(
                    "kitsu wants to {} {}",
                    req.action.verb(),
                    terminal_inline(&req.path)
                )),
                kitsu_render::approval_options(&format!(
                    "always ({})",
                    terminal_inline(&req.grant_dir)
                )),
            )
        });
        // Card T3-S7b: the in-loop path, taken only where a pane is published
        // *and* a live reader can read the key. It claims nothing: the
        // collector keeps its `RawStdinGuard`, so the kernel echoes neither
        // the cue nor the answer and the transcript stays clean.
        let mut modal = None;
        let mut pending = None;
        let mut reader_owner = None;
        if let (true, Some((header, options))) =
            (approval_read_in_loop_available(), styled.as_ref())
        {
            // Serialize with every other in-loop approval read, on a mutex
            // the collector never touches (see
            // `APPROVAL_PANE_READ_LOCK`): holding it across the blocking
            // read therefore cannot stall the thread that performs it.
            let owner = APPROVAL_PANE_READ_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // The draft row comes off the pane before the modal goes on it:
            // `ApprovalInputGuard` does this for the other path, and its
            // redraw would otherwise wipe the modal.
            kitsu_render::live::live_input_end();
            // Parked before the paint, so no key can exist before the
            // collector knows to serve it rather than type it.
            let receiver = publish_approval_key_request();
            if approval_modal_paint(header, &[], options, Some(APPROVAL_CUE)) {
                modal = Some(header.clone());
                pending = Some(receiver);
                reader_owner = Some(owner);
            } else {
                // No pane after all (a terminal below F19's bound): take the
                // request back and let the claim path below run.
                withdraw_approval_key_request();
                drop(owner);
            }
        }
        let in_loop = modal.is_some();
        let _input_guard = (!in_loop).then(ApprovalInputGuard::enter);
        if !in_loop {
            // Disable bracketed paste (ESC[?2004l) for the raw read; rustyline
            // re-enables it on the next prompt. Literal input never enables it.
            // The in-loop path leaves paste mode alone: the collector's guard
            // owns it, and one key is read from it rather than a line.
            if !kitsu_render::assistive_input_mode() {
                eprint!("\x1b[?2004l");
            }
            match styled.as_ref() {
                None => eprint!(
                    "\npermission> kitsu wants to {} {}\n  [y] allow once   [a] always ({})   [n] deny\n> ",
                    req.action.verb(),
                    terminal_inline(&req.path),
                    terminal_inline(&req.grant_dir)
                ),
                Some((header, options)) => {
                    // Requirement R7: in a TUI mode the prompt is a modal on
                    // the pinned bottom rows. Reached here only when no live
                    // reader is available (assistive or piped input), so the
                    // `> ` cue and the echoed answer stay in the scroll
                    // region -- the canonical read is still the kernel's.
                    if approval_modal_paint(header, &[], options, None) {
                        modal = Some(header.clone());
                        eprint!("> ");
                    } else {
                        eprint!("\n{header}\n{options}\n> ");
                    }
                }
            }
            let _ = std::io::stderr().flush();
        }
        let answer = match pending.as_ref() {
            Some(receiver) => await_approval_key(receiver),
            None => read_approval_line(),
        };
        let Some(answer) = answer else {
            approval_modal_dismiss(modal.as_deref());
            return Decision::Interrupt;
        };
        approval_modal_commit(modal.as_deref());
        // The read is over: let the next approval in.
        drop(reader_owner);
        parse_permission_answer(&answer).unwrap_or_else(|| {
            eprintln!("invalid permission response; denied (enter y, a/always, or n next time)");
            Decision::Deny
        })
    }
}

impl CommandPrompter for CliCommandPrompter {
    fn ask(&self, req: &CommandApprovalRequest) -> CommandDecision {
        use std::io::Write;
        let resolution = match req.executable.resolution {
            kitsu_tools::CommandExecutableResolution::AmbientPath => "ambient PATH lookup",
            kitsu_tools::CommandExecutableResolution::InvocationPath => "invocation path",
        };
        let observed = req
            .executable
            .observed_path
            .as_deref()
            .map(terminal_inline)
            .unwrap_or_else(|| "not observed".to_string());
        // Same weighted header design as the filesystem prompt above, with
        // the command's provenance details dimmed beneath it. Composed once,
        // for whichever path takes this prompt.
        let styled = (!kitsu_render::plain_mode()).then(|| {
            let header = kitsu_render::approval_header(&format!(
                "run {}",
                terminal_inline(&req.display_argv())
            ));
            // R7's "command approvals keep their provenance rows": the same
            // four rows, in the same order, whether they go to stderr or onto
            // the pinned rows.
            let details = vec![
                kitsu_render::approval_detail(&format!("cwd: {}", terminal_inline(&req.cwd))),
                kitsu_render::approval_detail(&format!(
                    "executable: {} ({resolution}; observed: {observed}; bytes not pinned)",
                    terminal_inline(&req.executable.invocation)
                )),
                kitsu_render::approval_detail(&format!("timeout: {} ms", req.timeout_ms)),
                kitsu_render::approval_detail(&format!(
                    "authority: {}",
                    terminal_inline(req.authority_notice())
                )),
            ];
            let options = kitsu_render::command_approval_options(
                kitsu_render::COMMAND_ALWAYS_LABEL,
                kitsu_render::live::terminal_columns(),
            );
            (header, details, options)
        });
        // Card T3-S7b, exactly as the filesystem prompt above: where a pane
        // is published and a live reader can read the key, nothing is claimed
        // and nothing is echoed.
        let mut modal = None;
        let mut pending = None;
        let mut reader_owner = None;
        if let (true, Some((header, details, options))) =
            (approval_read_in_loop_available(), styled.as_ref())
        {
            let owner = APPROVAL_PANE_READ_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            kitsu_render::live::live_input_end();
            let receiver = publish_approval_key_request();
            if approval_modal_paint(header, details, options, Some(APPROVAL_CUE)) {
                modal = Some(header.clone());
                pending = Some(receiver);
                reader_owner = Some(owner);
            } else {
                withdraw_approval_key_request();
                drop(owner);
            }
        }
        let in_loop = modal.is_some();
        let _input_guard = (!in_loop).then(ApprovalInputGuard::enter);
        if !in_loop {
            if !kitsu_render::assistive_input_mode() {
                eprint!("\x1b[?2004l");
            }
            match styled.as_ref() {
                None => eprint!(
                    "\ncommand permission> run {}\n  cwd: {}\n  executable: {} ({resolution}; observed: {observed}; bytes not pinned)\n  timeout: {} ms\n  authority: {}\n  [y] allow once   [a] allow this exact argv/cwd for session   [u] {}   [n] deny\n> ",
                    terminal_inline(&req.display_argv()),
                    terminal_inline(&req.cwd),
                    terminal_inline(&req.executable.invocation),
                    req.timeout_ms,
                    terminal_inline(req.authority_notice()),
                    kitsu_render::AUTO_MODE_OPTION,
                ),
                Some((header, details, options)) => {
                    if approval_modal_paint(header, details, options, None) {
                        modal = Some(header.clone());
                        eprint!("> ");
                    } else {
                        let details = details.join("\n");
                        eprint!("\n{header}\n{details}\n{options}\n> ");
                    }
                }
            }
            let _ = std::io::stderr().flush();
        }
        let answer = match pending.as_ref() {
            Some(receiver) => await_approval_key(receiver),
            None => read_approval_line(),
        };
        let Some(answer) = answer else {
            approval_modal_dismiss(modal.as_deref());
            return CommandDecision::Interrupt;
        };
        approval_modal_commit(modal.as_deref());
        drop(reader_owner);
        // `[u]` is the command prompt's own answer; the file prompt shares the
        // parser below and must keep rejecting it.
        if is_auto_mode_answer(&answer) {
            eprintln!("{AUTO_MODE_ON_NOTICE}");
            return CommandDecision::AllowAllForSession;
        }
        // One parser, finding F45: this prompt used to re-implement
        // `parse_permission_answer`'s trim, paste-marker strip, lowercase and
        // match inline -- the same three answers under different names. The
        // duplicate is retired; only the command-side *meaning* of `a`
        // remains here, as the `Decision` -> `CommandDecision` mapping.
        match parse_permission_answer(&answer) {
            Some(Decision::AllowOnce) => CommandDecision::AllowOnce,
            Some(Decision::AllowAlways) => CommandDecision::AllowForSession,
            Some(Decision::Deny) => CommandDecision::Deny,
            Some(Decision::Interrupt) => CommandDecision::Interrupt,
            None => {
                eprintln!("invalid command permission response; denied");
                CommandDecision::Deny
            }
        }
    }
}
