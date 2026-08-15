# Claude/Kitsu UX dogfood — cycle 3

Date: 2026-08-12

## Challenge

Run Kitsu through Hugging Face's OpenAI-compatible router, compare its terminal
experience with Claude CLI, and turn the first material UI failure into one
small product improvement. Credential values were never written to the
repository or captured in this report.

## What happened

Kitsu reached `https://router.huggingface.co/v1/chat/completions` and tried three
currently catalogued text-generation routes with tools disabled and a 16-token
output ceiling:

- `prism-ml/Ternary-Bonsai-27B-gguf:together`
- `Qwen/Qwen2.5-Coder-7B-Instruct:nscale`
- `openai/gpt-oss-20b:deepinfra`

All three were rejected before inference with HTTP 401. Each JSONL trace showed
`tools_offered: 0`, an authentication-classified provider attempt, discarded
assistant output, and zero usage. Trying unrelated providers ruled out a single
unavailable model. The credential needs Hugging Face's fine-grained **Make
calls to Inference Providers** permission; catalogue access alone does not prove
chat authorization.

The hosted challenge therefore did not generate an HF answer. Per the fallback
rule, Kitsu then ran the same diagnostic-writing task locally with
`qwen2.5-coder:latest` through Ollama. It completed in 8.8 seconds with 146
tokens and no tools. Claude Haiku completed the matched tool-free task in 5.4
seconds for $0.006669. Claude named the exact missing permission; the local
model described missing permissions but was less specific. Kitsu's JSONL was
more explicit about request attempts, tool exposure, usage, disposition, and
final status.

The HF interface also supports server-side route suffixes: `:fastest` (the
default), `:cheapest`, `:preferred`, and an explicit `:<provider>`. Kitsu passes
these model IDs through unchanged, so no provider-specific client is needed for
chat completions.

## UX defect found

The bundled HF profile already contained a recovery hint, but the hint also
contained a URL and an `hf_...` example. Kitsu correctly rejects
credential-shaped configured hints before displaying them, so the entire useful
permission instruction disappeared and users saw only the generic advice to
refresh their credential. Separately, human `doctor --online` output did not say
that its online check was catalogue-only even though JSON output carried that
fact.

## Improvement

- Replace the bundled HF recovery hint with secret-safe, actionable wording
  that names the exact Inference Providers permission.
- Label human online-doctor output as **model catalog only; chat/inference
  access was not tested**.
- Cover both the safe hint and human scope label with regression tests.

This keeps the security boundary intact while removing the export/debugging
loop that hid the real HF failure.
