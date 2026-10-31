//! Terminal type-ahead collection and active-turn controls.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::agent_commands::agent_explain_view;
use crate::agent_commands::format_agent_thread;
use crate::agent_commands::format_jobs;
use crate::agent_commands::format_jobs_view;
use crate::agent_commands::format_steering_receipt;
use crate::agent_commands::jobs_reply_receipt;
use crate::agent_commands::parse_jobs_reply;
use crate::agent_commands::terminal_agent_status_label;
use crate::agent_commands::JOBS_REPLY_USAGE;
use crate::chat::commands::execute_queue_command;
use crate::chat::commands::parse_queue_command;
use crate::chat::commands::TUI_MID_TURN_REFUSAL;
use crate::presentation::terminal_inline;
use crate::prompt_queue;
use crate::subagent;
use crate::terminal::approval::APPROVAL_INPUT_ACTIVE;
use crate::terminal::approval::APPROVAL_INPUT_LOCK;
use crate::terminal::approval::APPROVAL_KEY_READER;
use crate::terminal::approval::APPROVAL_KEY_REQUEST;
use crate::terminal::completion::typeahead_slash_hint;
#[cfg(unix)]
use crate::tui_bridge;

pub(crate) static FOREGROUND_CANCELLED: AtomicBool = AtomicBool::new(false);
pub(crate) static ACTIVE_QUIT_CANCEL: AtomicBool = AtomicBool::new(false);
pub(crate) static ASSISTIVE_INPUT_INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Outcome of a single (cancellable) turn.
pub(crate) enum TurnOutcome {
    Completed(Result<String, kitsu_core::AgentError>),
    Cancelled,
}

pub(crate) enum TypeaheadOutcome {
    Notice(String),
    SteeringError(String),
    QueueError(String),
}

#[cfg(unix)]
pub(crate) fn handle_active_jobs_command(
    registry: &Arc<subagent::AgentRegistry>,
    runtime: &tokio::runtime::Handle,
    argument: &str,
) -> Result<String, String> {
    let mut parts = argument.split_whitespace();
    match parts.next().unwrap_or("list") {
        "list" if parts.next().is_none() => Ok(format_jobs(registry)),
        "view" if parts.next().is_none() => Ok(format_jobs_view(registry)),
        "show" => {
            let id = parts
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| "usage: /jobs show <id>".to_string())?;
            if parts.next().is_some() {
                return Err("usage: /jobs show <id>".to_string());
            }
            let snapshot = registry
                .thread_snapshot(id)
                .ok_or_else(|| format!("no background job #{id}"))?;
            let mut output = format_agent_thread(&snapshot);
            if snapshot.phase != subagent::thread::AgentThreadPhase::Running {
                if let Some(result) = registry.peek_result(id) {
                    output.push_str("\n\nresult:\n");
                    output.push_str(&kitsu_render::sanitize_terminal_text(&result));
                }
                let _ = registry.acknowledge_result(id);
            }
            Ok(output)
        }
        "explain" => {
            let id = parts
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| "usage: /jobs explain <id>".to_string())?;
            if parts.next().is_some() {
                return Err("usage: /jobs explain <id>".to_string());
            }
            agent_explain_view(registry, id).ok_or_else(|| format!("no background job #{id}"))
        }
        "steer" => {
            let id = parts
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| "usage: /jobs steer <id> <correction>".to_string())?;
            let correction = parts.collect::<Vec<_>>().join(" ");
            if correction.is_empty() {
                return Err("usage: /jobs steer <id> <correction>".to_string());
            }
            let receipt = registry
                .steer(id, &correction)
                .map_err(|error| error.to_string())?;
            Ok(format_steering_receipt(&receipt))
        }
        "stop" => {
            let id = parts
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| "usage: /jobs stop <id>".to_string())?;
            if parts.next().is_some() {
                return Err("usage: /jobs stop <id>".to_string());
            }
            match runtime.block_on(registry.stop(id)) {
                subagent::AgentCancelOutcome::Cancelled(status) => {
                    Ok(format!("stopped and joined background job #{}", status.id))
                }
                subagent::AgentCancelOutcome::AlreadyTerminal(status) => Ok(format!(
                    "background job #{} is already {}",
                    status.id,
                    terminal_agent_status_label(&status.status)
                )),
                subagent::AgentCancelOutcome::NotFound => Err(format!("no background job #{id}")),
            }
        }
        "stop-all" if parts.next().is_none() => {
            let stopped = runtime.block_on(registry.cancel_all());
            Ok(format!(
                "stopped and joined {} background job(s)",
                stopped.len()
            ))
        }
        "reply" => {
            let (id, decision) =
                parse_jobs_reply(&mut parts).ok_or_else(|| JOBS_REPLY_USAGE.to_string())?;
            let outcome = registry.answer(id, decision);
            if matches!(outcome, subagent::AgentAnswerOutcome::NotFound) {
                return Err(format!("no background job #{id}"));
            }
            Ok(jobs_reply_receipt(id, outcome, decision))
        }
        _ => Err(
            "active-turn controls: /jobs [list|view|show <id>|explain <id>|steer <id> <text>|stop <id>|stop-all|reply <id> <y|a|n>]"
                .to_string(),
        ),
    }
}

#[derive(Default)]
pub(crate) struct TypeaheadCaptureResult {
    pub(crate) captured: usize,
    pub(crate) outcomes: Vec<TypeaheadOutcome>,
    pub(crate) eof: bool,
    /// Text the user was still composing on the live input row when the turn
    /// ended. Carried into the next prompt so mid-turn typing is never lost.
    pub(crate) pending_draft: String,
    /// Whether the live raw-mode editor ran (its receipts print immediately,
    /// so the post-turn bookkeeping summary is skipped).
    pub(crate) live: bool,
}

/// Routing shared by both type-ahead loops: active-turn controls (`/steer`,
/// `/jobs`, `/queue`, `/quit cancel`) execute immediately; everything else
/// queues as a future prompt. Live (raw-mode) sessions narrate outcomes as
/// they happen; canonical sessions defer them to the post-turn flush.
#[cfg(unix)]
pub(crate) struct TypeaheadDispatcher {
    pub(crate) queue: Arc<std::sync::Mutex<prompt_queue::PromptQueue>>,
    pub(crate) steering: prompt_queue::SteeringChannel,
    pub(crate) registry: Arc<subagent::AgentRegistry>,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) live: bool,
}

#[cfg(unix)]
impl TypeaheadDispatcher {
    pub(crate) fn deliver(&self, outcome: TypeaheadOutcome, result: &mut TypeaheadCaptureResult) {
        if !self.live {
            result.outcomes.push(outcome);
            return;
        }
        kitsu_render::live::with_live_row_suspended(|| match &outcome {
            TypeaheadOutcome::Notice(notice) => {
                eprintln!("~ {}", kitsu_render::sanitize_terminal_text(notice));
            }
            TypeaheadOutcome::SteeringError(error) => {
                eprintln!("steering refused: {}", terminal_inline(error));
            }
            TypeaheadOutcome::QueueError(error) => {
                eprintln!("queue refused: {}", terminal_inline(error));
            }
        });
    }

    pub(crate) fn dispatch(&self, line: &str, result: &mut TypeaheadCaptureResult) {
        if line == "/steer" || line.starts_with("/steer ") {
            let correction = line.strip_prefix("/steer").unwrap_or_default().trim();
            if correction.is_empty() {
                self.deliver(
                    TypeaheadOutcome::SteeringError(
                        "steering correction cannot be empty".to_string(),
                    ),
                    result,
                );
                return;
            }
            let Some(turn) = self.steering.active_turn() else {
                self.deliver(
                    TypeaheadOutcome::SteeringError(
                        "steering arrived after the foreground turn ended; it was not queued as a new prompt"
                            .to_string(),
                    ),
                    result,
                );
                return;
            };
            match self.steering.steer(turn, correction) {
                Ok(_) => {
                    if self.live {
                        let _ = kitsu_render::PromptSafeAsyncNotice::info(format!(
                            "steering the current turn: {correction}"
                        ))
                        .write_line(&mut std::io::stderr());
                    }
                }
                Err(error) => self.deliver(
                    TypeaheadOutcome::SteeringError(format!(
                        "{error} ({}-character correction was not converted into a future prompt)",
                        error.correction().text().chars().count()
                    )),
                    result,
                ),
            }
            return;
        }
        if line == "/jobs" || line.starts_with("/jobs ") {
            let argument = line.strip_prefix("/jobs").unwrap_or_default().trim();
            match handle_active_jobs_command(&self.registry, &self.runtime, argument) {
                Ok(message) => self.deliver(TypeaheadOutcome::Notice(message), result),
                Err(error) => self.deliver(TypeaheadOutcome::QueueError(error), result),
            }
            return;
        }
        if line == "/quit cancel" || line == "/exit cancel" {
            ACTIVE_QUIT_CANCEL.store(true, Ordering::Release);
            self.deliver(
                TypeaheadOutcome::Notice(
                    "exit cancellation accepted; stopping the foreground turn and all background agents".to_string(),
                ),
                result,
            );
            return;
        }
        // `/tui`, mid-turn: refused and narrated, and above all **not**
        // allowed to reach the `enqueue` fall-through below, which would turn
        // a renderer request into a future prompt for the model. Every
        // spelling is refused, including the bare report and the modes that
        // would be accepted between turns, because the answer the user wants
        // is a renderer now and a swap that landed silently at the turn
        // boundary would be indistinguishable from typing it again.
        if line == "/tui" || line.starts_with("/tui ") {
            self.deliver(
                TypeaheadOutcome::Notice(TUI_MID_TURN_REFUSAL.to_string()),
                result,
            );
            return;
        }
        if line == "/queue" || line.starts_with("/queue ") {
            let argument = line.strip_prefix("/queue").unwrap_or_default();
            match parse_queue_command(argument).and_then(|command| {
                execute_queue_command(&mut self.queue.lock().expect("prompt queue"), command)
            }) {
                Ok(message) => self.deliver(TypeaheadOutcome::Notice(message), result),
                Err(error) => self.deliver(TypeaheadOutcome::QueueError(error), result),
            }
            return;
        }
        match self
            .queue
            .lock()
            .expect("prompt queue")
            .enqueue(line.to_string())
        {
            Ok(status) => {
                result.captured += 1;
                if self.live {
                    // The draft row is ephemeral; a queue receipt keeps the
                    // submitted text visible in the transcript.
                    let message = if status.queued > 1 {
                        format!("queued ({} waiting): {line}", status.queued)
                    } else {
                        format!("queued: {line}")
                    };
                    let _ = kitsu_render::PromptSafeAsyncNotice::info(message)
                        .write_line(&mut std::io::stderr());
                }
            }
            Err(error) => self.deliver(
                TypeaheadOutcome::QueueError(format!(
                    "prompt queue is full (capacity {}); {}-character submitted prompt was refused and will not run later",
                    error.capacity(),
                    error.prompt().chars().count(),
                )),
                result,
            ),
        }
    }
}

/// Non-canonical stdin for the live type-ahead editor: byte-at-a-time reads
/// with no kernel echo (the editor repaints the draft itself). `ISIG` is left
/// on so Ctrl+C still cancels the foreground turn through the signal path.
#[cfg(unix)]
pub(crate) struct RawStdinGuard {
    pub(crate) original: libc::termios,
}

#[cfg(unix)]
impl RawStdinGuard {
    pub(crate) fn engage() -> Option<Self> {
        // SAFETY: tcgetattr/tcsetattr write through a valid pointer to one
        // termios struct; stdin's fd is process-owned and stays open.
        let original = unsafe {
            let mut modes: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut modes) != 0 {
                return None;
            }
            let original = modes;
            // ISIG is cleared so Ctrl+C reaches the editor as a byte: with a
            // non-empty draft it clears the draft (Claude-style), and only on
            // an empty draft does the collector raise(SIGINT) itself — the
            // exact signal the kernel would have sent, so turn cancellation
            // is unchanged. Approval handoffs restore the original termios
            // (ISIG on), so approval prompts keep kernel Ctrl+C.
            modes.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            modes.c_cc[libc::VMIN] = 1;
            modes.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &modes) != 0 {
                return None;
            }
            original
        };
        // Requirement N5: publish the snapshot for the dying-signal handler.
        // `Drop` below is the normal restore, and a signal is precisely the
        // exit path `Drop` does not run on, so the handler needs its own
        // reachable copy of the modes this guard displaced.
        tui_bridge::publish_canonical_termios(&original);
        // Bracketed paste on: a mid-turn paste arrives as one `ESC[200~`
        // delimited block instead of loose lines, so the editor can keep it
        // as a single draft. Terminals without support ignore the sequence.
        {
            use std::io::Write;
            let mut err = std::io::stderr();
            let _ = err.write_all(b"\x1b[?2004h");
            let _ = err.flush();
        }
        Some(Self { original })
    }
}

#[cfg(unix)]
impl Drop for RawStdinGuard {
    fn drop(&mut self) {
        // Bracketed paste off before handing stdin back: rustyline and the
        // approval reader manage their own paste mode expectations.
        {
            use std::io::Write;
            let mut err = std::io::stderr();
            let _ = err.write_all(b"\x1b[?2004l");
            let _ = err.flush();
        }
        // SAFETY: restores the termios snapshot captured in `engage`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
        }
        // Withdrawn after the restore, not before: a signal landing mid-drop
        // should still find the snapshot. Once the modes are back, whatever
        // claims the terminal next (rustyline, an approval prompt) owns them,
        // and a handler must not `tcsetattr` this stale copy over it.
        tui_bridge::forget_canonical_termios();
    }
}

/// Own complete lines submitted while a foreground turn is running. Rustyline
/// restores canonical terminal mode after `readline`, so `poll` only marks stdin
/// readable after Enter (or EOF); partial typing stays in the terminal buffer.
/// Approval prompts temporarily claim stdin through `APPROVAL_INPUT_ACTIVE`.
#[cfg(unix)]
pub(crate) struct TypeaheadCollector {
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) handle: Option<std::thread::JoinHandle<TypeaheadCaptureResult>>,
}

#[cfg(unix)]
impl TypeaheadCollector {
    /// `live_prompt`: when `Some`, stdin is a styled interactive TTY and the
    /// collector runs the raw-mode live editor (visible draft, cursor, and
    /// history on the shared status row). `None` keeps the canonical
    /// line-at-a-time loop for scripted/plain/assistive sessions. `history`
    /// seeds ↑/↓ recall; `initial_draft` restores typing carried across turns.
    pub(crate) fn start(
        queue: Arc<std::sync::Mutex<prompt_queue::PromptQueue>>,
        steering: prompt_queue::SteeringChannel,
        registry: Arc<subagent::AgentRegistry>,
        live_prompt: Option<String>,
        history: Vec<String>,
        initial_draft: String,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let runtime = tokio::runtime::Handle::current();
        let handle = std::thread::spawn(move || {
            let mut dispatcher = TypeaheadDispatcher {
                queue,
                steering,
                registry,
                runtime,
                live: false,
            };
            if let Some(prompt) = live_prompt {
                if let Some(raw) = RawStdinGuard::engage() {
                    dispatcher.live = true;
                    return run_live_typeahead(
                        raw,
                        &thread_stop,
                        &dispatcher,
                        prompt,
                        history,
                        initial_draft,
                    );
                }
            }
            run_canonical_typeahead(&thread_stop, &dispatcher)
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub(crate) fn finish(mut self) -> TypeaheadCaptureResult {
        self.stop.store(true, Ordering::Release);
        self.handle
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default()
    }
}

/// The pre-live collector loop: canonical stdin, complete submitted lines
/// only. Kept for sessions where the raw-mode editor must not run (piped
/// stdin, `--plain`, assistive input, or a terminal that refuses raw mode).
#[cfg(unix)]
pub(crate) fn run_canonical_typeahead(
    stop: &AtomicBool,
    dispatcher: &TypeaheadDispatcher,
) -> TypeaheadCaptureResult {
    let mut result = TypeaheadCaptureResult::default();
    while !stop.load(Ordering::Acquire) {
        if APPROVAL_INPUT_ACTIVE.load(Ordering::Acquire) > 0 {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        // Serialize ownership of canonical stdin with permission and
        // command prompts. Holding this lock from poll through
        // read_line closes the check-to-read race where one submitted
        // line could otherwise become either type-ahead or approval.
        let Ok(_input_owner) = APPROVAL_INPUT_LOCK.try_lock() else {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        };
        if APPROVAL_INPUT_ACTIVE.load(Ordering::Acquire) > 0 {
            continue;
        }

        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `descriptor` points to one initialized pollfd for the
        // duration of the call, and stdin's process fd is not owned or
        // closed by this collector.
        let ready = unsafe { libc::poll(&mut descriptor, 1, 50) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if ready == 0 || descriptor.revents & (libc::POLLIN | libc::POLLHUP) == 0 {
            continue;
        }

        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => {
                result.eof = true;
                break;
            }
            Ok(_) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                dispatcher.dispatch(&line, &mut result);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    result
}

/// What one batch of editor actions asks the collector's I/O loop to do.
/// Terminal writes, queue-depth syncs, notices, and signal raises are all
/// returned as data so the resolution logic is unit-testable without a
/// thread, raw stdin, or a live terminal (the collector-side counterpart of
/// the renderer's render_region seam).
#[cfg(unix)]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ActionEffects {
    /// The visible draft changed (keystrokes, hint acceptance, recall).
    pub(crate) needs_redraw: bool,
    /// A submission or recall may have moved the pending-queue depth.
    pub(crate) queue_touched: bool,
    /// A queued prompt was pulled into the draft; the caller prints the
    /// notice with this remaining count.
    pub(crate) recalled_remaining: Option<usize>,
    /// Raise this signal after the batch (SIGINT keeps raw mode; job-control
    /// and quit signals restore canonical termios first). Batching means the
    /// LAST signal-producing action wins — e.g. ^C then ^\ in one read
    /// quits rather than raising both, which matches what separate
    /// keystrokes would leave the user with.
    pub(crate) raise_signal: Option<i32>,
}

/// Resolve one feed batch's editor actions. `dispatch` routes a submitted
/// line (active-turn controls or enqueue); `recall_pop` removes and returns
/// the newest queued prompt plus the remaining count, if any.
#[cfg(unix)]
pub(crate) fn resolve_editor_actions(
    editor: &mut kitsu_editor::TypeaheadEditor,
    outcome: kitsu_editor::FeedOutcome,
    result: &mut TypeaheadCaptureResult,
    mut dispatch: impl FnMut(&str, &mut TypeaheadCaptureResult),
    mut recall_pop: impl FnMut() -> Option<(String, usize)>,
) -> ActionEffects {
    let mut effects = ActionEffects {
        needs_redraw: outcome.changed,
        ..ActionEffects::default()
    };
    for action in outcome.actions {
        match action {
            kitsu_editor::EditorAction::Submit(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                dispatch(&line, result);
                // Enqueues and /queue edits both move the pending count.
                effects.queue_touched = true;
            }
            kitsu_editor::EditorAction::Eof => {
                result.eof = true;
            }
            kitsu_editor::EditorAction::Interrupt => {
                effects.raise_signal = Some(libc::SIGINT);
            }
            kitsu_editor::EditorAction::Suspend => {
                effects.raise_signal = Some(libc::SIGTSTP);
            }
            // Ctrl+L (requirement R8): the during-turn half. The draft did
            // not change, so the redraw flag is what repaints the live row --
            // the same flag a keystroke sets, consulted once per batch.
            kitsu_editor::EditorAction::Redraw => {
                effects.needs_redraw = true;
            }
            kitsu_editor::EditorAction::Quit => {
                effects.raise_signal = Some(libc::SIGQUIT);
            }
            kitsu_editor::EditorAction::CompleteRequest => {
                // Tab accepts the inline slash hint, if one is showing.
                let buffer = editor.buffer_string();
                let hint = typeahead_slash_hint(&buffer);
                if !hint.is_empty() {
                    editor.replace_buffer(&format!("{buffer}{hint}"));
                    effects.needs_redraw = true;
                }
            }
            kitsu_editor::EditorAction::RecallRequest => {
                // ↑ on an empty draft: pull the newest queued prompt back
                // into the editor (it leaves the queue — the draft is now
                // its only copy; submitting re-enqueues it at the back,
                // which for the newest prompt is its original position).
                // Recall applies only to a still-empty draft: if other
                // bytes in the same batch already typed something, the
                // request is dropped and the queue stays intact — a
                // recall must never clobber text the user just typed.
                if !editor.buffer_string().is_empty() {
                    continue;
                }
                match recall_pop() {
                    Some((prompt, remaining)) => {
                        editor.replace_buffer(&prompt);
                        effects.recalled_remaining = Some(remaining);
                        effects.queue_touched = true;
                    }
                    None => {
                        // Queue empty: plain history walk, as before.
                        let mut forced = kitsu_editor::FeedOutcome::default();
                        editor.history_prev_forced(&mut forced);
                    }
                }
                effects.needs_redraw = true;
            }
        }
    }
    effects
}

/// Repaint the live row from editor state. The hint slot carries the
/// reverse-search status while Ctrl+R is active, the inline slash hint
/// otherwise.
#[cfg(unix)]
pub(crate) fn update_live_row(editor: &kitsu_editor::TypeaheadEditor, prompt: &str) {
    let buffer = editor.buffer_string();
    let search_status = editor.search_status();
    let hint_reserved = search_status.is_some();
    let hint = search_status.unwrap_or_else(|| typeahead_slash_hint(&buffer));
    // Inline mode: the pinned pane is the only place the draft can show (the
    // classic row is stood down). A parked approval owns the pane until its
    // answer is read, after which the collector repaints through here.
    if !approval_key_request_pending() {
        tui_bridge::paint_turn_draft(prompt, &buffer, editor.cursor(), &hint, hint_reserved);
    }
    // Fullscreen paints the draft into its framed box; the live row beside
    // the spinner keeps the status and not a second copy of the draft.
    if tui_bridge::fullscreen_turn_box_active() {
        return;
    }
    kitsu_render::live::live_input_update(buffer, editor.cursor(), hint, hint_reserved);
}

/// Card **T3-S7b**: whether a prompter has parked a request right now.
///
/// Checked after the poll and before the read, so a key that arrives while
/// this tick was already blocked in `poll` is left in the kernel buffer for
/// the service arm to read instead of being typed into the draft.
#[cfg(unix)]
pub(crate) fn approval_key_request_pending() -> bool {
    APPROVAL_KEY_REQUEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
}

/// Card **T3-S7b**, the collector's half: take the parked approval-key
/// request, if a prompter has published one.
#[cfg(unix)]
pub(crate) fn take_approval_key_request() -> Option<std::sync::mpsc::Sender<Option<char>>> {
    APPROVAL_KEY_REQUEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// Read **one** approval key from raw stdin, with the collector's
/// `RawStdinGuard` still engaged — which is the whole fix: `ICANON`, `ECHO`
/// and `ISIG` are clear, so the kernel echoes nothing and the typed answer
/// cannot reach the scroll region.
///
/// `None` is "closed without a decision": end of input, `\x04` (which the
/// canonical path saw as a zero-length read and the editor resolves as
/// `EditorAction::Eof`), a cancelled foreground turn, or a stopping
/// collector. The caller turns that into `Decision::Interrupt`, the case
/// T3-S7a's dismiss path already exists for.
///
/// A bare `\r`/`\n` is skipped rather than answered: the line the user typed
/// in the old protocol arrived terminated, so a terminator that lands in its
/// own read must not be mistaken for an answer. Every other key is returned
/// as-is and resolved by [`crate::terminal::approval::parse_permission_answer`] — the one parser — so an
/// unmapped key takes today's "invalid permission response; denied" branch.
#[cfg(unix)]
pub(crate) fn read_approval_key(stop: &AtomicBool) -> Option<char> {
    loop {
        if stop.load(Ordering::Acquire) || FOREGROUND_CANCELLED.load(Ordering::Acquire) {
            return None;
        }
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd for the duration of the call; stdin
        // stays process-owned.
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
        let mut buffer = [0u8; 64];
        // SAFETY: reads into a stack buffer of the stated length from the
        // process-owned stdin descriptor.
        let count = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                buffer.len(),
            )
        };
        if count < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return None;
        }
        if count == 0 {
            return None;
        }
        // The rest of the read is dropped: in the old protocol the kernel's
        // line discipline swallowed the terminator too, and one key is one
        // decision.
        let Some(key) = String::from_utf8_lossy(&buffer[..count as usize])
            .chars()
            .find(|key| !matches!(key, '\r' | '\n'))
        else {
            continue;
        };
        if key == '\u{4}' {
            return None;
        }
        return Some(key);
    }
}

/// The live during-turn editor: raw stdin bytes feed the pure
/// [`kitsu_editor::TypeaheadEditor`], and the draft renders on the shared
/// status row with a real cursor, ↑/↓ history, and editable text — while
/// streamed output and tool receipts keep flowing above it. Approval prompts
/// take stdin back by claiming `APPROVAL_INPUT_ACTIVE`: raw mode is released
/// and the draft row hidden until the approval completes.
/// The ambient tick: [`run_live_typeahead`]'s stdin poll timeout, in
/// milliseconds, and therefore the **staleness bound** on R5's status row and
/// on requirement **A8**'s ambient background-agent counter during a
/// foreground turn.
///
/// Named rather than inlined because it is load-bearing for a numbered
/// requirement: A8 gives the counter 250 ms, this loop re-pulls the registry
/// (`sync_agent_counts`) and repaints (`repaint_status_row`) once per
/// timeout, and `the_during_turn_ambient_tick_stays_inside_a8s_budget` fails
/// if the value is raised past the budget. Finding **F50**: the budget was
/// met by construction and asserted nowhere, so nothing failed if someone
/// raised the interval to a second.
///
/// Only this loop's poll uses it. The other `libc::poll` sites in this file
/// (the approval-key read, the canonical collector, the assistive line read)
/// refresh no counter, so they are not A8's business and keep their own
/// literals.
#[cfg(unix)]
pub(crate) const AMBIENT_TICK_POLL_MS: libc::c_int = 50;

#[cfg(unix)]
pub(crate) fn run_live_typeahead(
    raw: RawStdinGuard,
    stop: &AtomicBool,
    dispatcher: &TypeaheadDispatcher,
    prompt: String,
    history: Vec<String>,
    initial_draft: String,
) -> TypeaheadCaptureResult {
    use kitsu_render::live;

    let mut result = TypeaheadCaptureResult {
        live: true,
        ..TypeaheadCaptureResult::default()
    };
    let mut editor = kitsu_editor::TypeaheadEditor::with_draft(history, initial_draft);
    // Review 6: the during-turn box is painted through `paint_turn_draft`, the
    // same framed composer the idle reader uses, so ↑/↓ must walk its visual
    // rows here too — otherwise the same draft navigates by row at idle and by
    // history mid-turn. The row fallback keeps ↑/↓ as history (no motion set).
    if tui_bridge::input_box_enabled() {
        let prompt_for_motion = prompt.clone();
        editor.set_vertical_motion(Some(Box::new(
            kitsu_render::live::input_box_vertical_motion(&prompt_for_motion, || {
                kitsu_render::live::terminal_columns()
            }),
        )));
    }
    let mut raw = Some(raw);
    // Consecutive no-data poll timeouts; the lone-Esc resolver requires two.
    let mut quiet_ticks = 0u32;
    let sync_queue_depth = |dispatcher: &TypeaheadDispatcher| {
        let queue = dispatcher.queue.lock().expect("prompt queue");
        let queued = queue.status().queued;
        let previews: Vec<String> = queue.iter().take(3).map(str::to_string).collect();
        drop(queue);
        live::live_queue_state(queued, previews);
    };
    // The ambient background-agent tag: a cheap `watch` borrow of a `Copy`
    // snapshot, so polling it per tick costs nothing and the tag tracks
    // reality within one poll interval.
    let sync_agent_counts = |dispatcher: &TypeaheadDispatcher| {
        let agents = dispatcher.registry.snapshot();
        live::live_agents_state(agents.blocked, agents.running);
    };
    // Depth first, paint second: the row's first frame must reflect the
    // real queue, not the previous turn's persisted count (the main loop
    // dequeues between turns, so the stored value can be stale here). The
    // agent counts are synced ahead of the first paint for the same reason.
    sync_queue_depth(dispatcher);
    sync_agent_counts(dispatcher);
    // The turn's first frame, before the first poll: the row must show the
    // turn it is in, not the idle row the reader left behind.
    tui_bridge::repaint_status_row();
    live::live_input_begin(prompt.clone());
    update_live_row(&editor, &prompt);
    // Card T3-S7b: this loop can now serve an approval read itself, so say so
    // while the guard is held. Cleared on every path that lets the guard go.
    APPROVAL_KEY_READER.store(raw.is_some(), Ordering::Release);
    while !stop.load(Ordering::Acquire) {
        // Card T3-S7b, and deliberately the loop's **first** arm: a parked
        // request needs raw mode, so it is served before the claim protocol's
        // arm below can hand the terminal back. Nothing here takes
        // `APPROVAL_INPUT_LOCK` -- the waiting prompter holds it -- so the
        // service can never be blocked by the prompter it is serving.
        if let Some(reply) = take_approval_key_request() {
            if raw.is_none() {
                raw = RawStdinGuard::engage();
                APPROVAL_KEY_READER.store(raw.is_some(), Ordering::Release);
            }
            let key = if raw.is_some() {
                read_approval_key(stop)
            } else {
                None
            };
            // Send before repainting: the prompter's receipt and teardown are
            // the modal's, and it should not wait on a row.
            let _ = reply.send(key);
            // The key was read raw, so no paste can be half-consumed; reset
            // anyway, for the canonical path's reason -- a latched paste mode
            // would eat every later Enter.
            editor.reset_pending_input();
            if raw.is_some() {
                live::live_input_begin(prompt.clone());
                update_live_row(&editor, &prompt);
            }
            continue;
        }
        if APPROVAL_INPUT_ACTIVE.load(Ordering::Acquire) > 0 {
            if let Some(guard) = raw.take() {
                // Canonical mode (with echo) back on for the approval reader;
                // the draft row is already hidden by the approval guard. No
                // in-loop read can be served without the guard (T3-S7b).
                APPROVAL_KEY_READER.store(false, Ordering::Release);
                drop(guard);
                live::live_input_end();
                // The approval reader may consume the rest of an in-flight
                // paste (terminator included); dropping transient input state
                // prevents a latched paste mode from eating every later Enter.
                editor.reset_pending_input();
            }
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        if raw.is_none() {
            match RawStdinGuard::engage() {
                Some(guard) => {
                    raw = Some(guard);
                    APPROVAL_KEY_READER.store(true, Ordering::Release);
                    live::live_input_begin(prompt.clone());
                    update_live_row(&editor, &prompt);
                }
                None => break,
            }
        }
        let Ok(_input_owner) = APPROVAL_INPUT_LOCK.try_lock() else {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        };
        if APPROVAL_INPUT_ACTIVE.load(Ordering::Acquire) > 0 {
            continue;
        }
        // We own stdin and the row is painted: refresh the ambient agent tag
        // once per poll tick. Deliberately after the approval checks, so no
        // work happens while the approval reader owns the terminal and the
        // row is torn down.
        sync_agent_counts(dispatcher);
        // ...and repaint R5's status row on the same tick (card T3-S5's
        // deferred wiring). This is the row's whole cadence: elapsed ticks
        // every second and 50 ms is twenty times finer, so the row is never
        // more than a tick stale. `repaint_status_row` paints nothing when
        // the composed row is unchanged, and nothing at all in classic mode.
        tui_bridge::repaint_status_row();

        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd for the duration of the call; stdin
        // stays process-owned.
        let ready = unsafe { libc::poll(&mut descriptor, 1, AMBIENT_TICK_POLL_MS) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if ready == 0 {
            // Two consecutive true timeouts (~100ms) after a bare ESC byte
            // mean the user pressed the Esc key itself — real escape
            // sequences arrive atomically, and the double tick tolerates a
            // slow link splitting ESC from its tail across reads. Error
            // revents deliberately don't count as quiet: only a timeout is
            // evidence that no bytes are inbound.
            quiet_ticks += 1;
            if quiet_ticks >= 2 && editor.resolve_lone_escape() {
                update_live_row(&editor, &prompt);
            }
            continue;
        }
        if descriptor.revents & (libc::POLLIN | libc::POLLHUP) == 0 {
            continue;
        }
        // Card T3-S7b: a request parked while this tick sat in `poll` means
        // the readable byte is an approval answer, not draft text. Leave it
        // in the kernel buffer and let the service arm above read it.
        if approval_key_request_pending() {
            continue;
        }
        quiet_ticks = 0;

        let mut buffer = [0u8; 1024];
        // SAFETY: reads into a stack buffer of the stated length from the
        // process-owned stdin descriptor. VMIN=1 and a successful poll mean
        // a zero return is end-of-input, not an empty raw read.
        let count = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                buffer.len(),
            )
        };
        if count < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if count == 0 {
            result.eof = true;
            break;
        }
        let outcome = editor.feed(&buffer[..count as usize]);
        let effects = resolve_editor_actions(
            &mut editor,
            outcome,
            &mut result,
            |line, result| dispatcher.dispatch(line, result),
            || {
                let mut queue = dispatcher.queue.lock().expect("prompt queue");
                let queued = queue.status().queued;
                queued
                    .checked_sub(1)
                    .and_then(|newest| queue.remove(newest))
                    .map(|prompt| (prompt, queued - 1))
            },
        );
        if effects.queue_touched {
            sync_queue_depth(dispatcher);
        }
        if let Some(remaining) = effects.recalled_remaining {
            let _ = kitsu_render::PromptSafeAsyncNotice::info(format!(
                "recalled queued prompt for editing ({remaining} still queued)"
            ))
            .write_line(&mut std::io::stderr());
        }
        if let Some(signal) = effects.raise_signal {
            // Job-control and quit signals need canonical termios back
            // first (a suspended or dying process must not hold the
            // terminal raw); SIGINT keeps raw mode — the turn cancels and
            // this collector winds down via the stop flag, restoring
            // termios on guard drop.
            if signal != libc::SIGINT {
                if let Some(guard) = raw.take() {
                    APPROVAL_KEY_READER.store(false, Ordering::Release);
                    drop(guard);
                    live::live_input_end();
                }
            }
            // Raised through the bridge rather than with a bare
            // `libc::raise`: requirement N5's dying-signal handler must stay
            // out of the way of a signal kitsu is raising on *itself*. This
            // SIGINT is how the turn is cancelled (the `ctrl_c()` arm of the
            // turn's `select!` fires on it), so a restore-and-exit handler
            // answering it would turn Ctrl+C into a quit. For SIGTSTP the
            // loop's re-engage branch restores raw mode and repaints after
            // fg/SIGCONT.
            tui_bridge::raise_in_process(signal);
        }
        if result.eof {
            break;
        }
        if effects.needs_redraw {
            update_live_row(&editor, &prompt);
        }
    }
    // The reader is leaving: a prompter waiting on an in-loop key must stop
    // waiting (T3-S7b), so withdraw the offer before the guard goes.
    APPROVAL_KEY_READER.store(false, Ordering::Release);
    live::live_input_end();
    tui_bridge::end_turn_draft();
    drop(raw);
    result.pending_draft = editor.take_draft();
    result
}

#[cfg(not(unix))]
pub(crate) struct TypeaheadCollector;

#[cfg(not(unix))]
impl TypeaheadCollector {
    pub(crate) fn start(
        _queue: Arc<std::sync::Mutex<prompt_queue::PromptQueue>>,
        _steering: prompt_queue::SteeringChannel,
        _registry: Arc<subagent::AgentRegistry>,
        _live_prompt: Option<String>,
        _history: Vec<String>,
        _initial_draft: String,
    ) -> Self {
        Self
    }

    pub(crate) fn finish(self) -> TypeaheadCaptureResult {
        TypeaheadCaptureResult::default()
    }
}
