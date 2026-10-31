use super::*;

/// The card's safety question: **a filter that hides the selected agent
/// moves the selection; it never leaves a destructive key pointed at an
/// agent that is not on screen.**
///
/// This is the F68 family under another name — `x` is a privileged action
/// and the user's intent for it is the row they can see — and it is the
/// reason `AgentSelection::sync` is handed the **filtered** rows. The
/// POSITIVE CONTROL is the second half: the same two `x` presses with no
/// filter really do stop the first agent, so this is not a test that
/// passes because `x` stopped working.
#[cfg(unix)]
#[tokio::test]
async fn a_filter_that_hides_the_selected_agent_moves_the_selection_off_it() {
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    assert_eq!(
        overlay.selected(&registry),
        Some(ids[0]),
        "NON-VACUOUS: the first row is selected before the filter, so the \
             filter below really does hide the selected agent"
    );

    overlay.feed(&registry, b"/");
    type_filter(&mut overlay, &registry, "o:beta");
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay.selected(&registry),
        Some(ids[1]),
        "the selection is on the row the user can see: the filtered rows \
             are what `sync` is handed, so a hidden agent cannot stay \
             selected. Reports: {:?}",
        overlay.reports
    );

    overlay.feed(&registry, b"x");
    overlay.feed(&registry, b"x");
    let rows = agent_view_rows(&registry);
    let state_of = |id: u64| rows.iter().find(|row| row.id == id).map(|row| row.state);
    assert_eq!(
        state_of(ids[0]),
        Some(kitsu_render::AgentViewState::Working),
        "the hidden agent #{} was not stopped: `x` acted on the visible \
             row. Rows: {rows:?}, reports: {:?}",
        ids[0],
        overlay.reports
    );
    assert_ne!(
        state_of(ids[1]),
        Some(kitsu_render::AgentViewState::Working),
        "POSITIVE CONTROL: `x` twice really did stop the *visible* agent \
             #{}, so the assertion above is about which agent it hit and not \
             about a key that does nothing. Rows: {rows:?}, reports: {:?}",
        ids[1],
        overlay.reports
    );
}

/// The same question with the filter matching **nothing**: there is no row
/// to fall to, so there is **no selection**, and the action keys say so
/// rather than acting on the agent that was selected before.
///
/// `sync` already answers this — `None` when the displayed list is empty —
/// and the point of the test is that it is the *filtered* list it is asked
/// about. The NON-VACUOUS half is the three agents the registry still
/// holds: nothing was stopped, they are merely not shown.
#[cfg(unix)]
#[tokio::test]
async fn a_filter_that_matches_nothing_leaves_no_selection_for_a_key_to_act_on() {
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"/");
    type_filter(&mut overlay, &registry, "o:delta");
    overlay.feed(&registry, b"\r");
    assert!(
        overlay.visible(&registry).is_empty(),
        "`o:delta` matches no task the fixture gave: the view is empty"
    );
    assert_eq!(
        agent_view_rows(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        ids.to_vec(),
        "NON-VACUOUS: the agents are all still there — hidden, not gone"
    );
    assert_eq!(
        overlay.selected(&registry),
        None,
        "and nothing is selected, so no key can act on an agent that is \
             not on screen"
    );

    let before = overlay.reports.len();
    overlay.feed(&registry, b"x");
    assert_eq!(
        overlay.reports[before..],
        [agent_view_no_selection('x').line],
        "`x` reports that there is nothing selected — the view's own \
             no-selection line, not a stop"
    );
    assert!(
        agent_view_rows(&registry)
            .iter()
            .all(|row| row.state == kitsu_render::AgentViewState::Working),
        "and every agent is still running: {:?}",
        agent_view_rows(&registry)
    );
}

/// **The filter survives an agent finishing and the rows re-grouping, and
/// does not survive the view being closed.**
///
/// A filter that evaporated on a regroup would be useless: the rows
/// regroup constantly, which is T4-S4a's whole premise. A filter that
/// outlived the screen it was set on would be worse — the next `claude
/// agents` would open hiding agents for a reason nothing on screen
/// explains, and the keys would act inside that hidden state.
///
/// A reopen is a fresh [`OverlayState`], which is exactly what the loop
/// builds on entry, so a fresh harness is the reopen.
#[cfg(unix)]
#[tokio::test]
async fn the_filter_survives_a_regroup_and_not_a_reopen() {
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, b"/");
    type_filter(&mut overlay, &registry, "s:working");
    overlay.feed(&registry, b"\r");
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        ids.to_vec(),
        "NON-VACUOUS: `s:working` matches all three while all three are \
             working. Reports: {:?}",
        overlay.reports
    );

    // The regroup: one agent leaves `working`, which in A2's order is a
    // move to a later group.
    let _ = agent_view_stop_or_prune(&registry, ids[2]).await;
    assert_ne!(
        agent_view_rows(&registry)
            .iter()
            .find(|row| row.id == ids[2])
            .map(|row| row.state),
        Some(kitsu_render::AgentViewState::Working),
        "NON-VACUOUS: agent #{} really did leave `working`",
        ids[2]
    );
    assert_eq!(
        overlay
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![ids[0], ids[1]],
        "the filter is still in force across the regroup: the agent that \
             left `working` left the view with it"
    );

    let reopened = OverlayUnderTest::new();
    assert_eq!(
        reopened
            .visible(&registry)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>()
            .len(),
        3,
        "and a reopened view shows every agent: a filter does not outlive \
             the screen it was set on"
    );
}
