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

use kitsu_providers::{ChatMessage, Role};

/// Rough chars-per-token for prose/code.
const CHARS_PER_TOKEN: usize = 4;
/// Tool-call JSON args tokenize denser than prose.
const CHARS_PER_TOKEN_JSON: usize = 3;

/// Context window assumed when a gateway reports none. Deliberately small so an
/// unknown model is under-filled rather than overflowed (design §3.2).
pub const DEFAULT_CONTEXT_WINDOW: u64 = 8_192;

/// Always leave at least this many tokens for the model's reply.
pub const MIN_OUTPUT_RESERVE: u64 = 512;

/// Estimate tokens of an arbitrary string (e.g. serialized tool schemas) at the
/// prose rate.
pub fn estimate_str(s: &str) -> u64 {
    s.len().div_ceil(CHARS_PER_TOKEN) as u64
}

/// Rough token estimate of one message: text at the prose rate, tool-call args
/// at the denser JSON rate.
fn estimate_message(m: &ChatMessage) -> u64 {
    let text = m.content.len() + m.tool_call_id.as_ref().map_or(0, String::len);
    let json: usize = m
        .tool_calls
        .iter()
        .map(|c| c.tool_name.len() + c.args.len() + c.call_id.len())
        .sum();
    (text.div_ceil(CHARS_PER_TOKEN) + json.div_ceil(CHARS_PER_TOKEN_JSON)) as u64
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
    /// `(prompt_tokens, number of leading messages that request covered)`.
    anchor: Option<(u64, usize)>,
}

impl ContextMeter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the authoritative `prompt_tokens` from a response, for a request
    /// that sent `sent_len` leading messages.
    pub fn record(&mut self, prompt_tokens: u64, sent_len: usize) {
        self.anchor = Some((prompt_tokens, sent_len));
    }

    /// Estimated prompt tokens for the current `messages`: the last real-usage
    /// anchor plus a chars/token estimate of everything appended since — but only
    /// if the anchored leading messages are still intact. If the history was
    /// trimmed below the anchor (so the anchor no longer describes a prefix of
    /// `messages`), fall back to a full estimate.
    pub fn estimate(&self, messages: &[ChatMessage]) -> u64 {
        match self.anchor {
            Some((tokens, sent_len)) if sent_len <= messages.len() => {
                tokens
                    + messages[sent_len..]
                        .iter()
                        .map(estimate_message)
                        .sum::<u64>()
            }
            _ => messages.iter().map(estimate_message).sum(),
        }
    }

    /// Per-role breakdown (pure estimate; ignores the usage anchor so callers can
    /// attribute tokens to categories). Its `total()` is the anchor-free estimate.
    pub fn breakdown(&self, messages: &[ChatMessage]) -> ContextBreakdown {
        let mut b = ContextBreakdown::default();
        for m in messages {
            let t = estimate_message(m);
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
}

impl ContextBudget {
    /// Derive the budget from a window and the request's max output tokens.
    /// The reply reservation is `max(max_output, MIN_OUTPUT_RESERVE)`, clamped to
    /// at most half the window so tiny windows still leave room for the prompt.
    /// The trim buffer is `min(window/10, 2048)`.
    pub fn new(window: u64, max_output: Option<u32>) -> Self {
        let reserve = max_output
            .map(u64::from)
            .unwrap_or(0)
            .max(MIN_OUTPUT_RESERVE)
            .min(window / 2);
        let effective = window.saturating_sub(reserve);
        let buffer = (window / 10).min(2_048);
        let target = effective.saturating_sub(buffer);
        Self {
            window,
            effective,
            target,
        }
    }
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

    #[test]
    fn estimate_str_is_chars_over_four_rounded_up() {
        assert_eq!(estimate_str(""), 0);
        assert_eq!(estimate_str("abcd"), 1);
        assert_eq!(estimate_str("abcde"), 2); // 5/4 -> 2
    }

    #[test]
    fn full_estimate_without_anchor() {
        let meter = ContextMeter::new();
        let msgs = vec![msg(Role::System, "abcd"), msg(Role::User, "abcd")]; // 1 + 1
        assert_eq!(meter.estimate(&msgs), 2);
    }

    #[test]
    fn anchor_plus_tail_when_prefix_intact() {
        let mut meter = ContextMeter::new();
        let mut msgs = vec![msg(Role::System, "x".repeat(40).as_str())];
        // Pretend the last request sent these 1 message(s) and cost 1000 tokens.
        meter.record(1000, msgs.len());
        // Append an 8-char user turn: +2 tokens estimated on top of the anchor.
        msgs.push(msg(Role::User, "abcdefgh"));
        assert_eq!(meter.estimate(&msgs), 1002);
    }

    #[test]
    fn falls_back_to_full_estimate_when_trimmed_below_anchor() {
        let mut meter = ContextMeter::new();
        // Anchor claims 3 leading messages were sent...
        meter.record(1000, 3);
        // ...but history was trimmed to 1 message: anchor no longer describes a
        // prefix, so estimate ignores it and estimates the whole set.
        let msgs = vec![msg(Role::User, "abcd")];
        assert_eq!(meter.estimate(&msgs), 1);
    }

    #[test]
    fn breakdown_by_role_totals_match() {
        let meter = ContextMeter::new();
        let msgs = vec![
            msg(Role::System, "abcd"),      // 1
            msg(Role::User, "abcdefgh"),    // 2
            msg(Role::Assistant, "abcd"),   // 1
            msg(Role::Tool, "abcdefghijkl"), // 3
        ];
        let b = meter.breakdown(&msgs);
        assert_eq!(b.system, 1);
        assert_eq!(b.user, 2);
        assert_eq!(b.assistant, 1);
        assert_eq!(b.tool_results, 3);
        assert_eq!(b.total(), 7);
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
}
