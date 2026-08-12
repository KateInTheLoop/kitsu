# Cycle 14 — Hugging Face Inference Providers gateway audit (functionality)

Date: 2026-08-12
Binary: `./target/debug/kitsu`, config `./kitsu.toml` (`[gateways.huggingface]`,
`base_url = "https://router.huggingface.co"`, default model
`meta-llama/Llama-3.3-70B-Instruct`). Token supplied only as an inline
per-command `HF_TOKEN=***` env var, never written to a file.

## Acid-test verdict (stated plainly)

**Cycles 11–13's text-protocol tool-call pain is a local-model (qwen)
ceiling, not a kitsu ceiling.** Routed through HF Inference Providers,
`meta-llama/Llama-3.3-70B-Instruct` emitted genuine structured `tool_calls`
every time — kitsu showed clean `◆ Read(notes.txt)` / `◆ Edit(notes.txt)`
narration, never the bracket-JSON text-protocol fallback that qwen falls
back to locally. The multi-step docs task (test 4) even self-corrected
through two tool errors (empty `old_text`, missing
`expected_previous_content`) and committed a truthful, verified diff — a
tier of reliability the qwen baseline in `docs/chat-ux-cycle-13.md` never
reached.

## Per-test results

### 1. Catalog
`HF_TOKEN=*** ./target/debug/kitsu --config ./kitsu.toml --gateway huggingface models`
→ **130 models**, 0.222s. Fast, clean list (`CohereLabs/...` through
`zai-org/GLM-5.2-FP8`), both target models present
(`meta-llama/Llama-3.3-70B-Instruct`, `openai/gpt-oss-120b`, no `:fastest`
provider-pin variants listed separately — pins are a request-time modifier,
not a catalog entry). Verdict: solid, no gaps.

### 2. Basic chat, two models
**(a) `--model 'openai/gpt-oss-120b:fastest'`**, prompt "Reply with exactly:
pong": 2.10s wall, streamed cleanly, output `pong`. Reproduced the flagged
warning verbatim:
```
kitsu-usage: {"prompt_tokens":1557,"completion_tokens":26,"total_tokens":1583}
kitsu-context: {"used":1558,"effective":7680,"window":8192}
~ context window mismatch suspected: the provider accepted 1557 of ~1815
  estimated prompt tokens; the server likely serves a smaller window than
  the resolved 8192 and truncated the input (set --context-window 1557 or
  fix the gateway profile; /context shows provenance)
```
**This is a false positive**, not a real HF-side truncation. Root cause:
the `huggingface` gateway block in `kitsu.toml` sets no `context_window`,
HF's `/v1/models` response apparently doesn't carry one either (or kitsu
doesn't parse it), so kitsu falls back to
`DEFAULT_CONTEXT_WINDOW = 8_192` (`crates/kitsu-core/src/context.rs:22`) —
a value sized for small local models, wildly wrong for a 120B/70B hosted
model. The shortfall heuristic (`orchestrator.rs:786`,
`context_window_shortfall`) then fires because kitsu's chars/4 prompt
estimate (1815) overshoots HF's real tokenizer count (1557) by >1/8 —
ordinary estimator slop, not truncation. **The heuristic itself is sound
(it's designed to tolerate slop up to 1/8); the bug is that the HF profile
feeds it a wrong-by-an-order-of-magnitude window**, so the warning fires on
essentially every HF turn.

**(b) profile default `meta-llama/Llama-3.3-70B-Instruct`**: 3.45s wall.
Same false-positive warning, plus a second finding: the model chose to
`spawn_agent("ping")` for the trivial "reply with pong" prompt instead of
answering directly — 2 provider round-trips (`◆ Agent("ping")` then a
top-level reply) instead of 1, doubling latency and token spend for a
one-line task. This isn't a kitsu bug per se, but the tool/skill disclosure
kitsu shows up front plausibly nudges a capable model toward
over-engineering trivial asks — related to cycle-13 backlog gap #5 (skill
disclosure misleading small models), except here it recurred on a 70B
model.

### 3. Structured tool calls (acid test)
`meta-llama/Llama-3.3-70B-Instruct`, `--yes --allow-path .`, prompt "Read
notes.txt with your file tool and count its lines.": **2.44s**, clean
`◆ Read(notes.txt)` / `◇ 3 lines` narration, correct answer ("The file
'notes.txt' contains 3 lines."). Confirms proper OpenAI-protocol
`tool_calls`, not kitsu's text-protocol fallback path. See verdict above.

Side finding: in this run the mismatch warning claimed "the provider
accepted 292 of ~1252 estimated prompt tokens" while the preceding
`kitsu-usage` line reported `prompt_tokens: 1509`. The two numbers describe
different rounds (the warning reflects the post-tool-result round, usage
reflects a different accounting point) but nothing in the output says so —
read together they look self-contradictory.

### 4. Mini docs-task comparison
Same scratch dir, prompt "Read notes.txt and append a fourth line that
summarizes the first three, using your file tools.", `--allow-read-write .`:
**7.50s**. Model self-recovered from two tool errors —
`fs_edit_file` rejected an empty `old_text`, then `fs_write_file` rejected a
whole-file replace without `expected_previous_content` — before landing a
correct `fs_edit_file` diff. The run summary was truthful:
`~ 2 earlier tool error(s) were recovered within the turn; final state
committed` and `~ run summary: 1 turn(s) ok, 1 file(s) written (1
change(s)), session not saved`, with a verified host-side diff matching the
actual file. `notes.txt` ended with the correct fourth line. This is a
clean win against the qwen baseline in `docs/chat-ux-cycle-13.md`, where
the model never got past invoking the wrong tool/skill for a comparable
edit task — here the HF-routed 70B model completed the edit and kitsu's
receipts accurately reflected two transient errors plus a good final state.

### 5. Error UX
**(a) `--model 'no-such/model-xyz'`**: 0.19s, `HTTP 400`, message:
`! error: chat request was rejected (HTTP 400) - verify the selected
model, gateway flavor, and request compatibility`. Generic — kitsu
deliberately never echoes the provider's raw error body into the message
(by design, per `safe_provider_error_message`'s doc comment: "provider-
supplied body/detail strings are never interpolated"), but that means the
user never sees HF's actual reason (e.g. "model does not exist" vs "no
provider currently serves this model"), which for HF's federated routing
are two very different fixes.

**(b) `HF_TOKEN=hf_invalid0000`**: 0.18s, `HTTP 401`, message:
`! error: chat request authentication/access was rejected (HTTP 401) -
refresh the configured credential and confirm model access`. **The
profile's `refresh_hint` was NOT surfaced**, and this is a real, reproducible
bug, not a missed feature: `crates/kitsu/src/main.rs`'s
`refresh_hint_contains_credential()` rejects any hint containing `"://"`
or the substring `"hf_"` (to stop accidentally-pasted secrets from
leaking). But the huggingface profile's own hint in `kitsu.toml` is
`"create a token at https://huggingface.co/settings/tokens and \`export
HF_TOKEN=hf_...\`"` — it contains both `://` (its own URL) and `hf_` (its
own example token prefix), so `safe_auth_refresh_hint()` silently drops
the *entire* hint to `None` on every use. **kitsu's own reference config
for HF ships a refresh_hint that its own safety filter always suppresses.**
Confirmed by code inspection (`crates/kitsu/src/main.rs:1289-1350`,
`openai_compat.rs:1586-1591`), consistent with the observed empty-hint
output.

### 6. JSONL (rerun of 2a)
`--jsonl` output was clean and well-formed: `run_started` →
`turn_started` → `provider_request` → `provider_attempt`
(`category:"success"`, `status:200`, `outcome:"body_completed"`) →
`usage` (`prompt_tokens:1557`, `completion_tokens:27`) → `assistant_delta`
→ `assistant` → `context_window_mismatch` (own event type, carrying
`estimated_sent_tokens`, `reported_prompt_tokens`, `resolved_window`) →
`assistant_disposition` (`status:"committed"`) → `turn_finished` →
`final_summary`. The 401 case (test 5b) also emitted a fully-formed
`provider_attempt` (`category:"authentication"`, `status:401,
terminal:true`) followed by `assistant_disposition:"discarded"` and an
`error` event with `kind:"provider"`. No retry storms — both terminal
error categories (400, 401) correctly set `terminal:true` and stopped
after attempt 1/3 rather than burning retries on a request that can never
succeed. This is solid, sane automation surface for a remote provider.

## Latency table

| Test | Call | Wall time |
|---|---|---|
| 1 | `models` catalog (130 models) | 0.22s |
| 2a | chat, gpt-oss-120b:fastest, "pong" | 2.10s |
| 2b | chat, Llama-3.3-70B (spawned an agent) | 3.45s |
| 3 | chat + Read tool call, Llama-3.3-70B | 2.44s |
| 4 | chat + Edit tool call (2 retries), Llama-3.3-70B | 7.50s |
| 5a | chat, bad model id → HTTP 400 | 0.19s |
| 5b | chat, bad token → HTTP 401 | 0.18s |
| 6 | chat --jsonl, gpt-oss-120b:fastest, "pong" | 0.47s |

`docs/chat-ux-cycle-13.md`'s only comparable local-ollama numbers are
full-task timings against `claude -p` (81.0s claude vs 87.8s kitsu+qwen for
an edit-and-verify task) — not simple pings, so not directly comparable to
the sub-8s HF numbers above. Qualitatively: HF network round-trips
(2-3.5s for a bare completion) are markedly slower than a warm local
Ollama model's first token, but nowhere near proportionally slower for
tool-heavy work, because the 70B HF model needed far fewer corrective
rounds than local qwen needed in prior cycles — the network tax is more
than offset by fewer wasted turns.

## Ranked top-5 kitsu-side gaps/bugs (HF path)

1. **HF gateway's own `refresh_hint` self-suppresses on every 401.**
   `kitsu.toml`'s shipped hint text contains `://` and `hf_`, both of which
   trip `refresh_hint_contains_credential()` and get the whole hint
   dropped. Fix: reword the profile's hint to avoid the literal URL scheme
   and the `hf_...` example token (e.g. "create a token in your Hugging
   Face account settings under Access Tokens, then `export
   HF_TOKEN=<value>`"), or narrow the filter to reject hints containing an
   actual secret-shaped token rather than any string containing `hf_`.

2. **`context window mismatch suspected` false-positives on every HF
   turn** because the profile has no `context_window` and kitsu falls back
   to the local-model-sized `DEFAULT_CONTEXT_WINDOW = 8192`. Fix: set an
   explicit `context_window` in the `[gateways.huggingface]` block (HF
   models vary, but even a conservative 32k default beats 8192), or have
   kitsu widen its fallback / suppress the heuristic when the gateway is
   clearly a hosted provider rather than a local server.

3. **HTTP 400/404 errors never surface the provider's actual reason.**
   `no-such/model-xyz` produces a generic "verify the selected model,
   gateway flavor, and request compatibility" with no hint that HF
   specifically returned "model not found" vs. "no provider serves this
   model" vs. a malformed request — these need different fixes. Consider
   surfacing a short, bounded, sanitized excerpt of the provider's error
   body (already captured internally per `ProviderError::Http{body,..}`)
   instead of suppressing it entirely.

4. **Context-mismatch warning's own numbers can look self-contradictory.**
   In test 3 the warning cited "292 of ~1252" while the adjacent
   `kitsu-usage` line said `prompt_tokens: 1509` — both correct for
   different rounds, but nothing labels which round each number belongs
   to. Fix: tag the mismatch line with its round number, or reuse the same
   figure as the immediately-preceding usage line when there is one.

5. **A 70B model burns a spawn_agent round on a one-line "reply with X"
   prompt**, doubling latency/tokens for trivial asks. Not certainly
   fixable (model judgment), but worth watching alongside cycle-13 backlog
   gap #5 (skill disclosure nudging small models) — this shows the same
   over-tooling pattern on a large hosted model too, suggesting the
   disclosure itself (not just weak models) is a contributing factor.

## Scope note
No repo files were modified. All commands used `HF_TOKEN=***` inline only;
the token never appears in this report or in any file. Scratch working
files under `hf-test/` were removed after this report was written.
