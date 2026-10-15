//! The agent view's **one bindings table** (card **T4-S10**, design §4.7).
//!
//! §4.7's words are "bindings are declared in one table consumed by the help
//! overlay and `/help keys` so docs cannot drift". This module is that table.
//! It replaces three independent enumerations of the view's keys: the literal
//! list [`crate::agents_view::overlay_help_rows`] used to carry, the test list
//! that used to re-state the action keys beside it, and -- as far as a crate
//! boundary allows -- the `match` arms in the binary's `handle_overlay_input`.
//!
//! # What a crate boundary does and does not allow
//!
//! The dispatch lives in `kitsu`'s `main.rs` and matches on `OverlayKey`, a
//! type private to the binary; `kitsu-tui` cannot see it and therefore cannot
//! *be* the `match`. Finding **F57** is why that matters: a single source of
//! truth makes every *reader* agree with it and guarantees nothing about
//! whether anything *acts* on what they read, so a table that is only what
//! the help prints would let the help advertise a key nothing dispatches.
//!
//! The answer this card takes is that the table is keyed by [`Press`] -- a
//! decoded key press, spelled in `kitsu-tui` so both sides can name one --
//! and that the binary carries a **total sweep** over the key space the
//! overlay's decoder can produce: every press that changes the painted frame
//! must be in this table, and every press in this table must change it. The
//! sweep is total because `OverlayKey::Char` is `0x20..=0x7e` by
//! construction, so it is an enumeration of the *keyboard*, not of the arms,
//! and it cannot agree with the dispatch by reading it.
//!
//! # Who reads this module
//!
//! [`crate::agents_view::overlay_help_rows`] renders it as the help frame's
//! key rows, and the binary's `/help keys` prints the same rows as text; the
//! binary's sweep over the overlay's key space checks the dispatch against
//! it from the outside. The grammars the `/` and `+` rows carry live here
//! too, since the help's `what` is now where they are spelled.

/// One decoded key press, in the vocabulary both crates can name.
///
/// This is deliberately *not* the binary's `OverlayKey`: that type is private
/// to `kitsu` and carries the decoder's concerns. The binary maps its own key
/// onto this one in a single total function, which is the only place the two
/// vocabularies meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// A printable ASCII character, as the overlay's decoder produces it
    /// (`0x20..=0x7e`).
    Char(char),
    /// `\u{2191}`.
    Up,
    /// `\u{2193}`.
    Down,
    /// `Enter`.
    Enter,
    /// `\u{2192}`, the overlay's synonym for `Enter`.
    Right,
    /// `Esc`, resolved as a real press rather than as the start of a sequence.
    Esc,
}

/// What *kind* of binding a row is, because a flat list of letters cannot say
/// the things a user has to know before pressing one.
///
/// Every variant is a distinction the dispatch already makes and the help
/// already has to explain in prose: whether a press needs a selected agent,
/// whether it is inert while the help or a peek panel is up, whether it takes
/// effect at once or wants a second press, and whether what it opens reads a
/// single key or a whole edited line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// Moves the selection over the rows. Inert while the help or a peek
    /// panel is up, because the row list is not on screen there.
    Selection,
    /// Acts on the selected agent at once, and reports what it did even when
    /// nothing is selected. Inert while the help or a peek panel is up.
    SelectedAgent,
    /// Acts on the selected agent, and the **first** press only arms: the
    /// registry is unchanged until the second.
    TwoPress,
    /// Opens a read whose answer is the **next single key**, not a line.
    DecisionRead,
    /// Opens an edited line; the bytes after the key belong to the line.
    OpensLine,
    /// Acts on the view itself, and keeps working while the help or a peek
    /// panel is up.
    View,
}

/// One row of the bindings table.
pub struct Binding {
    /// Every press this one row binds. More than one when the row *is* a
    /// pair, as the arrows are: one row, one description, two keys.
    pub presses: &'static [Press],
    /// The key column the help overlay and `/help keys` both print.
    pub label: &'static str,
    /// What kind of binding this is; see [`BindingKind`].
    pub kind: BindingKind,
    /// The description column, in the one wording every reader prints.
    pub what: &'static str,
}

/// A3's filter grammar, in the one wording every reader prints: the `/` row
/// of [`BINDINGS`], the filter frame's footer and `/help keys` alike.
///
/// A macro rather than a plain `const` because [`Binding::what`] is a
/// `&'static str` built with [`concat!`], which takes literals and not
/// constants. The `const` below is the same literal, for the frame footers
/// that interpolate it at runtime.
macro_rules! filter_grammar {
    () => {
        "s:<state> n:<text> o:<text> p:<profile>"
    };
}

/// A6's dispatch grammar, spelled once for [`filter_grammar`]'s reason.
macro_rules! dispatch_grammar {
    () => {
        "<task>, or @<profile> <task>"
    };
}

/// A3's filter grammar. Moved here from `agents_view` by card T4-S10: the
/// help's `what` is now the single source, and the grammar is part of it.
pub const FILTER_GRAMMAR: &str = filter_grammar!();

/// A6's dispatch grammar. Moved here with [`FILTER_GRAMMAR`], for its reason.
pub const DISPATCH_GRAMMAR: &str = dispatch_grammar!();

/// **The** table: every key the agent view's overlay dispatch binds, in the
/// order the help overlay and `/help keys` both print them.
///
/// Three keys in it were dispatched and advertised nowhere before this card:
/// `q`, which closes the view, and `Enter`/`\u{2192}`, which attach. That is the
/// defect cards T4-S3, T4-S4b, T4-S5 and T4-S7 each closed once for the key
/// they added, left open in triplicate because each of them appended to a
/// literal list instead of replacing it.
///
/// Rows are *bindings*, not presses: `up`/`dn` and `Enter`/`\u{2192}` are each
/// **one** row with two presses, because one movement and one attach
/// described twice is two descriptions that can drift apart.
///
/// Not in it, deliberately: the inline composer's chords (`Ctrl+X a`,
/// `Ctrl+X d`, `\u{2190}\u{2190}`). They are decoded by a different decoder on a
/// different surface and never reach `handle_overlay_input`, so the binary's
/// total sweep over this view's key space cannot see them; folding them in
/// would buy one table and lose the guard on half of it. Their key column is
/// also computed rather than static, because the chord prefix is
/// configurable. [`Binding::label`] admits that later; nothing here needs it
/// now.
pub const BINDINGS: &[Binding] = &[
    Binding {
        presses: &[Press::Up, Press::Down],
        label: "up/dn",
        kind: BindingKind::Selection,
        what: "move the selection; moving disarms a half-pressed stop",
    },
    Binding {
        presses: &[Press::Enter, Press::Right],
        label: "enter/\u{2192}",
        kind: BindingKind::SelectedAgent,
        what: "attach to the selected agent and leave the view",
    },
    Binding {
        presses: &[Press::Char(' ')],
        label: "space",
        kind: BindingKind::SelectedAgent,
        what: "peek the selected agent; counts as collecting its result",
    },
    Binding {
        presses: &[Press::Char('r')],
        label: "r",
        kind: BindingKind::DecisionRead,
        what: "reply to the selected agent: the next key is y, a or n",
    },
    Binding {
        presses: &[Press::Char('s')],
        label: "s",
        kind: BindingKind::OpensLine,
        what: "steer the selected agent: type a correction, enter sends",
    },
    Binding {
        presses: &[Press::Char('x')],
        label: "x",
        kind: BindingKind::TwoPress,
        what: "stop the selected agent; press twice (the second press prunes \
               a finished one)",
    },
    Binding {
        presses: &[Press::Char('n')],
        label: "n",
        kind: BindingKind::OpensLine,
        what: "rename the selected agent: type a name, enter applies",
    },
    Binding {
        presses: &[Press::Char('p')],
        label: "p",
        kind: BindingKind::SelectedAgent,
        what: "acknowledge the selected agent's result",
    },
    Binding {
        presses: &[Press::Char('/')],
        label: "/",
        kind: BindingKind::OpensLine,
        what: concat!("filter the rows \u{2014} ", filter_grammar!()),
    },
    Binding {
        presses: &[Press::Char('+')],
        label: "+",
        kind: BindingKind::OpensLine,
        what: concat!("start an agent \u{2014} ", dispatch_grammar!()),
    },
    Binding {
        presses: &[Press::Char('?')],
        label: "?",
        kind: BindingKind::View,
        what: "show this help",
    },
    Binding {
        presses: &[Press::Esc],
        label: "esc",
        kind: BindingKind::View,
        what: "leave a prompt or the help, then close the view",
    },
    Binding {
        presses: &[Press::Char('q')],
        label: "q",
        kind: BindingKind::View,
        what: "close the view",
    },
];
