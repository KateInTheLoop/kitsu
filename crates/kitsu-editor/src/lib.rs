//! The pure keymap and line editor behind kitsu's during-turn input.
//!
//! This crate holds nothing but the bytes-in → state/actions-out editor
//! state machine: the draft buffer and cursor, history navigation with a
//! draft stash, reverse-i-search, bracketed paste, and the key bindings
//! that drive them. It owns no terminal, performs no I/O, and has no
//! dependencies at all.
//!
//! It lives in its own library crate because two consumers need it and a
//! library cannot import from a binary: the `kitsu` binary's raw-mode input
//! collector, and the terminal-UI crate that renders the prompt row. The
//! editor was previously a private module of the binary, which made the
//! second consumer impossible.
//!
//! Raw mode and terminal I/O stay in the collector, which is what keeps the
//! whole keymap unit-testable as bytes in, state and actions out.
//!
//! # Supported keys
//!
//! Printable UTF-8 insert, Enter (submit), Backspace, Delete, ←/→/Home/End
//! (also Ctrl+B/F/A/E), ↑/↓ history walk with a draft stash, Ctrl+U (kill to
//! start), Ctrl+K (kill to end), Ctrl+W and Alt+Backspace (delete word
//! behind), Alt+D and Ctrl+Delete (delete word ahead), Alt+B/F and Ctrl+←/→
//! (word motion), Ctrl+D (EOF on an empty draft, delete otherwise), and
//! Alt+Enter (literal newline).
//!
//! Tab requests inline-hint completion from the collector; the editor itself
//! is hint-agnostic.
//!
//! Lone Esc clears the draft. The collector resolves it through
//! [`TypeaheadEditor::resolve_lone_escape`] after two consecutive quiet
//! ~50ms poll timeouts, so real escape sequences, which arrive atomically,
//! are unaffected. A link slow enough to split a sequence across >100ms will
//! misread it — the standard escape-time tradeoff every terminal editor
//! makes.
//!
//! Ctrl+R opens reverse-i-search over history: Ctrl+R cycles older matches,
//! printable input extends the query, Backspace shrinks it, Enter accepts the
//! match into the draft without submitting, Ctrl+G cancels and restores the
//! pre-search draft, and any other control key accepts then acts.
//!
//! A bracketed paste (`ESC[200~`…`ESC[201~`) stays one draft block: its
//! newlines insert literally instead of submitting. Unknown escape sequences
//! are consumed and ignored so they never leak into the draft.

/// A side effect the collector must perform after feeding bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorAction {
    /// The user pressed Enter: the completed line (untrimmed).
    Submit(String),
    /// Ctrl+D on an empty draft: treat as end of input.
    Eof,
    /// Tab outside a paste: the collector may accept an inline
    /// completion hint (the editor itself knows nothing about hints).
    CompleteRequest,
    /// ↑ on an empty, walk-free draft: the collector may recall a queued
    /// prompt into the editor first (the editor knows nothing about the
    /// prompt queue); if it declines, it calls
    /// [`TypeaheadEditor::history_prev_forced`] for the plain history walk.
    RecallRequest,
    /// Ctrl+C on an EMPTY draft: the user wants the turn cancelled. (With a
    /// non-empty draft Ctrl+C clears the draft instead; during a search it
    /// aborts the search. Raw mode masks ISIG, so the collector re-raises
    /// the real SIGINT for this action.)
    Interrupt,
    /// Ctrl+Z: suspend kitsu. The collector restores canonical termios
    /// FIRST, then raises the real SIGTSTP; on resume the raw guard
    /// re-engages exactly like after an approval handoff.
    Suspend,
    /// Ctrl+\: quit with SIGQUIT. The collector restores canonical termios
    /// first so the terminal is sane after the default core-dump exit.
    Quit,
}

/// What one [`TypeaheadEditor::feed`] call produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FeedOutcome {
    /// The visible draft (buffer or cursor) changed and needs a redraw.
    pub changed: bool,
    pub actions: Vec<EditorAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// No partial sequence outstanding.
    None,
    /// A lone ESC arrived; the next byte decides CSI/SS3/discard.
    Escape,
    /// Inside `ESC [` collecting parameter/intermediate bytes.
    Csi,
    /// `ESC O` single-byte function keys (application cursor mode).
    Ss3,
}

/// Reverse-i-search state (Ctrl+R). The draft buffer shows the current
/// match while searching; the pre-search draft is restored on cancel.
struct SearchState {
    query: Vec<char>,
    saved_buffer: Vec<char>,
    saved_cursor: usize,
    /// History index of the current match (`None` while nothing matches).
    match_index: Option<usize>,
}

pub struct TypeaheadEditor {
    buffer: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Index into `history` while walking with ↑/↓; `None` means editing the
    /// live draft (which is stashed on the first ↑).
    history_cursor: Option<usize>,
    stash: Vec<char>,
    pending: Pending,
    csi_params: Vec<u8>,
    /// Partial UTF-8 sequence awaiting continuation bytes.
    utf8: Vec<u8>,
    /// Inside a bracketed paste (`ESC[200~` … `ESC[201~`): newlines insert
    /// literally instead of submitting, so a paste stays one draft block.
    in_paste: bool,
    /// The previous pasted byte, for collapsing `\r\n` into one newline.
    last_paste_byte: u8,
    /// Active Ctrl+R reverse search, if any.
    search: Option<SearchState>,
    /// A RecallRequest was already emitted in the current `feed` batch.
    /// Without this gate, ↑↑ arriving in one read would emit twice and the
    /// collector's second queue-pop would overwrite (lose) the first
    /// recalled prompt.
    recall_emitted: bool,
}

impl TypeaheadEditor {
    /// A fresh editor over `history` with an empty draft.
    pub fn new(history: Vec<String>) -> Self {
        Self::with_draft(history, String::new())
    }

    /// Start with `draft` already in the buffer (cursor at its end) — used to
    /// restore typing carried across an approval prompt or a queued turn.
    pub fn with_draft(history: Vec<String>, draft: String) -> Self {
        let buffer: Vec<char> = draft.chars().collect();
        let cursor = buffer.len();
        Self {
            buffer,
            cursor,
            history,
            history_cursor: None,
            stash: Vec::new(),
            pending: Pending::None,
            csi_params: Vec::new(),
            utf8: Vec::new(),
            in_paste: false,
            last_paste_byte: 0,
            search: None,
            recall_emitted: false,
        }
    }

    pub fn buffer_string(&self) -> String {
        self.buffer.iter().collect()
    }

    /// Cursor position as a char index into [`Self::buffer_string`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Surrender the unsubmitted draft (for carrying into the next prompt).
    /// An active search is cancelled first: the carried draft is the user's
    /// real typing, never the transient match display.
    pub fn take_draft(mut self) -> String {
        let mut outcome = FeedOutcome::default();
        self.search_exit(false, &mut outcome);
        self.buffer.into_iter().collect()
    }

    /// Replace the whole draft (cursor at the end) — used when the collector
    /// accepts a completion hint. Ends any history walk; the stash is kept
    /// draft-shaped no longer, so it is cleared.
    pub fn replace_buffer(&mut self, text: &str) {
        self.buffer = text.chars().collect();
        self.cursor = self.buffer.len();
        self.history_cursor = None;
        self.stash.clear();
    }

    /// Status line for an active Ctrl+R search, rendered by the collector in
    /// the row's hint slot (display-only): `⌕ reverse-search 'query'`, with a
    /// `(no match)` marker when nothing in history contains the query.
    pub fn search_status(&self) -> Option<String> {
        self.search.as_ref().map(|search| {
            let query: String = search.query.iter().collect();
            if search.match_index.is_some() || search.query.is_empty() {
                format!("  ⌕ reverse-search '{query}'")
            } else {
                format!("  ⌕ reverse-search '{query}' (no match)")
            }
        })
    }

    /// Enter search mode, or cycle to the next-older match when already in
    /// it (Ctrl+R semantics).
    fn search_enter_or_cycle(&mut self, outcome: &mut FeedOutcome) {
        match &mut self.search {
            None => {
                self.search = Some(SearchState {
                    query: Vec::new(),
                    saved_buffer: std::mem::take(&mut self.buffer),
                    saved_cursor: self.cursor,
                    match_index: None,
                });
                self.cursor = 0;
                self.history_cursor = None;
                self.stash.clear();
                outcome.changed = true;
            }
            Some(_) => self.search_refresh(true, outcome),
        }
    }

    /// Re-run the search. `older` continues past the current match; otherwise
    /// the search restarts from the newest entry (query changed).
    fn search_refresh(&mut self, older: bool, outcome: &mut FeedOutcome) {
        let Some(search) = &mut self.search else {
            return;
        };
        let query: String = search.query.iter().collect();
        let start = match (older, search.match_index) {
            (true, Some(index)) => index,
            _ => self.history.len(),
        };
        search.match_index = self.history[..start]
            .iter()
            .rposition(|entry| !query.is_empty() && entry.contains(&query));
        // The draft shows the match; an unmatched query leaves the previous
        // match visible only if cycling found nothing older (readline keeps
        // the last match), else clears.
        if let Some(index) = search.match_index {
            self.buffer = self.history[index].chars().collect();
        } else if !older {
            self.buffer.clear();
        }
        self.cursor = self.buffer.len();
        outcome.changed = true;
    }

    /// Leave search mode. `keep_match` keeps the matched text as the draft;
    /// otherwise the pre-search draft and cursor are restored.
    fn search_exit(&mut self, keep_match: bool, outcome: &mut FeedOutcome) {
        let Some(search) = self.search.take() else {
            return;
        };
        if !keep_match {
            self.buffer = search.saved_buffer;
            self.cursor = search.saved_cursor.min(self.buffer.len());
        }
        outcome.changed = true;
    }

    /// Resolve a lone Esc keypress: when an `ESC` byte arrived with no
    /// follow-up (the collector calls this on a poll tick with no input),
    /// it was a real Esc key, not a sequence start — clear the whole draft
    /// (and any history walk). Returns whether the visible draft changed.
    /// Never fires mid-paste: a paste stalling between its `ESC` and the
    /// rest of the terminator must not wipe the pasted draft.
    pub fn resolve_lone_escape(&mut self) -> bool {
        if self.pending != Pending::Escape || self.in_paste {
            return false;
        }
        self.pending = Pending::None;
        if self.buffer.is_empty() {
            return false;
        }
        self.buffer.clear();
        self.cursor = 0;
        self.history_cursor = None;
        self.stash.clear();
        true
    }

    /// Drop all transient input state (paste mode, partial escape/UTF-8
    /// sequences). Called when the editor loses stdin mid-stream — e.g. an
    /// approval prompt takes over and may consume a paste's terminator — so
    /// a latched `in_paste` can never survive the handoff and eat every
    /// later Enter. The draft buffer and history are untouched.
    pub fn reset_pending_input(&mut self) {
        let mut outcome = FeedOutcome::default();
        self.search_exit(false, &mut outcome);
        self.in_paste = false;
        self.last_paste_byte = 0;
        self.pending = Pending::None;
        self.csi_params.clear();
        self.utf8.clear();
    }

    /// Feed raw stdin bytes; returns whether a redraw is needed and any
    /// submissions/EOF that completed.
    pub fn feed(&mut self, bytes: &[u8]) -> FeedOutcome {
        self.recall_emitted = false;
        let mut outcome = FeedOutcome::default();
        for &byte in bytes {
            self.feed_byte(byte, &mut outcome);
        }
        outcome
    }

    fn feed_byte(&mut self, byte: u8, outcome: &mut FeedOutcome) {
        match self.pending {
            Pending::Escape => {
                self.pending = Pending::None;
                match byte {
                    b'[' => {
                        self.pending = Pending::Csi;
                        self.csi_params.clear();
                    }
                    b'O' => self.pending = Pending::Ss3,
                    // Alt+Enter (ESC then CR/LF): insert a literal newline so
                    // multi-line prompts can be composed by hand.
                    b'\r' | b'\n' => self.insert_char('\n', outcome),
                    // Readline meta keys: Alt+B/F word motion, Alt+D deletes
                    // the word ahead, Alt+Backspace the word behind. Inside a
                    // paste these bytes are content, not keys; the fallback
                    // below re-feeds them into the paste path.
                    b'b' if !self.in_paste => self.move_word_left(outcome),
                    b'f' if !self.in_paste => self.move_word_right(outcome),
                    b'd' if !self.in_paste => self.delete_word_right(outcome),
                    0x7f | 0x08 if !self.in_paste => self.delete_word_left(outcome),
                    // A bare ESC followed by an ordinary key: drop the ESC
                    // and process the key normally.
                    _ => self.feed_byte(byte, outcome),
                }
                return;
            }
            Pending::Csi => {
                if (0x30..=0x3f).contains(&byte) || (0x20..=0x2f).contains(&byte) {
                    self.csi_params.push(byte);
                    return;
                }
                self.pending = Pending::None;
                if (0x40..=0x7e).contains(&byte) {
                    self.dispatch_csi(byte, outcome);
                }
                return;
            }
            Pending::Ss3 => {
                self.pending = Pending::None;
                // Pasted content can carry SS3 sequences (e.g. a captured
                // transcript); inside a paste they are data-noise, not keys.
                if self.in_paste {
                    return;
                }
                match byte {
                    b'A' => self.history_prev(outcome),
                    b'B' => self.history_next(outcome),
                    b'C' => self.move_right(outcome),
                    b'D' => self.move_left(outcome),
                    b'H' => self.move_home(outcome),
                    b'F' => self.move_end(outcome),
                    _ => {}
                }
                return;
            }
            Pending::None => {}
        }

        if !self.utf8.is_empty() {
            if byte & 0xc0 == 0x80 {
                self.utf8.push(byte);
                let expected = utf8_expected_len(self.utf8[0]);
                if self.utf8.len() == expected {
                    let taken = std::mem::take(&mut self.utf8);
                    if let Ok(text) = std::str::from_utf8(&taken) {
                        if let Some(ch) = text.chars().next() {
                            self.input_char(ch, outcome);
                        }
                    }
                }
                return;
            }
            // Malformed sequence: discard it and process this byte fresh.
            self.utf8.clear();
        }

        // Bracketed paste: everything up to `ESC[201~` is content, one block.
        // Newlines insert literally (`\r\n` collapsed to one), tabs are kept,
        // other control bytes are dropped. ESC still parses normally so the
        // paste terminator (and any stray sequences) never enter the draft.
        if self.in_paste && byte != 0x1b {
            let previous = std::mem::replace(&mut self.last_paste_byte, byte);
            match byte {
                b'\r' => self.insert_char('\n', outcome),
                b'\n' => {
                    if previous != b'\r' {
                        self.insert_char('\n', outcome);
                    }
                }
                b'\t' => self.insert_char('\t', outcome),
                0x00..=0x1f | 0x7f => {}
                _ => {
                    match utf8_expected_len(byte) {
                        1 => self.insert_char(byte as char, outcome),
                        // Stray continuation/invalid lead bytes (0x80..):
                        // dropping them keeps C1 controls out of the draft.
                        0 => {}
                        _ => {
                            self.utf8.clear();
                            self.utf8.push(byte);
                        }
                    }
                }
            }
            return;
        }

        // Reverse-i-search intercepts everything: Ctrl+R cycles, Ctrl+G
        // cancels (restores the pre-search draft), Enter accepts the match
        // into the draft WITHOUT submitting, Backspace edits the query,
        // printable input extends it, and any other control key (arrows
        // included, via ESC) accepts the match first and then acts normally.
        if self.search.is_some() {
            match byte {
                0x12 => self.search_enter_or_cycle(outcome),
                // Ctrl+G and Ctrl+C both abort the search, restoring the
                // pre-search draft (readline behavior).
                0x07 | 0x03 => self.search_exit(false, outcome),
                b'\r' | b'\n' => self.search_exit(true, outcome),
                0x7f | 0x08 => {
                    if let Some(search) = &mut self.search {
                        search.query.pop();
                    }
                    self.search_refresh(false, outcome);
                }
                0x1b => {
                    self.search_exit(true, outcome);
                    self.pending = Pending::Escape;
                }
                0x00..=0x1f => {
                    self.search_exit(true, outcome);
                    self.feed_byte(byte, outcome);
                }
                _ => match utf8_expected_len(byte) {
                    1 => self.input_char(byte as char, outcome),
                    0 => {}
                    _ => {
                        self.utf8.clear();
                        self.utf8.push(byte);
                    }
                },
            }
            return;
        }

        match byte {
            0x1b => self.pending = Pending::Escape,
            b'\r' | b'\n' => {
                let line: String = std::mem::take(&mut self.buffer).into_iter().collect();
                self.cursor = 0;
                self.history_cursor = None;
                self.stash.clear();
                if !line.trim().is_empty()
                    && self.history.last().map(String::as_str) != Some(line.as_str())
                {
                    self.history.push(line.clone());
                }
                outcome.changed = true;
                outcome.actions.push(EditorAction::Submit(line));
            }
            0x7f | 0x08 => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.buffer.remove(self.cursor);
                    outcome.changed = true;
                }
            }
            0x04 => {
                if self.buffer.is_empty() {
                    outcome.actions.push(EditorAction::Eof);
                } else if self.cursor < self.buffer.len() {
                    self.buffer.remove(self.cursor);
                    outcome.changed = true;
                }
            }
            0x01 => self.move_home(outcome),
            0x05 => self.move_end(outcome),
            0x02 => self.move_left(outcome),
            0x06 => self.move_right(outcome),
            0x15 => {
                if self.cursor > 0 {
                    self.buffer.drain(..self.cursor);
                    self.cursor = 0;
                    outcome.changed = true;
                }
            }
            0x0b => {
                if self.cursor < self.buffer.len() {
                    self.buffer.truncate(self.cursor);
                    outcome.changed = true;
                }
            }
            0x17 => self.delete_word_left(outcome),
            0x09 => outcome.actions.push(EditorAction::CompleteRequest),
            0x12 => self.search_enter_or_cycle(outcome),
            0x1a => outcome.actions.push(EditorAction::Suspend),
            0x1c => outcome.actions.push(EditorAction::Quit),
            0x03 => {
                if self.buffer.is_empty() {
                    outcome.actions.push(EditorAction::Interrupt);
                } else {
                    // First Ctrl+C abandons the draft; the next one (empty
                    // draft) interrupts the turn.
                    self.buffer.clear();
                    self.cursor = 0;
                    self.history_cursor = None;
                    self.stash.clear();
                    outcome.changed = true;
                }
            }
            0x00..=0x1f => {
                // Other control bytes are ignored: control characters must
                // never enter the draft.
            }
            _ => {
                match utf8_expected_len(byte) {
                    1 => self.input_char(byte as char, outcome),
                    // Stray continuation/invalid lead bytes never become
                    // C1 controls in the draft.
                    0 => {}
                    _ => {
                        self.utf8.clear();
                        self.utf8.push(byte);
                    }
                }
            }
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8, outcome: &mut FeedOutcome) {
        // Inside a paste, escape sequences are content noise (a pasted script
        // capture full of `ESC[A` must not walk history or move the cursor);
        // only the paste terminator itself is honored.
        if self.in_paste {
            if final_byte == b'~' && self.csi_params.as_slice() == b"201" {
                self.in_paste = false;
            }
            return;
        }
        match final_byte {
            b'A' => self.history_prev(outcome),
            b'B' => self.history_next(outcome),
            // Ctrl+→ / Alt+→ (modifier params 5/3) jump by words.
            b'C' => match self.csi_params.as_slice() {
                b"1;5" | b"1;3" => self.move_word_right(outcome),
                _ => self.move_right(outcome),
            },
            b'D' => match self.csi_params.as_slice() {
                b"1;5" | b"1;3" => self.move_word_left(outcome),
                _ => self.move_left(outcome),
            },
            b'H' => self.move_home(outcome),
            b'F' => self.move_end(outcome),
            b'~' => match self.csi_params.as_slice() {
                b"1" | b"7" => self.move_home(outcome),
                b"3" => {
                    if self.cursor < self.buffer.len() {
                        self.buffer.remove(self.cursor);
                        outcome.changed = true;
                    }
                }
                // Ctrl+Delete: delete the word ahead.
                b"3;5" => self.delete_word_right(outcome),
                b"4" | b"8" => self.move_end(outcome),
                b"200" => {
                    self.in_paste = true;
                    self.last_paste_byte = 0;
                }
                b"201" => self.in_paste = false,
                _ => {}
            },
            _ => {}
        }
    }

    /// Route one decoded character: into the search query while searching,
    /// into the draft otherwise.
    fn input_char(&mut self, ch: char, outcome: &mut FeedOutcome) {
        if self.search.is_some() {
            if let Some(search) = &mut self.search {
                search.query.push(ch);
            }
            self.search_refresh(false, outcome);
        } else {
            self.insert_char(ch, outcome);
        }
    }

    fn insert_char(&mut self, ch: char, outcome: &mut FeedOutcome) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += 1;
        outcome.changed = true;
    }

    fn move_left(&mut self, outcome: &mut FeedOutcome) {
        if self.cursor > 0 {
            self.cursor -= 1;
            outcome.changed = true;
        }
    }

    fn move_right(&mut self, outcome: &mut FeedOutcome) {
        if self.cursor < self.buffer.len() {
            self.cursor += 1;
            outcome.changed = true;
        }
    }

    fn move_home(&mut self, outcome: &mut FeedOutcome) {
        if self.cursor != 0 {
            self.cursor = 0;
            outcome.changed = true;
        }
    }

    fn move_end(&mut self, outcome: &mut FeedOutcome) {
        if self.cursor != self.buffer.len() {
            self.cursor = self.buffer.len();
            outcome.changed = true;
        }
    }

    fn word_start(&self) -> usize {
        let mut index = self.cursor;
        while index > 0 && self.buffer[index - 1].is_whitespace() {
            index -= 1;
        }
        while index > 0 && !self.buffer[index - 1].is_whitespace() {
            index -= 1;
        }
        index
    }

    fn word_end(&self) -> usize {
        let mut index = self.cursor;
        while index < self.buffer.len() && self.buffer[index].is_whitespace() {
            index += 1;
        }
        while index < self.buffer.len() && !self.buffer[index].is_whitespace() {
            index += 1;
        }
        index
    }

    fn move_word_left(&mut self, outcome: &mut FeedOutcome) {
        let start = self.word_start();
        if start != self.cursor {
            self.cursor = start;
            outcome.changed = true;
        }
    }

    fn move_word_right(&mut self, outcome: &mut FeedOutcome) {
        let end = self.word_end();
        if end != self.cursor {
            self.cursor = end;
            outcome.changed = true;
        }
    }

    fn delete_word_left(&mut self, outcome: &mut FeedOutcome) {
        let start = self.word_start();
        if start < self.cursor {
            self.buffer.drain(start..self.cursor);
            self.cursor = start;
            outcome.changed = true;
        }
    }

    fn delete_word_right(&mut self, outcome: &mut FeedOutcome) {
        let end = self.word_end();
        if end > self.cursor {
            self.buffer.drain(self.cursor..end);
            outcome.changed = true;
        }
    }

    fn history_prev(&mut self, outcome: &mut FeedOutcome) {
        // The very first ↑ on an untouched draft is a recall point: queued
        // prompts (owned by the collector) take precedence over history.
        // At most one recall per feed batch — a second ↑ in the same read
        // walks history instead, exactly as it would in a separate batch
        // once the recall has landed in the draft.
        if self.buffer.is_empty() && self.history_cursor.is_none() && !self.recall_emitted {
            self.recall_emitted = true;
            outcome.actions.push(EditorAction::RecallRequest);
            return;
        }
        self.history_prev_forced(outcome);
    }

    /// The plain history walk, bypassing the queued-prompt recall point.
    /// Public so the collector can fall through to it when the queue is
    /// empty; any draft change is reported through `outcome.changed`.
    pub fn history_prev_forced(&mut self, outcome: &mut FeedOutcome) {
        let next_index = match self.history_cursor {
            None if self.history.is_empty() => return,
            None => {
                self.stash = std::mem::take(&mut self.buffer);
                self.history.len() - 1
            }
            Some(0) => return,
            Some(index) => index - 1,
        };
        self.history_cursor = Some(next_index);
        self.buffer = self.history[next_index].chars().collect();
        self.cursor = self.buffer.len();
        outcome.changed = true;
    }

    fn history_next(&mut self, outcome: &mut FeedOutcome) {
        let Some(index) = self.history_cursor else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_cursor = Some(index + 1);
            self.buffer = self.history[index + 1].chars().collect();
        } else {
            self.history_cursor = None;
            self.buffer = std::mem::take(&mut self.stash);
        }
        self.cursor = self.buffer.len();
        outcome.changed = true;
    }
}

fn utf8_expected_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        // Continuation or invalid lead byte: nothing sensible to expect.
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submit_actions(outcome: &FeedOutcome) -> Vec<&str> {
        outcome
            .actions
            .iter()
            .filter_map(|action| match action {
                EditorAction::Submit(line) => Some(line.as_str()),
                EditorAction::Eof
                | EditorAction::CompleteRequest
                | EditorAction::RecallRequest
                | EditorAction::Interrupt
                | EditorAction::Suspend
                | EditorAction::Quit => None,
            })
            .collect()
    }

    #[test]
    fn typing_inserts_at_cursor_and_enter_submits() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        let outcome = editor.feed(b"hello\r");
        assert_eq!(submit_actions(&outcome), vec!["hello"]);
        assert_eq!(editor.buffer_string(), "");
        assert_eq!(editor.cursor(), 0);
    }

    #[test]
    fn utf8_input_spanning_feeds_reassembles() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        let bytes = "fér".as_bytes();
        editor.feed(&bytes[..2]);
        editor.feed(&bytes[2..]);
        assert_eq!(editor.buffer_string(), "fér");
        assert_eq!(editor.cursor(), 3);
    }

    #[test]
    fn arrows_move_and_backspace_deletes_before_cursor() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"abcd");
        editor.feed(b"\x1b[D\x1b[D"); // left, left
        assert_eq!(editor.cursor(), 2);
        editor.feed(&[0x7f]); // backspace removes 'b'
        assert_eq!(editor.buffer_string(), "acd");
        editor.feed(b"\x1b[3~"); // delete removes 'c'
        assert_eq!(editor.buffer_string(), "ad");
        editor.feed(b"\x1b[H");
        assert_eq!(editor.cursor(), 0);
        editor.feed(b"\x1b[F");
        assert_eq!(editor.cursor(), 2);
    }

    #[test]
    fn word_motions_jump_and_delete_by_words() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"run the tests");
        editor.feed(b"\x1bb"); // Alt+B: start of "tests"
        assert_eq!(editor.cursor(), 8);
        editor.feed(b"\x1b[1;5D"); // Ctrl+Left: start of "the"
        assert_eq!(editor.cursor(), 4);
        editor.feed(b"\x1b[1;3C"); // Alt+Right: end of "the"
        assert_eq!(editor.cursor(), 7);
        editor.feed(b"\x1bf"); // Alt+F: end of "tests"
        assert_eq!(editor.cursor(), 13);
        editor.feed(b"\x1b\x7f"); // Alt+Backspace: drop "tests"
        assert_eq!(editor.buffer_string(), "run the ");
        editor.feed(b"\x1b[H\x1bd"); // Home, Alt+D: drop "run"
        assert_eq!(editor.buffer_string(), " the ");
        assert_eq!(editor.cursor(), 0);
        editor.feed(b"\x1b[3;5~"); // Ctrl+Delete: drop " the"
        assert_eq!(editor.buffer_string(), " ");
    }

    #[test]
    fn word_motion_bytes_inside_a_paste_are_content_not_keys() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"\x1b[200~one \x1bbtwo\x1b[201~");
        // The pasted ESC is dropped, its trailing 'b' lands as text and the
        // cursor never jumps mid-paste.
        assert_eq!(editor.buffer_string(), "one btwo");
        assert_eq!(editor.cursor(), 8);
    }

    #[test]
    fn history_walk_stashes_and_restores_the_draft() {
        let mut editor = TypeaheadEditor::new(vec!["first".to_string(), "second".to_string()]);
        editor.feed(b"draf");
        editor.feed(b"\x1b[A");
        assert_eq!(editor.buffer_string(), "second");
        editor.feed(b"\x1b[A");
        assert_eq!(editor.buffer_string(), "first");
        editor.feed(b"\x1b[A"); // at oldest: stays
        assert_eq!(editor.buffer_string(), "first");
        editor.feed(b"\x1b[B");
        assert_eq!(editor.buffer_string(), "second");
        editor.feed(b"\x1b[B"); // past newest: restore draft
        assert_eq!(editor.buffer_string(), "draf");
        assert_eq!(editor.cursor(), 4);
    }

    #[test]
    fn submit_appends_to_history_without_consecutive_duplicates() {
        let mut editor = TypeaheadEditor::new(vec!["old".to_string()]);
        editor.feed(b"new\r");
        editor.feed(b"new\r");
        // Post-submit the draft is empty, so ↑ goes through the recall
        // point; the forced walk is what the collector falls back to.
        let mut outcome = FeedOutcome::default();
        editor.history_prev_forced(&mut outcome);
        assert_eq!(editor.buffer_string(), "new");
        editor.feed(b"\x1b[A");
        assert_eq!(editor.buffer_string(), "old");
    }

    #[test]
    fn up_on_an_untouched_empty_draft_requests_recall_without_state_change() {
        let mut editor = TypeaheadEditor::new(vec!["old".to_string()]);
        let outcome = editor.feed(b"\x1b[A");
        assert_eq!(outcome.actions, vec![EditorAction::RecallRequest]);
        assert_eq!(editor.buffer_string(), "");
        // The collector's fallback then performs the plain walk.
        let mut forced = FeedOutcome::default();
        editor.history_prev_forced(&mut forced);
        assert!(forced.changed);
        assert_eq!(editor.buffer_string(), "old");
    }

    #[test]
    fn double_up_in_one_batch_emits_a_single_recall_then_walks_history() {
        let mut editor = TypeaheadEditor::new(vec!["old".to_string()]);
        // ↑↑ arriving in one read: exactly one RecallRequest (the collector
        // pops exactly one queued prompt — nothing can be overwritten), and
        // the second ↑ walks history as it would in a separate batch.
        let outcome = editor.feed(b"\x1b[A\x1b[A");
        let recalls = outcome
            .actions
            .iter()
            .filter(|action| **action == EditorAction::RecallRequest)
            .count();
        assert_eq!(recalls, 1);
        assert_eq!(editor.buffer_string(), "old");
        // A fresh batch re-arms the recall point once the draft is empty.
        editor.feed(b"\x15"); // Ctrl+U clears the walked entry
        editor.replace_buffer("");
        let outcome = editor.feed(b"\x1b[A");
        assert_eq!(outcome.actions, vec![EditorAction::RecallRequest]);
    }

    #[test]
    fn up_after_recall_into_the_draft_walks_history_normally() {
        let mut editor = TypeaheadEditor::new(vec!["old".to_string()]);
        // Collector recalled a queued prompt into the draft.
        editor.replace_buffer("queued prompt");
        let outcome = editor.feed(b"\x1b[A");
        assert!(
            outcome.actions.is_empty(),
            "non-empty draft never re-requests recall"
        );
        assert_eq!(editor.buffer_string(), "old");
        // ↓ restores the recalled prompt from the stash.
        editor.feed(b"\x1b[B");
        assert_eq!(editor.buffer_string(), "queued prompt");
    }

    #[test]
    fn ctrl_c_clears_a_draft_then_interrupts_when_empty() {
        let mut editor = TypeaheadEditor::new(vec!["old".to_string()]);
        editor.feed(b"half a thought");
        let outcome = editor.feed(&[0x03]);
        assert!(outcome.actions.is_empty(), "first Ctrl+C only clears");
        assert_eq!(editor.buffer_string(), "");
        assert_eq!(editor.cursor(), 0);
        let outcome = editor.feed(&[0x03]);
        assert_eq!(outcome.actions, vec![EditorAction::Interrupt]);
    }

    #[test]
    fn ctrl_c_aborts_a_search_restoring_the_draft_without_interrupt() {
        let mut editor = TypeaheadEditor::new(vec!["cargo build".to_string()]);
        editor.feed(b"my draft");
        editor.feed(&[0x12]);
        editor.feed(b"cargo");
        assert_eq!(editor.buffer_string(), "cargo build");
        let outcome = editor.feed(&[0x03]);
        assert!(outcome.actions.is_empty());
        assert!(editor.search_status().is_none());
        assert_eq!(editor.buffer_string(), "my draft");
    }

    #[test]
    fn ctrl_c_during_a_history_walk_clears_and_ends_the_walk() {
        let mut editor = TypeaheadEditor::new(vec!["old entry".to_string()]);
        editor.feed(b"x");
        editor.feed(b"\x1b[A"); // walk to "old entry"
        assert_eq!(editor.buffer_string(), "old entry");
        editor.feed(&[0x03]);
        assert_eq!(editor.buffer_string(), "");
        // ↓ must not resurrect the stash after the clear.
        editor.feed(b"\x1b[B");
        assert_eq!(editor.buffer_string(), "");
    }

    #[test]
    fn ctrl_z_and_ctrl_backslash_emit_job_control_actions() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"draft stays");
        let outcome = editor.feed(&[0x1a]);
        assert_eq!(outcome.actions, vec![EditorAction::Suspend]);
        // Suspension must not eat the draft.
        assert_eq!(editor.buffer_string(), "draft stays");
        let outcome = editor.feed(&[0x1c]);
        assert_eq!(outcome.actions, vec![EditorAction::Quit]);
    }

    #[test]
    fn ctrl_c_inside_a_paste_is_dropped_as_content_noise() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        let outcome = editor.feed(b"\x1b[200~ab\x03cd\x1b[201~");
        assert!(outcome.actions.is_empty());
        assert_eq!(editor.buffer_string(), "abcd");
    }

    #[test]
    fn ctrl_d_is_eof_only_on_an_empty_draft() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"ab\x1b[D");
        let outcome = editor.feed(&[0x04]); // deletes 'b'
        assert!(outcome.actions.is_empty());
        assert_eq!(editor.buffer_string(), "a");
        editor.feed(&[0x7f]);
        let outcome = editor.feed(&[0x04]);
        assert_eq!(outcome.actions, vec![EditorAction::Eof]);
    }

    #[test]
    fn kill_shortcuts_edit_the_line() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"queue the next task");
        editor.feed(&[0x17]); // Ctrl+W: drop "task"
        assert_eq!(editor.buffer_string(), "queue the next ");
        editor.feed(&[0x15]); // Ctrl+U: clear to start
        assert_eq!(editor.buffer_string(), "");
        editor.feed(b"abc\x01"); // Ctrl+A home
        editor.feed(&[0x0b]); // Ctrl+K: kill to end
        assert_eq!(editor.buffer_string(), "");
    }

    #[test]
    fn lone_escape_clears_the_draft_on_the_quiet_tick() {
        let mut editor = TypeaheadEditor::new(vec!["old".to_string()]);
        editor.feed(b"half-typed junk");
        editor.feed(b"\x1b");
        assert!(editor.resolve_lone_escape());
        assert_eq!(editor.buffer_string(), "");
        assert_eq!(editor.cursor(), 0);
        // Esc on an already-empty draft changes nothing.
        editor.feed(b"\x1b");
        assert!(!editor.resolve_lone_escape());
        // Without a pending ESC the resolver is inert.
        assert!(!editor.resolve_lone_escape());
    }

    #[test]
    fn escape_sequences_and_pastes_are_immune_to_the_lone_escape_resolver() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"abc");
        // Mid-sequence (ESC [ received, final byte pending): not a lone Esc.
        editor.feed(b"\x1b[");
        assert!(!editor.resolve_lone_escape());
        editor.feed(b"D"); // completes ←
        assert_eq!(editor.cursor(), 2);
        // Mid-paste stall on a bare ESC must never wipe the pasted draft.
        editor.feed(b"\x1b[200~pasted\x1b");
        assert!(!editor.resolve_lone_escape());
        editor.feed(b"[201~");
        assert!(editor.buffer_string().contains("pasted"));
    }

    #[test]
    fn escape_then_delayed_key_still_types_the_key() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"\x1b");
        // Quiet tick resolves the ESC (empty draft: no change)...
        assert!(!editor.resolve_lone_escape());
        // ...and the late key arrives as itself, not as Alt+key.
        editor.feed(b"x");
        assert_eq!(editor.buffer_string(), "x");
    }

    fn search_history() -> Vec<String> {
        vec![
            "cargo build".to_string(),
            "fix the parser".to_string(),
            "cargo test -p kitsu".to_string(),
        ]
    }

    #[test]
    fn ctrl_r_searches_cycles_and_accepts_without_submitting() {
        let mut editor = TypeaheadEditor::new(search_history());
        editor.feed(b"draft in progress");
        editor.feed(&[0x12]); // enter search: draft saved, row empty
        assert_eq!(editor.buffer_string(), "");
        assert!(editor
            .search_status()
            .unwrap()
            .contains("reverse-search ''"));
        let outcome = editor.feed(b"cargo");
        assert!(outcome.actions.is_empty());
        // Newest match first.
        assert_eq!(editor.buffer_string(), "cargo test -p kitsu");
        editor.feed(&[0x12]); // cycle older
        assert_eq!(editor.buffer_string(), "cargo build");
        // Enter accepts the match into the draft; it must NOT submit.
        let outcome = editor.feed(b"\r");
        assert!(outcome.actions.is_empty());
        assert!(editor.search_status().is_none());
        assert_eq!(editor.buffer_string(), "cargo build");
        // The next Enter submits normally.
        let outcome = editor.feed(b"\r");
        assert_eq!(submit_actions(&outcome), vec!["cargo build"]);
    }

    #[test]
    fn ctrl_g_cancels_search_and_restores_the_draft() {
        let mut editor = TypeaheadEditor::new(search_history());
        editor.feed(b"my draft");
        editor.feed(&[0x12]);
        editor.feed(b"parser");
        assert_eq!(editor.buffer_string(), "fix the parser");
        editor.feed(&[0x07]); // Ctrl+G
        assert!(editor.search_status().is_none());
        assert_eq!(editor.buffer_string(), "my draft");
        assert_eq!(editor.cursor(), 8);
    }

    #[test]
    fn search_backspace_edits_the_query_and_reports_no_match() {
        let mut editor = TypeaheadEditor::new(search_history());
        editor.feed(&[0x12]);
        editor.feed(b"parserZZ");
        assert!(editor.search_status().unwrap().contains("(no match)"));
        editor.feed(&[0x7f, 0x7f]); // drop ZZ
        assert!(!editor.search_status().unwrap().contains("(no match)"));
        assert_eq!(editor.buffer_string(), "fix the parser");
    }

    #[test]
    fn other_control_keys_accept_the_match_then_act() {
        let mut editor = TypeaheadEditor::new(search_history());
        editor.feed(&[0x12]);
        editor.feed(b"build");
        assert_eq!(editor.buffer_string(), "cargo build");
        // Ctrl+A: accept then home.
        editor.feed(&[0x01]);
        assert!(editor.search_status().is_none());
        assert_eq!(editor.buffer_string(), "cargo build");
        assert_eq!(editor.cursor(), 0);
    }

    #[test]
    fn take_draft_mid_search_carries_the_real_typing_not_the_match() {
        let mut editor = TypeaheadEditor::new(search_history());
        editor.feed(b"real typing");
        editor.feed(&[0x12]);
        editor.feed(b"cargo");
        assert_eq!(editor.buffer_string(), "cargo test -p kitsu");
        assert_eq!(editor.take_draft(), "real typing");
    }

    #[test]
    fn approval_handoff_cancels_search_and_restores_the_draft() {
        let mut editor = TypeaheadEditor::new(search_history());
        editor.feed(b"keep me");
        editor.feed(&[0x12]);
        editor.feed(b"cargo");
        editor.reset_pending_input();
        assert!(editor.search_status().is_none());
        assert_eq!(editor.buffer_string(), "keep me");
    }

    #[test]
    fn unknown_escape_sequences_never_reach_the_draft() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"\x1b[200~\x1b[5;2R\x1b[Z");
        assert_eq!(editor.buffer_string(), "");
        editor.feed(b"ok");
        assert_eq!(editor.buffer_string(), "ok");
    }

    #[test]
    fn control_bytes_are_ignored_and_tab_requests_completion() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        let outcome = editor.feed(b"a\tb\x07c");
        assert_eq!(editor.buffer_string(), "abc");
        assert_eq!(outcome.actions, vec![EditorAction::CompleteRequest]);
    }

    #[test]
    fn tab_inside_a_paste_inserts_a_literal_tab_not_a_completion() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        let outcome = editor.feed(b"\x1b[200~a\tb\x1b[201~");
        assert!(outcome.actions.is_empty());
        assert_eq!(editor.buffer_string(), "a\tb");
    }

    #[test]
    fn replace_buffer_sets_cursor_at_end_and_ends_history_walk() {
        let mut editor = TypeaheadEditor::new(vec!["old".to_string()]);
        editor.feed(b"dra");
        editor.feed(b"\x1b[A"); // walk into history
        editor.replace_buffer("/queue");
        assert_eq!(editor.buffer_string(), "/queue");
        assert_eq!(editor.cursor(), 6);
        // ↓ after a replace must not resurrect the pre-walk stash.
        editor.feed(b"\x1b[B");
        assert_eq!(editor.buffer_string(), "/queue");
    }

    #[test]
    fn unbracketed_newlines_still_submit_each_line() {
        // Terminals without bracketed paste send pasted text as plain bytes;
        // line-at-a-time submission remains the honest interpretation there.
        let mut editor = TypeaheadEditor::new(Vec::new());
        let outcome = editor.feed(b"one\ntwo");
        assert_eq!(submit_actions(&outcome), vec!["one"]);
        assert_eq!(editor.buffer_string(), "two");
    }

    #[test]
    fn bracketed_paste_stays_one_block_and_submits_once() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        let outcome = editor.feed(b"\x1b[200~one\r\ntwo\nthree\x1b[201~");
        assert!(outcome.actions.is_empty());
        assert_eq!(editor.buffer_string(), "one\ntwo\nthree");
        let outcome = editor.feed(b"\r");
        assert_eq!(submit_actions(&outcome), vec!["one\ntwo\nthree"]);
    }

    #[test]
    fn bracketed_paste_keeps_tabs_and_drops_other_controls() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"\x1b[200~a\tb\x07c\x1b[201~");
        assert_eq!(editor.buffer_string(), "a\tbc");
    }

    #[test]
    fn paste_mode_ends_at_terminator_so_enter_submits_again() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"\x1b[200~block\x1b[201~ tail");
        let outcome = editor.feed(b"\r");
        assert_eq!(submit_actions(&outcome), vec!["block tail"]);
    }

    #[test]
    fn alt_enter_inserts_a_literal_newline() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"first\x1b\rsecond");
        assert_eq!(editor.buffer_string(), "first\nsecond");
        let outcome = editor.feed(b"\r");
        assert_eq!(submit_actions(&outcome), vec!["first\nsecond"]);
    }

    #[test]
    fn navigation_sequences_inside_paste_are_inert_content_noise() {
        let mut editor = TypeaheadEditor::new(vec!["old entry".to_string()]);
        // A pasted transcript carrying cursor/history sequences must neither
        // walk history nor move/delete; only the terminator is honored.
        editor.feed(b"\x1b[200~line1\x1b[A\x1b[D\x1b[3~\x1bOHline2\x1b[201~");
        assert_eq!(editor.buffer_string(), "line1line2");
        let outcome = editor.feed(b"\r");
        assert_eq!(submit_actions(&outcome), vec!["line1line2"]);
    }

    #[test]
    fn reset_pending_input_unlatches_paste_mode_and_keeps_the_draft() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"\x1b[200~stuck");
        // Simulate an approval prompt consuming the rest of the paste,
        // terminator included: the collector resets transient input state.
        editor.reset_pending_input();
        editor.feed(b" more");
        let outcome = editor.feed(b"\r");
        assert_eq!(submit_actions(&outcome), vec!["stuck more"]);
    }

    #[test]
    fn stray_high_bytes_never_become_c1_controls_in_the_draft() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        // 0x85 (a bare continuation byte) would decode as U+0085 NEL; it must
        // be dropped in both the paste path and the normal path.
        editor.feed(b"\x1b[200~a\x85b\x1b[201~");
        assert_eq!(editor.buffer_string(), "ab");
        editor.feed(&[0x85, b'c']);
        assert_eq!(editor.buffer_string(), "abc");
    }

    #[test]
    fn utf8_inside_bracketed_paste_survives() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        let mut bytes = b"\x1b[200~f".to_vec();
        bytes.extend("é".as_bytes());
        bytes.extend(b"\nr\x1b[201~");
        editor.feed(&bytes);
        assert_eq!(editor.buffer_string(), "fé\nr");
    }
}
