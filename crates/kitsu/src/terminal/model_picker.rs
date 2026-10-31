//! Terminal model selection.

use crate::models::filter_model_catalog;
use crate::terminal::completion::current_terminal_columns;
#[cfg(unix)]
use crate::terminal::overlay_input::decode_overlay_keys;
#[cfg(unix)]
use crate::terminal::overlay_input::stdin_waiting;
#[cfg(unix)]
use crate::terminal::overlay_input::OverlayKey;
#[cfg(unix)]
use crate::terminal::overlay_input::OverlayPending;
#[cfg(unix)]
use crate::terminal::overlay_input::ESC_SEQUENCE_GRACE;
#[cfg(unix)]
use crate::terminal::typeahead::RawStdinGuard;
#[cfg(unix)]
use crate::tui_bridge;

/// What one key did to the model popup (pure, so filter/selection/pick logic
/// is tested without a terminal). The popup over the composer: type to filter,
/// arrows to move, Tab to complete the selection into the filter, Enter to
/// pick, Esc/Ctrl-C to cancel.
#[cfg(unix)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ModelPopupStep {
    /// Filter or selection changed (or an inert key); keep the loop running.
    Continue,
    /// `Enter`: commit this model id.
    Pick(String),
    /// `Esc`/`Ctrl-C`, or a drained descriptor: close without switching.
    Cancel,
}

/// The popup's mutable state: the live filter text and the selected row within
/// the *filtered* list. Selection is an index into the filtered view, clamped
/// on every filter change so it never points past the end.
#[cfg(unix)]
#[derive(Clone, Debug, Default)]
pub(crate) struct ModelPopup {
    pub(crate) filter: String,
    pub(crate) selected: usize,
}

#[cfg(unix)]
impl ModelPopup {
    /// The models matching the current filter, in catalog order. One source
    /// (`filter_model_catalog`) so the popup narrows exactly as `/models <x>`
    /// does — no second filter to drift.
    pub(crate) fn filtered(&self, models: &[String]) -> Vec<String> {
        filter_model_catalog(models.to_vec(), &self.filter)
    }

    /// Clamp the selection to the current filtered length (empty list → 0).
    pub(crate) fn clamp(&mut self, filtered_len: usize) {
        self.selected = self.selected.min(filtered_len.saturating_sub(1));
    }
}

/// Apply one decoded key to the popup. Printable chars extend the filter;
/// Backspace trims it; arrows move; Tab completes the selected id into the
/// filter (so the next keystroke refines from there); Enter picks; Esc/Ctrl-C
/// cancel. A paste arrives as its characters and therefore only ever edits the
/// filter — it can never act as a command, which is the copy-paste-safety rule.
#[cfg(unix)]
pub(crate) fn model_popup_step(
    key: OverlayKey,
    popup: &mut ModelPopup,
    models: &[String],
) -> ModelPopupStep {
    let filtered = popup.filtered(models);
    match key {
        OverlayKey::Up => {
            popup.selected = popup.selected.saturating_sub(1);
            ModelPopupStep::Continue
        }
        OverlayKey::Down => {
            if !filtered.is_empty() {
                popup.selected = (popup.selected + 1).min(filtered.len() - 1);
            }
            ModelPopupStep::Continue
        }
        OverlayKey::Enter | OverlayKey::Right => match filtered.get(popup.selected) {
            Some(model) => ModelPopupStep::Pick(model.clone()),
            None => ModelPopupStep::Continue,
        },
        OverlayKey::StepBack => ModelPopupStep::Cancel,
        OverlayKey::Tab => {
            // Tab completes the highlighted id into the filter, so typing
            // continues from a concrete model rather than a fragment.
            if let Some(model) = filtered.get(popup.selected) {
                popup.filter = model.clone();
                popup.selected = 0;
            }
            ModelPopupStep::Continue
        }
        OverlayKey::Backspace => {
            popup.filter.pop();
            popup.clamp(popup.filtered(models).len());
            ModelPopupStep::Continue
        }
        // Ctrl-C: raw mode clears ISIG, so it arrives as a byte (review F4).
        OverlayKey::Interrupt => ModelPopupStep::Cancel,
        OverlayKey::Char(c) if !c.is_control() => {
            popup.filter.push(c);
            popup.selected = 0;
            ModelPopupStep::Continue
        }
        // Other control chars are inert.
        OverlayKey::Char(_) => ModelPopupStep::Continue,
    }
}

/// Render the popup rows: a framed title with the live filter, the scrolled
/// slice of matching models (selected marked `>`, current tagged), and a
/// footer cue. Pure projection so the driver just paints what this returns.
/// `max_rows` is the pane budget (terminal height less the composer row).
#[cfg(unix)]
pub(crate) fn model_popup_view(
    models: &[String],
    current: &str,
    popup: &ModelPopup,
    cols: usize,
    max_rows: usize,
) -> Vec<String> {
    let filtered = popup.filtered(models);
    let header = if popup.filter.is_empty() {
        "select a model — type to filter · ↑/↓ move · Tab complete · Enter select · Esc cancel"
            .to_string()
    } else {
        format!("select a model · filter: {}", popup.filter)
    };
    // Title + blank + footer + ≤2 "… more" markers are the fixed chrome.
    let viewport = max_rows.saturating_sub(5).max(1);
    let selected = popup.selected.min(filtered.len().saturating_sub(1));
    let (start, end) = scroll_window(filtered.len(), selected, viewport);

    let mut rows = Vec::with_capacity(max_rows);
    rows.push(fit_overlay_row(&header, cols));
    rows.push(String::new());
    if filtered.is_empty() {
        rows.push(fit_overlay_row("  (no model matches this filter)", cols));
    } else {
        if start > 0 {
            rows.push(fit_overlay_row(&format!("  ↑ {start} more"), cols));
        }
        for (index, model) in filtered.iter().enumerate().take(end).skip(start) {
            let marker = if index == selected { ">" } else { " " };
            let tag = if *model == current { "  (current)" } else { "" };
            rows.push(fit_overlay_row(&format!("{marker} {model}{tag}"), cols));
        }
        if end < filtered.len() {
            rows.push(fit_overlay_row(
                &format!("  ↓ {} more", filtered.len() - end),
                cols,
            ));
        }
    }
    rows
}

/// The `[start, end)` slice of a `len`-item list to show in a `viewport`-row
/// window that keeps `selected` visible: centred on the selection, clamped to
/// both ends. Pure arithmetic, unit-tested. The caller shows `start > 0` /
/// `end < len` as "… more" markers.
#[cfg(unix)]
pub(crate) fn scroll_window(len: usize, selected: usize, viewport: usize) -> (usize, usize) {
    if len <= viewport {
        return (0, len);
    }
    let start = selected.saturating_sub(viewport / 2);
    let end = (start + viewport).min(len);
    (end.saturating_sub(viewport), end)
}

/// Truncate one overlay row to the terminal width so a long model id never
/// wraps and costs a frame row (the agent view's `fit_all` rule).
#[cfg(unix)]
pub(crate) fn fit_overlay_row(row: &str, cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    let chars: Vec<char> = row.chars().collect();
    if chars.len() <= cols {
        row.to_string()
    } else if cols == 1 {
        "…".to_string()
    } else {
        let mut out: String = chars[..cols - 1].iter().collect();
        out.push('…');
        out
    }
}

/// A model picker rendered as a POPUP over the composer (the pinned modal pane),
/// not a fullscreen takeover. Type to filter, ↑/↓ to move, Tab to complete,
/// Enter to pick, Esc/Ctrl-C to cancel. Returns the chosen id, or `None` on
/// cancel / unavailable surface.
///
/// Reuses the pane machinery (`paint_modal_rows` grows the viewport over the
/// composer; `clear_approval_modal` restores it) and the overlay byte decoder
/// (`decode_overlay_keys` + the 50 ms lone-`ESC` grace). `MODAL_PANE` is live
/// for the whole `BridgeGuard` lifetime, so this works from `handle_slash`
/// post-submit; we engage our own `RawStdinGuard` because the reader's is gone
/// by then. Declines (None) when no pane is published (classic/non-tty), so the
/// caller falls back to the text list.
#[cfg(unix)]
pub(crate) fn run_model_picker(
    models: &[String],
    current: &str,
    initial_filter: &str,
) -> Option<String> {
    use std::io::Read as _;

    if tui_bridge::live_mode() != tui_bridge::TuiMode::Inline {
        return None;
    }
    if !kitsu_render::stdin_is_tty() || !kitsu_render::stderr_styled() || models.is_empty() {
        return None;
    }
    let budget = tui_bridge::modal_pane_budget();
    if budget == 0 {
        return None; // No pinned pane to paint the popup into.
    }
    let _raw = RawStdinGuard::engage()?;

    // Pre-fill the filter (from `/models <filter>`), else start selected on the
    // current model if present.
    let mut popup = ModelPopup {
        filter: initial_filter.to_string(),
        selected: if initial_filter.is_empty() {
            models.iter().position(|m| m == current).unwrap_or(0)
        } else {
            0
        },
    };
    let cols = current_terminal_columns().unwrap_or(80);
    let mut stdin = std::io::stdin();
    let mut buffer = [0u8; 256];
    let mut pending = OverlayPending::None;
    let mut held: Vec<u8> = Vec::new();
    let outcome = loop {
        let budget = tui_bridge::modal_pane_budget().max(3) as usize;
        let rows = model_popup_view(models, current, &popup, cols, budget);
        tui_bridge::paint_modal_rows(&rows);

        if held == [0x1b] && !stdin_waiting(ESC_SEQUENCE_GRACE) {
            break None;
        }
        let count = match stdin.read(&mut buffer) {
            Ok(0) => break None,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break None,
        };
        held.extend_from_slice(&buffer[..count]);
        if held == [0x1b] {
            continue;
        }
        let chunk = std::mem::take(&mut held);
        let mut done: Option<Option<String>> = None;
        for key in decode_overlay_keys(&chunk, &mut pending) {
            match model_popup_step(key, &mut popup, models) {
                ModelPopupStep::Continue => {}
                ModelPopupStep::Pick(model) => {
                    done = Some(Some(model));
                    break;
                }
                ModelPopupStep::Cancel => {
                    done = Some(None);
                    break;
                }
            }
        }
        if let Some(result) = done {
            break result;
        }
    };

    // Take the popup back down and restore the composer's status row.
    tui_bridge::clear_approval_modal();
    outcome
}
