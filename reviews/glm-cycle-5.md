# GLM Cycle 5 — Streaming Parse Robustness / Error-Recovery / Termination

Scope: `crates/kitsu-providers/src/openai_compat.rs`, the OpenAI-compatible SSE
streaming path only. Reviewed by reading the following ranges (no full-file
read, no re-reads after this draft):

- `StreamChunk` wire types: lines 255–320
- `SseDecoder` + `process_line` + `finalize`: lines 355–560
- `SseState` enum: lines 560–590
- `finish_sse` + `sse_step` state machine: lines 1040–1190
- `chat_stream_with_telemetry` retry/steering + stream wiring: lines 1414–1745

The pipeline is: `chat_stream_with_telemetry` (1414) builds a `ByteStream` and
drives it through `stream::unfold(sse_step)` (1679); `sse_step` (1095) feeds
wire chunks into `SseDecoder::push_bytes` (406), which splits on `\n` and calls
`process_line` (439) per line; termination runs through `finish_sse` (1072) →
`SseDecoder::finalize` (530). Findings are ranked by severity × confidence;
every citation quotes the exact text read from the cited line(s).

**At a glance —** 1 `error:null` false positive + discarded detail (medium/high);
2 cancellation disposition is Drop-only (medium/verify); 3 `Stop` synthesized
on `[DONE]`-without-finish + unknown-reason mapping (medium/split); 4 line cap
256 KiB < arg cap 1 MiB (low–medium/high); 5 extra `choices` silently dropped
(low/high); 6 multi-line `data:` not accumulated (low/high, known); 7 EOF
mid-JSON reported as "malformed JSON" not truncation (low/high, diagnostics).
Items 2 and 3b fall outside the read ranges and are marked "verify".

## Ranked findings

### 1. Mid-stream `error` object: `error: null` false positive AND discarded gateway detail
**Severity: medium. Confidence: high.**

`process_line` rejects any `data:` record carrying an `error` field:

```
457:        if value.get("error").is_some() {
458:            return Err(ProviderError::InvalidResponse(
459:                "provider returned an error object in a successful SSE response".to_string(),
460:            ));
461:        }
```

Two distinct problems share this one line:

- **`error: null` is a false positive.** `serde_json::Value::get("error")` on
  `{"error": null}` returns `Some(&Value::Null)`, so `.is_some()` is true and a
  *successful* payload that explicitly carries `"error": null` is rejected.
  Some gateways emit `"error": null` on healthy chunks.
- **The gateway's error detail is discarded.** When a real mid-stream error
  arrives as `{"error":{"message":...,"type":"...","code":...}}` (e.g. a
  rate-limit or content filter fired during streaming), the user sees only the
  fixed string `"provider returned an error object in a successful SSE
  response"` — the message, type, and code the gateway supplied are dropped.

**Proposal:** ignore a null error and surface the gateway detail otherwise, e.g.:

```rust
if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
    let detail = err.get("message").and_then(|m| m.as_str())
        .or_else(|| err.as_str())
        .unwrap_or("provider returned an error object in a successful SSE response");
    return Err(ProviderError::InvalidResponse(
        format!("provider error in SSE stream: {detail}"),
    ));
}
```

This fixes the false positive and turns an opaque failure into a diagnosable
one in a single change.

### 2. `ResponseBodyAttemptGuard` disposition on consumer cancellation relies solely on Drop
**Severity: medium. Confidence: medium (explicit paths verified; Drop impl not read).**

`attempt_guard` is moved into the `Reading` state and only explicitly
dispositioned on three terminal paths: a `push_bytes` decode error (1126), a
transport error (1150–1151), and via `finish_sse` → `finalize` success/failure
(1078–1088):

```
1118:                    Some(Ok(bytes)) => {
1119:                        match dec.push_bytes(&bytes) {
1120:                            Ok(deltas) => {
1121:                                for d in deltas {
1122:                                    queue.push_back(d);
1123:                                }
1124:                            }
1125:                            Err(error) => {
1126:                                attempt_guard.failed(&error);
1127:                                return Some((Err(error), SseState::Done));
1128:                            }
1129:                        }
```
```
1149:                    Some(Err(e)) => {
1150:                        let error = map_send_error(e);
1151:                        attempt_guard.failed(&error);
1152:                        return Some((Err(error), SseState::Done));
1153:                    }
```

If the *consumer* drops the `ChatStream` mid-flight (the common case: the agent
loop aborts a tool, the user interrupts chat, or a compaction cascade cancels
the request), execution leaves `sse_step` without ever reaching any of these
returns. The guard — created at 1657 — is then dropped while still in the
`Reading` state:

```
1657:        let attempt_guard = ResponseBodyAttemptGuard::new(
1658:            telemetry.as_ref(),
1659:            final_attempt,
1660:            max_attempts,
1661:            status.as_u16(),
1662:            started,
1663:            self.timeout,
1664:        );
```

There is no explicit cancellation branch; correctness of the terminal
telemetry trace on cancellation depends entirely on
`ResponseBodyAttemptGuard::Drop` emitting a "cancelled" disposition.

**Proposal:** verify `ResponseBodyAttemptGuard::Drop` publishes a
cancelled/aborted terminal disposition (distinct from `completed`/`failed`)
and that doing so exactly once is idempotent against the three explicit paths
above (i.e. a `dispositioned: bool` flag). If Drop does *not* emit a terminal
event, every consumer-driven cancellation leaves an open-ended attempt in the
trace. If it does, add a one-line comment at the `Reading` match arm noting
that cancellation is Drop-handled so the invariant is not silently regressed.

### 3. `finish_from_str` unknown reasons + silent `Stop` synthesis on `[DONE]` without `finish_reason`
**Severity: medium. Confidence: split (synthesis is concrete; `finish_from_str` body not read).**

Two termination-shaping concerns, one fully evidenced, one to verify:

**3a. Concrete — `Stop` is synthesized when `[DONE]` arrives with no `finish_reason`.**
`finalize` only requires that *either* `saw_done` or `finish` be set:

```
537:        if !self.saw_done && self.finish.is_none() {
538:            return Err(ProviderError::InvalidResponse(
539:                "SSE stream ended without data: [DONE] or an explicit finish_reason".to_string(),
540:            ));
541:        }
```

then unconditionally emits a terminal `Done`, defaulting any absent finish to
`Stop`:

```
555:        out.push(ChatDelta::Done(self.finish.unwrap_or(FinishReason::Stop)));
```

So a gateway that sends `data: [DONE]` *without* ever emitting a
`finish_reason` chunk is reported as a clean `Stop`, even if the real reason
was a token-budget truncation (`length`) or a content filter that the gateway
chose to signal only via `[DONE]`. The synthesized `Stop` masks truncation from
the caller, which then cannot distinguish "model finished naturally" from
"stream was cut". This is a termination-correctness leak.

**3b. To verify — `finish_from_str` on unknown reason strings.** When a
`finish_reason` *is* present, it is mapped opaquely:

```
521:        if let Some(fr) = choice.finish_reason {
522:            self.finish = Some(finish_from_str(&fr));
523:        }
```

`finish_from_str` was outside the read ranges. If it falls back to `Stop` (or
any single default) for unrecognized strings, then a gateway emitting a
non-standard but meaningful `finish_reason` (e.g. `"model_length"`,
`"content_filter"`, a vendor-specific code) would be silently reclassified.

**Proposal:** (3a) do not synthesize `Stop` silently — if `saw_done` is true but
`self.finish.is_none()`, either propagate a distinct "finish reason missing"
warning on the telemetry channel, or treat it as `Stop` *only* when
`saw_semantic_choice` is true and at least one non-empty content delta was
emitted (so a truly empty `[DONE]`-only stream is not dressed up as a
successful stop). (3b) confirm `finish_from_str` preserves unknown reasons
(e.g. an `Other(String)`/`Unknown` variant) rather than collapsing to `Stop`,
so downstream context/telemetry can see the real termination cause.

### 4. Per-line cap (256 KiB) is smaller than the tool-argument cap (1 MiB)
**Severity: low–medium. Confidence: high.**

The three ceilings are:

```
359:const MAX_SSE_LINE_BYTES: usize = 256 * 1024;
360:const MAX_SSE_STREAM_BYTES: usize = 16 * 1024 * 1024;
361:const MAX_STREAMED_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
```

A line is rejected when `buf + segment` exceeds `MAX_SSE_LINE_BYTES`:

```
422:            if self.buf.len().saturating_add(segment.len()) > MAX_SSE_LINE_BYTES {
423:                return Err(ProviderError::ResponseTooLarge {
424:                    part: ProviderResponsePart::SseLine,
425:                    limit_bytes: MAX_SSE_LINE_BYTES,
426:                });
427:            }
```

while tool arguments are allowed to grow to `MAX_STREAMED_TOOL_ARGUMENT_BYTES`:

```
510:                if let Some(args) = f.arguments {
511:                    if acc.args.len().saturating_add(args.len()) > MAX_STREAMED_TOOL_ARGUMENT_BYTES
512:                    {
513:                        return Err(ProviderError::ResponseTooLarge {
514:                            part: ProviderResponsePart::ToolArguments,
515:                            limit_bytes: MAX_STREAMED_TOOL_ARGUMENT_BYTES,
516:                        });
517:                    }
```

These two limits are inconsistent: a gateway that emits a *single* SSE line
carrying > 256 KiB of tool-call argument delta (one large JSON argument object,
not split across chunks) is rejected by the **line** ceiling first, with the
misleading `SseLine` part label, before the 1 MiB argument budget is ever
reached. The argument cap is therefore unreachable for single-line deltas.

**Proposal:** either raise `MAX_SSE_LINE_BYTES` to at least
`MAX_STREAMED_TOOL_ARGUMENT_BYTES` + framing overhead so the argument cap is
the true ceiling for argument-bearing lines, or document the intended
invariant (single-line deltas are capped at 256 KiB; only *aggregated*
arguments may reach 1 MiB). The former is safer against real gateways that do
not fragment large tool arguments.

### 5. Extra `choices` silently dropped via `.next()`
**Severity: low. Confidence: high.**

Only the first choice is ever consumed:

```
474:        let Some(choice) = chunk.choices.into_iter().next() else {
475:            return Ok(());
476:        };
```

kitsu does not request `n > 1`, so a conformant gateway returns exactly one
choice. But a misconfigured gateway (or a proxy that injects `n`) returning
multiple choices has every choice after the first silently discarded with no
telemetry or warning. The user gets a partial answer with no signal that data
was dropped.

**Proposal:** when `chunk.choices.len() > 1`, emit a one-shot telemetry warning
(or at minimum a `log::warn!`) naming the count, so a misconfigured upstream is
discoverable rather than invisible. No behavioral change to the answer is
required.

### 6. Multi-line `data:` fields are not accumulated (OpenAI-specific assumption)
**Severity: low (known limitation). Confidence: high.**

`process_line` treats each `data:` line as a self-contained JSON record:

```
438:    fn process_line(&mut self, line: &str, out: &mut Vec<ChatDelta>) -> Result<(), ProviderError> {
439:        if self.saw_done {
440:            return Ok(());
441:        }
442:        let Some(data) = line.strip_prefix("data:") else {
443:            return Ok(());
444:        };
445:        let data = data.trim();
446:        if data == "[DONE]" {
447:            self.saw_done = true;
448:            return Ok(());
449:        }
450:        if data.is_empty() {
451:            return Ok(());
452:        }
```

Per the SSE spec, consecutive `data:` lines are joined with `\n` to form one
event; this decoder never accumulates them. For the OpenAI chat-completions
streaming protocol this is correct (each `data:` line is a complete JSON
chunk), but it means this decoder is not a general SSE parser and would
silently mis-parse a spec-compliant multi-line `data:` event as several
malformed JSON records.

**Proposal:** add a one-line doc comment on `process_line` stating the
single-line-`data:` invariant explicitly ("each `data:` record is parsed
independently; OpenAI-compatible streaming never splits one event across
multiple `data:` lines"), so the assumption is recorded and a future reader
does not try to reuse this for generic SSE.

### 7. EOF mid-JSON is reported as "malformed JSON" rather than truncation
**Severity: low (diagnostics). Confidence: high.**

On EOF, `finalize` flushes any buffered partial line through `process_line`:

```
532:        if !self.buf.is_empty() {
533:            let rest = std::mem::take(&mut self.buf);
534:            let line = String::from_utf8_lossy(&rest).to_string();
535:            self.process_line(line.trim(), &mut out)?;
536:        }
```

If the connection dropped mid-record, the partial bytes are a truncated JSON
document, and `process_line` surfaces:

```
454:        let value: serde_json::Value = serde_json::from_str(data).map_err(|_| {
455:            ProviderError::InvalidResponse("malformed JSON in SSE data record".to_string())
456:        })?;
```

i.e. "malformed JSON in SSE data record" — which reads as a gateway bug, when
the real cause is a truncated stream (TCP reset masked as clean EOF, or a
proxy cutting the body). The truncation case and the genuinely-malformed-JSON
case are indistinguishable in the error today.

**Proposal:** in `finalize`, distinguish "buffer non-empty AND no trailing
`\n` was ever seen for this line" from a parse failure on a complete line:
emit a truncation-specific message (e.g. `"SSE stream truncated mid-record"`)
when the flush path fails to parse, and keep the generic malformed-JSON
message for lines that *did* terminate with `\n` but still failed to parse.

## Verified-solid (do not regress)

These are correctly handled and should be preserved during any change above:

- **Exact byte accounting with overflow safety.** `received_bytes` uses
  `checked_add` + `filter(<= MAX_SSE_STREAM_BYTES)` (408–415), and the line
  check uses `saturating_add` (422) — no arithmetic panic on hostile input.
- **Line-buffer ceiling is independent of chunk size.** The
  `split_inclusive(b'\n')` loop checks `buf + segment` *before* each append
  (421–428), so a single huge reqwest chunk of many small records cannot
  transiently blow past the line limit. The comment at 417–420 records this.
- **`saw_semantic_choice` guard.** `finalize` rejects an empty/error payload
  that never carried text, a tool call, or a finish reason (542–546), so a
  `[DONE]`-only or usage-only stream cannot be dressed up as a successful
  answer.
- **`[DONE]` releases the answer without waiting for body drain.** The
  early-out at 1133–1135 (with its comment at 1130–1132) correctly makes the
  completed answer usable as soon as the sentinel arrives, rather than
  depending on the gateway closing the HTTP body.
- **Mid-stream transport failure is surfaced, not swallowed.** `Some(Err(e))`
  at 1149–1152 maps through `map_send_error` and marks the attempt failed, so a
  TCP reset mid-stream is not mistaken for a clean EOF.
- **Error-object detection precedes struct deserialization.** The `error` check
  at 457 runs before `serde_json::from_value` (462), so a provider error shape
  that does not match `StreamChunk` is still caught — the ordering is right
  (the defect in finding 1 is the *content* of the resulting error, not its
  timing).

## Process note

This review was written from five targeted ranged reads of
`openai_compat.rs` (the `StreamChunk` types, the `SseDecoder`/`process_line`/
`finalize` block, the `SseState` enum, the `finish_sse`/`sse_step` machine, and
`chat_stream_with_telemetry`) plus grep to locate those ranges. No sub-agents
were used; all reads were performed directly. Two items are flagged
specifically because they fall outside those ranges and were intentionally
not re-read: the body of `finish_from_str` (finding 3b) and
`ResponseBodyAttemptGuard::Drop` (finding 2). Both are presented as
"verify"-class proposals with the exact citation that motivates the check.