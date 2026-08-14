//! Pure line-editor state machine for during-turn type-ahead.
//!
//! While a foreground turn runs, stdin is switched to raw (non-canonical)
//! mode and every byte is fed through this editor. It owns the draft buffer,
//! cursor, and history navigation; terminal I/O stays in the collector so the
//! whole keymap is unit-testable as bytes-in → state/actions-out.
//!
//! Supported keys: printable UTF-8 insert, Enter (submit), Backspace, Delete,
//! ←/→/Home/End (also Ctrl+B/F/A/E), ↑/↓ history walk with a draft stash,
//! Ctrl+U (kill to start), Ctrl+K (kill to end), Ctrl+W (delete word),
//! Ctrl+D (EOF on an empty draft, delete otherwise). Unknown escape
//! sequences are consumed and ignored so they never leak into the draft.

/// A side effect the collector must perform after feeding bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorAction {
    /// The user pressed Enter: the completed line (untrimmed).
    Submit(String),
    /// Ctrl+D on an empty draft: treat as end of input.
    Eof,
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
}

impl TypeaheadEditor {
    #[cfg(test)]
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
    pub fn take_draft(self) -> String {
        self.buffer.into_iter().collect()
    }

    /// Feed raw stdin bytes; returns whether a redraw is needed and any
    /// submissions/EOF that completed.
    pub fn feed(&mut self, bytes: &[u8]) -> FeedOutcome {
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
                            self.insert_char(ch, outcome);
                        }
                    }
                }
                return;
            }
            // Malformed sequence: discard it and process this byte fresh.
            self.utf8.clear();
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
            0x17 => {
                let start = self.word_start();
                if start < self.cursor {
                    self.buffer.drain(start..self.cursor);
                    self.cursor = start;
                    outcome.changed = true;
                }
            }
            0x00..=0x1f => {
                // Other control bytes (Tab included) are ignored: the
                // during-turn editor has no completion surface, and control
                // characters must never enter the draft.
            }
            _ => {
                let expected = utf8_expected_len(byte);
                if expected <= 1 {
                    self.insert_char(byte as char, outcome);
                } else {
                    self.utf8.clear();
                    self.utf8.push(byte);
                }
            }
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8, outcome: &mut FeedOutcome) {
        match final_byte {
            b'A' => self.history_prev(outcome),
            b'B' => self.history_next(outcome),
            b'C' => self.move_right(outcome),
            b'D' => self.move_left(outcome),
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
                b"4" | b"8" => self.move_end(outcome),
                // Bracketed-paste markers (200/201) and other tilde keys
                // are ignored; pasted text itself arrives as ordinary bytes.
                _ => {}
            },
            _ => {}
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

    fn history_prev(&mut self, outcome: &mut FeedOutcome) {
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
                EditorAction::Eof => None,
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
        editor.feed(b"\x1b[A");
        assert_eq!(editor.buffer_string(), "new");
        editor.feed(b"\x1b[A");
        assert_eq!(editor.buffer_string(), "old");
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
    fn unknown_escape_sequences_never_reach_the_draft() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"\x1b[200~\x1b[5;2R\x1b[Z");
        assert_eq!(editor.buffer_string(), "");
        editor.feed(b"ok");
        assert_eq!(editor.buffer_string(), "ok");
    }

    #[test]
    fn control_bytes_and_tab_are_ignored() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        editor.feed(b"a\tb\x07c");
        assert_eq!(editor.buffer_string(), "abc");
    }

    #[test]
    fn paste_with_embedded_newline_submits_each_line() {
        let mut editor = TypeaheadEditor::new(Vec::new());
        let outcome = editor.feed(b"one\ntwo");
        assert_eq!(submit_actions(&outcome), vec!["one"]);
        assert_eq!(editor.buffer_string(), "two");
    }
}
