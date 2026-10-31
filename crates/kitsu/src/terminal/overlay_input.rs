//! Input decoding and editable lines for terminal overlays.

/// What one chunk of bytes did to the steer line (card T4-S4b).
#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))] // Bound by T4-S4b's loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SteerLineStep {
    /// Still editing: repaint the echo and the caret.
    Editing,
    /// `Enter` outside a paste: send this correction.
    Submit(String),
    /// A resolved lone `Esc`: leave the line, send nothing.
    Cancelled,
}

/// A line of text collected inside the overlay **and bound to one agent**:
/// `s`'s correction, and `n`'s name (card F70).
///
/// The name is `s`'s because `s` had it first; what the type *is*, is an
/// [`OverlayLine`] plus the id the line was opened on. `n` joined it when the
/// rename line moved off the decoded key prompt: a name may be non-ASCII and
/// `OverlayKey::Char` is ASCII-only by construction, so the keyboard could
/// not express a name the registry accepts (**F70**).
///
/// # Where the input lives, and why
///
/// It is `kitsu-editor`'s [`kitsu_editor::TypeaheadEditor`], driven from here.
/// `kitsu-tui` depends on `kitsu-render` alone and cannot reach the editor —
/// but the overlay's loop lives in this file, which does depend on it, so the
/// editor is reusable exactly where the bytes are read. The alternative was a
/// second line editor inside `kitsu-tui`, which would have had to re-solve
/// split UTF-8, bracketed paste and the `ESC` ambiguity of **F65** — a second
/// parser of the same bytes, which is what **F45** was closed by removing.
/// `kitsu-tui` keeps the *painting* (`agents_view::steer_prompt_rows`); this
/// keeps the editing; neither grows a copy of the other.
///
/// # `ESC`
///
/// It **cancels the line**, sending nothing, and it inherits T4-S4a's 50 ms
/// grace: inside this overlay `Esc` already means *step back out of the frame
/// you are in* (the help, a peek, the view), and the steer line is one more
/// frame. The grace is what keeps an arrow key from reading as a cancel —
/// **F65**, met again by a sub-mode that reads arbitrary bytes.
///
/// # A newline inside a paste
///
/// **Content.** T4-S2 decided a prefix byte inside a paste is content, and the
/// editor already holds that line for newlines: a bracketed paste never
/// submits. The `Enter` that submits is one typed outside a paste.
#[cfg(unix)]
pub(crate) struct SteerLine {
    pub(crate) line: OverlayLine,
    pub(crate) id: u64,
}

#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))] // Bound to `s` by T4-S4b's loop.
impl SteerLine {
    /// A fresh, empty line for one agent.
    pub(crate) fn new(id: u64) -> Self {
        Self {
            line: OverlayLine::new(),
            id,
        }
    }

    /// Which agent this line will steer, fixed when the line opened rather
    /// than re-read from the selection on submit: a repaint may have moved it.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// The echo, for `agents_view::steer_prompt_rows` and, for `n`'s line,
    /// `agents_view::rename_prompt_rows`.
    pub(crate) fn draft(&self) -> &str {
        self.line.draft()
    }

    /// The caret, as a char offset into [`SteerLine::draft`].
    pub(crate) fn cursor(&self) -> usize {
        self.line.cursor()
    }

    /// Put a refused submission back into the line, because the caller
    /// refused it and the line stays open: see [`OverlayLine::restore`].
    pub(crate) fn restore(&mut self, text: &str) {
        self.line.restore(text);
    }

    /// Feed one chunk of raw bytes.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> SteerLineStep {
        self.line.feed(bytes)
    }

    /// What the overlay does when the grace expires with no byte waiting.
    pub(crate) fn quiet_tick(&mut self) -> SteerLineStep {
        self.line.quiet_tick()
    }

    /// Whether a lone `ESC` is held, so the overlay knows to spend the grace
    /// period on it and nothing else.
    pub(crate) fn escape_held(&self) -> bool {
        self.line.escape_held()
    }
}

/// One line of text collected inside the overlay, over `kitsu-editor`.
///
/// Two keys open one of these: `s`'s correction (card T4-S4b-ii) and `/`'s
/// filter (card T4-S5). What they do with the submitted string differs and
/// lives at the call site; the *editing* is identical and lives here, so
/// there is one parser of these bytes and not two (**F45**).
#[cfg(unix)]
pub(crate) struct OverlayLine {
    pub(crate) editor: kitsu_editor::TypeaheadEditor,
    /// The editor's buffer as a string, refreshed by every method that can
    /// change it.
    ///
    /// The editor keeps its buffer as `Vec<char>` and spells it out on
    /// demand, so there is nothing inside it to borrow; the frames and the
    /// line's own callers want a `&str`. It is written at the one place the
    /// buffer is written and read nowhere else, so it is a derived field and
    /// not a second copy of the draft.
    pub(crate) echo: String,
}

#[cfg(unix)]
impl OverlayLine {
    /// A fresh, empty line. No history: neither of these lines is the
    /// composer, and recalling a prompt into a correction — or into a filter —
    /// would be a surprise with a real effect.
    pub(crate) fn new() -> Self {
        Self {
            editor: kitsu_editor::TypeaheadEditor::new(Vec::new()),
            echo: String::new(),
        }
    }

    /// The echo, for the frame that paints it.
    pub(crate) fn draft(&self) -> &str {
        &self.echo
    }

    /// Re-read the editor's buffer into [`OverlayLine::echo`].
    pub(crate) fn sync_echo(&mut self) {
        self.echo = self.editor.buffer_string();
    }

    /// The caret, as a char offset into [`OverlayLine::draft`].
    pub(crate) fn cursor(&self) -> usize {
        self.editor.cursor()
    }

    /// Put a submitted string back into the line, because the caller refused
    /// it: `/`'s parser rejects a malformed term and the line stays open, so
    /// the user corrects the one wrong character rather than retyping.
    pub(crate) fn restore(&mut self, text: &str) {
        self.editor.replace_buffer(text);
        self.sync_echo();
    }

    /// Feed one chunk of raw bytes.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> SteerLineStep {
        // The raw bytes, straight into the editor: that is what keeps the
        // bracketed-paste rule *the editor's*, so a pasted multi-line
        // correction cannot submit at its first newline. The overlay's own
        // `OverlayDecoder` is deliberately not in this path — it strips the
        // delimiters and reports `OverlayInput::Paste` (finding **F68**), and
        // unbracketed text fed to the editor would submit on that newline,
        // which is F68 again one layer down.
        let outcome = self.editor.feed(bytes);
        self.sync_echo();
        for action in outcome.actions {
            // Only `Submit` means anything here. The editor's other actions
            // belong to the composer's collector — history recall, EOF,
            // suspend, a repaint request — and a correction line has no use
            // for any of them, so they leave the line open rather than ending
            // it in a way the overlay never advertised.
            if let kitsu_editor::EditorAction::Submit(correction) = action {
                return SteerLineStep::Submit(correction);
            }
        }
        SteerLineStep::Editing
    }

    /// What the overlay does when the grace expires with no byte waiting: a
    /// held `ESC` was the `Esc` key.
    pub(crate) fn quiet_tick(&mut self) -> SteerLineStep {
        // `escape_pending` and not `resolve_lone_escape`'s bool: the latter
        // reports whether the *draft changed*, so it answers `false` for an
        // `Esc` pressed on an empty line — which is still a cancel.
        if !self.editor.escape_pending() {
            return SteerLineStep::Editing;
        }
        self.editor.resolve_lone_escape();
        self.sync_echo();
        SteerLineStep::Cancelled
    }

    /// Whether a lone `ESC` is held, so the overlay knows to spend the grace
    /// period on it and nothing else.
    pub(crate) fn escape_held(&self) -> bool {
        self.editor.escape_pending()
    }
}

/// How long the overlay waits for the rest of an escape sequence before it
/// treats a trailing `ESC` byte as the `Esc` key.
///
/// A terminal writes `ESC [ B` in one go, so anything still absent after this
/// was never coming. The value matches the editor collector's own ~50ms poll
/// tick, so the two surfaces resolve a lone `Esc` on the same timescale.
#[cfg(unix)]
pub(crate) const ESC_SEQUENCE_GRACE: std::time::Duration = std::time::Duration::from_millis(50);

/// Where the overlay's byte decoder is in an escape sequence. The overlay
/// reads bytes, not keys, so this is the whole difference between `Esc` and
/// the first byte of `\u{2191}`.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OverlayPending {
    None,
    /// An `ESC` has arrived and nothing after it yet. Still ambiguous.
    Escape,
    /// `ESC [` -- a CSI sequence, consumed up to and including its final byte.
    Csi,
    /// `ESC O` -- the application-keypad spelling of the same arrows.
    Ss3,
}

/// One key the agent view understands, decoded from the byte stream.
///
/// A printable byte is reported as the character it is rather than as a named
/// key, because the same byte means two things depending on the frame: `n` is
/// *rename* over the row list and the letter `n` inside a name. Which it is
/// belongs to the loop, which knows the frame; the decoder's job is the one
/// thing a `match` on bytes cannot do at the call site -- telling `Esc` from
/// the first byte of an arrow (card T4-S4a, finding F65).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OverlayKey {
    Up,
    Down,
    /// `\u{2192}`: attach to the selected agent, the synonym requirement A3
    /// gives `Enter` (card T4-S6a). Decoded from `ESC [ C` and `ESC O C`, the
    /// two spellings `arrow` already reads `Up` and `Down` from.
    Right,
    /// A resolved `Esc`: step back out of the frame on screen.
    StepBack,
    /// `Enter`: submit whatever prompt is open. Inert over the row list.
    Enter,
    /// `Backspace` or `Delete`: take a character back out of a prompt.
    Backspace,
    /// `Tab` (0x09). Completes the model popup's selection; inert elsewhere.
    Tab,
    /// `Ctrl-C` (0x03). Raw mode clears ISIG, so this arrives as a byte, not a
    /// signal. Cancels the model popup; inert elsewhere.
    Interrupt,
    /// One printable ASCII character. Non-ASCII bytes are dropped: the view's
    /// prompts are a name and a correction typed into an overlay that paints
    /// plain ASCII rows, and a half-decoded UTF-8 character has no business
    /// reaching either (`kitsu-editor` owns that problem for the composer).
    Char(char),
}

/// Decode a chunk of raw bytes into the keys the overlay acts on, carrying
/// `pending` across chunks because a read can split a sequence.
///
/// The one thing this exists to get right: an arrow key is **not one byte**.
/// `ESC` alone is only *possibly* `Esc`, so it is held pending and resolved
/// either by the next byte (part of a sequence, or a key in its own right
/// after the `Esc`) or by the caller's quiet grace period. Bytes that are not
/// keys here are dropped rather than forwarded: the overlay owns the keyboard
/// while it is up, and a stray byte reaching the composer underneath is the
/// defect this decoder replaces.
#[cfg(unix)]
pub(crate) fn decode_overlay_keys(bytes: &[u8], pending: &mut OverlayPending) -> Vec<OverlayKey> {
    /// The arrows, in both the CSI and the SS3 spelling, from the final byte.
    fn arrow(final_byte: u8) -> Option<OverlayKey> {
        match final_byte {
            b'A' => Some(OverlayKey::Up),
            b'B' => Some(OverlayKey::Down),
            // Card **T4-S6a**: `\u{2192}`, the synonym requirement A3 gives
            // `Enter`. Both spellings reach here, so one arm covers
            // `ESC [ C` and the application-cursor-mode `ESC O C` alike.
            b'C' => Some(OverlayKey::Right),
            _ => None,
        }
    }
    fn plain(byte: u8) -> Option<OverlayKey> {
        match byte {
            b'\r' | b'\n' => Some(OverlayKey::Enter),
            0x7f | 0x08 => Some(OverlayKey::Backspace),
            b'\t' => Some(OverlayKey::Tab),
            0x03 => Some(OverlayKey::Interrupt),
            0x20..=0x7e => Some(OverlayKey::Char(byte as char)),
            _ => None,
        }
    }
    let mut keys = Vec::new();
    for &byte in bytes {
        match *pending {
            OverlayPending::Escape => {
                *pending = OverlayPending::None;
                match byte {
                    b'[' => *pending = OverlayPending::Csi,
                    b'O' => *pending = OverlayPending::Ss3,
                    // `ESC ESC`: the first was a real `Esc`, the second is the
                    // next one's ambiguity.
                    0x1b => {
                        keys.push(OverlayKey::StepBack);
                        *pending = OverlayPending::Escape;
                    }
                    // A key that arrived too late to be part of a sequence:
                    // the `Esc` happened, and then the key did.
                    other => {
                        keys.push(OverlayKey::StepBack);
                        keys.extend(plain(other));
                    }
                }
            }
            // Parameter and intermediate bytes carry no key; the final byte
            // (0x40..=0x7e) ends the sequence.
            OverlayPending::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    *pending = OverlayPending::None;
                    keys.extend(arrow(byte));
                }
            }
            OverlayPending::Ss3 => {
                *pending = OverlayPending::None;
                keys.extend(arrow(byte));
            }
            OverlayPending::None => match byte {
                0x1b => *pending = OverlayPending::Escape,
                other => keys.extend(plain(other)),
            },
        }
    }
    keys
}

/// One thing the overlay found in a chunk of raw bytes: a **keystroke**, or a
/// **paste** — text that arrived as a burst and is therefore content, never a
/// command (finding **F68**).
///
/// # Why a second decoded form at all
///
/// [`decode_overlay_keys`] answers "what keys are in these bytes", which is
/// the right question for `↑`, `Esc` and `?` — and the wrong one the moment a
/// key has an effect. `r` then `y` answers a pending permission and `x` twice
/// stops an agent, so a chunk of pasted prose containing those letters would
/// *act*: **a chunk of bytes is not a sequence of intentions**, which is
/// **F65**'s root with the opposite sign.
///
/// Pasted text is recognised two ways, because one of them is not always
/// available:
///
/// 1. **Bracketed paste.** The overlay owns the terminal while it is up and
///    already writes its own control sequences there, so it enables `ESC[?2004h`
///    on entry and disables it on exit, exactly as the inline reader does
///    outside rustyline (`tui_bridge`'s `KeyFilter`). The payload between
///    `ESC[200~` and `ESC[201~` is one [`OverlayInput::Paste`], and the block
///    may be split across reads, so the state rides in `pending`.
/// 2. **A burst, with no brackets.** A terminal that does not report pastes
///    delivers them as plain bytes, so a chunk carrying **more than one**
///    printable character or newline is a burst and not typing: no human
///    produces two printable keystrokes in one read. That chunk is one
///    `Paste` too, carrying its text.
///
/// Everything else is unchanged: one printable byte is one `Key`, and `ESC`'s
/// pending state and its 50 ms lone-`Esc` grace (**F65**, card T4-S4a) are
/// untouched — a chunk that is exactly one arrow is still one `Key`.
#[cfg(unix)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OverlayInput {
    Key(OverlayKey),
    /// Burst text, verbatim and newlines included.
    Paste(String),
}

/// The overlay's byte decoder, carrying everything that cannot be decided
/// from one chunk: the `ESC` ambiguity of **F65**, a bracketed-paste block in
/// progress, and a delimiter split across two reads.
///
/// It is a struct rather than [`decode_overlay_keys`]'s `&mut OverlayPending`
/// because a paste block has *text* in flight, not just a state — the same
/// reason `tui_bridge::KeyFilter` holds `held` bytes.
///
/// This replaces [`decode_overlay_keys`] at the overlay's read site: the loop
/// must route a `Paste` as text, which means **content** inside a text prompt
/// (newline and all — a paste never submits, `kitsu-editor`'s line and
/// T4-S2's) and **nothing at all** over the row list or inside `r`'s decision
/// read, where a paste has no meaning and acting on it would be a privileged
/// action the user never asked for.
#[cfg(unix)]
pub(crate) struct OverlayDecoder {
    pub(crate) pending: OverlayPending,
    /// The parameter and intermediate bytes of the CSI sequence in progress,
    /// which is the whole reason this is a struct and not a `Copy` enum: the
    /// difference between an arrow and a paste delimiter is `200`/`201`
    /// *before* the final `~`, and a read can cut a delimiter anywhere.
    ///
    /// Recognising the delimiters inside the CSI state is deliberate. The
    /// obvious alternative — holding a partial `ESC[200~` prefix — would
    /// silently kill **F65**'s lone-`ESC` grace, because `ESC` alone is a
    /// strict prefix of `ESC[200~`: a held `ESC` would stop being observable
    /// as a pending `Esc` and the key would stop working.
    pub(crate) csi: Vec<u8>,
    /// The payload of the bracketed-paste block in progress, if one is open.
    /// A block may span reads, so the text rides here until `ESC[201~`.
    pub(crate) pasting: Option<Vec<u8>>,
}

#[cfg(unix)]
impl OverlayDecoder {
    /// The bracketed-paste delimiters' CSI parameters: `ESC[200~` opens a
    /// pasted block and `ESC[201~` closes it.
    pub(crate) const PASTE_START_PARAMS: &'static [u8] = b"200";
    pub(crate) const PASTE_END_PARAMS: &'static [u8] = b"201";

    pub(crate) fn new() -> Self {
        Self {
            pending: OverlayPending::None,
            csi: Vec::new(),
            pasting: None,
        }
    }

    /// One `read` of the descriptor.
    ///
    /// Each byte is handed to [`decode_overlay_keys`], which stays the single
    /// authority on the `ESC`/CSI/SS3 transitions and on what a plain byte
    /// means (finding **F45**: one parser, not two). What this adds around it
    /// is the one thing keys cannot express — *where the human's intent
    /// ended*:
    ///
    /// * the CSI parameters are observed as they go by, so `ESC[200~` and
    ///   `ESC[201~` are recognised without a second state machine and without
    ///   holding an `ESC` prefix;
    /// * bytes inside an open block are payload, not keys;
    /// * and a chunk that yields more than one printable key or newline is a
    ///   burst on a terminal that does not report pastes, so it collapses into
    ///   one [`OverlayInput::Paste`] carrying its text.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<OverlayInput> {
        let mut inputs: Vec<OverlayInput> = Vec::new();
        for &byte in bytes {
            // Payload: inside an open block every byte is text. `ESC` is the
            // exception, because the closing delimiter starts with one.
            if self.pasting.is_some() && self.pending == OverlayPending::None && byte != 0x1b {
                if let Some(text) = self.pasting.as_mut() {
                    text.push(byte);
                }
                continue;
            }
            let was = self.pending;
            let keys = decode_overlay_keys(&[byte], &mut self.pending);
            let mut delimiter = false;
            match was {
                // `ESC [`: a CSI sequence begins, so its parameters do too.
                OverlayPending::Escape if byte == b'[' => self.csi.clear(),
                OverlayPending::Csi => {
                    if self.pending == OverlayPending::Csi {
                        // Still a parameter or intermediate byte.
                        self.csi.push(byte);
                    } else if byte == b'~' {
                        delimiter = match self.csi.as_slice() {
                            Self::PASTE_START_PARAMS => {
                                self.pasting = Some(Vec::new());
                                true
                            }
                            Self::PASTE_END_PARAMS => {
                                if let Some(text) = self.pasting.take() {
                                    // Lossy: the payload is whatever the
                                    // terminal sent, and a prompt echoing
                                    // plain ASCII rows is no place for a
                                    // half-decoded character.
                                    inputs.push(OverlayInput::Paste(
                                        String::from_utf8_lossy(&text).into_owned(),
                                    ));
                                }
                                true
                            }
                            _ => false,
                        };
                        self.csi.clear();
                    } else {
                        self.csi.clear();
                    }
                }
                _ => {}
            }
            // A delimiter yields no keys anyway (`~` is not an arrow); an
            // `ESC` sequence *inside* a block is not content either, and its
            // keys are not the user's keystrokes, so neither is forwarded.
            if !delimiter && self.pasting.is_none() {
                inputs.extend(keys.into_iter().map(OverlayInput::Key));
            }
        }
        Self::collapse_a_burst(inputs)
    }

    /// The unbracketed half of the rule: a terminal with no paste reporting
    /// delivers a paste as plain bytes, so a chunk carrying **more than one**
    /// printable character or newline is a burst and not typing — no human
    /// produces two printable keystrokes inside one `read`.
    ///
    /// The heuristic is deliberately one-sided. Its false positive is a very
    /// fast typist whose two keys land in one read, and then the keys are
    /// dropped rather than acted on; its false negative would be a paste
    /// executed as privileged actions, which is finding **F68** itself. The
    /// dropped case is reported on screen by the loop, so "nothing happened"
    /// is still distinguishable from "it worked" (requirement T2-S5b).
    pub(crate) fn collapse_a_burst(inputs: Vec<OverlayInput>) -> Vec<OverlayInput> {
        let text_key = |input: &OverlayInput| {
            matches!(
                input,
                OverlayInput::Key(OverlayKey::Char(_)) | OverlayInput::Key(OverlayKey::Enter)
            )
        };
        if inputs.iter().filter(|input| text_key(input)).count() < 2 {
            return inputs;
        }
        let first = inputs
            .iter()
            .position(text_key)
            .expect("a counted text key has a position");
        let burst: String = inputs
            .iter()
            .filter_map(|input| match input {
                OverlayInput::Key(OverlayKey::Char(typed)) => Some(*typed),
                OverlayInput::Key(OverlayKey::Enter) => Some('\n'),
                _ => None,
            })
            .collect();
        let bracketed = |input: &&OverlayInput| matches!(input, OverlayInput::Paste(_));
        let mut collapsed: Vec<OverlayInput> = inputs
            .iter()
            .take(first)
            .filter(bracketed)
            .cloned()
            .collect();
        collapsed.push(OverlayInput::Paste(burst));
        collapsed.extend(inputs.iter().skip(first).filter(bracketed).cloned());
        collapsed
    }

    /// Whether a lone `ESC` is held, still ambiguous — the question the 50 ms
    /// grace answers (**F65**, card T4-S4a). A chunk that ends inside a paste
    /// block or part-way through a delimiter is **not** a held `Esc`.
    pub(crate) fn escape_pending(&self) -> bool {
        self.pending == OverlayPending::Escape && self.pasting.is_none()
    }

    /// The grace expired with nothing waiting: the held `ESC` was the `Esc`
    /// key, and the caller is about to act on it.
    pub(crate) fn resolve_escape(&mut self) {
        self.pending = OverlayPending::None;
    }
}

/// The prompt the overlay has open, if any (card T4-S4b). A prompt is the
/// innermost frame of the view: `Esc` leaves it before it leaves the help, a
/// peek or the view itself.
///
/// **It is a decision read and nothing else.** It once also held a line of
/// typed text, which is why it is an enum at all; card F70 took the last one
/// away, because a prompt is fed *decoded keys* and `OverlayKey::Char` is
/// ASCII-only by construction, so `n`'s name could never be a name the
/// registry accepts. The lines are fed **bytes**, through `kitsu-editor`, and
/// live in their own fields beside this one. `r`'s read legitimately stays
/// decoded: it is a single key, `y`, `a` or `n`, all ASCII.
///
/// The enum is kept rather than collapsed to an `Option<u64>` because
/// [`OverlayPrompt::report_line`] is what paints the decision row, and a bare
/// id would leave that behaviour with nowhere to live.
#[cfg(unix)]
pub(crate) enum OverlayPrompt {
    /// `r`: a **decision read**, not a text prompt. One key, resolved by
    /// [`crate::terminal::approval::parse_permission_answer`] -- the one parser (finding F45).
    Decision(u64),
}

#[cfg(unix)]
impl OverlayPrompt {
    /// The prompt as the one report row the frame has room for. Neither of
    /// the typed lines is one of these any more: `s`'s went to
    /// `agents_view::steer_prompt_rows` in card T4-S4b-ii and `n`'s to
    /// `agents_view::rename_prompt_rows` in card F70, both with a real caret
    /// over `kitsu-editor`.
    pub(crate) fn report_line(&self) -> String {
        match self {
            Self::Decision(id) => format!(
                "reply to background agent #{id}: y allow once, a allow always, n deny (esc cancels)"
            ),
        }
    }
}

/// Await one of the registry's `async` actions from the overlay's synchronous
/// key loop.
///
/// `AgentRegistry::stop` is `async` and this loop is not, and the overlay has
/// **two** call sites with different contexts: the inline reader's, which runs
/// off the runtime, and `handle_jobs_command`'s, which is an `async fn` and so
/// runs *on* a runtime worker. A bare `Handle::block_on` panics on the second
/// and `block_in_place` panics when there is no multi-threaded runtime around
/// at all, so neither is right for both. Driving the future on a thread of its
/// own is: the future leaves whichever thread it was called from, this one
/// blocks on the join, and the result is the same in both contexts. The work
/// the future awaits (an aborted task's `JoinHandle`) is driven by the
/// runtime that owns it, which is still running on its other workers.
#[cfg(unix)]
pub(crate) fn overlay_block_on<F>(future: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => std::thread::scope(|scope| {
            scope
                .spawn(|| handle.block_on(future))
                .join()
                .expect("the agent view's action thread")
        }),
        // No runtime in sight (a test, or a carrier that never built one): one
        // current-thread runtime for the duration of this one action.
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime for one agent view action")
            .block_on(future),
    }
}

/// Whether stdin has a byte waiting within `timeout`.
///
/// Used for one thing only: deciding that the rest of an escape sequence is
/// not coming, so a trailing `ESC` was the `Esc` key.
#[cfg(unix)]
pub(crate) fn stdin_waiting(timeout: std::time::Duration) -> bool {
    let mut remaining = timeout;
    loop {
        let mut fds = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let started = std::time::Instant::now();
        // SAFETY: one `pollfd` for a descriptor this process owns, with the
        // count matching the slice length.
        let ready = unsafe { libc::poll(&mut fds, 1, remaining.as_millis() as libc::c_int) };
        if ready >= 0 {
            return ready > 0;
        }
        // A signal woke the poll (a `SIGWINCH`, typically). Waiting out the
        // rest of the grace is the safe answer: resolving the `ESC` here would
        // close the view on an arrow key that a resize interrupted.
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
        remaining = remaining.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return false;
        }
    }
}
