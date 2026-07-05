# ISSUE-008 — Buffering (ISSUE-002 fix) over-suppressed real answers before/around tool calls

- **Status:** RESOLVED (gated suppression on the synthesized-call flag; verified live on HF)
- **Severity:** Medium (hides read/grep output and pre-tool reasoning on structured-tool providers)
- **Found by:** dogfooding iteration 2 (HF Llama-3.3-70B; `refactor_04` read-backs printed nothing despite 124 completion tokens)
- **Area:** `kitsu-core` orchestrator streaming render

## Symptom
"Read a file and show its contents" (and read-back turns) printed **nothing** —
the model's answer was swallowed.

## Root cause
The ISSUE-002 fix buffered and suppressed **all** text in any tool-offered round,
printing only on a terminal (no-tool-call) round. But a model using **structured**
tool calls (HF's path) commonly emits its real answer/reasoning text in the SAME
round as the tool call — that text was suppressed. Only **text-protocol JSON**
(the synthesized case) needs suppression.

## Fix
Print the buffered round text unless the round was a **synthesized text-protocol**
call (`synthesized_call`). So: structured-tool-round reasoning and terminal
answers print normally; only the raw text-protocol JSON is hidden. Both ISSUE-002
(no JSON echo, Ollama text-protocol) and ISSUE-008 (answers show, HF structured)
now hold.

## Verification
- HF: "read a file and show its contents" prints the content (marker visible). ✓
- Ollama: file-gen still shows the tool confirmation, not raw JSON. ✓ (suppression
  gated on the synthesized flag)
