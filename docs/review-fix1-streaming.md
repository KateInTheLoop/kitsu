# Review: Fix 1 — real SSE streaming (model: claude-sonnet-4-6, via kitsu, 2026-06-19)

Here are the most important issues, in priority order:

- **Mid-stream error silently terminates without a `Done`**: When `Some(Err(e))` is hit, the state transitions to `SseState::Done` and yields the error, but `finalize()` is never called. Consumers that expect a terminal `ChatDelta::Done` will never see one, potentially hanging or misreporting the conversation state.

- **`SseState::Done` arm is unreachable dead code**: `sse_step` is called by `stream::unfold`, which stops calling once `None` is returned. The `Draining` branch already returns `None` when the queue empties, so `SseState::Done` is never entered — but its presence is misleading and the error path sets it incorrectly (see above).

- **`bytes::Bytes → Vec<u8>` copy on every chunk**: `b.to_vec()` allocates and copies every wire chunk. `ByteStream` could use `bytes::Bytes` directly (it's already a transitive dependency via reqwest) and `extend_from_slice` accepts `&[u8]` from `Bytes` deref, avoiding the allocation entirely.

- **`String::from_utf8_lossy` replaces invalid UTF-8 with U+FFFD rather than buffering**: A multi-byte UTF-8 sequence split across chunk boundaries will be silently corrupted. The buffer should track a byte-level split point and only decode once a full codepoint is confirmed (or use `std::str::from_utf8` and hold back the partial tail).

- **`[DONE]` on a line with no trailing `\n` is silently dropped**: If the server closes the connection after `data: [DONE]` without a final newline, `push_bytes` never drains that line (no `\n` found), and `finalize` will attempt to parse `data: [DONE]` as JSON, fail silently, and still emit `Done` via the fallback — so it works by accident, but `saw_done` is never set, meaning `finish_from_str` may overwrite `self.finish` unexpectedly if there is a finish_reason line after `[DONE]` in the buffer.

- **`queue` in `SseState::Reading` is superfluous on the entry path**: The initial drain at the top of `sse_step` is correct, but because `unfold` always re-enters from the returned state, and that state already has the remaining queue, items are never dropped. However, the `queue` carried inside `Reading` is a second allocation alongside `VecDeque` in `Draining`; merging the two states would simplify the logic and remove one potential source of confusion.

- **`dec.push_bytes` can return multiple deltas but only the first is dequeued before looping**: The loop does `pop_front` once per iteration and continues reading if the queue drains, but if the queue still has items it reads more bytes anyway — this is a liveness issue if the stream is slow: items sit in the queue until the next `next().await` resolves, adding unnecessary latency. The fix is to check `!queue.is_empty()` before calling `next()`.

- **`ToolCallAcc` index is taken directly from the wire (`dtc.index`)**: There is no upper-bound check; a malicious or buggy server sending `index: usize::MAX` would cause `resize_with` to attempt a Vec allocation of `usize::MAX + 1` elements, almost certainly panicking via OOM or arithmetic overflow.

- **`choice.finish_reason` after `[DONE]` is already guarded, but `self.accs` tool calls with an empty `id` and empty `name` are silently dropped**: If a tool-call stream is interrupted mid-accumulation, partial accumulators with only `args` filled are dropped in `finalize`. Depending on the contract this may be intentional, but it should be documented as a deliberate choice.

- **`stream.next()` requires `StreamExt` in scope inside `sse_step`**: There is no `use futures::StreamExt` visible in the diff; this will fail to compile unless it is imported elsewhere in the file. Confirm the import exists, otherwise this is a silent compile error waiting to surface when the surrounding context changes.
