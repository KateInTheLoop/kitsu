use super::*;

/// **Test (R6-b): the `/` byte opens the overlay's query line, the typed
/// bytes become the query, and `Enter` runs the search.**
///
/// The whole keystroke path in one pass, through the real decoder. `0x2f`,
/// the printable bytes behind it, `0x7f` and `0x0d` must each reach the
/// overlay; today every one of them is inert -- the printable bytes become
/// `Press::Char`s that `closes` declines, and `0x7f` and `0x0d` are
/// control bytes that the loop's `_ => continue` arm drops -- so the frame
/// is repainted unchanged, the view is still following and the loop ends
/// `Drained`. A key the footer will advertise and nothing dispatches is
/// **F75**.
///
/// `0x7f` is in the payload deliberately: a query line a typo cannot be
/// corrected in is one the user has to abandon, and the byte is dropped by
/// the same arm.
///
/// Not vacuous, and specifically not satisfiable by a loop that merely
/// repainted: the painted content rows are pinned **by value** on both
/// sides, so the opening frame pins what was painted before the bytes and
/// the landing pins where they took the frame.
#[cfg(unix)]
#[test]
fn the_slash_byte_opens_the_query_line_and_enter_runs_the_search() {
    // `Z` then `0x7f`: the backspace byte has to reach the query line too,
    // or the query submitted below is `find mZe` and matches nothing.
    let (exit, following, frames) = drive_transcript_with_matches(&[b"/find mZ\x7fe\r"]);
    assert_eq!(
        exit,
        TranscriptExit::Drained,
        "`/` searches; it is not a close key"
    );
    assert!(
        !following,
        "and the submitted search pinned the overlay, which is \
             transition 12: a search that left the view following did nothing"
    );
    assert_eq!(frames.len(), 2, "painted before the bytes and after them");
    assert_eq!(
        &frames[0][..5],
        OPENING,
        "the opening frame, ordinals 4..=8"
    );
    assert_eq!(
        &frames[1][..5],
        [
            "find me second",
            "answer two",
            "answer three",
            "answer four",
            "answer five",
        ],
        "ordinals 2..=6: the newest match is on the top row, so `/`, the \
             query bytes, the backspace and the `Enter` all arrived"
    );
}

/// **Test (R6-b): the `n` and `N` bytes walk the matches through the real
/// decoder, and neither wraps.**
///
/// One chunk per keystroke, so each landing is its own painted frame
/// rather than only the final state. Bidirectional, because `N` walking
/// while `n` did nothing is a live failure mode, and the two inert presses
/// at the ends are asserted to leave the frame **byte-identical** --
/// which is also the no-wrap decision, reached through the bytes.
#[cfg(unix)]
#[test]
fn the_n_and_shift_n_bytes_walk_the_search_matches_without_wrapping() {
    let (exit, following, frames) =
        drive_transcript_with_matches(&[b"/find me\r", b"N", b"N", b"n", b"n"]);
    assert_eq!(
        exit,
        TranscriptExit::Drained,
        "none of these is a close key"
    );
    assert!(!following, "the walk leaves the overlay pinned");
    assert_eq!(
        frames.len(),
        6,
        "the opening frame, one per keystroke, and the frame the drained \
             read was painted against"
    );

    let oldest = [
        "find me first",
        "answer one",
        "find me second",
        "answer two",
        "answer three",
    ];
    let newest = [
        "find me second",
        "answer two",
        "answer three",
        "answer four",
        "answer five",
    ];
    assert_eq!(&frames[0][..5], OPENING, "ordinals 4..=8");
    assert_eq!(&frames[1][..5], newest, "the submit lands on ordinal 2");
    assert_eq!(&frames[2][..5], oldest, "`N` walks to ordinal 0");
    assert_eq!(
        &frames[3][..5],
        oldest,
        "`N` again does NOT wrap round to the newest match: there is no \
             third match above, so the frame is byte-identical"
    );
    assert_eq!(&frames[4][..5], newest, "`n` comes back to ordinal 2");
    assert_eq!(
        &frames[5][..5],
        newest,
        "`n` again does NOT wrap round to the oldest match either"
    );
}

/// **Test (R6-b): a NEWLINE inside a bracketed paste is query CONTENT,
/// not a submit and not a keypress.**
///
/// **F72**'s shape, measured on this surface rather than assumed from the
/// agent view: the overlay has its own byte loop, so what a pasted newline
/// does here is this loop's answer. **Measured today**: `0x0a` is a
/// control byte this loop drops and the `q` behind it is read as the close
/// key, so `b"/find me\nq"` ends `Closed` -- the overlay gone, nothing
/// searched, and the bytes of a paste dispatched as commands.
///
/// The signal needed to fix that is already on the wire and already being
/// thrown away: the alternate-screen route writes `ESC[?2004h`, so a paste
/// arrives wrapped in `ESC[200~` / `ESC[201~`, and this loop swallows both
/// markers whole in its generic sequence arm. R6-a's lesson is the rule --
/// a sequence inside a paste is **content**, not a keypress -- so between
/// the two markers a newline neither submits nor closes anything. A query
/// line is one line, so the pasted text up to its first newline is the
/// query and the remainder of the paste is **discarded**; the `Enter` that
/// runs it is the one the user then presses, outside the paste.
///
/// Bidirectional, and the second half is what keeps the first from being a
/// rule about chunk boundaries -- which this loop's own header calls wrong
/// (**F65**): **outside** a paste the bytes after an `Enter` are still
/// keys, so a `q` typed straight after submitting really does close the
/// overlay. Discarding it would lose a keystroke to how the reads happened
/// to split.
#[cfg(unix)]
#[test]
fn a_newline_inside_a_bracketed_paste_is_query_text_and_closes_nothing() {
    let (exit, following, frames) =
        drive_transcript_with_matches(&[b"/\x1b[200~find me\nq\x1b[201~\r"]);
    assert_eq!(
        exit,
        TranscriptExit::Drained,
        "the `q` is pasted CONTENT: a multi-line paste must not take the \
             overlay away from under the user"
    );
    assert!(
        !following,
        "and the trailing `Enter`, which is outside the paste, ran the \
             search"
    );
    assert_eq!(
        &frames[frames.len() - 1][..5],
        [
            "find me second",
            "answer two",
            "answer three",
            "answer four",
            "answer five",
        ],
        "ordinals 2..=6: the query is the pasted text up to its newline, \
             and everything after that newline was discarded rather than \
             dispatched"
    );

    // The other half: outside a paste, an `Enter` submits and the bytes
    // behind it are still keys. This is what makes the rule above a rule
    // about the paste markers and not about where a read happened to end.
    let (exit, following, _) = drive_transcript_with_matches(&[b"/find me\rq"]);
    assert_eq!(
        exit,
        TranscriptExit::Closed,
        "a `q` typed straight after submitting is the close key, and must \
             not be lost to the chunk it shared with the `Enter`"
    );
    assert!(!following, "and the search still ran before the close");
}

/// **Test (R6-b): while the query line is open, `q` is text and `Esc`
/// cancels the query rather than closing the overlay.**
///
/// Two enumerated cases that are each a disappearing surface if missed.
/// `q` closes this overlay, so unless it is ordinary text while a query is
/// open the word `query` cannot be searched for and the overlay vanishes
/// mid-typing. And `Esc` has to be the way out of a query line -- the only
/// other way out would be submitting one the user no longer wants -- so it
/// cancels the query **and leaves the overlay up**, which is one press
/// away from the surface closing under them.
///
/// The `Esc` half is driven as its own chunk on purpose: a lone `0x1b`
/// with nothing queued behind it is exactly the ambiguous case this loop
/// resolves with the grace (**F65**), so this also pins that cancelling a
/// query does not go through a second `Esc` mechanism.
///
/// Not vacuous: the frame is pinned by value, so a `q` that was swallowed
/// into a query *and* scrolled something fails, and the follow state is
/// asserted, so a cancel that pinned on its way out fails.
#[cfg(unix)]
#[test]
fn the_close_keys_are_query_text_and_an_escape_while_a_query_is_open() {
    let (exit, following, frames) = drive_transcript_with_matches(&[b"/query"]);
    assert_eq!(
        exit,
        TranscriptExit::Drained,
        "`q` inside an open query line is a character, not the close key"
    );
    assert!(following, "and nothing was submitted, so nothing pinned");
    assert_eq!(
        &frames[frames.len() - 1][..5],
        OPENING,
        "the transcript rows are untouched: typing a query scrolls nothing"
    );

    let (exit, following, frames) = drive_transcript_with_matches(&[b"/find me", b"\x1b"]);
    assert_eq!(
        exit,
        TranscriptExit::Drained,
        "`Esc` cancels the query line; it does not also close the overlay \
             the user is still reading"
    );
    assert!(following, "and the cancel did not pin on its way out");
    assert_eq!(
        &frames[frames.len() - 1][..5],
        OPENING,
        "the frame is the opening one, byte-identical"
    );
}

/// **Test (R6-c): the byte `{` reaches the overlay's prompt jump, and
/// `}` comes back.**
///
/// Driven through the real loop, not a predicate beside it (**F69**/
/// **F105**): `kitsu_tui`'s own tests prove `TranscriptOverlay::jump`
/// answers `Jump::Prev`, and prove nothing about whether any byte ever
/// becomes one. `0x7b` and `0x7d` both already fall inside this loop's
/// `0x20..=0x7e` printable arm, so **measured before asserting**
/// (**F99**/**F100**): today each becomes a `Press::Char`, `closes` says
/// no, and the loop reads again — the frame is repainted unchanged, the
/// view is still following and the loop ends `Drained`. The key is inert
/// while R6-c's footer advertises it, which is **F75** exactly.
///
/// Not vacuous, and specifically not satisfiable by a loop that merely
/// repainted: the painted content rows are asserted **by value** on both
/// sides, so the pre-jump frame pins what the loop painted before the byte
/// and the post-jump frame pins where the byte took it. Bidirectional,
/// because `{` moving while `}` did nothing is a live failure mode.
#[cfg(unix)]
#[test]
fn the_open_brace_byte_jumps_the_overlay_to_the_previous_prompt_block() {
    let (exit, following, frames) = drive_transcript_with_prompts(&[b"{"]);
    assert_eq!(
        exit,
        TranscriptExit::Drained,
        "`{{` scrolls; it is not a close key"
    );
    assert!(
        !following,
        "and it pinned the overlay, which is transition 12: a `{{` that \
             left the view following did nothing"
    );
    assert_eq!(frames.len(), 2, "painted before the byte and after it");
    assert_eq!(
        &frames[0][..5],
        [
            "second question",
            "with a tail",
            "answer three",
            "answer four",
            "answer five",
        ],
        "the opening frame, ordinals 3..=7"
    );
    assert_eq!(
        &frames[1][..5],
        [
            "first question",
            "answer one",
            "answer two",
            "second question",
            "with a tail",
        ],
        "and `{{` took it to the oldest block's first row, ordinals 0..=4"
    );
}

/// **Test (R6-c): `}` is reachable too, and a two-row prompt block is ONE
/// stop.**
///
/// `{` then `}` in a single chunk is a round trip to the block the overlay
/// opened on; `{` then `}` then `}` must stay there, because the second
/// block's two rows are one stop and there is no third block to walk into.
/// A loop wired to a row-walking jump would land on `with a tail` as a top
/// row instead, which the by-value frame catches.
#[cfg(unix)]
#[test]
fn the_close_brace_byte_jumps_forward_and_a_two_row_block_is_one_stop() {
    let opening = [
        "second question".to_string(),
        "with a tail".to_string(),
        "answer three".to_string(),
        "answer four".to_string(),
        "answer five".to_string(),
    ];

    let (exit, following, frames) = drive_transcript_with_prompts(&[b"{}"]);
    assert_eq!(exit, TranscriptExit::Drained, "neither brace closes");
    assert!(!following, "and the jump pinned rather than re-latching");
    assert_eq!(
        &frames.last().expect("a frame was painted")[..5],
        opening.as_slice(),
        "`{{` then `}}` is a round trip to the block the overlay opened on"
    );

    let (_, _, frames) = drive_transcript_with_prompts(&[b"{}}"]);
    assert_eq!(
        &frames.last().expect("a frame was painted")[..5],
        opening.as_slice(),
        "and a second `}}` stays on it: the two-row prompt is one stop, \
             not two, so there is nothing further to walk to"
    );
}

/// **Test: a lone `Esc` closes the overlay.**
///
/// Measured before it was asserted (**F99**), and it was red for a reason
/// no mirror-test could have found: `[0x1b]` is a strict prefix of all
/// four paging sequences, so the partial-hold took it and the loop went
/// back to a blocking read. The overlay stayed up until the user pressed
/// something else, and the footer said `q/Esc close`.
///
/// Not vacuous: the exit **reason** is asserted, not merely that the loop
/// returned. A drained descriptor also ends the loop, and that is the
/// value the defect produced here.
#[cfg(unix)]
#[test]
fn a_lone_esc_closes_the_transcript_overlay() {
    let (exit, following, frames) = drive_transcript(&[b"\x1b"]);
    assert_eq!(
        exit,
        TranscriptExit::Closed,
        "a lone Esc is the close press the footer advertises, not the \
             first byte of a sequence that never came"
    );
    assert!(following, "and nothing paged");
    assert!(!frames.is_empty(), "the overlay painted before it read");
}

/// **Test: a lone `Esc` after another key, alone in its own chunk, closes
/// the overlay.**
///
/// The chunk-boundary case: `PgUp` arrives and is answered, and the `Esc`
/// that follows is a read of its own with nothing behind it.
#[cfg(unix)]
#[test]
fn a_lone_esc_at_a_chunk_boundary_closes_the_overlay() {
    let (exit, following, _) = drive_transcript(&[b"\x1b[5~", b"\x1b"]);
    assert_eq!(
        exit,
        TranscriptExit::Closed,
        "the Esc in its own read is the press"
    );
    assert!(
        !following,
        "and the PgUp before it was answered, so this is not a loop that \
             dropped both bytes"
    );
}

/// **Test: `ESC [ B` split across two reads is one keystroke, is dropped,
/// and does not close the overlay (F65).**
///
/// The regression the grace must not buy at the expense of: an arrow key
/// whose first byte lands at the end of a chunk must not be read as a
/// close.
#[cfg(unix)]
#[test]
fn a_split_arrow_sequence_is_dropped_and_does_not_close_the_overlay() {
    let (exit, following, _) = drive_transcript(&[b"\x1b", b"[B"]);
    assert_eq!(
        exit,
        TranscriptExit::Drained,
        "the down arrow is not a close key, so the loop ran out of input \
             instead of closing"
    );
    assert!(
        following,
        "and the arrow moved nothing: this overlay has no \
             selection for it"
    );
}

/// **Test: `q` closes the overlay, and the paging keys do not.**
#[cfg(unix)]
#[test]
fn q_closes_the_transcript_overlay_and_a_paging_key_does_not() {
    let (exit, following, _) = drive_transcript(&[b"q"]);
    assert_eq!(exit, TranscriptExit::Closed, "q closes");
    assert!(following);

    let (exit, following, _) = drive_transcript(&[b"\x1b[5~\x1b[1;5H"]);
    assert_eq!(
        exit,
        TranscriptExit::Drained,
        "PgUp and Ctrl+Home scroll; neither closes"
    );
    assert!(!following, "and both were answered");
}
