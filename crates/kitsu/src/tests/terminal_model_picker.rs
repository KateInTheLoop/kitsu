use super::*;

#[test]
fn model_popup_step_filters_moves_completes_picks_and_cancels() {
    let models = vec![
        "global.anthropic.claude-opus-5-5".to_string(),
        "global.anthropic.claude-opus-4-8".to_string(),
        "gemma-4-26b".to_string(),
    ];
    let mut p = ModelPopup::default();
    // Typing filters; selection resets to the top of the narrowed list.
    for c in "opus".chars() {
        assert_eq!(
            model_popup_step(OverlayKey::Char(c), &mut p, &models),
            ModelPopupStep::Continue
        );
    }
    assert_eq!(p.filter, "opus");
    let matched = p.filtered(&models);
    assert_eq!(matched.len(), 2, "only the two opus ids match");
    // Down moves within the filtered list and clamps.
    model_popup_step(OverlayKey::Down, &mut p, &models);
    assert_eq!(p.selected, 1);
    model_popup_step(OverlayKey::Down, &mut p, &models);
    assert_eq!(p.selected, 1, "clamped at the bottom of the filtered list");
    // Enter picks the highlighted filtered id (order is the filter's, not
    // the test's — filter_model_catalog sorts).
    assert_eq!(
        model_popup_step(OverlayKey::Enter, &mut p, &models),
        ModelPopupStep::Pick(matched[1].clone()),
    );
    // Backspace trims the filter and re-clamps.
    model_popup_step(OverlayKey::Backspace, &mut p, &models);
    assert_eq!(p.filter, "opu");
    // Tab completes the selection into the filter.
    let mut t = ModelPopup {
        filter: "gem".to_string(),
        selected: 0,
    };
    model_popup_step(OverlayKey::Tab, &mut t, &models);
    assert_eq!(t.filter, "gemma-4-26b", "Tab completes the highlighted id");
    // Esc and Ctrl-C cancel.
    assert_eq!(
        model_popup_step(OverlayKey::StepBack, &mut p, &models),
        ModelPopupStep::Cancel
    );
    assert_eq!(
        model_popup_step(OverlayKey::Interrupt, &mut p, &models),
        ModelPopupStep::Cancel,
    );
}

#[test]
fn overlay_decoder_emits_tab_and_ctrl_c_for_the_model_popup() {
    // Review F4: through the REAL decoder, not hand-built keys. Before the
    // fix 0x09 and 0x03 were dropped, so Tab-complete and Ctrl-C cancel in
    // the popup could never fire.
    let mut pending = OverlayPending::None;
    assert_eq!(
        decode_overlay_keys(b"\t\x03", &mut pending),
        vec![OverlayKey::Tab, OverlayKey::Interrupt]
    );
    // And the popup acts on them end to end.
    let models = vec!["gemma-4-26b".to_string(), "opus".to_string()];
    let mut p = ModelPopup {
        filter: "gem".to_string(),
        selected: 0,
    };
    for key in decode_overlay_keys(b"\t", &mut pending) {
        model_popup_step(key, &mut p, &models);
    }
    assert_eq!(p.filter, "gemma-4-26b", "Tab from the wire completes");
    let step = decode_overlay_keys(b"\x03", &mut pending)
        .into_iter()
        .map(|key| model_popup_step(key, &mut p, &models))
        .next();
    assert_eq!(
        step,
        Some(ModelPopupStep::Cancel),
        "Ctrl-C from the wire cancels"
    );
}

#[test]
fn model_popup_step_paste_only_edits_the_filter_never_acts() {
    // A paste arrives as its characters. Even containing what look like
    // commands, it can only extend the filter — never pick or cancel.
    let models = vec!["opus".to_string(), "sonnet".to_string()];
    let mut p = ModelPopup::default();
    for c in "son".chars() {
        assert_eq!(
            model_popup_step(OverlayKey::Char(c), &mut p, &models),
            ModelPopupStep::Continue
        );
    }
    assert_eq!(p.filter, "son");
    assert_eq!(p.filtered(&models), vec!["sonnet".to_string()]);
}

#[test]
fn model_popup_view_marks_selection_filter_and_scrolls() {
    let models: Vec<String> = (0..48).map(|i| format!("m{i:02}")).collect();
    // Unfiltered, ample budget: header + blank + current tag + selection.
    let p = ModelPopup {
        filter: String::new(),
        selected: 2,
    };
    let rows = model_popup_view(&models, "m01", &p, 80, 24);
    assert!(
        rows[0].contains("type to filter"),
        "header cue: {:?}",
        rows[0]
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("m01") && r.contains("(current)")),
        "current tagged"
    );
    assert!(
        rows.iter().any(|r| r.starts_with("> m02")),
        "selection marked"
    );
    // Filtered header shows the filter text; only matches render.
    let f = ModelPopup {
        filter: "m4".to_string(),
        selected: 0,
    };
    let frows = model_popup_view(&models, "m01", &f, 80, 24);
    assert!(
        frows[0].contains("filter: m4"),
        "filter shown: {:?}",
        frows[0]
    );
    assert!(frows
        .iter()
        .skip(2)
        .all(|r| r.trim().is_empty() || r.contains("m4") || r.contains("more")));
    // A filter that matches nothing says so, never panics.
    let none = ModelPopup {
        filter: "zzz".to_string(),
        selected: 5,
    };
    let nrows = model_popup_view(&models, "m01", &none, 80, 24);
    assert!(
        nrows.iter().any(|r| r.contains("no model matches")),
        "empty-filter message"
    );
    // Small budget scrolls with markers, keeps selection visible.
    let deep = ModelPopup {
        filter: String::new(),
        selected: 40,
    };
    let drows = model_popup_view(&models, "m00", &deep, 80, 10);
    assert!(drows.len() <= 10, "respects budget: {}", drows.len());
    assert!(
        drows.iter().any(|r| r.starts_with("> m40")),
        "deep selection visible: {drows:?}"
    );
}

#[test]
fn scroll_window_keeps_selection_visible_and_pins_ends() {
    // Fits entirely: whole list, no clipping.
    assert_eq!(scroll_window(3, 1, 10), (0, 3));
    // Top of a long list: window starts at 0.
    assert_eq!(scroll_window(100, 0, 10), (0, 10));
    // Middle: selection centred, both edges clipped.
    let (s, e) = scroll_window(100, 50, 10);
    assert!(
        s <= 50 && 50 < e && e - s == 10,
        "selection in a full window: {s}..{e}"
    );
    // Bottom: window pins to the end rather than running past it.
    assert_eq!(scroll_window(100, 99, 10), (90, 100));
    // The selected index is always inside [start, end) for every position.
    for sel in 0..100 {
        let (s, e) = scroll_window(100, sel, 10);
        assert!(s <= sel && sel < e, "sel {sel} fell outside {s}..{e}");
        assert_eq!(e - s, 10, "window stays full at sel {sel}");
    }
}
