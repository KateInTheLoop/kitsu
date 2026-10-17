//! The inline composer's completion seam.
//!
//! # Why a trait and not a function
//!
//! Everything a candidate list is *made of* lives in the `kitsu` binary
//! crate: the slash-command registry and `attachments::complete`. This crate
//! is a dependency of that one, so it cannot reach either, and the dependency
//! cannot be reversed (a library cannot depend on a binary crate). The
//! composer therefore declares *what a completion is* and asks for one
//! through [`CompletionSource`]; the single implementation lives in
//! `kitsu/src/tui_bridge.rs` and delegates to the one shared function the
//! classic Rustyline `ChatHelper` also calls.
//!
//! The point is a single source, not two that agree: this module contains **no
//! completion logic at all**, only the application of a candidate to a draft.
//! A future fork would have to put logic here, and
//! `candidates_come_only_from_the_completion_source` fails the moment it does.
//!
//! # The two insertion models, reconciled
//!
//! Rustyline hands its editor a start offset plus candidates and performs the
//! splice itself. The inline composer owns its own buffer, so the same
//! `(start, candidates)` shape is carried explicitly in [`Completion`]:
//! `start` is a **byte** offset into the line and the replacement covers
//! `start..pos`, where `pos` is the cursor the completion was requested at.
//! That is exactly Rustyline's contract, so one function can serve both hosts
//! without either host reinterpreting it.

/// One offered completion: what the menu shows, and what is spliced in.
///
/// `display` is menu text and may be padded or decorated; `replacement` is
/// the literal text that replaces `start..pos`, trailing space or `/`
/// included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub display: String,
    pub replacement: String,
}

impl Candidate {
    pub fn new(display: impl Into<String>, replacement: impl Into<String>) -> Self {
        Self {
            display: display.into(),
            replacement: replacement.into(),
        }
    }
}

/// A candidate list plus the byte offset the replacement starts at.
///
/// An empty `candidates` means "nothing to offer"; `start` is then
/// meaningless and must be `0`, so that two hosts with nothing to offer
/// compare equal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Completion {
    pub start: usize,
    pub candidates: Vec<Candidate>,
}

impl Completion {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

/// The host-side supplier of candidates. One implementation, in
/// `kitsu/src/tui_bridge.rs`.
pub trait CompletionSource {
    /// Candidates for `line` with the cursor at byte offset `pos`.
    fn complete(&self, line: &str, pos: usize) -> Completion;
}

/// The host-side supplier of the row's hint and its styling — the re-hosted
/// Rustyline `Hinter` and `Highlighter`, which are host knowledge
/// (slash registry, live model, cwd, `NO_COLOR`) for the same reason.
pub trait ComposerDecoration {
    /// The hint shown after the draft, unstyled.
    fn hint(&self, line: &str) -> String;

    /// The hint with terminal styling applied, which must preserve the
    /// display width of `hint`.
    fn highlight_hint(&self, hint: &str) -> String;
}

/// The inline composer's draft and cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composer {
    draft: String,
    cursor: usize,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A composer whose draft is `draft` with the cursor at its end.
    pub fn with_draft(draft: &str) -> Self {
        Self {
            draft: draft.to_string(),
            cursor: draft.len(),
        }
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Ask `source` for candidates at the cursor and return them **verbatim**.
    ///
    /// When there is exactly one candidate it is also applied to the draft,
    /// which is the Tab-completes-uniquely behaviour; two or more are returned
    /// for the caller to present and nothing is spliced.
    pub fn request_completion(&mut self, source: &dyn CompletionSource) -> Completion {
        let offered = source.complete(&self.draft, self.cursor);
        if let [only] = offered.candidates.as_slice() {
            let only = only.clone();
            self.apply_candidate(offered.start, &only);
        }
        offered
    }

    /// Splice `candidate.replacement` over `start..cursor` and leave the
    /// cursor at the end of the inserted text.
    pub fn apply_candidate(&mut self, start: usize, candidate: &Candidate) {
        let start = start.min(self.cursor);
        if !self.draft.is_char_boundary(start) || !self.draft.is_char_boundary(self.cursor) {
            return;
        }
        self.draft
            .replace_range(start..self.cursor, &candidate.replacement);
        self.cursor = start + candidate.replacement.len();
    }

    /// The styled hint for the current draft, taken from `decoration`.
    pub fn styled_hint(&self, decoration: &dyn ComposerDecoration) -> String {
        decoration.highlight_hint(&decoration.hint(&self.draft))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source that answers with sentinels no real completion could produce.
    /// If the composer ever computes candidates itself, these cannot appear.
    struct SentinelSource;

    impl CompletionSource for SentinelSource {
        fn complete(&self, line: &str, pos: usize) -> Completion {
            Completion {
                start: 2,
                candidates: vec![
                    Candidate::new(
                        format!("sentinel-display-for({line:?},{pos})"),
                        "sentinel-replacement-one ",
                    ),
                    Candidate::new("sentinel-display-two", "sentinel-replacement-two "),
                ],
            }
        }
    }

    struct SentinelDecoration;

    impl ComposerDecoration for SentinelDecoration {
        fn hint(&self, line: &str) -> String {
            format!("sentinel-hint-for({line})")
        }

        fn highlight_hint(&self, hint: &str) -> String {
            format!("<<{hint}>>")
        }
    }

    /// The reconciliation of the two insertion models: a replacement covers
    /// `start..cursor` and the cursor lands after it.
    #[test]
    fn a_single_candidate_is_applied_over_the_source_range() {
        let mut composer = Composer::with_draft("tell me about @fi");
        assert_eq!(composer.cursor(), 17, "cursor starts at the draft's end");

        composer.apply_candidate(14, &Candidate::new("@file.rs", "@file.rs "));

        assert_eq!(composer.draft(), "tell me about @file.rs ");
        assert_eq!(composer.cursor(), 23);
    }

    /// The structural half of the parity requirement: this crate holds no
    /// completion logic, so everything the composer offers is its source's
    /// answer, element for element, including the start offset.
    #[test]
    fn candidates_come_only_from_the_completion_source() {
        let mut composer = Composer::with_draft("@fi");

        let offered = composer.request_completion(&SentinelSource);

        assert_eq!(offered, SentinelSource.complete("@fi", 3));
        assert_eq!(
            composer.draft(),
            "@fi",
            "two candidates are presented, not spliced"
        );
    }

    /// With exactly one candidate the composer splices it, and still returns
    /// the source's answer unaltered.
    #[test]
    fn a_unique_candidate_is_spliced_and_still_reported() {
        struct OneCandidate;
        impl CompletionSource for OneCandidate {
            fn complete(&self, _line: &str, _pos: usize) -> Completion {
                Completion {
                    start: 0,
                    candidates: vec![Candidate::new("/steer <text>", "/steer ")],
                }
            }
        }

        let mut composer = Composer::with_draft("/ste");

        let offered = composer.request_completion(&OneCandidate);

        assert_eq!(offered, OneCandidate.complete("/ste", 4));
        assert_eq!(composer.draft(), "/steer ");
        assert_eq!(composer.cursor(), 7);
    }

    /// The re-hosted hinter and highlighter are likewise pass-through: the
    /// composer picks neither the hint text nor its styling.
    #[test]
    fn the_hint_and_its_styling_come_only_from_the_decoration() {
        let composer = Composer::with_draft("/st");

        assert_eq!(
            composer.styled_hint(&SentinelDecoration),
            "<<sentinel-hint-for(/st)>>"
        );
    }
}
