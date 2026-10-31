use super::*;

/// Finding **F68**, inside a text prompt: **a multi-line paste is
/// content, and its tail never executes.**
///
/// What the newline does is the decision this card had to make, and it is
/// *content*: `kitsu-editor` already holds that a bracketed paste never
/// submits, and T4-S2 decided a prefix byte inside a paste is content.
/// Dropping the tail would be a second answer to the same question in the
/// same binary. So the whole paste lands in the draft, the prompt stays
/// open, nothing is sent, and the user presses Enter themselves — which
/// is also the only reading under which the bytes after the newline can
/// never be keys, because there is no submit for them to be after.
///
/// The POSITIVE CONTROL is the typed Enter at the end: it submits, with
/// the whole pasted correction as the name, so the draft really was live
/// and the prompt really was open.
#[cfg(unix)]
#[tokio::test]
async fn a_multi_line_paste_into_the_rename_line_is_content_and_its_tail_never_runs() {
    assert!(
        PASTED_CORRECTION.contains('\n')
            && !PASTED_CORRECTION.ends_with('\n')
            && PASTED_CORRECTION.lines().count() == 2,
        "NON-VACUOUS: the sample is two lines, so there is a tail after \
             the newline that could execute: {PASTED_CORRECTION:?}"
    );

    let registry = subagent::AgentRegistry::new();
    let id = registry.start("needs a name".to_string()).unwrap();
    let mut overlay = OverlayUnderTest::new();
    // `n` opens the rename line, typed as its own read.
    overlay.feed(&registry, b"n");
    assert_eq!(
        overlay.draft(),
        Some(""),
        "NON-VACUOUS: the rename line is open and empty before the paste"
    );
    let mut block = Vec::from(OVERLAY_PASTE_START);
    block.extend_from_slice(PASTED_CORRECTION.as_bytes());
    block.extend_from_slice(OVERLAY_PASTE_END);
    overlay.feed(&registry, &block);
    assert_eq!(
        overlay.draft(),
        Some(PASTED_CORRECTION),
        "the whole paste is content, newline included: a paste never \
             submits, so the tail is text in the draft and not keys"
    );
    assert!(
        overlay.reports.is_empty(),
        "and nothing was submitted and nothing acted: {:?}",
        overlay.reports
    );
    assert_eq!(
        agent_view_rows(&registry)
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| row.name.clone()),
        None,
        "the agent is still unnamed: the newline inside the paste did not \
             submit the line"
    );

    // The Enter the user types themselves *does* submit — and the
    // registry refuses a name carrying a control character, which is its
    // own rule (`AgentRenameOutcome::Unprintable`) and not this card's.
    // What matters here is that the submit happened only now.
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay.reports.len(),
        1,
        "the typed Enter submitted the line, once: {:?}",
        overlay.reports
    );
    assert_eq!(
        agent_view_rows(&registry)
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| row.name.clone()),
        None,
        "and the registry refuses a name with a newline in it, which is \
             `AgentRenameOutcome::Unprintable`'s rule: {:?}",
        overlay.reports
    );

    // POSITIVE CONTROL, on a fresh prompt: a name typed one read at a
    // time and a typed Enter really do rename the agent, so every
    // assertion above is about the paste and not about a prompt that had
    // stopped working.
    let mut typed = OverlayUnderTest::new();
    typed.feed(&registry, b"n");
    for byte in b"ok" {
        typed.feed(&registry, &[*byte]);
    }
    assert_eq!(typed.draft(), Some("ok"));
    typed.feed(&registry, b"\r");
    assert_eq!(
        agent_view_rows(&registry)
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| row.name.clone()),
        Some("ok".to_string()),
        "POSITIVE CONTROL: typed keys fill the line and a typed Enter \
             submits it. Reports: {:?}",
        typed.reports
    );
    assert!(
        typed.draft().is_none(),
        "and the prompt is closed after a submit"
    );
}
