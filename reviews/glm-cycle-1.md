# Review: `crates/kitsu-core/src/context.rs` — Cycle 1

Scope: `context.rs` only (378 lines). Three review axes: **estimate accuracy**,
**budget policy**, and the **compaction breaker** (the anchor invalidation /
length-only fallback that breaks a stale post-compaction anchor). Proposals are
ranked by impact (P0 highest). Every line citation was verified by re-reading
that exact line; quotes are copied verbatim from the file.

---

## P0-1 — `message_tokens` / `estimate_str` count bytes, not characters

`String::len()` and `str::len()` return a **UTF-8 byte count**, not a character
count. For CJK or emoji text a single grapheme is 3–4 bytes; at the prose rate
(`CHARS_PER_TOKEN = 4`, line 16: `const CHARS_PER_TOKEN: usize = 4;`), four CJK
chars (12 bytes) estimate to `ceil(12/4) = 3` tokens while real BPE tokenizers
typically emit ~4. The estimator therefore systematically **undercounts** the
most token-dense scripts exactly where context pressure bites hardest.

The byte-length usage is pervasive:

- Line 30 (`estimate_str`): `    s.len().div_ceil(CHARS_PER_TOKEN) as u64`
- Line 36 (`message_tokens`, text bucket): `    let text = m.content.len() + m.tool_call_id.as_ref().map_or(0, String::len);`
- Line 40 (tool-call JSON bucket): `        .map(|c| c.tool_name.len() + c.args.len() + c.call_id.len())`

**Proposal:** document the byte assumption explicitly (or, where a tokenizer is
known, divide on `chars().count()` / a grapheme-aware count), and consider a
per-script safety multiplier for non-ASCII so the estimate errs toward
over-counting under context pressure.

---

## P0-2 — No per-message structural overhead in the estimate

Real chat tokenizers add per-message framing tokens (role tags, start/end
delimiters, separators) — a roughly constant cost per message. `message_tokens`
only sums content + JSON bytes:

- Line 42: `    (text.div_ceil(CHARS_PER_TOKEN) + json.div_ceil(CHARS_PER_TOKEN_JSON)) as u64`

A long conversation of many short messages is materially undercounted because
that per-message constant is never modeled. The anchored tail sum inherits the
same omission for newly appended rounds:

- Line 103: `                tokens + messages[sent_len..].iter().map(message_tokens).sum::<u64>()`

**Proposal:** add a small constant per-message overhead term (configurable per
provider family) to `message_tokens`, so the estimate reflects real request
token usage for multi-turn, short-message conversations.

---

## P0-3 — The compaction breaker is a length-only guard relying on caller discipline

`estimate`'s validity check is a count, not a structural/content identity:

- Line 102: `            Some((tokens, sent_len, _)) if sent_len <= messages.len() => {`

The `invalidate` doc comment itself admits this is unsound after a reshape +
regrowth:

- Line 86: `    /// spuriously satisfy `sent_len <= messages.len()` again while no longer`
- Line 87: `    /// describing an actual prefix of `messages`. Callers that reshape the`

The breaker therefore **breaks correctly only if every reshape path remembers to
call `invalidate()`** (line 91: `    pub fn invalidate(&mut self) {`). Any code
that drops/summarizes messages and forgets produces a silently wrong estimate —
trusting a real-usage number that no longer describes the prefix. There is no
content hash or monotonic epoch to detect this autonomously; the entire safety
property rests on the caller invariant documented at lines 87–90.

**Proposal:** add an autonomous staleness signal — e.g. a generation/epoch
counter incremented on every reshape and compared against the anchor, or a
cheap content fingerprint of the anchored prefix — so the breaker can refuse a
stale anchor even when a caller forgets `invalidate()`. At minimum, gate the
length check with a debug-assert that the prefix content is unchanged.

---

## P1-1 — `anchor_includes_schema` inherits the same length-only fragility

`anchor_includes_schema` guards on `had_schema` **and** the same count check:

- Line 123: `        matches!(self.anchor, Some((_, sent_len, had_schema)) if had_schema && sent_len <= messages.len())`

If a caller reshapes messages without invalidating, this returns the stale
`had_schema` for content it never measured — so a schema-less anchor could be
wrongly treated as schema-inclusive (or vice-versa), causing callers to skip or
double-add the schema estimate. It shares the root cause of P0-3 but is
narrower and more consequential because it directly drives whether schema
overhead is added on the next request.

**Proposal:** share the staleness signal from P0-3 so this method cannot report
`true` against a stale anchor; until then, document that it is only valid when
`anchored()` is true **and** no reshape occurred.

---

## P1-2 — `estimate` discards `had_schema`, so schema-presence asymmetry is invisible to it

The anchor stores `had_schema` (line 68: `    anchor: Option<(u64, usize, bool)>,`),
but `estimate` binds it as `_`:

- Line 102: `            Some((tokens, sent_len, _)) if sent_len <= messages.len() => {`

So `estimate` cannot tell whether the anchored usage included schema overhead.
If the active tool set changed between the anchor round and a later round, the
tail estimate (line 103) adds only message bytes and silently misses the schema
delta. The flag's value is only exposed via `anchor_includes_schema` (line
123); the two methods can disagree in spirit about what the anchor covers, with
no single source of truth for "what does this anchor include?"

**Proposal:** make `estimate`'s contract explicit about what it covers (anchor's
measured prefix only; never schema delta), and consider a single helper that
returns both the anchored estimate and the schema-coverage bit so callers can't
mix the two views.

---

## P1-3 — `tool_call_id` is billed at the prose rate alongside content

In `message_tokens`, the message-level `tool_call_id` is folded into the `text`
bucket at the 4-bytes-per-token prose rate:

- Line 36: `    let text = m.content.len() + m.tool_call_id.as_ref().map_or(0, String::len);`

IDs are short opaque strings that tokenize near 1:1, so this is a minor
over-allowance, but more importantly it conflates structural metadata with
authored content in the same bucket, making the per-role `breakdown` (lines
128–140) and the tail estimate (line 103) slightly less principled. The tool-call
JSON fields, by contrast, correctly use the denser rate (line 42).

**Proposal:** bucket `tool_call_id` separately (or with the JSON fields) and
document the rate choice so the prose bucket reflects authored text only.

---

## P1-4 — Trim buffer is decoupled from the output reservation

The buffer is a pure function of the window, ignoring `reserve`:

- Line 166: `        let buffer = (window / 10).min(2_048);`
- Line 167: `        let target = effective.saturating_sub(buffer);`

On a 200k window with a 64k `max_output`, `effective` is 136k and the trim
target is 136k − 2048 (~1.5% headroom) — the breaker fires very close to
`effective`, leaving little hysteresis before an emergency trim. On a tiny
window the same formula eats a full 10%. Buffering hysteresis should arguably
scale with the reservation or the expected compaction step size so the breaker
trips early enough to avoid a last-minute emergency cascade.

**Proposal:** tie the buffer to `max(reserve, expected_step)` or a percentage of
`effective`, not `window` alone, so large-reservation models get proportionally
more headroom and the compaction breaker trips with adequate margin.

---

## P2-1 — `None` max_output reserves only `MIN_OUTPUT_RESERVE`, which can under-reserve capable models

When `max_output` is `None` it is mapped to 0, then floored to
`MIN_OUTPUT_RESERVE` (512):

- Line 162: `            .unwrap_or(0)`
- Line 163: `            .max(MIN_OUTPUT_RESERVE)`

A model that supports large completions but whose config omits `max_output` gets
only 512 tokens reserved for the reply (line 25: `pub const MIN_OUTPUT_RESERVE:
u64 = 512;`), which can truncate long answers. The fallback is safe for the
*window* (never overflows) but pessimistic for *output capability*.

**Proposal:** let the model/provider default for `max_output` flow through where
known, and reserve only `MIN_OUTPUT_RESERVE` as a true last resort; document that
`None` means "unknown capability" rather than "small capability."

---

## P2-2 — `target == effective` when `window < 10` (zero trim buffer)

For a window smaller than 10 tokens, `window / 10 == 0`:

- Line 166: `        let buffer = (window / 10).min(2_048);`

So `buffer = 0` and `target = effective` (line 167), offering no hysteresis band
at all — the compaction breaker has no room to trip before hitting `effective`.
These windows are pathological, but the code has no assertion or guard
documenting the assumption that `window >= 10`.

**Proposal:** floor the buffer at 1 (or a small absolute minimum) and add a
debug-assert on a sane minimum window so the zero-band case is explicit rather
than silent.

---

## P2-3 — `record` accepts `sent_len` with no sanity bound

`record` stores `sent_len` verbatim with no validation:

- Line 80: `        self.anchor = Some((prompt_tokens, sent_len, had_schema));`

Nothing rejects `sent_len == 0` (an anchor claiming it sent zero messages). With
`sent_len == 0`, line 102 (`sent_len <= messages.len()`) is always true, so
`estimate` returns `tokens + messages[0..]` — i.e. the real anchor tokens **plus
a full recompute of every message**, silently double-counting. A `sent_len`
beyond any plausible message count is likewise accepted unchallenged.

**Proposal:** add a debug-assert / minimum bound (e.g. `sent_len >= 1`) and
optionally clamp implausibly large values, so misuse fails loudly instead of
producing a plausible-looking but wrong estimate.

---

## P2-4 — `estimate` tail uses `message_tokens`, which omits schema added in later rounds

Because the tail is estimated purely from message bytes, schema-bearing rounds
appended after the anchor contribute only their message content, not the
tool-schema tokens the request actually sends:

- Line 103: `                tokens + messages[sent_len..].iter().map(message_tokens).sum::<u64>()`

This is by design (schema is "static overhead" handled by callers, per the
`anchor_includes_schema` doc at lines 115–121), but it is undocumented in
`estimate` itself. A reader of the `estimate` doc (lines 95–99) could reasonably
assume the tail estimate is request-complete.

**Proposal:** add a note to the `estimate` doc (lines 95–99) that the result
excludes tool-schema tokens for any schema-bearing round in the tail, and point
to `anchor_includes_schema` for callers that need to account for that overhead.