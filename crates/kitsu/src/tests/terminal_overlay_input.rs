use super::*;

/// Card T4-S4b, `s`'s text sub-mode: it **edits, echoes, cancels and
/// submits**, over `kitsu-editor`'s editor rather than a second byte loop.
///
/// The three cases that are not happy paths are the point:
///
/// 1. a lone `ESC` cancels — on the *quiet tick*, inheriting T4-S4a's
///    50 ms grace, so nothing is sent;
/// 2. an `ESC` that turns out to be an arrow does **not** cancel and does
///    not corrupt the draft. That is **F65**, which cost the overlay's
///    close key its first arrow, met again by a sub-mode that reads
///    arbitrary bytes;
/// 3. a newline **inside a paste** is content, as T4-S2 decided for a
///    prefix byte inside a paste; the `Enter` that submits is one typed
///    outside one.
#[cfg(unix)]
#[test]
fn the_steer_line_edits_echoes_cancels_and_submits() {
    let mut line = SteerLine::new(7);
    assert_eq!(line.id(), 7, "the line is fixed to the agent it opened on");
    assert_eq!(line.draft(), "");
    assert_eq!(line.cursor(), 0);

    assert_eq!(line.feed(b"slow "), SteerLineStep::Editing);
    assert_eq!(line.feed(b"downn"), SteerLineStep::Editing);
    assert_eq!(line.draft(), "slow downn");
    // Backspace: the minimum editing a line needs to be usable.
    assert_eq!(line.feed(b"\x7f"), SteerLineStep::Editing);
    assert_eq!(line.draft(), "slow down");
    assert_eq!(line.cursor(), "slow down".chars().count());

    // A multi-byte character split across two reads, which a raw byte
    // loop of its own would have had to re-solve.
    assert_eq!(line.feed(b" caf\xc3"), SteerLineStep::Editing);
    assert_eq!(line.feed(b"\xa9"), SteerLineStep::Editing);
    assert_eq!(line.draft(), "slow down café");

    // An `ESC` that is the first byte of an arrow: not a cancel, and the
    // draft survives it.
    assert_eq!(line.feed(b"\x1b"), SteerLineStep::Editing);
    assert_eq!(line.feed(b"[D"), SteerLineStep::Editing);
    assert_eq!(
        line.draft(),
        "slow down café",
        "F65: an arrow is not a cancel and no byte of it enters the draft"
    );
    assert_eq!(
        line.cursor(),
        "slow down café".chars().count() - 1,
        "and the arrow moved the caret, so it was read as the arrow it is"
    );

    // `Enter` outside a paste submits, verbatim and untrimmed-of-content.
    assert_eq!(
        line.feed(b"\r"),
        SteerLineStep::Submit("slow down café".to_string()),
        "Enter sends the line that was typed"
    );

    // A newline inside a bracketed paste is content: a paste never
    // submits, so a pasted multi-line correction is not cut at its first
    // line, and the `Enter` after it is what sends the whole thing.
    let mut pasted = SteerLine::new(7);
    assert_eq!(
        pasted.feed(b"\x1b[200~one\ntwo\x1b[201~"),
        SteerLineStep::Editing,
        "a newline inside a paste is content, not a submit"
    );
    assert_eq!(pasted.draft(), "one\ntwo");
    assert_eq!(
        pasted.feed(b"\r"),
        SteerLineStep::Submit("one\ntwo".to_string()),
        "and the Enter after the paste sends all of it"
    );

    // A lone `ESC`, resolved the way T4-S4a resolves one: on a quiet tick
    // once the grace has passed with nothing waiting. It cancels, and the
    // cancel is not a submit of the empty line -- which the refusal above
    // would merely have reported, while this sends nothing at all.
    let mut cancelled = SteerLine::new(7);
    assert_eq!(cancelled.feed(b"half typed"), SteerLineStep::Editing);
    assert_eq!(
        cancelled.feed(b"\x1b"),
        SteerLineStep::Editing,
        "an ESC is still ambiguous when it arrives"
    );
    assert_eq!(
        cancelled.quiet_tick(),
        SteerLineStep::Cancelled,
        "the grace passed with nothing after it, so it was the Esc key"
    );

    // NON-VACUOUS, the other half of the same claim: a quiet tick with no
    // ESC held changes nothing, so the cancel above is about the `ESC`
    // and not about every quiet tick ending the line.
    let mut quiet = SteerLine::new(7);
    assert_eq!(quiet.feed(b"half typed"), SteerLineStep::Editing);
    assert_eq!(quiet.quiet_tick(), SteerLineStep::Editing);
    assert_eq!(quiet.draft(), "half typed");
    // And a quiet tick in the middle of an arrow's sequence does not end
    // the line either: the ESC there is spoken for.
    assert_eq!(quiet.feed(b"\x1b["), SteerLineStep::Editing);
    assert_eq!(quiet.quiet_tick(), SteerLineStep::Editing);
}

/// Finding **F68**, the decoder: **a pasted chunk is one paste, not a
/// sequence of keys** — bracketed or not.
///
/// The POSITIVE CONTROLS are the second half and they are the test: the
/// same letters typed as their own reads are still ordinary keys, and
/// F65's arrow, lone `ESC` and `ESC`-then-key are decoded exactly as
/// before. A "fix" that simply stops decoding letters fails those.
/// Card **T4-S6-ordinal**, the **scope**: `Ctrl+X 1..9` is the
/// **composer's** chord, exactly as `Ctrl+X a` and `Ctrl+X d` are, and it
/// is not a key in the agent view.
///
/// The view's decoder is a different decoder on a different key space
/// (`keys.rs:140`), and this pins that the prefix byte is nothing to it:
/// a chord cannot reach `handle_overlay_input`, so inside the view a
/// digit stays what it is today -- a plain character, which the row list
/// ignores through `OverlayKey::Char(_) => {}` and a text prompt takes as
/// content. Selecting in the view is the arrow keys; the ordinal is the
/// shortcut from *outside* it. No third scope is invented.
#[cfg(unix)]
#[test]
fn the_ordinal_chord_is_the_composers_and_is_not_a_key_in_the_agent_view() {
    /// The byte a terminal sends for the default `[tui] prefix`.
    const PREFIX: u8 = 0x18;
    let mut pending = OverlayPending::None;
    assert_eq!(
        decode_overlay_keys(&[PREFIX], &mut pending),
        Vec::new(),
        "the chord prefix is not a key in the view: the composer's \
             decoder is the only one that reads it"
    );
    for digit in '1'..='9' {
        let mut pending = OverlayPending::None;
        let mut buffer = [0u8; 1];
        digit.encode_utf8(&mut buffer);
        assert_eq!(
            decode_overlay_keys(&[PREFIX, buffer[0]], &mut pending),
            vec![OverlayKey::Char(digit)],
            "inside the view the prefix is dropped and {digit:?} is a \
                 plain character, not an ordinal"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_pasted_chunk_decodes_as_one_paste_and_never_as_action_keys() {
    let mut bracketed = Vec::from(OVERLAY_PASTE_START);
    bracketed.extend_from_slice(PASTED_CORRECTION.as_bytes());
    bracketed.extend_from_slice(OVERLAY_PASTE_END);
    let mut decoder = OverlayDecoder::new();
    assert_eq!(
        decoder.feed(&bracketed),
        vec![OverlayInput::Paste(PASTED_CORRECTION.to_string())],
        "a bracketed paste is one paste carrying its text verbatim, \
             newline included"
    );
    assert!(
        !decoder.escape_pending(),
        "and a closed paste block holds no `Esc`: the `ESC` bytes of the \
             delimiters are the paste's, not the key's (F65's grace must not \
             fire for them)"
    );

    // The same paste split across two reads, which is how a long one
    // really arrives: still one paste in total, and still no keys.
    let mut split = OverlayDecoder::new();
    let cut = OVERLAY_PASTE_START.len() + 7;
    let head = split.feed(&bracketed[..cut]);
    let tail = split.feed(&bracketed[cut..]);
    let pasted: String = head
        .iter()
        .chain(tail.iter())
        .map(|input| match input {
            OverlayInput::Paste(text) => text.clone(),
            other => panic!("a split paste yields no keys, got {other:?}"),
        })
        .collect();
    assert_eq!(
        pasted, PASTED_CORRECTION,
        "a paste split across reads is the same text"
    );

    // A terminal that does not report pastes sends the same text as plain
    // bytes, which is the case the overlay cannot opt out of: a chunk
    // carrying more than one printable character is a burst, not typing.
    let mut unbracketed = OverlayDecoder::new();
    assert_eq!(
        unbracketed.feed(PASTED_CORRECTION.as_bytes()),
        vec![OverlayInput::Paste(PASTED_CORRECTION.to_string())],
        "an unbracketed burst is a paste too: no human produces two \
             printable keystrokes in one read"
    );

    // POSITIVE CONTROL: typed one read at a time, every one of those
    // letters is still the key it was.
    for letter in ['r', 'y', 'x', 'n', 'p', 's', 'q', '?', ' '] {
        let mut typed = OverlayDecoder::new();
        let mut buffer = [0u8; 1];
        letter.encode_utf8(&mut buffer);
        assert_eq!(
            typed.feed(&buffer),
            vec![OverlayInput::Key(OverlayKey::Char(letter))],
            "POSITIVE CONTROL: {letter:?} typed as its own read is still \
                 one key, so the paste assertions above are about bursts and \
                 not about letters having stopped working"
        );
    }

    // POSITIVE CONTROL, **F65**: the pending-state decoder is unchanged.
    // An arrow is one key, a lone `ESC` is held, and a key after a held
    // `ESC` is both.
    let mut f65 = OverlayDecoder::new();
    assert_eq!(
        f65.feed(b"\x1b[A"),
        vec![OverlayInput::Key(OverlayKey::Up)],
        "POSITIVE CONTROL (F65): an arrow key is not one byte and is not \
             a burst either"
    );
    assert_eq!(
        f65.feed(b"\x1b"),
        Vec::new(),
        "POSITIVE CONTROL (F65): a lone `ESC` is still held pending, \
             which is what the 50 ms grace resolves"
    );
    assert!(
        f65.escape_pending(),
        "POSITIVE CONTROL (F65): and the overlay can tell, which is what \
             the grace asks"
    );
    assert_eq!(
        f65.feed(b"q"),
        vec![
            OverlayInput::Key(OverlayKey::StepBack),
            OverlayInput::Key(OverlayKey::Char('q')),
        ],
        "POSITIVE CONTROL (F65): the `Esc` happened, and then the key did"
    );
    assert_eq!(
        OverlayDecoder::new().feed(b"\r"),
        vec![OverlayInput::Key(OverlayKey::Enter)],
        "POSITIVE CONTROL: a typed Enter is one key, so the paste rule is \
             about bursts and not about newlines"
    );
}

/// Finding **F68**, the sharp case: **a pasted correction never answers a
/// blocked agent's permission.**
///
/// `r` opens a decision read and the next key answers the gate, so a
/// paste whose bytes contain `r` and then `y` would approve a filesystem
/// write on a blocked agent. The agent's request must still be pending
/// afterwards, and that it is pending is proved rather than assumed: the
/// test answers it itself and the registry reports `Answered`, which only
/// a still-parked request can.
///
/// The POSITIVE CONTROL is the second half: the same two letters as two
/// reads really do answer, so this is not a test that passes because `r`
/// and `y` stopped working.
#[cfg(unix)]
#[tokio::test]
async fn a_pasted_correction_never_answers_a_blocked_agents_permission() {
    let first_r = PASTED_CORRECTION
        .find('r')
        .expect("NON-VACUOUS: the sample carries an `r`");
    assert!(
        PASTED_CORRECTION[first_r + 1..].contains('y'),
        "NON-VACUOUS: the sample really does carry `r` and then `y`, which \
             is what would answer the gate: {PASTED_CORRECTION:?}"
    );

    let (registry, id, answer) = registry_with_one_blocked_agent().await;
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, PASTED_CORRECTION.as_bytes());
    assert_eq!(
        registry.answer(id, Decision::Deny),
        subagent::AgentAnswerOutcome::Answered,
        "the paste answered nothing: agent #{id}'s request is still \
             parked, so this test's own answer is the one that reaches it. \
             Reports the paste produced: {:?}",
        overlay.reports
    );
    assert_eq!(
        answer.await.expect("the gate is resolved"),
        Decision::Deny,
        "and the decision the agent receives is this test's `Deny`, not a \
             pasted `y`"
    );

    // POSITIVE CONTROL, on a fresh blocked agent: `r` then `y`, each as
    // its own read, is exactly the keystroke pair the paste must not be.
    let (registry, id, answer) = registry_with_one_blocked_agent().await;
    let mut typed = OverlayUnderTest::new();
    typed.feed(&registry, b"r");
    typed.feed(&registry, b"y");
    assert_eq!(
        answer.await.expect("the gate is resolved"),
        Decision::AllowOnce,
        "POSITIVE CONTROL: typed as two reads, `r` then `y` really does \
             answer agent #{id}, so the assertion above is about the paste. \
             Reports: {:?}",
        typed.reports
    );
}

/// Requirement A3, `/ filter`: **the key opens a line and the filter it
/// collects applies to the rows.**
///
/// The filter is `o:beta`, read off the second agent's own task text, and
/// `o:` is the task by T4-S1's decision. The NON-VACUOUS halves are the
/// three rows visible before it and the row that is left after it: a view
/// that showed nothing, or everything, fails one of them.
///
/// **F65:** `/` is `0x2f`, which is not in a CSI sequence's final-byte
/// range (`0x40..=0x7e`), so it cannot be mistaken for the end of an
/// arrow — asserted here rather than reasoned about.
#[cfg(unix)]
#[tokio::test]
async fn the_filter_line_opens_on_slash_and_its_filter_applies_to_the_rows() {
    assert_eq!(
        OverlayDecoder::new().feed(b"\x1b[B/"),
        vec![
            OverlayInput::Key(OverlayKey::Down),
            OverlayInput::Key(OverlayKey::Char('/')),
        ],
        "F65: `/` is a key in its own right and no sequence's final byte, \
             so an arrow immediately before it is still an arrow"
    );

    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        ids.to_vec(),
        "NON-VACUOUS: all three agents are on screen before any filter"
    );

    overlay.feed(&registry, b"/");
    type_filter(&mut overlay, &registry, "o:beta");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        ids.to_vec(),
        "and nothing is filtered while the line is still being typed: the \
             filter applies on Enter, not per keystroke"
    );
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![ids[1]],
        "`o:beta` leaves the one agent whose task says `beta`, which is \
             the task text the fixture gave it. Reports: {:?}",
        overlay.reports
    );
}

/// **An invalid filter term is refused, and the rows are left exactly as
/// they were.**
///
/// A3 already decided a malformed filter is an error and never a silently
/// dropped term; what this card had to decide is what the *key* does with
/// that error, and the answer is: report the parser's own message and keep
/// the line open. Filtering to nothing would be the worst of the three
/// readings — the user sees an empty view and nothing says why — and
/// closing the line would make the user retype a filter that is one
/// character wrong.
///
/// The message is read **out of the parser** rather than retyped, so the
/// two cannot drift (**F58**).
#[cfg(unix)]
#[tokio::test]
async fn an_invalid_filter_term_is_refused_and_leaves_the_rows_alone() {
    let rejection = kitsu_tui::agents_view::parse_filter("z:foo")
        .expect_err("NON-VACUOUS: `z:` is not a key the grammar has")
        .message()
        .to_string();

    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"/");
    type_filter(&mut overlay, &registry, "z:foo");
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        ids.to_vec(),
        "a refused filter is not applied: all three rows are still on \
             screen, rather than a view filtered to nothing. Reports: {:?}",
        overlay.reports
    );
    assert!(
        overlay
            .reports
            .last()
            .is_some_and(|line| line.contains(&rejection)),
        "and the report is the parser's own message, which names the key \
             and lists the grammar: {rejection:?} is not in {:?}",
        overlay.reports
    );

    // The line is still open, so the filter can be corrected rather than
    // retyped from `/`. Observable without an accessor: over the row list
    // `x` would arm a stop and say so, and inside an open line it is text.
    let before = overlay.reports.len();
    overlay.feed(&registry, b"x");
    assert_eq!(
        overlay.reports.len(),
        before,
        "the line is still open: `x` is a character in it, not an armed \
             stop. Reports: {:?}",
        overlay.reports
    );

    // POSITIVE CONTROL for that reading: with no line open, the same byte
    // really does arm a stop and report it.
    let mut over_rows = OverlayUnderTest::new();
    over_rows.feed(&registry, b"x");
    assert_eq!(
        over_rows.reports.len(),
        1,
        "POSITIVE CONTROL: over the row list `x` arms and reports, so the \
             silence above is the open line and not a dead key: {:?}",
        over_rows.reports
    );
}

/// **An empty line clears the filter.**
///
/// The parser already says so — an empty string is the empty filter, which
/// matches every row — and this pins that `/` then Enter is the way back
/// to the whole list, rather than needing the view closed and reopened.
#[cfg(unix)]
#[tokio::test]
async fn an_empty_filter_line_clears_the_filter() {
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"/");
    type_filter(&mut overlay, &registry, "o:gamma");
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![ids[2]],
        "NON-VACUOUS: a filter really is in force before it is cleared. \
             Reports: {:?}",
        overlay.reports
    );
    overlay.feed(&registry, b"/");
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        ids.to_vec(),
        "an empty line is the empty filter: every row is back. Reports: \
             {:?}",
        overlay.reports
    );
}

/// **`Esc` cancels the line and leaves the filter in force**, on the same
/// 50 ms grace every other frame of this overlay uses (**F65**): `Esc`
/// steps back out of the frame you are in, and the line is the innermost
/// frame.
///
/// Cancelling is not clearing. A cancelled line that cleared the filter
/// would make `Esc` do two things at once, and the user who meant to
/// abandon a half-typed correction would lose the view they had set up.
///
/// The grace is the loop's and not the dispatch's, so the resolved `Esc`
/// is delivered here the way the loop delivers it: `ESC` and then a byte
/// that cannot be part of a sequence.
#[cfg(unix)]
#[tokio::test]
async fn escape_cancels_the_filter_line_and_keeps_the_filter_already_in_force() {
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"/");
    type_filter(&mut overlay, &registry, "o:beta");
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![ids[1]],
        "NON-VACUOUS: the filter is in force before the cancelled line"
    );

    overlay.feed(&registry, b"/");
    type_filter(&mut overlay, &registry, "o:gamma");
    // One `ESC` byte, then the grace expiring with nothing behind it —
    // which is the only thing that distinguishes `Esc` from the first byte
    // of an arrow, and the loop's own two steps.
    overlay.feed(&registry, b"\x1b");
    assert!(
        !overlay.quiet_tick(),
        "and the cancelled line did not close the view: `Esc` steps out of \
             one frame at a time"
    );
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![ids[1]],
        "the cancelled line changed nothing: `o:beta` is still the filter, \
             and `Esc` neither applied `o:gamma` nor cleared it. Reports: {:?}",
        overlay.reports
    );
}

/// **The filter line takes a non-ASCII term**, which is why it is an
/// editor line over raw bytes and not a prompt fed `OverlayKey::Char`.
///
/// `AgentRegistry::rename` accepts `café` — its rule is control
/// characters, not ASCII — so `n:café` is a filter for a name the registry
/// really can hold. `OverlayKey::Char` is `0x20..=0x7e` by construction,
/// asserted here: a line fed decoded keys could not express this filter at
/// all, which is the same shipped defect the `n` key has today.
#[cfg(unix)]
#[tokio::test]
async fn the_filter_line_takes_a_non_ascii_term_because_a_name_may_be_non_ascii() {
    const NAME: &str = "café";
    assert!(
        OverlayDecoder::new()
            .feed(NAME.as_bytes())
            .iter()
            .all(|input| {
                !matches!(input, OverlayInput::Key(OverlayKey::Char(typed)) if !typed.is_ascii())
            }),
        "NON-VACUOUS: the overlay's key decoder cannot carry a non-ASCII \
             character, so this filter is only expressible by a line that \
             reads the raw bytes: {:?}",
        OverlayDecoder::new().feed(NAME.as_bytes())
    );

    let (registry, ids) = registry_with_three_agents();
    assert!(
        matches!(
            registry.rename(ids[0], NAME),
            subagent::AgentRenameOutcome::Renamed { .. }
        ),
        "NON-VACUOUS: the registry really does accept the name `{NAME}` — \
             its rule is control characters, not ASCII-only"
    );

    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"/");
    // Byte by byte, as a terminal delivers typing: the multi-byte
    // character arrives split across reads, which is the other thing the
    // editor is here for.
    type_filter(&mut overlay, &registry, &format!("n:{NAME}"));
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![ids[0]],
        "`n:{NAME}` leaves the agent that carries that name. Reports: {:?}",
        overlay.reports
    );
}

/// **F68 in the filter line: a paste is content, and it never submits.**
///
/// Consistent with every other text field in this view — `kitsu-editor`'s
/// rule, T4-S2's prefix-byte rule and T4-S4b-ii's steer line. The
/// POSITIVE CONTROL is the Enter the user types afterwards, which applies
/// the pasted filter: the paste really did land in the line.
#[cfg(unix)]
#[tokio::test]
async fn a_paste_into_the_filter_line_is_content_and_never_submits() {
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"/");
    let mut pasted = Vec::from(OVERLAY_PASTE_START);
    pasted.extend_from_slice(b"o:beta");
    pasted.extend_from_slice(OVERLAY_PASTE_END);
    overlay.feed(&registry, &pasted);
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        ids.to_vec(),
        "the paste did not submit the line: the rows are untouched until \
             the user presses Enter. Reports: {:?}",
        overlay.reports
    );
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![ids[1]],
        "POSITIVE CONTROL: the typed Enter applies the pasted filter, so \
             the paste was content in the line and not nothing at all. \
             Reports: {:?}",
        overlay.reports
    );
}

/// **F70: a non-ASCII name typed at the `n` key reaches the registry
/// whole**, both typed byte by byte and with the multi-byte character
/// split across two reads.
///
/// The two NON-VACUOUS halves are the shape of the defect: the key
/// decoder cannot carry a single byte of `é`, and the registry accepts
/// the name anyway. The failure this replaces renamed the agent to `caf`.
#[cfg(unix)]
#[tokio::test]
async fn the_rename_line_takes_a_non_ascii_name_typed_and_split_across_reads() {
    assert!(
        NON_ASCII_NAME.len() > NON_ASCII_NAME.chars().count()
            && NON_ASCII_NAME
                .bytes()
                .any(|byte| !(0x20..=0x7e).contains(&byte)),
        "NON-VACUOUS: the name really is multi-byte and really does carry \
             a byte outside `OverlayKey::Char`'s `0x20..=0x7e`: {:?}",
        NON_ASCII_NAME.as_bytes()
    );
    assert!(
        decode_overlay_keys(NON_ASCII_NAME.as_bytes(), &mut OverlayPending::None)
            .iter()
            .all(|key| !matches!(key, OverlayKey::Char(typed) if !typed.is_ascii())),
        "NON-VACUOUS: the key decoder drops every non-ASCII byte, so this \
             name is expressible only by a line that reads the raw bytes: {:?}",
        decode_overlay_keys(NON_ASCII_NAME.as_bytes(), &mut OverlayPending::None)
    );

    let registry = subagent::AgentRegistry::new();
    let typed_id = registry
        .start("alpha: audit the overlay decoder".to_string())
        .unwrap();
    let split_id = registry
        .start("beta: rewrite the repaint loop".to_string())
        .unwrap();

    // Typed, one read per byte — which is itself the split case for `é`,
    // because `é` is two bytes and a typist produces one keystroke.
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"n");
    assert_eq!(
        overlay.draft(),
        Some(""),
        "NON-VACUOUS: `n` opened an empty rename line on the selected agent"
    );
    type_line(&mut overlay, &registry, NON_ASCII_NAME);
    assert_eq!(
        overlay.draft(),
        Some(NON_ASCII_NAME),
        "the line holds every character typed into it, `é` included"
    );
    overlay.feed(&registry, b"\r");
    assert_eq!(
        registry
            .thread_snapshot_dtos()
            .iter()
            .find(|dto| dto.thread.id == typed_id)
            .and_then(|dto| dto.thread.name.clone()),
        Some(NON_ASCII_NAME.to_string()),
        "the typed Enter renamed the agent to the whole name: {:?}",
        overlay.reports
    );

    // The same name with `é` deliberately torn between two reads: the
    // first chunk ends mid-character. Byte-by-byte typing really produces
    // this, and a decoder that drops the lead byte loses the character.
    let mut split = OverlayUnderTest::new();
    split.feed(&registry, b"\x1b[B");
    assert_eq!(
        split.selected(&registry),
        Some(split_id),
        "NON-VACUOUS: the second agent is the selected one, so the split \
             name below lands on an agent the typed one did not"
    );
    split.feed(&registry, b"n");
    split.feed(&registry, b"caf\xc3");
    split.feed(&registry, b"\xa9");
    assert_eq!(
        split.draft(),
        Some(NON_ASCII_NAME),
        "a multi-byte character split across two reads is one character \
             in the line, not two dropped bytes"
    );
    split.feed(&registry, b"\r");
    assert_eq!(
        registry
            .thread_snapshot_dtos()
            .iter()
            .find(|dto| dto.thread.id == split_id)
            .and_then(|dto| dto.thread.name.clone()),
        Some(NON_ASCII_NAME.to_string()),
        "and the split name reached the registry whole: {:?}",
        split.reports
    );
}

/// **The name reaches the row through the snapshot** (card T4-S4a's rule),
/// and the rows the frame paints are those rows.
///
/// `agent_view_rows` builds from `AgentRegistry::thread_snapshot_dtos`,
/// so the assertion is on the path the view really reads and not on a
/// private map a renamer might have written instead.
#[cfg(unix)]
#[tokio::test]
async fn the_typed_name_reaches_the_row_through_the_snapshot() {
    let registry = subagent::AgentRegistry::new();
    let id = registry
        .start("alpha: audit the overlay decoder".to_string())
        .unwrap();
    let mut overlay = OverlayUnderTest::new();
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| row.name.clone()),
        None,
        "NON-VACUOUS: the row carries no name before the rename"
    );
    overlay.feed(&registry, b"n");
    type_line(&mut overlay, &registry, NON_ASCII_NAME);
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| row.name.clone()),
        Some(NON_ASCII_NAME.to_string()),
        "the row the view paints carries the typed name, read back out of \
             the snapshot: {:?}",
        overlay.reports
    );
}

/// **F71: every registry refusal of a typed name is painted where the
/// user is looking**, and a refused rename leaves the previous name
/// intact.
///
/// The three refusals are the registry's own — `Empty`, `TooLong { max }`
/// and `Unprintable` — and each is asserted against
/// [`overlay_frame_rows`], the frame chain the loop paints from. That is
/// the whole point of the test: `self.reports` would say the message
/// exists while the user, looking at an open line, saw nothing. A test
/// that asserts a message exists has not asserted the message is on
/// screen.
///
/// The line stays open carrying the text, as `/`'s refused term does, so
/// the message and the text it is about are on screen together.
#[cfg(unix)]
#[tokio::test]
async fn each_refused_rename_is_painted_on_the_frame_the_name_was_typed_on() {
    let registry = subagent::AgentRegistry::new();
    let id = registry
        .start("alpha: audit the overlay decoder".to_string())
        .unwrap();
    // A name the agent already has, so every refusal below has something
    // to leave intact.
    assert!(
        matches!(
            registry.rename(id, "alpha"),
            subagent::AgentRenameOutcome::Renamed { .. }
        ),
        "NON-VACUOUS: the agent starts the test named, so a refusal that \
             silently cleared the name would be visible here"
    );

    let max = subagent::AgentRegistry::NAME_MAX_CHARS;
    let long = over_long_name();
    for (typed, expected) in [
            (
                String::new(),
                format!("a name for background agent #{id} cannot be empty"),
            ),
            (
                long.clone(),
                format!(
                    "a name for background agent #{id} is at most {max} characters; it was not shortened for you"
                ),
            ),
            (
                "two\nlines".to_string(),
                format!(
                    "a name for background agent #{id} cannot carry control characters"
                ),
            ),
        ] {
            let mut overlay = OverlayUnderTest::new();
            overlay.feed(&registry, b"n");
            // Delivered as a BRACKETED paste, not as typing. F72: the editor
            // submits unbracketed text at its first newline, so the control
            // character this case is about could never reach `rename` by being
            // typed — the assertion would be unsatisfiable by construction
            // (F58), for a reason that is the gap F72 records and not this
            // test's subject. A real paste in this surface *is* bracketed,
            // since the overlay writes `ESC[?2004h` on entry.
            let mut block = Vec::from(OVERLAY_PASTE_START);
            block.extend_from_slice(typed.as_bytes());
            block.extend_from_slice(OVERLAY_PASTE_END);
            overlay.feed(&registry, &block);
            assert_eq!(
                overlay.draft(),
                Some(typed.as_str()),
                "NON-VACUOUS: the line holds {typed:?} before the Enter that \
                 submits it"
            );
            let frame = overlay.feed_and_frame_at(&registry, b"\r", 120);
            assert!(
                frame.iter().any(|row| row.trim() == expected),
                "the refusal is painted on the frame that is on screen, not \
                 under a row list the open line covers (**F71**). Expected \
                 {expected:?} among the painted rows: {frame:?}"
            );
            assert_eq!(
                overlay.draft(),
                Some(typed.as_str()),
                "and the line is still open carrying {typed:?}, so the one \
                 wrong character is corrected rather than the name retyped — \
                 `/`'s refused term does exactly this"
            );
            assert_eq!(
                registry
                    .thread_snapshot_dtos()
                    .iter()
                    .find(|dto| dto.thread.id == id)
                    .and_then(|dto| dto.thread.name.clone()),
                Some("alpha".to_string()),
                "and the refused rename left the previous name intact"
            );
        }

    // POSITIVE CONTROL: an accepted name on the same path really does
    // rename, so every refusal above is the registry's rule and not a
    // line that had stopped working.
    let mut accepted = OverlayUnderTest::new();
    accepted.feed(&registry, b"n");
    type_line(&mut accepted, &registry, NON_ASCII_NAME);
    accepted.feed(&registry, b"\r");
    assert_eq!(
        registry
            .thread_snapshot_dtos()
            .iter()
            .find(|dto| dto.thread.id == id)
            .and_then(|dto| dto.thread.name.clone()),
        Some(NON_ASCII_NAME.to_string()),
        "POSITIVE CONTROL: {:?}",
        accepted.reports
    );
    assert!(
        accepted.draft().is_none(),
        "and an accepted name closes the line"
    );
}

/// **F68 in the rename line: a paste is content**, consistent with every
/// other text field in this view, and a pasted multi-line name is still
/// refused on submit — which is `AgentRenameOutcome::Unprintable`'s own
/// rule and not this card's.
///
/// The paste is **bracketed**, because the overlay enables `ESC[?2004h`
/// on entry and a real paste therefore arrives bracketed. The first
/// assertion is why that matters: fed unbracketed, the editor submits at
/// the newline (`kitsu-editor`'s `unbracketed_newlines_still_submit_each_line`),
/// which is F68 one layer down and is a hole the filter line already has.
#[cfg(unix)]
#[tokio::test]
async fn a_paste_into_the_rename_line_is_content_and_a_multi_line_name_is_refused() {
    assert_eq!(
        OverlayLine::new().feed(b"two\nlines"),
        SteerLineStep::Submit("two".to_string()),
        "NON-VACUOUS: an *unbracketed* newline submits inside the editor, \
             so the bracketing below is what makes this a paste and not two \
             lines of typing"
    );

    let registry = subagent::AgentRegistry::new();
    let id = registry
        .start("alpha: audit the overlay decoder".to_string())
        .unwrap();

    // One pasted line: content, and it does not submit.
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"n");
    let mut pasted = Vec::from(OVERLAY_PASTE_START);
    pasted.extend_from_slice("fast lane".as_bytes());
    pasted.extend_from_slice(OVERLAY_PASTE_END);
    overlay.feed(&registry, &pasted);
    assert_eq!(
        overlay.draft(),
        Some("fast lane"),
        "the paste is content in the line"
    );
    assert_eq!(
        registry
            .thread_snapshot_dtos()
            .iter()
            .find(|dto| dto.thread.id == id)
            .and_then(|dto| dto.thread.name.clone()),
        None,
        "and it did not submit: a paste never does"
    );
    overlay.feed(&registry, b"\r");
    assert_eq!(
        registry
            .thread_snapshot_dtos()
            .iter()
            .find(|dto| dto.thread.id == id)
            .and_then(|dto| dto.thread.name.clone()),
        Some("fast lane".to_string()),
        "POSITIVE CONTROL: the Enter the user types applies the pasted \
             name: {:?}",
        overlay.reports
    );

    // A multi-line paste: still content, newline included, and refused by
    // the registry when the user submits it.
    let mut multi = OverlayUnderTest::new();
    multi.feed(&registry, b"n");
    let mut block = Vec::from(OVERLAY_PASTE_START);
    block.extend_from_slice(PASTED_CORRECTION.as_bytes());
    block.extend_from_slice(OVERLAY_PASTE_END);
    multi.feed(&registry, &block);
    assert_eq!(
        multi.draft(),
        Some(PASTED_CORRECTION),
        "the whole block is content, newline included, and nothing \
             submitted at the newline"
    );
    let frame = multi.feed_and_frame(&registry, b"\r");
    assert!(
        frame.iter().any(|row| row.trim()
            == format!("a name for background agent #{id} cannot carry control characters")),
        "and the registry's refusal of a multi-line name is painted where \
             the user is looking (**F71**): {frame:?}"
    );
    assert_eq!(
        registry
            .thread_snapshot_dtos()
            .iter()
            .find(|dto| dto.thread.id == id)
            .and_then(|dto| dto.thread.name.clone()),
        Some("fast lane".to_string()),
        "and the previous name is intact"
    );
}

/// **F65 in the rename line: `Esc` cancels under the 50 ms grace, and an
/// arrow is not a cancel.**
///
/// The same grace the steer and filter lines inherit, through the same two
/// functions the loop takes (`overlay_escape_held` and
/// `overlay_resolve_held_escape`, **F69**). The NON-VACUOUS halves are
/// that the lone `ESC` byte has *not* cancelled before the grace expires,
/// and that `ESC [ D` leaves the line open with its text.
#[cfg(unix)]
#[tokio::test]
async fn esc_cancels_the_rename_line_under_the_grace_and_an_arrow_does_not() {
    let registry = subagent::AgentRegistry::new();
    let id = registry
        .start("alpha: audit the overlay decoder".to_string())
        .unwrap();

    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"n");
    type_line(&mut overlay, &registry, "half typed");
    overlay.feed(&registry, b"\x1b");
    assert_eq!(
        overlay.draft(),
        Some("half typed"),
        "NON-VACUOUS: a lone `ESC` byte is ambiguous until the grace \
             expires, so the line is still open and still holds its text"
    );
    assert!(
        !overlay.quiet_tick(),
        "the grace expired on the rename line, which cancels the line and \
             not the view"
    );
    assert!(
        overlay.draft().is_none(),
        "`Esc` cancelled the line: it steps out of one frame"
    );
    assert_eq!(
        registry
            .thread_snapshot_dtos()
            .iter()
            .find(|dto| dto.thread.id == id)
            .and_then(|dto| dto.thread.name.clone()),
        None,
        "and nothing was renamed: a cancel sends nothing. {:?}",
        overlay.reports
    );

    // An arrow is not one byte, and it is not a cancel (**F65**).
    let mut arrowed = OverlayUnderTest::new();
    arrowed.feed(&registry, b"n");
    type_line(&mut arrowed, &registry, "half typed");
    arrowed.feed(&registry, b"\x1b");
    arrowed.feed(&registry, b"[D");
    assert_eq!(
        arrowed.draft(),
        Some("half typed"),
        "the `ESC` was the first byte of a left arrow, so the line is \
             still open with its text — **F65**"
    );
    assert!(
        !arrowed.quiet_tick(),
        "and the sequence resolved, so no `ESC` is held any more"
    );
    assert_eq!(
        arrowed.draft(),
        Some("half typed"),
        "NON-VACUOUS: the following quiet tick did not cancel the line \
             either, because the arrow consumed the `ESC`"
    );
}

/// Requirement A3, *`Enter`/`→` attach*: **the key names the agent and
/// leaves the view.**
///
/// The help frame is the non-vacuous control, and it is the rule every
/// other key over the row list already follows (`↑/↓`, `Space`, the
/// action keys): the selected row is not on screen there, so a key that
/// acted on it would act on something invisible.
#[cfg(unix)]
#[tokio::test]
async fn enter_attaches_the_selected_agent_and_leaves_the_view() {
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    assert_eq!(
        overlay.selected(&registry),
        Some(ids[0]),
        "NON-VACUOUS: the first row is selected before any key"
    );
    assert_eq!(
        overlay.state.attaching, None,
        "NON-VACUOUS: nothing is attached before the key"
    );

    // Inert while the help is up.
    overlay.feed(&registry, b"?");
    let closing = overlay.feed_closing(&registry, b"\r");
    assert_eq!(
        overlay.state.attaching, None,
        "`Enter` must not attach while the help frame covers the row list"
    );
    assert!(!closing, "and must not close the view from the help either");

    // Out of the help, and attach.
    overlay.feed(&registry, b"\x1b");
    assert!(!overlay.quiet_tick(), "`Esc` leaves the help, not the view");
    let closing = overlay.feed_closing(&registry, b"\r");
    assert_eq!(
        overlay.state.attaching,
        Some(ids[0]),
        "A3: `Enter` attaches the selected agent"
    );
    assert!(
        closing,
        "and leaves the view: A5 streams the agent into the main viewport, \
             which a full-screen overlay would cover"
    );
}

/// `→` is the same key as `Enter` here, and it has to be **decoded**
/// first: `arrow` reads `Up` and `Down` out of a sequence's final byte
/// and drops `C` today, so the decoder half is asserted in its own right
/// before the dispatch half.
///
/// Both spellings, because a terminal in application-cursor mode sends
/// the SS3 one and `arrow` is already reached from both.
#[cfg(unix)]
#[tokio::test]
async fn the_right_arrow_decodes_and_attaches_exactly_as_enter_does() {
    assert_eq!(
        OverlayDecoder::new().feed(b"\x1b[C"),
        vec![OverlayInput::Key(OverlayKey::Right)],
        "CSI C is `\u{2192}`"
    );
    assert_eq!(
        OverlayDecoder::new().feed(b"\x1bOC"),
        vec![OverlayInput::Key(OverlayKey::Right)],
        "and so is SS3 C, which an application-cursor-mode terminal sends"
    );
    // NON-VACUOUS: the neighbouring final byte still decodes to the key it
    // always did, so this is a new arm and not a widened one.
    assert_eq!(
        OverlayDecoder::new().feed(b"\x1b[B"),
        vec![OverlayInput::Key(OverlayKey::Down)],
        "CSI B is still `\u{2193}`"
    );

    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    let closing = overlay.feed_closing(&registry, b"\x1b[C");
    assert_eq!(
        overlay.state.attaching,
        Some(ids[0]),
        "A3: `\u{2192}` attaches the selected agent, as `Enter` does"
    );
    assert!(closing, "and leaves the view, as `Enter` does");
}

/// **Requirement A6: the key opens the view's fourth line.**
///
/// Pinned through the painter and not through the state (**F71**): the
/// frame the loop would paint after the keystroke *is* the dispatch
/// line's, built by the one function that builds it.
///
/// The NON-VACUOUS half is the frame before the key: the row list, which
/// is not the dispatch frame.
#[cfg(unix)]
#[tokio::test]
async fn the_dispatch_key_opens_the_views_fourth_edited_line() {
    let (registry, _) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    let (dispatcher, _seen) = recording_dispatcher(Ok(4));
    overlay.state.dispatcher = Some(dispatcher);

    let before = overlay.feed_and_frame(&registry, b"");
    let empty_line = kitsu_tui::agents_view::dispatch_prompt_rows("", 0, OVERLAY_TEST_COLS, None);
    assert_ne!(
        before, empty_line,
        "NON-VACUOUS: the view is painting its row list before the key"
    );

    let frame = overlay.feed_and_frame(&registry, &dispatch_key());
    assert_eq!(
        frame, empty_line,
        "the key opens an empty dispatch line, and the frame is the one \
             `dispatch_prompt_rows` paints"
    );
}

/// **A6's two clauses, typed**: plain text dispatches with the default
/// profile, and `@<profile> <task>` selects one -- through the loop's own
/// byte path, one `read` per keystroke, which is how a terminal delivers
/// typing.
#[cfg(unix)]
#[tokio::test]
async fn a_typed_dispatch_reaches_the_dispatcher_with_and_without_a_profile() {
    let (registry, _) = registry_with_three_agents();
    for (typed, expected) in [
        (
            "measure the paste latency",
            ("measure the paste latency".to_string(), None),
        ),
        (
            "@reviewer review the diff",
            ("review the diff".to_string(), Some("reviewer".to_string())),
        ),
    ] {
        let mut overlay = OverlayUnderTest::new();
        let (dispatcher, seen) = recording_dispatcher(Ok(4));
        overlay.state.dispatcher = Some(dispatcher);
        overlay.feed(&registry, &dispatch_key());
        type_filter(&mut overlay, &registry, typed);
        assert!(
            seen.lock().expect("the recording dispatcher").is_empty(),
            "nothing is dispatched while the line is still being typed: a \
                 dispatch happens on Enter, not per keystroke"
        );
        overlay.feed(&registry, b"\r");
        assert_eq!(
            *seen.lock().expect("the recording dispatcher"),
            vec![expected.clone()],
            "{typed:?} dispatches exactly once, with the task and profile \
                 A6 says it has. Reports: {:?}",
            overlay.reports
        );
    }
}

/// **The card's named acceptance: at `max_running` the dispatch is refused,
/// the refusal names the ceiling, and the user can see it.**
///
/// *Can see it* is the whole point (**F71**, and four findings in that
/// family): the refusal is asserted on the rows
/// [`overlay_frame_rows`] would paint, which is the only surface that
/// answers whether a message is on screen. A refused dispatch leaves the
/// line **open** carrying the task, as `/`'s refused term and `n`'s
/// refused name do, so the message and the text it is about are on screen
/// together and the user can wait and press Enter again rather than
/// retyping the task.
///
/// **F78/F58**: painted at 120 columns, where the registry's sentence
/// demonstrably fits -- at 80 it would be truncated and the comparison
/// below would be unsatisfiable arithmetic.
#[cfg(unix)]
#[tokio::test]
async fn an_over_capacity_dispatch_is_refused_on_the_frame_and_the_line_stays_open() {
    const WIDE: usize = 120;
    let error = subagent::AgentStartError {
        kind: subagent::AgentCapacityKind::Running,
        current: 8,
        limit: 8,
    };
    assert!(
        error.to_string().chars().count() + 2 <= WIDE,
        "F58: the refusal must fit the width it is asserted at, indent \
             included: {}",
        error
    );

    let (registry, _) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    let (dispatcher, seen) = recording_dispatcher(Err(error.to_string()));
    overlay.state.dispatcher = Some(dispatcher);
    overlay.feed(&registry, &dispatch_key());
    type_filter(&mut overlay, &registry, "one more agent");
    let frame = overlay.feed_and_frame_at(&registry, b"\r", WIDE);

    assert_eq!(
        seen.lock().expect("the recording dispatcher").len(),
        1,
        "the dispatch really was attempted, and refused by the registry"
    );
    assert!(
        frame.iter().any(|row| row.trim() == error.to_string()),
        "the ceiling that refused is painted on the frame the task was \
             typed on, in the registry's own words:\n{}",
        frame.join("\n")
    );
    assert!(
        frame.iter().any(|row| row.contains("one more agent")),
        "and the task is still on screen beside it, because the line \
             stayed open:\n{}",
        frame.join("\n")
    );

    // The line really is still open, asserted on the paint and not on a
    // flag: inside a line `x` is a character, and over the row list it
    // arms a stop and says so. The refused dispatch can therefore be
    // retried with one keystroke rather than retyped.
    let after_x = overlay.feed_and_frame_at(&registry, b"x", WIDE);
    assert!(
        after_x.iter().any(|row| row.contains("one more agentx")),
        "the line is still open and `x` went into it as text:\n{}",
        after_x.join("\n")
    );
    let mut over_rows = OverlayUnderTest::new();
    let armed = over_rows.feed_and_frame_at(&registry, b"x", WIDE);
    assert!(
        armed.iter().any(|row| row.contains("press x again")),
        "POSITIVE CONTROL: over the row list the same byte arms a stop and \
             says so, so the frame above is an open line and not a dead \
             key:\n{}",
        armed.join("\n")
    );
}

/// **A6 names both ceilings, and they fail differently.**
///
/// `max_records` is not `max_running`: a pruned-but-recorded agent still
/// occupies a record, so a view with nothing running can still refuse, and
/// the correction is different -- prune a terminal agent rather than wait
/// for a running one. Both are painted, and the two sentences differ.
#[cfg(unix)]
#[tokio::test]
async fn a_record_capacity_refusal_is_painted_and_says_something_else() {
    const WIDE: usize = 120;
    let running = subagent::AgentStartError {
        kind: subagent::AgentCapacityKind::Running,
        current: 8,
        limit: 8,
    };
    let records = subagent::AgentStartError {
        kind: subagent::AgentCapacityKind::Records,
        current: 128,
        limit: 128,
    };
    assert_ne!(
        running.to_string(),
        records.to_string(),
        "NON-VACUOUS: the registry already distinguishes the two ceilings"
    );
    assert!(
        records.to_string().chars().count() + 2 <= WIDE,
        "F58: {records}"
    );

    let (registry, _) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    let (dispatcher, _seen) = recording_dispatcher(Err(records.to_string()));
    overlay.state.dispatcher = Some(dispatcher);
    overlay.feed(&registry, &dispatch_key());
    type_filter(&mut overlay, &registry, "one more agent");
    let frame = overlay.feed_and_frame_at(&registry, b"\r", WIDE);
    assert!(
        frame.iter().any(|row| row.trim() == records.to_string()),
        "the record ceiling is painted in its own words:\n{}",
        frame.join("\n")
    );
    assert!(
        !frame.iter().any(|row| row.trim() == running.to_string()),
        "and not as the other ceiling's:\n{}",
        frame.join("\n")
    );
}

/// **A started dispatch is on screen at once, and it does not move the
/// selection.**
///
/// Two claims a user meets immediately. The first is that the view does
/// not need closing and reopening to show the agent just started -- its
/// rows are read off the registry every repaint, so the record the
/// dispatcher made is already among them. The display order is derived
/// through `agents_view::group_rows` and never off `overlay_visible_rows`,
/// which is **registry** order (**F73**).
///
/// The second is a safety property, and it is T4-S4a's: the keys act on
/// the **selected** agent, so a dispatch that silently moved the selection
/// would repoint the next `x` or `p` at an agent the user never chose.
#[cfg(unix)]
#[tokio::test]
async fn a_dispatched_agent_is_on_screen_at_once_and_moves_no_selection() {
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    // The dispatcher a session builds really does make a record; this one
    // makes the same record without a provider.
    let registered = registry.clone();
    let dispatcher: subagent::ViewDispatcher =
        Arc::new(move |task, _profile| registered.start(task).map_err(|e| e.to_string()));
    overlay.state.dispatcher = Some(dispatcher);

    overlay.feed(&registry, b"\x1b[B");
    assert_eq!(
        overlay.selected(&registry),
        Some(ids[1]),
        "NON-VACUOUS: the selection is on the second agent, which is not \
             where a fresh view leaves it and not the agent about to start"
    );

    overlay.feed(&registry, &dispatch_key());
    type_filter(&mut overlay, &registry, "delta: dispatched from the view");
    overlay.feed(&registry, b"\r");

    let displayed: Vec<u64> = kitsu_tui::agents_view::group_rows(overlay.visible(&registry))
        .into_iter()
        .flat_map(|group| group.rows)
        .map(|row| row.id)
        .collect();
    assert_eq!(
        displayed.len(),
        4,
        "the agent just dispatched is among the rows on screen, with no \
             reopen: {displayed:?}. Reports: {:?}",
        overlay.reports
    );
    assert_eq!(
        displayed.iter().filter(|id| !ids.contains(id)).count(),
        1,
        "and it is a row the fixture did not put there: {displayed:?}"
    );
    assert_eq!(
        overlay.selected(&registry),
        Some(ids[1]),
        "and the selection is where the user left it: a dispatch is not a \
             selection move, and moving it would repoint the next x or p"
    );
    assert!(
        overlay
            .reports
            .last()
            .is_some_and(|line| line.contains('#')),
        "and the view says which agent it started: {:?}",
        overlay.reports
    );
}

/// **A paste into the dispatch line is content and never submits** --
/// finding **F68**, one layer down, which is why this line is on the byte
/// bypass with the other three: the editor applies the bracketed-paste
/// rule and the overlay's decoder is not in the path.
///
/// A pasted task really does contain newlines, and a line that submitted
/// at the first one would start an agent on half a task and leave the rest
/// as keys over the row list -- where `x` stops agents.
#[cfg(unix)]
#[tokio::test]
async fn a_paste_into_the_dispatch_line_is_content_and_never_submits() {
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    let (dispatcher, seen) = recording_dispatcher(Ok(4));
    overlay.state.dispatcher = Some(dispatcher);
    overlay.feed(&registry, &dispatch_key());

    let mut pasted = Vec::new();
    pasted.extend_from_slice(OVERLAY_PASTE_START);
    pasted.extend_from_slice(PASTED_CORRECTION.as_bytes());
    pasted.extend_from_slice(OVERLAY_PASTE_END);
    let frame = overlay.feed_and_frame(&registry, &pasted);

    assert!(
        seen.lock().expect("the recording dispatcher").is_empty(),
        "a paste never submits: nothing was dispatched"
    );
    assert_eq!(
        registry.progress_snapshot().total,
        ids.len(),
        "and the `x` bytes inside the paste stopped nothing"
    );
    assert_eq!(
        frame,
        kitsu_tui::agents_view::dispatch_prompt_rows(
            &overlay
                .state
                .dispatching
                .as_ref()
                .map(|line| line.draft().to_string())
                .expect("the line is still open, carrying the pasted text"),
            overlay
                .state
                .dispatching
                .as_ref()
                .map(OverlayLine::cursor)
                .expect("the line is still open"),
            OVERLAY_TEST_COLS,
            None,
        ),
        "the view is still painting the dispatch line, with the paste in \
             it as content"
    );
    assert!(
        overlay
            .state
            .dispatching
            .as_ref()
            .is_some_and(|line| line.draft().contains("and fix the bug")),
        "and the pasted text really is the draft: {:?}",
        overlay.state.dispatching.as_ref().map(OverlayLine::draft)
    );

    // POSITIVE CONTROL: the same line, typed and submitted, really does
    // dispatch -- so the silence above is the paste rule and not a line
    // that had stopped working.
    overlay.feed(&registry, b"\r");
    assert_eq!(
        seen.lock().expect("the recording dispatcher").len(),
        1,
        "POSITIVE CONTROL: a typed Enter submits the line the paste filled"
    );
}

/// **`Esc` cancels the dispatch line and starts nothing**, resolved on the
/// loop's own 50 ms grace, exactly as the other three lines resolve theirs
/// -- and an arrow key is still an arrow and not a cancel.
#[cfg(unix)]
#[tokio::test]
async fn esc_cancels_the_dispatch_line_and_starts_nothing() {
    let (registry, _) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    let (dispatcher, seen) = recording_dispatcher(Ok(4));
    overlay.state.dispatcher = Some(dispatcher);
    overlay.feed(&registry, &dispatch_key());
    type_filter(&mut overlay, &registry, "delta: never started");

    // An arrow arrives as `ESC [ B`: the line is fed the bytes and no
    // grace is held, so this is not a cancel.
    overlay.feed(&registry, b"\x1b[B");
    assert!(
        !overlay.quiet_tick(),
        "an arrow is not a held ESC, so the grace has nothing to resolve"
    );
    assert!(
        overlay.state.dispatching.is_some(),
        "and the line is still open after it"
    );

    overlay.feed(&registry, b"\x1b");
    assert!(
        !overlay.quiet_tick(),
        "the view does not close: Esc steps out of the line it is in"
    );
    assert!(
        overlay.state.dispatching.is_none(),
        "the held ESC resolved as the Esc key and cancelled the line"
    );
    assert!(
        seen.lock().expect("the recording dispatcher").is_empty(),
        "and nothing was dispatched"
    );
    assert_eq!(
        overlay.feed_and_frame(&registry, b""),
        {
            let rows = overlay.visible(&registry);
            kitsu_tui::agents_view::overlay_rows_with_report(
                rows,
                OVERLAY_TEST_COLS,
                overlay.state.selection.selected(),
                None,
            )
        },
        "the view is back on its row list with no stale report under it"
    );
}
