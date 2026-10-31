//! Classic, inline, fullscreen, and assistive input adapters.

use std::sync::atomic::Ordering;

use crate::presentation::terminal_inline;
use crate::subagent;
#[cfg(unix)]
use crate::terminal::agent_view::apply_agent_view_exit;
#[cfg(unix)]
use crate::terminal::agent_view::apply_inline_attach_ordinal;
#[cfg(unix)]
use crate::terminal::agent_view::apply_inline_cycle_agent;
#[cfg(unix)]
use crate::terminal::agent_view::apply_inline_detach;
#[cfg(unix)]
use crate::terminal::agent_view::attached_route_target;
#[cfg(unix)]
use crate::terminal::agent_view::route_attached_submission;
#[cfg(unix)]
use crate::terminal::agent_view::run_agent_view_overlay;
use crate::terminal::completion::ChatHelper;
#[cfg(unix)]
use crate::terminal::transcript::run_transcript_overlay;
#[cfg(unix)]
use crate::terminal::typeahead::RawStdinGuard;
use crate::terminal::typeahead::ASSISTIVE_INPUT_INTERRUPTED;
#[cfg(unix)]
use crate::tui_bridge;

pub(crate) enum ChatReadline {
    Line(String),
    Eof,
    Interrupted,
    Error(String),
}

pub(crate) fn read_assistive_line_from<R: std::io::BufRead, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    prompt: &str,
) -> std::io::Result<Option<String>> {
    // Assistive/plain input is deliberately a simple canonical line reader:
    // no raw mode, cursor movement, bracketed-paste toggles, redraw sequences,
    // or asynchronous prompt rewriting. This trades completion/history editing
    // for a literal screen-reader/transcript-safe byte stream.
    writer.write_all(terminal_inline(prompt).as_bytes())?;
    writer.flush()?;
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    Ok(Some(line))
}

#[cfg(unix)]
pub(crate) fn read_assistive_stdin_line(prompt: &str) -> std::io::Result<Option<String>> {
    use std::io::Write;

    let mut stderr = std::io::stderr().lock();
    stderr.write_all(terminal_inline(prompt).as_bytes())?;
    stderr.flush()?;
    ASSISTIVE_INPUT_INTERRUPTED.store(false, Ordering::Release);
    loop {
        if ASSISTIVE_INPUT_INTERRUPTED.swap(false, Ordering::AcqRel) {
            return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
        }
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the initialized pollfd lives for the call and Kitsu does not
        // close process stdin. Canonical mode ensures a readable descriptor
        // contains a complete submitted line (or EOF).
        let ready = unsafe { libc::poll(&mut descriptor, 1, 50) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(std::io::Error::last_os_error());
        }
        if ready == 0 {
            continue;
        }
        return read_assistive_line_from(&mut std::io::stdin().lock(), &mut std::io::sink(), "");
    }
}

/// Portable fallback: a plain blocking line read. It cannot observe the
/// interrupt flag mid-wait the way the unix `poll` loop does, but every
/// non-unix caller gets the same prompt-on-stderr, line-from-stdin contract.
#[cfg(not(unix))]
pub(crate) fn read_assistive_stdin_line(prompt: &str) -> std::io::Result<Option<String>> {
    let stdin = std::io::stdin();
    let stderr = std::io::stderr();
    read_assistive_line_from(&mut stdin.lock(), &mut stderr.lock(), prompt)
}

pub(crate) fn read_chat_line(
    editor: &mut rustyline::Editor<ChatHelper, rustyline::history::DefaultHistory>,
    prompt: &str,
    assistive: bool,
    initial: &str,
) -> ChatReadline {
    if assistive {
        let result = read_assistive_stdin_line(prompt);
        return match result {
            Ok(Some(line)) => ChatReadline::Line(line),
            Ok(None) => ChatReadline::Eof,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                ChatReadline::Interrupted
            }
            Err(error) => ChatReadline::Error(error.to_string()),
        };
    }
    // A draft carried over from the during-turn live editor pre-fills the
    // prompt (cursor at the end) instead of being silently dropped.
    let result = if initial.is_empty() {
        editor.readline(prompt)
    } else {
        editor.readline_with_initial(prompt, (initial, ""))
    };
    match result {
        Ok(line) => ChatReadline::Line(line),
        Err(rustyline::error::ReadlineError::Eof) => ChatReadline::Eof,
        Err(rustyline::error::ReadlineError::Interrupted) => ChatReadline::Interrupted,
        Err(error) => ChatReadline::Error(error.to_string()),
    }
}

/// The idle prompt's reader: the inline one when the viewport is installed on
/// an interactive terminal, rustyline otherwise.
///
/// History comes from rustyline either way — `add_history_entry` still runs on
/// every submitted line — so Up/Down at the inline prompt walks the same list,
/// even though rustyline's own composer never runs.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_idle_chat_line(
    editor: &mut rustyline::Editor<ChatHelper, rustyline::history::DefaultHistory>,
    bridge: Option<&tui_bridge::BridgeGuard>,
    // Finding **F97**: the live fullscreen segment, when this session holds
    // the alternate screen. It is the third reader, and it is `Some` exactly
    // when `bridge` is `None` for the fullscreen reason -- the two are
    // mutually exclusive by construction, because every route into one
    // renderer tears the other down first.
    fullscreen: Option<&mut tui_bridge::FullscreenSegment>,
    registry: &subagent::AgentRegistry,
    prompt: &str,
    assistive: bool,
    initial: &str,
    attached: &mut Option<u64>,
    // Card **T4-S7**: the agent view's dispatcher, on its way to the overlay
    // the inline reader can open.
    view_dispatcher: Option<&subagent::ViewDispatcher>,
) -> ChatReadline {
    // Assistive and non-TTY carriers keep their existing readers: neither can
    // be painted into a viewport row or onto a frame's bottom row.
    let interactive = !assistive && kitsu_render::stdin_is_tty() && kitsu_render::stderr_styled();
    // Fullscreen first, because a session that holds the alternate screen has
    // no `BridgeGuard` at all: `install` is inert for the mode, which is what
    // left this surface on rustyline (finding **F97**).
    if let Some(segment) = fullscreen {
        if interactive {
            let history: Vec<String> = editor.history().iter().cloned().collect();
            return read_chat_line_fullscreen(segment, prompt, &history, initial);
        }
    }
    if let Some(bridge) = bridge {
        if interactive {
            let history: Vec<String> = editor.history().iter().cloned().collect();
            return read_chat_line_inline(
                bridge,
                registry,
                prompt,
                &history,
                initial,
                attached,
                view_dispatcher,
            );
        }
    }
    read_chat_line(editor, prompt, assistive, initial)
}

/// The inline mode's idle prompt (story T0-S5d): one line read through
/// [`tui_bridge::read_line_inline`] instead of rustyline, with the draft
/// painted onto the pinned viewport row.
///
/// # Raw mode is `RawStdinGuard`'s, and only `RawStdinGuard`'s
///
/// The guard clears `ICANON`, `ECHO` and `ISIG` in `c_lflag` and leaves
/// `c_oflag` alone, so `OPOST`/`ONLCR` stay on and the binary's several
/// hundred direct `println!`/`eprintln!` writers keep getting a carriage
/// return with every line feed. Enabling a second, fuller raw mode here
/// (crossterm's, which clears `OPOST`) would staircase all of them down the
/// screen, so this path engages the existing guard and nothing else.
///
/// Because `ISIG` is masked, Ctrl+C arrives as a byte rather than a signal.
/// On an empty draft the reader reports `Interrupted` and this caller raises
/// the `SIGINT` the kernel would have sent — the same division of labour the
/// during-turn collector uses. Termios is restored first: unlike the
/// collector, this function returns rather than continuing to read, so it must
/// not hand back a terminal that is still raw.
///
/// Known preview gaps, carded rather than fixed here: no Tab completion and no
/// slash-command palette at the idle prompt. **History persistence is not one
/// of them** — an earlier version of this comment said it was, wrongly. Inline
/// mode borrows rustyline's persistence wholesale without running its
/// composer: `load_history` seeds the editor at startup, `read_idle_chat_line`
/// hands the inline reader that same list, and `add_history_entry` sits on the
/// common path after the read, so a line submitted here is saved like any
/// other. Reverse search works too, **including** its status indicator, which
/// the hint slot now carries.
///
/// Still missing here, and they are **not** one cause — an earlier version of
/// this comment claimed they were, wrongly. The hint slot carries one line
/// beside the draft, so widening it restored the reverse-search status and the
/// inline slash-completion suffix, and nothing else. What remains:
///
/// * **Tab completion.** `EditorAction::CompleteRequest` is still dropped here.
///   Reaching parity with the during-turn row is small — see
///   `update_live_row`'s neighbourhood, where the whole of Tab is seven lines
///   that append the showing hint to the buffer.
/// * **The `@file` and slash-palette completion lists.** Rustyline helper
///   surfaces that draw over several rows, so they need a viewport taller than
///   one row and belong with the multi-row composer.
#[cfg(unix)]
pub(crate) fn read_chat_line_inline(
    bridge: &tui_bridge::BridgeGuard,
    registry: &subagent::AgentRegistry,
    base_prompt: &str,
    history: &[String],
    initial: &str,
    attached: &mut Option<u64>,
    // Card **T4-S7**: carried through to the overlay this reader opens, which
    // is one of the agent view's two routes.
    view_dispatcher: Option<&subagent::ViewDispatcher>,
) -> ChatReadline {
    use std::io::Read as _;

    // A draft carried over from the during-turn live editor is replayed into
    // the editor as if it had just been typed, which keeps the "typing is
    // never dropped" contract the rustyline path has. Finding F21 reuses the
    // same replay for a draft that survived a suspend, which is why this is a
    // `mut` binding and the read below sits in a loop.
    // Finding F16: the `SIGWINCH` handler, installed once per process. It is
    // installed here rather than at the bridge install because this is the
    // only place the flag is ever checked -- a flag set while no inline read
    // is in flight has nobody to re-latch for.
    static SIGWINCH: std::sync::Once = std::sync::Once::new();
    SIGWINCH.call_once(tui_bridge::install_sigwinch_handler);

    // The frame characters follow the renderer's styling, not this process's
    // stderr: the surface is the renderer's, so it is the renderer that says
    // whether this terminal gets Unicode borders.
    let rich = kitsu_render::stderr_styled();
    let mut carried = initial.to_string();
    // Raw mode spans the read, but not the *whole* function any more: a
    // suspend must hand a canonical terminal back to the shell before the
    // process stops and re-engage after it is foregrounded. So the guard
    // lives in an `Option` owned by this frame rather than being bound once:
    // every `return` below drops it (restoring termios), the suspend path
    // takes it out before the stop and puts a fresh one back after, and a
    // re-engage that fails leaves it `None` and returns. No path can therefore
    // return with raw mode still engaged.
    let mut raw: Option<RawStdinGuard> = None;
    loop {
        // Card **T4-S6a**: the agent this composer is routed to, and the
        // label that is the standing evidence of it (design \u{00a7}4.5).
        //
        // It is derived **per read** rather than once per call, because the
        // two things that change it both resume this loop: an attach made in
        // the agent view (the `OpenAgentView` arm below) and a detach (the
        // `Detached` arm). Both the prompt label and the key route the reader
        // takes come off this one binding, so the row and the reader read the
        // same single source rather than two.
        //
        // The binding itself is the **session's**, owned by the composer loop
        // in `chat::run_chat` and borrowed here: `OverlayState` is
        // per-invocation and this function returns after every submitted
        // line, so neither can hold an attachment that must outlive them
        // (finding **F75**).
        let labelled = tui_bridge::composer_prompt(base_prompt, *attached);
        let prompt = labelled.as_str();
        if raw.is_none() {
            match RawStdinGuard::engage() {
                Some(guard) => raw = Some(guard),
                None => {
                    return ChatReadline::Error("failed to switch stdin to raw mode".to_string())
                }
            }
        }
        let mut input = std::io::Cursor::new(carried.clone().into_bytes()).chain(std::io::stdin());
        let outcome = {
            let mut paint = |draft: &str, cursor: usize, hint: &str, hint_reserved: bool| {
                // The whole composer, not just its row: the slash palette
                // above the draft, re-derived and fully repainted on every
                // keystroke (finding F27). `rich` is the *renderer's* styling
                // decision, which is what picks Unicode or ASCII frames.
                bridge.paint_composer(prompt, draft, cursor, hint, hint_reserved, rich);
            };
            // Finding F16: re-probe and re-latch. The notification carries
            // no dimensions, so the terminal is the only source of truth for
            // the new size.
            let mut on_resize = || {
                if let Ok((_cols, rows)) = crossterm::terminal::size() {
                    // The height is the viewport's own, not a hardcoded row:
                    // an open palette must survive a resize instead of
                    // collapsing to its bottom row and leaving the rest of
                    // the box inside the scroll region.
                    bridge.resize_rows(rows);
                }
            };
            tui_bridge::read_line_inline_with_keys(
                &mut input,
                &mut paint,
                history,
                &mut on_resize,
                tui_bridge::inline_keys(),
                // Card **T4-S6a** threads the attached agent here, from the
                // same binding the prompt label above was derived from: a
                // second carrier for it would be a second source of truth.
                *attached,
                // Inline pages nothing: the keys are live only while the
                // fullscreen segment holds the screen, and a surface with no
                // frame to move has nothing for a press to mean.
                None,
                // Stage 1B: the framed box walks its visual rows with ↑/↓,
                // through the layout it paints with; the row fallback keeps
                // ↑/↓ as history.
                tui_bridge::input_box_enabled().then(|| {
                    Box::new(kitsu_render::live::input_box_vertical_motion(
                        prompt,
                        kitsu_render::live::terminal_columns,
                    )) as kitsu_editor::VerticalMotion
                }),
            )
        };
        // Whatever happened, the row must not keep showing a draft that is
        // gone. On the suspend path the release below clears the row anyway,
        // but doing it here keeps the invariant on every outcome.
        // Through `paint_composer`, not `paint_draft`: an empty draft has no
        // palette, and this is the call that must take the viewport back down
        // to one row. `paint_draft` would paint the status row and leave an
        // open palette's frame pinned above it until something else repainted.
        // Back to the one status row: an empty draft's palette and the input
        // box must not stay pinned while the turn streams (review F7).
        bridge.reset_composer();
        // Card T4-S2: the agent view. The reader is pure for the same reason
        // it is pure for a suspend, so the overlay's terminal, alternate
        // screen and key loop are sequenced here — and the draft comes back
        // through the same replay a suspend uses, so `\u{2190}\u{2190}` on a
        // composer that was *not* empty by the time the chord arrived cannot
        // cost the user a character.
        if let Ok(tui_bridge::InlineRead::OpenAgentView(draft)) = outcome {
            // Raw mode is already engaged by this frame's guard, so the
            // overlay must not engage a second one: `RawStdinGuard::engage`
            // snapshots the modes it displaces, and nesting would publish the
            // *raw* modes as the canonical ones for the dying-signal restore.
            let exit = run_agent_view_overlay(registry, false, view_dispatcher);
            // Card **T4-S6a**, requirement A3: the view's `Enter`/`\u{2192}`
            // attaches, and this is the one place that attachment becomes the
            // composer's. The view is gone by now (attach closes it), so the
            // id has to be carried out of it rather than read off it.
            apply_agent_view_exit(&exit, attached);
            bridge.reinstall_after_resume();
            carried = draft;
            continue;
        }
        // Requirement **R6**: `Ctrl+O`. Sequenced here for the agent view's
        // reason -- the reader is pure and the overlay owns the screen, the
        // alternate screen and its own key loop -- and the draft comes back
        // through the same replay (**F76**), so the key cannot cost the user a
        // character. The lines are the installed viewport's ring, read before
        // the overlay paints anything: that ring is the streamed conversation
        // and the only copy of it (**N4**).
        if let Ok(tui_bridge::InlineRead::OpenTranscript(draft)) = outcome {
            let lines = bridge.cell_lines().unwrap_or_default();
            run_transcript_overlay(&lines, true);
            bridge.reinstall_after_resume();
            carried = draft;
            continue;
        }
        // Card **T4-S6a**, requirement A5: `\u{2190}` on an empty composer
        // and `Ctrl+X d` both report `Detached`, and detaching is this loop's
        // to perform: the reader is pure, exactly as it is for a suspend and
        // for the agent view. Clearing the session binding is the whole of
        // it — the next iteration re-derives the prompt label off it, so the
        // composer visibly goes back to the session's own prompt and the
        // `\u{2190}` route goes back to the agent view's two-press arm. The
        // `\u{26E9}` *attach* milestone landed in T4-S6b and is painted in
        // `apply_agent_view_exit`; only the **recap** is still outstanding
        // (T4-S6c, design \u{00a7}4.5). This comment claimed both were
        // outstanding until T4-S6b shipped one of them -- the **F77** shape,
        // a comment true when written and wrong once a later card landed.
        if let Ok(tui_bridge::InlineRead::Detached(draft)) = outcome {
            carried = apply_inline_detach(attached, draft);
            continue;
        }
        // Card **T4-S6-ordinal**: `Ctrl+X 1..9`. Sequenced here for the
        // agent view's and the detach's reason -- the reader is pure -- and
        // because this is the one place that holds both the registry the
        // ordinal is resolved against and the session attachment it changes.
        // The draft comes back through the same replay (**F76**).
        if let Ok(tui_bridge::InlineRead::AttachOrdinal(ordinal, draft)) = outcome {
            carried = apply_inline_attach_ordinal(registry, attached, ordinal, draft);
            continue;
        }
        // Shift+Left / Shift+Right: cycle the attached agent. Sequenced here
        // with the ordinal attach, for the same reason -- this is the one place
        // holding both the registry (to resolve the step over display order)
        // and the attachment it mutates. The draft replays through `carried`
        // (**F76**), so cycling never costs a typed line.
        if let Ok(tui_bridge::InlineRead::CycleAgent(delta, draft)) = outcome {
            carried = apply_inline_cycle_agent(registry, attached, delta, draft);
            continue;
        }
        // Card **T4-S6b**, requirement A5: while attached, a submitted line
        // is the attached agent's -- a steer, or an answer while it is
        // blocked -- and never a turn. Routed here, before the line can
        // become a `ChatReadline::Line`, because this is the one place that
        // can still replay a refusal's draft through `carried` (**F76**): a
        // refused line is back in the composer on the next read.
        if let Ok(tui_bridge::InlineRead::Submitted(line)) = &outcome {
            if let Some(id) = attached_route_target(line, *attached) {
                // The feed is inside `route_attached_submission`, which is the
                // one place that knows whether the line was delivered.
                carried = route_attached_submission(registry, id, line).draft;
                continue;
            }
        }
        // Finding F21: Ctrl+Z. The reader is pure, so the whole suspend is
        // sequenced here, through the seam that makes the ordering testable.
        if let Ok(tui_bridge::InlineRead::Suspended(draft)) = outcome {
            let mut effects = InlineSuspendEffects {
                bridge,
                raw: &mut raw,
            };
            if !tui_bridge::suspend_and_resume(&mut effects) {
                return ChatReadline::Error(
                    "failed to switch stdin back to raw mode after a suspend".to_string(),
                );
            }
            // Resume reading with the draft preserved: it replays through the
            // same cursor-chain as a carried-over draft, so typing survives a
            // suspend/resume cycle.
            carried = draft;
            continue;
        }
        return read_chat_line_inline_outcome(outcome);
    }
}

/// The non-suspend outcomes of one inline read, split out so the loop above
/// reads as the sequencing it is.
#[cfg(unix)]
pub(crate) fn read_chat_line_inline_outcome(
    outcome: std::io::Result<tui_bridge::InlineRead>,
) -> ChatReadline {
    match outcome {
        Ok(tui_bridge::InlineRead::Submitted(line)) => ChatReadline::Line(line),
        Ok(tui_bridge::InlineRead::Eof) => ChatReadline::Eof,
        // Both handled by the caller's loop before this is reached.
        Ok(tui_bridge::InlineRead::Suspended(_)) => ChatReadline::Eof,
        Ok(tui_bridge::InlineRead::OpenAgentView(_)) => ChatReadline::Eof,
        // Card T4-S6a: handled by the caller's loop too -- detaching clears
        // the session's attachment, which re-labels the composer, and reads
        // again. Unreachable from there, like the two outcomes above it.
        Ok(tui_bridge::InlineRead::Detached(_)) => ChatReadline::Eof,
        // Requirement **R6**: handled by the caller's loop, like the two
        // outcomes above it. Unreachable from here.
        Ok(tui_bridge::InlineRead::OpenTranscript(_)) => ChatReadline::Eof,
        // Card **T4-S6-ordinal**: handled by the caller's loop, which is the
        // only place that holds both the registry the ordinal is resolved
        // against and the session's attachment -- so this arm is unreachable
        // from here, like the three above it.
        Ok(tui_bridge::InlineRead::AttachOrdinal(_, _)) => ChatReadline::Eof,
        // Shift+Left/Right: handled by the caller's loop, the only place that
        // holds both the registry and the attachment -- unreachable from here,
        // like `AttachOrdinal` above it.
        Ok(tui_bridge::InlineRead::CycleAgent(_, _)) => ChatReadline::Eof,
        Ok(tui_bridge::InlineRead::Interrupted) => {
            // Deliberately NOT re-raising SIGINT here, unlike the during-turn
            // collector. The collector raises because a turn is in flight and
            // the signal is what cancels it; at an idle prompt there is
            // nothing to cancel, and every `ctrl_c()` in this binary sits
            // inside a `select!` scoped to one operation rather than being a
            // process-lifetime handler -- so at the *first* idle prompt no
            // handler is installed and the raise would take the default
            // disposition and kill kitsu. Classic mode prints `^C` and keeps
            // the session, so raising here would make Ctrl+C mean "exit" in
            // inline mode and "clear the line" in classic. `Interrupted` is
            // handled by the caller, which prints exactly that notice.
            ChatReadline::Interrupted
        }
        Err(error) => ChatReadline::Error(error.to_string()),
    }
}

/// The fullscreen renderer's own idle prompt: one line read through
/// [`tui_bridge::read_line_inline_with_keys`] with the draft painted onto the
/// bottom row of the alternate screen.
///
/// # Why this surface needs a reader at all (finding **F97**)
///
/// `tui_bridge::install` is inert in fullscreen -- the mode pins no DECSTBM
/// region and installs no surface -- so there is no `BridgeGuard` here and
/// [`read_idle_chat_line`] used to fall through to rustyline. Rustyline runs
/// in **cooked** mode and its `ChatHelper` paints a prompt, a hint deck and
/// the whole slash palette as ordinary lines at the cursor, which on a screen
/// the frame fills means scrolling frame rows off the top: measured on a pty,
/// a single `/c` cost eleven of them, and backspacing brought none of them
/// back. So the keystrokes on this surface are read here, by the same editor
/// and the same key decoder the inline composer uses, and painted by
/// [`tui_bridge::FullscreenSegment::paint_composer`], which addresses the
/// bottom row absolutely and never feeds a line.
///
/// # What is shared with the inline reader, and what differs
///
/// Shared: the whole keystroke-to-draft path. `read_line_inline_with_keys` is
/// generic over the reader and takes its painter as a closure, it touches no
/// guard, pane or surface, and raw mode is the caller's -- so the only thing
/// this function supplies that the inline caller does not is a different
/// `paint`. History is rustyline's, exactly as it is for inline: the caller
/// hands over the same list and `add_history_entry` still runs on the
/// submitted line.
///
/// Differs, and deliberately:
///
/// * **The key routes are off** ([`tui_bridge::InlineKeys::disabled`]). Both
///   of them lead to the agent view, which takes the alternate screen this
///   segment is already holding, and `/jobs` is where that view has an answer
///   that does not need the screen. With the routes off, the reader's
///   `OpenAgentView` and `Detached` outcomes cannot arrive on this surface.
/// * **There is no palette row**, for the reason
///   `FullscreenSegment::paint_composer` gives: the only rows it could use
///   are the frame's.
/// * **`on_resize` paints nothing.** The geometry is probed inside `paint`,
///   per paint, so the frame that answers a `SIGWINCH` wake is already at the
///   new size and there is nothing for a resize hook to re-latch -- unlike
///   the inline reader, whose viewport owns a latched region.
///
/// # Raw mode is new terminal state on this surface
///
/// `RawStdinGuard::engage` clears `ICANON`, `ECHO` and `ISIG`, publishes the
/// displaced termios for the dying-signal handler and enables bracketed
/// paste; its `Drop` puts all three back. The guard lives in an `Option`
/// owned by this frame for the reason the inline reader's does: every
/// `return` below drops it, and the suspend path takes it out before the
/// process stops. A dying signal is the exit path no `Drop` runs on, and it
/// is covered by `tui_bridge::install_exit_restore`, which every route into
/// fullscreen now registers.
#[cfg(unix)]
pub(crate) fn read_chat_line_fullscreen(
    segment: &mut tui_bridge::FullscreenSegment,
    prompt: &str,
    history: &[String],
    initial: &str,
) -> ChatReadline {
    use std::io::Read as _;

    // Finding F16, exactly as the inline reader installs it: the handler is
    // process-wide and `Once`-gated, and a `SIGWINCH` is delivered by
    // interrupting the blocking `read` the reader below sits in.
    static SIGWINCH: std::sync::Once = std::sync::Once::new();
    SIGWINCH.call_once(tui_bridge::install_sigwinch_handler);

    let mut carried = initial.to_string();
    let mut raw: Option<RawStdinGuard> = None;
    let outcome = loop {
        if raw.is_none() {
            match RawStdinGuard::engage() {
                Some(guard) => raw = Some(guard),
                None => {
                    return ChatReadline::Error("failed to switch stdin to raw mode".to_string())
                }
            }
        }
        let mut input = std::io::Cursor::new(carried.clone().into_bytes()).chain(std::io::stdin());
        let read = {
            // `paint` and `page` both need the segment mutably, and the reader
            // holds both for the whole read, so the segment is shared through
            // a `RefCell` rather than borrowed twice: the two are called from
            // the same loop and never from inside one another, so no borrow
            // of it overlaps another at run time. The cell dies with this
            // block, which is what lets the outcome arms below take the
            // segment back.
            let shared = std::cell::RefCell::new(&mut *segment);
            let mut paint = |draft: &str, cursor: usize, hint: &str, hint_reserved: bool| {
                // Probed per paint rather than captured: see the resize note
                // above. The fallback is the conventional 80x24 every other
                // geometry probe in this binary falls back to.
                let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                // The renderer's own composition value, so this row and the
                // inline composer's are told the same things about a draft
                // (the ordering `read_line_inline_with_keys` documents).
                let composed = kitsu_render::live::LiveDraft {
                    prompt: prompt.to_string(),
                    buffer: draft.to_string(),
                    cursor,
                    hint: hint.to_string(),
                    hint_reserved,
                };
                shared
                    .borrow_mut()
                    .paint_composer(&composed, rows, cols as usize);
            };
            // One press, one repaint, and the geometry probed per press for
            // `paint`'s reason. The frame follows the ring only when the ring
            // grows, so the press's own repaint is the only thing that can
            // answer the key.
            let mut page = |press: kitsu_tui::fullscreen::Page| {
                let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                shared.borrow_mut().page(press, rows, cols as usize);
            };
            let mut on_resize = || {};
            tui_bridge::read_line_inline_with_keys(
                &mut input,
                &mut paint,
                history,
                &mut on_resize,
                tui_bridge::InlineKeys::disabled(),
                None,
                // The paging sink: this is the surface that has a frame for a
                // press to move, so this is the only call site that passes
                // one. The sink *is* the gate -- the inline reader's `None`
                // leaves all four sequences to the editor.
                Some(&mut page),
                None,
            )
        };
        match read {
            // Finding F21: the reader is pure, so the suspend is sequenced
            // here, through the same seam the inline reader uses -- which is
            // what keeps "termios is canonical before the process stops" one
            // property with one implementation rather than two.
            Ok(tui_bridge::InlineRead::Suspended(draft)) => {
                let (_cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                let mut effects = FullscreenSuspendEffects {
                    segment,
                    raw: &mut raw,
                    rows,
                };
                if !tui_bridge::suspend_and_resume(&mut effects) {
                    return ChatReadline::Error(
                        "failed to switch stdin back to raw mode after a suspend".to_string(),
                    );
                }
                carried = draft;
                continue;
            }
            // Requirement **R6**: `Ctrl+O` is a literal control byte and not
            // one of the two chord routes, so it *is* reachable here with
            // `InlineKeys::disabled()` -- which is the point: the overlay is
            // reachable from both surfaces (**F89**). The alternate screen is
            // already this segment's, so the overlay does not take a second
            // one, and the close repaints the frame from the **segment's**
            // anchor: the overlay paged its own, so this is what puts back the
            // frame the user left.
            Ok(tui_bridge::InlineRead::OpenTranscript(draft)) => {
                let lines = segment.transcript_lines();
                run_transcript_overlay(&lines, false);
                let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                segment.repaint_frame(rows, cols as usize);
                carried = draft;
                continue;
            }
            // Unreachable with both key routes off, and the draft is replayed
            // rather than dropped because that is what a key which did
            // nothing owes the user (finding F76's replay, same mechanism).
            Ok(tui_bridge::InlineRead::OpenAgentView(draft))
            | Ok(tui_bridge::InlineRead::Detached(draft))
            | Ok(tui_bridge::InlineRead::CycleAgent(_, draft))
            | Ok(tui_bridge::InlineRead::AttachOrdinal(_, draft)) => {
                // Agent routes are inline-only. Here they are inert: replay the
                // draft instead of falling to `other`, which would map them to
                // Eof and end the session (review F3).
                carried = draft;
                continue;
            }
            other => break other,
        }
    };
    // The composer row is taken back *before* this returns, and the cursor
    // homed on it: whatever the submitted line starts -- a turn's streamed
    // output, a slash command's report -- is written at the cursor, and a
    // caret left in the middle of a drafted row would start the transcript
    // there.
    let (_cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    segment.clear_composer(rows);
    read_chat_line_inline_outcome(outcome)
}

/// The fullscreen segment's [`tui_bridge::SuspendEffects`]: the composer row
/// for the screen and this frame's `RawStdinGuard` slot for termios.
///
/// The screen halves are small because a fullscreen segment pins no region:
/// there is nothing latched to release, only a composer row that must not be
/// left on the screen the shell is about to write on, and the alternate
/// screen's contents are the terminal's to keep across the stop. The termios
/// halves are the inline effects' verbatim, because the obligation is: the
/// shell must not inherit a raw terminal.
#[cfg(unix)]
pub(crate) struct FullscreenSuspendEffects<'a> {
    pub(crate) segment: &'a mut tui_bridge::FullscreenSegment,
    pub(crate) raw: &'a mut Option<RawStdinGuard>,
    /// The terminal height as it was probed when the Ctrl+Z arrived.
    pub(crate) rows: u16,
}

#[cfg(unix)]
impl tui_bridge::SuspendEffects for FullscreenSuspendEffects<'_> {
    fn release_viewport(&mut self) {
        self.segment.release_for_suspend(self.rows);
    }

    fn restore_terminal_modes(&mut self) {
        // Dropping the guard is the restore, and it must precede the stop.
        self.raw.take();
    }

    fn stop_process(&mut self) {
        // SAFETY: raising a default-disposition job-control signal
        // in-process, exactly as the inline effects do. Blocks here until the
        // job is foregrounded again.
        unsafe {
            libc::raise(libc::SIGTSTP);
        }
    }

    fn reengage_terminal_modes(&mut self) -> bool {
        *self.raw = RawStdinGuard::engage();
        self.raw.is_some()
    }

    fn reinstall_viewport(&mut self) {
        // Nothing to re-latch, and nothing to paint: the next read repaints
        // the composer row as its first frame.
    }
}

/// The real [`tui_bridge::SuspendEffects`]: the viewport guard for the screen
/// and the frame's `RawStdinGuard` slot for termios (finding F21).
#[cfg(unix)]
pub(crate) struct InlineSuspendEffects<'a> {
    pub(crate) bridge: &'a tui_bridge::BridgeGuard,
    pub(crate) raw: &'a mut Option<RawStdinGuard>,
}

#[cfg(unix)]
impl tui_bridge::SuspendEffects for InlineSuspendEffects<'_> {
    fn release_viewport(&mut self) {
        self.bridge.release_for_suspend();
    }

    fn restore_terminal_modes(&mut self) {
        // Dropping the guard is the restore: canonical mode, `ISIG` back on
        // and bracketed paste off. It must happen before the stop, so the
        // shell does not inherit a raw terminal.
        self.raw.take();
    }

    fn stop_process(&mut self) {
        // SAFETY: raising a default-disposition job-control signal
        // in-process, exactly as the during-turn collector does. The region
        // is already released and termios already canonical, so the shell
        // takes over a normal full-screen terminal. This blocks here until
        // the job is foregrounded again (SIGCONT).
        unsafe {
            libc::raise(libc::SIGTSTP);
        }
    }

    fn reengage_terminal_modes(&mut self) -> bool {
        *self.raw = RawStdinGuard::engage();
        self.raw.is_some()
    }

    fn reinstall_viewport(&mut self) {
        self.bridge.reinstall_after_resume();
    }
}
