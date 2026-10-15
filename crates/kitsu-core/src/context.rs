//! Context-window accounting (design §3.1–3.2, Stage 1).
//!
//! Two concerns:
//! - [`ContextMeter`] estimates how many prompt tokens the running conversation
//!   costs, anchored on the last real API `usage` and estimating only what was
//!   appended since (never a full re-tokenize).
//! - [`ContextBudget`] turns a resolved model context window into the effective
//!   window and trim target, reserving headroom for the reply.
//!
//! Neither performs trimming — that is the cascade (Stage 2+). These are the
//! measurement primitives the trimming and `/context` command build on.

use std::cell::Cell;
use std::hash::{Hash, Hasher};

use kitsu_providers::{ChatMessage, Role};

/// Rough chars-per-token for prose/code.
///
/// Measured against Ollama's tokenizer (2026-08-17), this rate is:
/// - conservative for prose (4.50 real chars/token — we over-count, which is safe)
/// - optimistic for unified diffs (3.48 real chars/token — we under-count ~1.15x)
///
/// The optimistic direction only bites where code/diff text dominates a small
/// window; the system prompt and tool schemas that fill most of a small budget
/// are prose/JSON, where this rate still over-counts. Recorded here so anyone
/// resizing the trim buffer starts from data rather than a guess.
///
/// `pub(crate)` so callers that convert a remaining *token* budget back into a
/// *byte* budget for ASCII-ish content (e.g. the compaction summarizer sizing
/// its transcript input) use the same rate `estimate_str`/`message_tokens` do.
pub(crate) const CHARS_PER_TOKEN: usize = 4;
/// Tool-call JSON args tokenize denser than prose.
const CHARS_PER_TOKEN_JSON: usize = 3;
/// Per-message role/separator overhead real chat tokenizers add. Deliberately
/// at the low end of the true ~3–4 so the bias stays conservative (over-fill
/// is safer than overflow, design §3.2).
///
/// `pub(crate)` so callers that hand-construct a single-message request against
/// a byte budget (e.g. the compaction summarizer's bounded request) can reserve
/// headroom for this overhead up front, instead of discovering the request is
/// already over budget only after estimating it.
pub(crate) const PER_MESSAGE_TOKENS: u64 = 3;

/// Context window assumed when a gateway reports none. Deliberately small so an
/// unknown model is under-filled rather than overflowed (design §3.2).
pub const DEFAULT_CONTEXT_WINDOW: u64 = 8_192;

/// Always leave at least this many tokens for the model's reply.
pub const MIN_OUTPUT_RESERVE: u64 = 512;

/// Smallest window a shortfall clamp may set. Even with strong evidence that
/// the server truncates to `reported` tokens, never budget below this floor:
/// it must still fit a minimal system+user turn plus the reply reserve, and a
/// pathological `reported` value (e.g. a gateway that bills instead of
/// counting) must not strand the session at a zero-sized budget.
pub const MIN_VIABLE_WINDOW_FLOOR: u64 = 2_048;

/// Fraction of the pre-buffer budget assumed to be estimate error; the trim
/// buffer scales with this on large windows (floored at 2048, capped at
/// window/10) because absolute estimate error grows with fill.
pub(crate) const ERROR_FRACTION: f64 = 0.05;

/// Token estimate for a string with `ascii` ASCII chars and `non_ascii`
/// multibyte chars at the given ASCII chars-per-token rate. Non-ASCII text
/// (CJK etc.) tokenizes at roughly one token per char, so it is counted
/// directly — a flat bytes/rate or chars/rate computation would undercount it
/// by ~3x against real tokenizers.
fn estimate_mixed(ascii: usize, non_ascii: usize, rate: usize) -> u64 {
    (ascii.div_ceil(rate) + non_ascii) as u64
}

/// Split a string into (ASCII chars, non-ASCII chars).
fn ascii_split(s: &str) -> (usize, usize) {
    s.chars().fold(
        (0, 0),
        |(a, n), c| {
            if c.is_ascii() {
                (a + 1, n)
            } else {
                (a, n + 1)
            }
        },
    )
}

/// Estimate tokens of an arbitrary string (e.g. serialized tool schemas).
/// Two-rate heuristic: ASCII at the prose rate, non-ASCII at ~1 token/char,
/// so multibyte text is no longer undercounted (a raw byte count confuses
/// CJK's ~3 bytes/char with the 4-bytes/token rate).
pub fn estimate_str(s: &str) -> u64 {
    let (ascii, non_ascii) = ascii_split(s);
    estimate_mixed(ascii, non_ascii, CHARS_PER_TOKEN)
}

/// Rough token estimate of one message: text at the prose rate, tool-call args
/// at the denser JSON rate (both via the two-rate ASCII/non-ASCII heuristic),
/// plus a small per-message overhead for the role/separator tokens real chat
/// tokenizers add.
pub fn message_tokens(m: &ChatMessage) -> u64 {
    let (text_a, text_n) = ascii_split(&m.content);
    let (id_a, id_n) = m.tool_call_id.as_deref().map_or((0, 0), ascii_split);
    let (json_a, json_n) = m.tool_calls.iter().fold((0, 0), |(a, n), c| {
        let (a1, n1) = ascii_split(&c.tool_name);
        let (a2, n2) = ascii_split(&c.args);
        let (a3, n3) = ascii_split(&c.call_id);
        (a + a1 + a2 + a3, n + n1 + n2 + n3)
    });
    estimate_mixed(text_a + id_a, text_n + id_n, CHARS_PER_TOKEN)
        + estimate_mixed(json_a, json_n, CHARS_PER_TOKEN_JSON)
        + PER_MESSAGE_TOKENS
}

/// Per-role token breakdown for `/context` (design §3.4).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ContextBreakdown {
    pub system: u64,
    pub user: u64,
    pub assistant: u64,
    pub tool_results: u64,
}

impl ContextBreakdown {
    pub fn total(&self) -> u64 {
        self.system + self.user + self.assistant + self.tool_results
    }
}

/// Estimates conversation token usage, anchored on the last real API usage.
#[derive(Debug, Default, Clone)]
pub struct ContextMeter {
    /// `(prompt_tokens, number of leading messages that request covered, whether
    /// that request carried tool and/or response schema constraints, fingerprint
    /// of those leading messages, fingerprint of the serialized constraints)`.
    /// The constraint flag prevents a schema-free request from covering later
    /// schema overhead. The schema fingerprint detects a changed tool registry
    /// or response schema, while the message fingerprint detects in-place edits
    /// inside the anchored prefix that keep the message count unchanged.
    anchor: Option<(u64, usize, bool, u64, u64)>,
    /// Telemetry: how many times [`Self::estimate`] rejected a recorded anchor
    /// as implausible (outside the wide 0.4x–2.5x band around the anchor-free
    /// full estimate) and fell back to a full recompute. Advisory only; a
    /// nonzero count points at a buggy/proxying gateway's `usage` numbers.
    /// `Cell` because `estimate` takes `&self`.
    anchor_rejections: Cell<u64>,
}

/// Plausibility band for a recorded anchor versus the anchor-free full
/// estimate. Deliberately WIDE: real API tokenizers legitimately differ from
/// our chars/token heuristic by up to ~2x, so only absurd values (a gateway
/// reporting 0, or an aggregate many times the conversation) trip it.
/// Compared as exact integer ratios to avoid float rounding at the boundary.
const ANCHOR_BAND_LOW_NUM: u128 = 4; // anchor >= 0.4 * full
const ANCHOR_BAND_LOW_DEN: u128 = 10;
const ANCHOR_BAND_HIGH_NUM: u128 = 25; // anchor <= 2.5 * full
const ANCHOR_BAND_HIGH_DEN: u128 = 10;

/// True when `anchor` is within the generous [0.4x, 2.5x] band of the
/// anchor-free `full` estimate. Never panics; u128 math cannot overflow for
/// any realistic token count.
fn anchor_plausible(anchor: u64, full: u64) -> bool {
    let a = u128::from(anchor);
    let f = u128::from(full);
    a * ANCHOR_BAND_LOW_DEN >= f * ANCHOR_BAND_LOW_NUM
        && a * ANCHOR_BAND_HIGH_DEN <= f * ANCHOR_BAND_HIGH_NUM
}

/// Cheap content-identity fingerprint over a message prefix: role discriminant
/// plus LENGTHS of content, tool_call_id, and each tool call's fields — never
/// the string bytes themselves, so it stays O(n) and near-free even for huge
/// message bodies. A collision risk of 2^-64 is acceptable here: a false match
/// only keeps an already-approximate estimate, while a false miss falls back
/// to the safe full recompute.
fn prefix_fingerprint(messages: &[ChatMessage]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for m in messages {
        std::mem::discriminant(&m.role).hash(&mut h);
        m.content.len().hash(&mut h);
        m.tool_call_id.as_ref().map_or(0, String::len).hash(&mut h);
        m.tool_calls.len().hash(&mut h);
        for c in &m.tool_calls {
            c.tool_name.len().hash(&mut h);
            c.args.len().hash(&mut h);
            c.call_id.len().hash(&mut h);
        }
    }
    h.finish()
}

/// Cheap content-identity fingerprint of the serialized tool-schema JSON, for
/// the anchor's schema-coverage check. Unlike [`prefix_fingerprint`] this DOES
/// hash the string bytes: schema JSON is built once per turn and is small
/// (kilobytes), so a full `DefaultHasher` pass over it is still near-free,
/// while a length-only fingerprint would miss the common real-world change of
/// swapping one same-sized tool description for another. A 2^-64 collision
/// risk is acceptable for the same reason as the prefix fingerprint.
pub fn schema_fingerprint(schema_json: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    schema_json.hash(&mut h);
    h.finish()
}

impl ContextMeter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the authoritative `prompt_tokens` from a response, for a request
    /// that sent exactly `messages` and — per `had_schema` — did or didn't
    /// include tool or response schemas that round. `schema_json` is the
    /// serialized constraint set that request carried (pass `""` when
    /// `had_schema` is false). Only its fingerprint is stored, so later schema
    /// changes are detected and re-estimated. The message prefix is
    /// fingerprinted as well.
    pub fn record(
        &mut self,
        prompt_tokens: u64,
        messages: &[ChatMessage],
        had_schema: bool,
        schema_json: &str,
    ) {
        self.anchor = Some((
            prompt_tokens,
            messages.len(),
            had_schema,
            prefix_fingerprint(messages),
            schema_fingerprint(schema_json),
        ));
    }

    /// Clear the anchor. `sent_len` is a plain message COUNT, not an identity —
    /// once something restructures `messages` (drops or summarizes whole
    /// exchanges, `/clear`), a later, unrelated state of the conversation can
    /// spuriously satisfy `sent_len <= messages.len()` again while no longer
    /// describing an actual prefix of `messages`. Callers that reshape the
    /// message list (as opposed to appending to it) must call this so the next
    /// [`Self::estimate`] falls back to a full recompute instead of trusting a
    /// stale anchor against content it never measured.
    pub fn invalidate(&mut self) {
        self.anchor = None;
    }

    /// Estimated prompt tokens for the current `messages`: the last real-usage
    /// anchor plus a chars/token estimate of everything appended since — but only
    /// if the anchored leading messages are still intact (same count AND same
    /// content fingerprint). If the history was trimmed below the anchor, or
    /// edited in place inside the anchored prefix, fall back to a full estimate.
    ///
    /// Even a structurally valid anchor is sanity-checked against the
    /// anchor-free full estimate: outside a generous 0.4x–2.5x band the anchor
    /// is treated as a buggy/proxying gateway number and ignored FOR THIS CALL
    /// ONLY (the stored anchor is left untouched; each rejection increments
    /// [`Self::anchor_rejection_count`]). This is advisory — it can never error.
    pub fn estimate(&self, messages: &[ChatMessage]) -> u64 {
        let full: u64 = messages.iter().map(message_tokens).sum();
        match self.anchor {
            Some((tokens, sent_len, _, fingerprint, _))
                if sent_len <= messages.len()
                    && prefix_fingerprint(&messages[..sent_len]) == fingerprint =>
            {
                if anchor_plausible(tokens, full) {
                    tokens + messages[sent_len..].iter().map(message_tokens).sum::<u64>()
                } else {
                    self.anchor_rejections.set(self.anchor_rejections.get() + 1);
                    full
                }
            }
            _ => full,
        }
    }

    /// Number of times [`Self::estimate`] has rejected a recorded anchor as
    /// implausible (outside the 0.4x–2.5x band of the anchor-free estimate)
    /// and fallen back to a full recompute. Telemetry only.
    pub fn anchor_rejection_count(&self) -> u64 {
        self.anchor_rejections.get()
    }

    /// True when [`Self::estimate`] would use the real-usage anchor (rather than
    /// a full chars/token re-tokenize) for `messages`.
    pub fn anchored(&self, messages: &[ChatMessage]) -> bool {
        matches!(self.anchor, Some((_, sent_len, _, fingerprint, _))
            if sent_len <= messages.len()
                && prefix_fingerprint(&messages[..sent_len]) == fingerprint)
    }

    /// True when the CURRENT anchor is valid for `messages` *and* the request
    /// that produced it already included the same serialized tool/response
    /// constraints. A schema-free request or a request whose constraint set has
    /// changed does not cover the current schema overhead.
    pub fn anchor_includes_schema(&self, messages: &[ChatMessage], schema_json: &str) -> bool {
        matches!(self.anchor, Some((_, sent_len, had_schema, fingerprint, schema_fp))
            if had_schema
                && sent_len <= messages.len()
                && prefix_fingerprint(&messages[..sent_len]) == fingerprint
                && schema_fingerprint(schema_json) == schema_fp)
    }

    /// Per-role breakdown (pure estimate; ignores the usage anchor so callers can
    /// attribute tokens to categories). Its `total()` is the anchor-free estimate.
    pub fn breakdown(&self, messages: &[ChatMessage]) -> ContextBreakdown {
        let mut b = ContextBreakdown::default();
        for m in messages {
            let t = message_tokens(m);
            match m.role {
                Role::System => b.system += t,
                Role::User => b.user += t,
                Role::Assistant => b.assistant += t,
                Role::Tool => b.tool_results += t,
            }
        }
        b
    }
}

/// A resolved context window and the token budget derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    /// The model's full context window (tokens).
    pub window: u64,
    /// Window minus the reply reservation — what the prompt may fill.
    pub effective: u64,
    /// Trim target: stay at or below this, leaving a small buffer under effective.
    pub target: u64,
    /// The reply reservation actually used: `max(max_output, MIN_OUTPUT_RESERVE)`
    /// clamped to at most half the window. Callers must bound the provider's
    /// `max_tokens` by this so the promised `effective` holds end-to-end.
    pub reserve: u64,
}

impl ContextBudget {
    /// Derive the budget from a window and the request's max output tokens.
    /// The reply reservation is `max(max_output, MIN_OUTPUT_RESERVE)`, clamped to
    /// at most half the window so tiny windows still leave room for the prompt.
    /// The trim buffer is `min(window/10, max(2048, effective * ERROR_FRACTION))`:
    /// it scales with estimate error on large windows instead of going flat.
    pub fn new(window: u64, max_output: Option<u32>) -> Self {
        let reserve = max_output
            .map(u64::from)
            .unwrap_or(0)
            .max(MIN_OUTPUT_RESERVE)
            .min(window / 2);
        let effective = window.saturating_sub(reserve);
        // Base the error scaling on `effective` (window - reserve), which is
        // already known here; using `target` would be circular since
        // target = effective - buffer.
        let error_buffer = (effective as f64 * ERROR_FRACTION) as u64;
        let buffer = error_buffer.max(2_048).min(window / 10);
        let target = effective.saturating_sub(buffer);
        Self {
            window,
            effective,
            target,
            reserve,
        }
    }

    /// The clamped reserve as a `u32`, suitable for rewriting a request's
    /// `max_tokens` when the caller's value exceeds the reserve. Always
    /// returns the reserve actually used to derive `effective` — callers
    /// decide whether to apply it (clamp DOWN only; never inflate a smaller
    /// caller-supplied value, and never invent a cap for `None`).
    pub fn clamped_max_output(&self) -> u32 {
        self.reserve.min(u64::from(u32::MAX)) as u32
    }
}

/// Clamp a request's `max_tokens` down to the budget's reserve. Only ever
/// lowers a caller-supplied value; `None` stays `None` (no artificial cap is
/// invented for a caller that never asked for one).
pub fn clamp_max_tokens_to_reserve(max_tokens: Option<u32>, budget: &ContextBudget) -> Option<u32> {
    max_tokens.map(|t| t.min(budget.clamped_max_output()))
}

/// Compute the window a caller should feed to [`ContextBudget::new`] given
/// repeated evidence that the server truncates to a smaller real window.
///
/// Two-strike rule: a single shortfall (`shortfall_count < 2`) never clamps —
/// token estimates are heuristic, so one odd usage report is not actionable.
/// From the second shortfall for the SAME resolved window onward, the session
/// budget is clamped to `reported.max(MIN_VIABLE_WINDOW_FLOOR)`, provided
/// `reported` passes the same plausibility guard the orchestrator's
/// `context_window_shortfall` uses (`reported * 4 >= resolved_window`): when
/// the implied real window is under a quarter of the resolved one, the far
/// likelier story is the chars/4 estimator overshooting an accurate tokenizer,
/// not a clamped server.
///
/// Pure: takes its inputs by value and returns a new number. Nothing here
/// holds or rewrites the resolved window or config — the caller decides what
/// to pass to `ContextBudget::new`.
pub fn clamp_window_for_shortfall(
    resolved_window: u64,
    reported: u64,
    shortfall_count: u32,
) -> u64 {
    if shortfall_count < 2 {
        return resolved_window;
    }
    let plausible_clamp = reported.saturating_mul(4) >= resolved_window;
    if !plausible_clamp {
        return resolved_window;
    }
    reported.max(MIN_VIABLE_WINDOW_FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.to_string(),
            tool_call_id: None,
            tool_calls: vec![],
        }
    }

    /// Two distinct serialized tool-schema payloads for the P7 schema-
    /// fingerprint tests (different tool sets, as after a registry change).
    const SCHEMA_A: &str = r#"[{"name":"read_file","description":"read a file"}]"#;
    const SCHEMA_B: &str = r#"[{"name":"read_file","description":"read a file"},{"name":"write_file","description":"write a file"}]"#;

    #[test]
    fn estimate_str_is_chars_over_four_rounded_up() {
        assert_eq!(estimate_str(""), 0);
        assert_eq!(estimate_str("abcd"), 1);
        assert_eq!(estimate_str("abcde"), 2); // 5/4 -> 2
    }

    #[test]
    fn full_estimate_without_anchor() {
        let meter = ContextMeter::new();
        let msgs = vec![msg(Role::System, "abcd"), msg(Role::User, "abcd")]; // (1+3) + (1+3)
        assert_eq!(meter.estimate(&msgs), 8);
    }

    #[test]
    fn anchor_plus_tail_when_prefix_intact() {
        let mut meter = ContextMeter::new();
        let mut msgs = vec![msg(Role::System, "x".repeat(40).as_str())];
        // Pretend the last request sent these 1 message(s) and cost 20 tokens.
        // (Anchor updated from 1000 to 20 for P5: 1000 is outside the 0.4x–2.5x
        // plausibility band of the ~18-token full estimate and would now be
        // rejected; 20 is a plausible anchor and exercises the same path.)
        meter.record(20, &msgs, false, "");
        // Append an 8-char user turn: +2 text tokens + 3 per-message overhead.
        msgs.push(msg(Role::User, "abcdefgh"));
        assert_eq!(meter.estimate(&msgs), 25);
    }

    #[test]
    fn falls_back_to_full_estimate_when_trimmed_below_anchor() {
        let mut meter = ContextMeter::new();
        // Anchor claims 3 leading messages were sent...
        let sent = vec![
            msg(Role::System, "abcd"),
            msg(Role::User, "abcd"),
            msg(Role::User, "abcd"),
        ];
        meter.record(1000, &sent, false, "");
        // ...but history was trimmed to 1 message: anchor no longer describes a
        // prefix, so estimate ignores it and estimates the whole set.
        let msgs = vec![msg(Role::User, "abcd")];
        assert_eq!(meter.estimate(&msgs), 4); // 1 text + 3 per-message
    }

    #[test]
    fn breakdown_by_role_totals_match() {
        let meter = ContextMeter::new();
        let msgs = vec![
            msg(Role::System, "abcd"),       // 1 + 3
            msg(Role::User, "abcdefgh"),     // 2 + 3
            msg(Role::Assistant, "abcd"),    // 1 + 3
            msg(Role::Tool, "abcdefghijkl"), // 3 + 3
        ];
        let b = meter.breakdown(&msgs);
        assert_eq!(b.system, 4);
        assert_eq!(b.user, 5);
        assert_eq!(b.assistant, 4);
        assert_eq!(b.tool_results, 6);
        assert_eq!(b.total(), 19);
    }

    #[test]
    fn shortfall_single_strike_never_clamps() {
        // One shortfall is heuristic noise; the resolved window is unchanged.
        assert_eq!(clamp_window_for_shortfall(8_192, 4_096, 1), 8_192);
    }

    #[test]
    fn shortfall_two_strikes_clamp_to_reported_with_floor() {
        // Second shortfall for the same window: budget against the reported
        // real window (above the floor), strictly below the resolved window.
        let clamped = clamp_window_for_shortfall(8_192, 4_096, 2);
        assert_eq!(clamped, 4_096);
        assert!(clamped < 8_192);
        // A reported value below the floor clamps to the floor, not to zero.
        assert_eq!(
            clamp_window_for_shortfall(4_096, 1_024, 2),
            MIN_VIABLE_WINDOW_FLOOR
        );
    }

    #[test]
    fn shortfall_clamp_never_rewrites_resolved_window() {
        // The resolved window is an input, not a mutable slot: the function
        // returns the budget window while the original stays untouched.
        let resolved_window = 8_192;
        let clamped = clamp_window_for_shortfall(resolved_window, 4_096, 3);
        assert_eq!(resolved_window, 8_192);
        assert_ne!(clamped, resolved_window);
    }

    #[test]
    fn shortfall_implausible_report_never_clamps() {
        // reported*4 < resolved_window: the estimator overshooting an accurate
        // tokenizer is far likelier than a real clamp — stay at the resolved
        // window even with many strikes.
        assert_eq!(clamp_window_for_shortfall(32_768, 4_096, 2), 32_768);
        assert_eq!(clamp_window_for_shortfall(32_768, 4_096, 9), 32_768);
    }

    #[test]
    fn budget_reserves_output_and_buffer() {
        // 32k window, reserve 4k reply -> effective 28k; buffer min(3276,2048)=2048.
        let b = ContextBudget::new(32_768, Some(4_096));
        assert_eq!(b.window, 32_768);
        assert_eq!(b.effective, 32_768 - 4_096);
        assert_eq!(b.target, 32_768 - 4_096 - 2_048);
    }

    #[test]
    fn budget_uses_min_reserve_when_output_unset() {
        let b = ContextBudget::new(8_192, None);
        assert_eq!(b.effective, 8_192 - MIN_OUTPUT_RESERVE);
    }

    #[test]
    fn budget_clamps_reserve_to_half_window_on_tiny_windows() {
        // Asking to reserve 20k of a 8k window would zero the prompt; clamp to
        // half the window.
        let b = ContextBudget::new(8_192, Some(20_000));
        assert_eq!(b.effective, 8_192 - 8_192 / 2);
        assert!(b.target < b.effective);
    }

    #[test]
    fn budget_buffer_stays_at_floor_on_small_windows() {
        // No-regression: on a 32k window effective*0.05 = 1433 < 2048, so the
        // buffer still hits the 2048 floor exactly as before P8.
        let b = ContextBudget::new(32_768, Some(4_096));
        assert_eq!(b.target, 32_768 - 4_096 - 2_048);
    }

    #[test]
    fn budget_buffer_scales_with_error_on_large_windows() {
        // window=1M, max_output=None -> reserve=512 -> effective=999_488.
        // error buffer = floor(999_488 * 0.05) = 49_974, which is above the
        // 2048 floor and below the window/10 = 100_000 cap (cap does NOT bind
        // here; the scaled value is asserted directly).
        let b = ContextBudget::new(1_000_000, None);
        let effective = 1_000_000 - MIN_OUTPUT_RESERVE;
        let expected_buffer = (effective as f64 * ERROR_FRACTION) as u64; // 49_974
        assert_eq!(expected_buffer, 49_974);
        assert!(expected_buffer > 2_048);
        assert!(expected_buffer < 1_000_000 / 10);
        assert_eq!(b.effective, effective);
        assert_eq!(b.target, effective - expected_buffer);
    }

    #[test]
    fn budget_invariant_holds_on_large_windows() {
        // target <= effective <= window must hold at window=1_000_000.
        let b = ContextBudget::new(1_000_000, None);
        assert!(b.target <= b.effective && b.effective <= b.window);
    }

    #[test]
    fn clamped_max_output_exposes_reserve_used() {
        // P9 (a): window=8192, max_output=Some(8192) -> reserve clamps to
        // window/2 = 4096, and clamping the request rewrites 8192 -> 4096.
        let b = ContextBudget::new(8_192, Some(8_192));
        assert_eq!(b.reserve, 4_096);
        assert_eq!(b.clamped_max_output(), 4_096);
        assert_eq!(clamp_max_tokens_to_reserve(Some(8_192), &b), Some(4_096));
        // The budget's promise (effective) now holds end-to-end against the
        // value actually sent to the provider.
        assert_eq!(b.effective, 8_192 - 4_096);
    }

    #[test]
    fn clamp_max_tokens_leaves_fitting_value_unchanged() {
        // P9 (b): max_output already under the reserve is NOT clamped down.
        let b = ContextBudget::new(131_072, Some(1_000));
        assert_eq!(b.reserve, 1_000);
        assert_eq!(clamp_max_tokens_to_reserve(Some(1_000), &b), Some(1_000));
        // A smaller caller value is never inflated to the reserve.
        let b2 = ContextBudget::new(131_072, Some(4_096));
        assert_eq!(clamp_max_tokens_to_reserve(Some(1_000), &b2), Some(1_000));
    }

    #[test]
    fn clamp_max_tokens_none_stays_none() {
        // P9 (c): max_output=None -> reserve is the floor, but no artificial
        // cap is invented for the request.
        let b = ContextBudget::new(8_192, None);
        assert_eq!(b.reserve, MIN_OUTPUT_RESERVE);
        assert_eq!(clamp_max_tokens_to_reserve(None, &b), None);
    }

    #[test]
    fn tool_call_args_estimated_at_denser_json_rate() {
        // content empty; json = tool_name(2)+args(6)+call_id(2) = 10 -> ceil(10/3)=4.
        let m = ChatMessage {
            role: Role::Assistant,
            content: String::new(),
            tool_call_id: None,
            tool_calls: vec![kitsu_providers::ToolCall {
                call_id: "cd".to_string(),
                tool_name: "ab".to_string(),
                args: "abcdef".to_string(),
            }],
        };
        assert_eq!(message_tokens(&m), 7); // 4 + PER_MESSAGE_TOKENS
                                           // The same bytes as plain content would be ceil(10/4)=3 — JSON is denser.
        let as_text = msg(Role::Assistant, "abababcdef"); // 10 chars
        assert_eq!(message_tokens(&as_text), 6); // 3 + PER_MESSAGE_TOKENS
    }

    #[test]
    fn record_overwrites_the_previous_anchor() {
        let mut meter = ContextMeter::new();
        // (Anchors updated from 100/200 to 6/12 for P5: the full estimate of
        // these two messages is 8, so 100/200 are outside the 0.4x–2.5x
        // plausibility band and would now be rejected; 6 and 12 are inside it.)
        meter.record(6, &[msg(Role::System, "abcd")], false, "");
        let msgs = vec![msg(Role::System, "abcd"), msg(Role::User, "abcd")];
        meter.record(12, &msgs, false, "");
        // Two messages, anchor covers both, no tail -> exactly the latest anchor.
        assert_eq!(meter.estimate(&msgs), 12);
    }

    #[test]
    fn anchor_only_when_no_new_messages() {
        let mut meter = ContextMeter::new();
        let msgs = vec![msg(Role::System, "abcd"), msg(Role::User, "abcd")];
        // (Anchor updated from 500 to 12 for P5: 500 is outside the
        // 0.4x–2.5x band of the 8-token full estimate and would be rejected.)
        meter.record(12, &msgs, false, "");
        assert_eq!(meter.estimate(&msgs), 12);
    }

    /// Regression: a structural reshape (drop/summarize) can shrink the
    /// conversation and then have it regrow past the OLD anchor's `sent_len` —
    /// at which point the length-only validity check would wrongly treat the
    /// stale anchor as still describing a prefix of the new messages.
    /// `invalidate()` must be called by anything that reshapes `messages`, and
    /// afterward `estimate` must fall back to a full recompute even once the
    /// conversation regrows past the old `sent_len`.
    #[test]
    fn invalidate_forces_full_recompute_even_after_regrowth_past_old_sent_len() {
        let mut meter = ContextMeter::new();
        // Anchor claims a real API response covered 5 leading messages at a cost
        // of 1000 tokens.
        let sent = vec![
            msg(Role::System, "abcd"),
            msg(Role::User, "abcd"),
            msg(Role::User, "abcd"),
            msg(Role::User, "abcd"),
            msg(Role::User, "abcd"),
        ];
        meter.record(1000, &sent, false, "");
        meter.invalidate();

        // The conversation was reshaped (e.g. dropped/summarized) and then grew
        // back to 5+ messages again — same length as the stale anchor claimed,
        // which is exactly the scenario where a length-only check would wrongly
        // trust it.
        let msgs = vec![
            msg(Role::System, "abcd"),
            msg(Role::User, "abcd"),
            msg(Role::User, "abcd"),
            msg(Role::User, "abcd"),
            msg(Role::User, "abcd"),
        ];
        assert!(
            !meter.anchored(&msgs),
            "an invalidated anchor must never report itself as anchored"
        );
        // Full recompute: 5 messages * (1 text + 3 per-message) = 20.
        assert_eq!(
            meter.estimate(&msgs),
            20,
            "must fall back to a full recompute, not the stale 1000-token anchor"
        );
    }

    /// Regression: a turn's final round withholds tools (forcing an answer), so
    /// an anchor recorded from THAT round's real usage does NOT cover tool-schema
    /// tokens. `anchored()` alone can't distinguish this from a normal, schema-
    /// inclusive anchor — `anchor_includes_schema()` must say `false` here so the
    /// caller still adds the schema estimate on the next round that offers tools.
    #[test]
    fn anchor_includes_schema_is_false_when_recorded_without_tools() {
        let mut meter = ContextMeter::new();
        let msgs = vec![msg(Role::System, "abcd"), msg(Role::User, "abcd")];
        meter.record(500, &msgs, false, ""); // that request had NO tools field
        assert!(
            meter.anchored(&msgs),
            "sanity: anchor is structurally valid"
        );
        assert!(
            !meter.anchor_includes_schema(&msgs, ""),
            "a schema-less anchor must not be reported as already covering schema tokens"
        );
    }

    #[test]
    fn anchor_includes_schema_is_true_when_recorded_with_tools() {
        let mut meter = ContextMeter::new();
        let msgs = vec![msg(Role::System, "abcd"), msg(Role::User, "abcd")];
        meter.record(500, &msgs, true, SCHEMA_A); // that request DID include tools
        assert!(meter.anchor_includes_schema(&msgs, SCHEMA_A));
    }

    /// P7(a): fast path preserved — recording with a schema and querying with
    /// the SAME schema content must still report the anchor as covering the
    /// schema tokens (no re-estimate on the normal, registry-unchanged flow).
    #[test]
    fn anchor_includes_schema_is_true_when_schema_unchanged() {
        let mut meter = ContextMeter::new();
        let msgs = vec![msg(Role::System, "abcd"), msg(Role::User, "abcd")];
        meter.record(500, &msgs, true, SCHEMA_A);
        assert!(
            meter.anchor_includes_schema(&msgs, SCHEMA_A),
            "an unchanged schema must keep the schema-covered fast path"
        );
    }

    /// P7(b): the tool registry changed mid-session (tools toggled, subagent
    /// profile switch) — the anchored request's schema differs from the
    /// CURRENT schema_json, so the anchor must NOT be treated as covering the
    /// new schema's tokens (they must be re-estimated).
    #[test]
    fn anchor_includes_schema_is_false_when_schema_changed() {
        let mut meter = ContextMeter::new();
        let msgs = vec![msg(Role::System, "abcd"), msg(Role::User, "abcd")];
        meter.record(500, &msgs, true, SCHEMA_A);
        assert!(
            meter.anchored(&msgs),
            "sanity: the message anchor itself is still valid"
        );
        assert!(
            !meter.anchor_includes_schema(&msgs, SCHEMA_B),
            "a changed tool registry must force schema re-estimation"
        );
    }

    /// P7(c): a schema-less anchor stays schema-less regardless of the schema
    /// fingerprint — the new field must not change the existing
    /// had_schema=false behavior.
    #[test]
    fn anchor_includes_schema_is_false_without_schema_regardless_of_fingerprint() {
        let mut meter = ContextMeter::new();
        let msgs = vec![msg(Role::System, "abcd"), msg(Role::User, "abcd")];
        meter.record(500, &msgs, false, "");
        assert!(!meter.anchor_includes_schema(&msgs, ""));
        assert!(!meter.anchor_includes_schema(&msgs, SCHEMA_A));
        assert!(!meter.anchor_includes_schema(&msgs, SCHEMA_B));
    }

    /// (a) Guards the anchor fast path itself: an UNCHANGED prefix (same
    /// messages, possibly with an appended tail) must still use the recorded
    /// anchor, so the fingerprint check doesn't regress the normal append-only
    /// flow into a full re-tokenize on every estimate.
    #[test]
    fn unchanged_prefix_still_uses_the_anchor() {
        let mut meter = ContextMeter::new();
        let mut msgs = vec![
            msg(Role::System, "x".repeat(40).as_str()),
            msg(Role::User, "abcd"),
        ];
        // (Anchor updated from 1000 to 30 for P5: 1000 is outside the
        // 0.4x–2.5x band of the ~22-token full estimate and would be rejected.)
        meter.record(30, &msgs, false, "");
        msgs.push(msg(Role::User, "abcdefgh")); // identical prefix + 8-char tail (+2+3)
        assert!(meter.anchored(&msgs));
        assert_eq!(meter.estimate(&msgs), 35);
    }

    /// (b) an important regression: an in-place content edit
    /// INSIDE the anchored prefix that preserves the message COUNT (rewritten
    /// tool result, edited queued message) must invalidate the anchor. On the
    /// old length-only code this silently returned `anchor + tail` for content
    /// the anchor never measured (this test fails there: estimate == 1000 and
    /// anchored() == true); with the fingerprint it must fall back to a full
    /// recompute, and `anchored()` must agree.
    #[test]
    fn in_place_edit_inside_anchored_prefix_invalidates_the_anchor() {
        let mut meter = ContextMeter::new();
        let sent = vec![
            msg(Role::System, "abcd"),
            msg(Role::User, "short"),
            msg(Role::User, "abcd"),
        ];
        meter.record(1000, &sent, false, "");

        // SAME count (3), but message [1] was rewritten in place.
        let edited = vec![
            msg(Role::System, "abcd"),
            msg(
                Role::User,
                "a much longer rewritten body the anchor never saw",
            ),
            msg(Role::User, "abcd"),
        ];
        assert_eq!(sent.len(), edited.len(), "the edit preserves the count");
        assert!(
            !meter.anchored(&edited),
            "an in-place edit inside the anchored prefix must invalidate the anchor"
        );
        let full: u64 = edited.iter().map(message_tokens).sum();
        assert_eq!(meter.estimate(&edited), full, "must be a full recompute");
        assert_ne!(
            meter.estimate(&edited),
            1000,
            "must not trust the stale anchor"
        );
    }

    /// (c) Append-only growth must keep using the anchor: recording over N
    /// messages and then appending NEW messages to the end (prefix untouched)
    /// is the normal per-round flow and must stay on the cheap
    /// `anchor + tail_estimate` path — no regression.
    #[test]
    fn appending_to_the_tail_still_uses_the_anchor() {
        let mut meter = ContextMeter::new();
        let mut msgs = vec![
            msg(Role::System, "x".repeat(40).as_str()),
            msg(Role::User, "abcd"),
        ];
        // (Anchor updated from 800 to 40 for P5: 800 is outside the 0.4x–2.5x
        // band of the ~31-token full estimate and would now be rejected.)
        meter.record(40, &msgs, true, SCHEMA_A);
        msgs.push(msg(Role::Assistant, "abcdefgh")); // +2+3
        msgs.push(msg(Role::Tool, "abcd")); // +1+3
        assert!(meter.anchored(&msgs));
        assert!(meter.anchor_includes_schema(&msgs, SCHEMA_A));
        assert_eq!(meter.estimate(&msgs), 49);
    }

    /// P5(a): a PLAUSIBLE anchor — within the 0.4x–2.5x band of the anchor-free
    /// full estimate — is still trusted exactly as before (anchor + tail).
    #[test]
    fn plausible_anchor_is_still_trusted() {
        let mut meter = ContextMeter::new();
        let mut msgs = vec![
            msg(Role::System, "x".repeat(40).as_str()), // 10 + 3
            msg(Role::User, "abcd"),                    // 1 + 3
        ];
        // Full estimate of the prefix is 17; 30 is well inside [6.8, 42.5].
        meter.record(30, &msgs, false, "");
        msgs.push(msg(Role::User, "abcdefgh")); // +2+3
        assert_eq!(meter.estimate(&msgs), 35, "plausible anchor must be used");
        assert_eq!(
            meter.anchor_rejection_count(),
            0,
            "a plausible anchor must never trip the clamp"
        );
    }

    /// P5(b): an IMPLAUSIBLE anchor (gateway reports 0, or 100x the real
    /// content) is rejected per-call: estimate() returns the anchor-free full
    /// estimate, and the stored anchor is NOT mutated (a later record can
    /// still replace it, and the rejection is counted for telemetry).
    #[test]
    fn implausible_anchor_is_rejected_and_counted() {
        let msgs = vec![
            msg(Role::System, "x".repeat(40).as_str()),
            msg(Role::User, "abcd"),
        ];
        let full: u64 = msgs.iter().map(message_tokens).sum();
        assert_eq!(full, 17);

        // Case 1: gateway reports 0 prompt tokens for real content.
        let mut meter = ContextMeter::new();
        meter.record(0, &msgs, false, "");
        assert_eq!(
            meter.estimate(&msgs),
            full,
            "a zero anchor must fall back to the full estimate"
        );
        assert_eq!(meter.anchor_rejection_count(), 1);
        // Advisory only: the stored anchor is untouched and still structurally
        // valid (anchored() reports prefix identity, not plausibility).
        assert!(meter.anchored(&msgs));
        // Each rejecting estimate call counts.
        let _ = meter.estimate(&msgs);
        assert_eq!(meter.anchor_rejection_count(), 2);

        // Case 2: gateway reports ~100x the real content (aggregate usage).
        let mut meter = ContextMeter::new();
        meter.record(100 * full, &msgs, false, "");
        assert_eq!(
            meter.estimate(&msgs),
            full,
            "a 100x anchor must fall back to the full estimate"
        );
        assert_eq!(meter.anchor_rejection_count(), 1);
    }

    /// P5(c): band boundary behavior — an anchor exactly at 0.4x or 2.5x of
    /// the full estimate is trusted (inclusive band); just outside either edge
    /// is rejected. Uses a full estimate of 40 so the boundaries are exact
    /// integers: 0.4x = 16, 2.5x = 100.
    #[test]
    fn anchor_band_boundaries_are_inclusive() {
        let msgs = vec![
            msg(Role::System, "x".repeat(148).as_str()), // 37 + 3 = 40
        ];
        let full: u64 = msgs.iter().map(message_tokens).sum();
        assert_eq!(full, 40);

        for anchor in [16_u64, 17, 100, 99] {
            let mut meter = ContextMeter::new();
            meter.record(anchor, &msgs, false, "");
            assert_eq!(
                meter.estimate(&msgs),
                anchor,
                "anchor {anchor} is inside/at the band edge and must be trusted"
            );
            assert_eq!(meter.anchor_rejection_count(), 0);
        }
        for anchor in [15_u64, 101] {
            let mut meter = ContextMeter::new();
            meter.record(anchor, &msgs, false, "");
            assert_eq!(
                meter.estimate(&msgs),
                full,
                "anchor {anchor} is just outside the band and must be rejected"
            );
            assert_eq!(meter.anchor_rejection_count(), 1);
        }
    }

    #[test]
    fn budget_saturates_on_a_tiny_window() {
        // window 100, no max_output -> reserve min(512,50)=50 -> effective 50;
        // buffer min(10,2048)=10 -> target 40.
        let b = ContextBudget::new(100, None);
        assert_eq!(b.effective, 50);
        assert_eq!(b.target, 40);
        assert!(b.target <= b.effective && b.effective <= b.window);
    }

    /// P4(a): a history of N short messages must include N * PER_MESSAGE_TOKENS
    /// of role/separator overhead on top of the old byte-only, no-overhead
    /// computation.
    #[test]
    fn short_message_history_includes_per_message_overhead() {
        const N: usize = 10;
        let msgs: Vec<ChatMessage> = (0..N).map(|_| msg(Role::User, "abcd")).collect();
        // Old computation: bytes-only, no overhead -> N * ceil(4/4) = 10.
        let old: u64 = msgs
            .iter()
            .map(|m| (m.content.len().div_ceil(CHARS_PER_TOKEN)) as u64)
            .sum();
        let new: u64 = msgs.iter().map(message_tokens).sum();
        assert_eq!(old, N as u64);
        assert_eq!(
            new,
            old + N as u64 * PER_MESSAGE_TOKENS,
            "estimate must add N * PER_MESSAGE_TOKENS over the old no-overhead math"
        );
    }

    /// P4(b): CJK text is ~3 bytes per char and tokenizes at roughly one token
    /// per char, so the old byte-based `s.len()/CHARS_PER_TOKEN` undercounted
    /// it by ~3x. The new estimate must be HIGHER and close to the true count.
    #[test]
    fn multibyte_text_is_not_undercounted() {
        let s = "你好世界，这是测试文本。"; // 12 chars, 36 bytes, ~12 real tokens
        assert_eq!(s.chars().count(), 12);
        assert_eq!(s.len(), 36);
        let old = (s.len().div_ceil(CHARS_PER_TOKEN)) as u64; // 9 -> underestimate
        let new = estimate_str(s); // 12 (one token per non-ASCII char)
        assert_eq!(new, 12);
        assert!(
            new > old,
            "new estimate ({new}) must exceed the old byte-based one ({old})"
        );
    }

    /// P4(c): plain ASCII is 1 byte per char, so the estimate must be
    /// unchanged versus the old byte-based computation (no regression).
    #[test]
    fn ascii_estimate_unchanged() {
        let s = "The quick brown fox jumps over the lazy dog.";
        let old = (s.len().div_ceil(CHARS_PER_TOKEN)) as u64;
        assert_eq!(estimate_str(s), old);
        // Per-message overhead aside, the text portion is identical too.
        let m = msg(Role::User, s);
        assert_eq!(
            message_tokens(&m),
            old + PER_MESSAGE_TOKENS,
            "ASCII text estimate must match the old rate plus only the new overhead"
        );
    }
}
