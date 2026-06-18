<!-- Review of Fixes #2/#3/#4 by claude-sonnet-4-6 via kitsu (gateway), 2026-06-19 -->

## A: `backoff_delay`

- **Overflow prevention is correct.** `attempt.saturating_sub(1).min(6)` guarantees `exp ∈ [0,6]`, so `1u64 << exp ≤ 64` and `200 * 64 = 12_800` fits easily in `u64`. No panic possible for any `u32` input. ✓
- **Cap is reasonable but undocumented upper bound:** max sleep is 12.8 s. With `retries = 6` (default-like) that's fine; but callers can set arbitrarily large `retries` and every retry past attempt 7 still sleeps 12.8 s — total wall-clock is unbounded. Consider also capping total retry count or documenting this. Minor, not a correctness bug.

## B: `AuthConfig` manual `Debug`

- **Token redaction is correct and transitive.** `Gateway` and `Config` derive `Debug` and call into `AuthConfig`'s manual `Debug`, so `{:?}` on either container will show `token: Some("<redacted>")` — verified by the regression test. ✓
- **`refresh_hint` leakage risk:** if `refresh_hint` contains a token or sensitive URL fragment (e.g. a one-time refresh token embedded in a hint URL), it prints verbatim. Low likelihood with the current design but worth a comment or redaction.
- **`format_args!` vs `&str` mixing:** `field("token", &format_args!("{token}"))` where `token` is already `&str` is fine — outputs the string directly — but is unnecessarily roundabout; `.field("token", &token)` is cleaner. Not a bug.
- **No `finish()` / `finish_non_exhaustive()` difference:** if new sensitive fields are added later, `finish()` won't warn. Prefer `finish_non_exhaustive()` to make future additions explicit — low impact today.

## C: `FinishReason::Length` handling

- **Text-path correctness:** when `tool_calls.is_empty()` and `finish == Length`, a warning is logged and the truncation notice is appended before return. The sentinel string is appended to `assistant_text` (local variable), and `last_assistant_text` was already set to the same string on the line above — but the appended suffix only appears on the returned `assistant_text`, not in `messages`. That's acceptable (the pushed `ChatMessage::assistant` doesn't include the sentinel, which is the right behaviour), but verify intentional. ✓
- **`Length` during a tool-call round is silently ignored.** If `finish == Length` but `tool_calls` is non-empty, the code falls through to the tool-execution branch and loops. The truncated output may cause malformed tool-call JSON (arguments cut mid-stream). The loop should detect `finish == Length && !tool_calls.is_empty()` and return an error or surface a warning rather than blindly executing potentially corrupt tool calls.
- **`ToolCalls` with no actual tool calls:** `FinishReason::ToolCalls` + empty `tool_calls` silently returns `Ok(assistant_text)` — correct as a graceful fallback, but worth a `tracing::warn!` since it indicates a model misbehaviour.
