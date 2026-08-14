# Claude/Kitsu UX cycle 2 — tool-free session continuity

- Date: 2026-08-11
Branch: `dogfood/claude-ux-cycles`

## Exercise

The random task for this cycle was a two-turn continuity check:

1. Remember continuity marker `FOX-731` and a preference for concise, concrete
   documentation.
2. Resume the same conversation and recall both facts without restating them in
   the second prompt.

A separate generic observer agent proposed extending this into a fork-isolation
exercise. That is a good candidate for the next cycle; this cycle stayed bounded
to create/resume behavior so the two CLIs could be compared directly.

## Provider path

Kitsu's Hugging Face catalog check authenticated successfully, returned 129
models, and advertised the configured Llama model. The subsequent chat request
returned HTTP 401, demonstrating that catalog authentication does not guarantee
inference authorization for a particular model/provider route. No credential is
recorded in this report or in the repository.

The exercise therefore used the already installed local Ollama model
`qwen2.5-coder:latest`. For future hosted rotations, Hugging Face's own
`hf-inference` provider can be selected with a provider suffix such as
`HuggingFaceBio/Carbon-3B:hf-inference`; it still consumes monthly credits and
then pay-as-you-go usage. See the [model catalog](https://huggingface.co/inference/models),
[HF Inference provider guide](https://huggingface.co/docs/inference-providers/providers/hf-inference),
and [pricing guide](https://huggingface.co/docs/inference-providers/pricing).

## Matched results

| Surface | Claude CLI | Kitsu before this change |
|---|---|---|
| Runtime/model | Claude CLI 2.1.228, Haiku | Local Ollama, Qwen 2.5 Coder |
| Tool policy | Explicit empty tool set with `--tools ""` | Four agent tools were still offered in a deliberately tool-free run |
| First turn | Recalled both facts exactly | Recalled both facts, but normalized `FOX` to `Fox` |
| Resumed turn | Returned the requested two bullets exactly | Returned both requested bullets correctly |
| Continuity visibility | Returned a resumable session ID | Reported session ID, durable/resumed state, snapshot state, provider attempt, and usage |
| Lifecycle UI | Resume by session ID/name | Provider-free list/show/resume/fork/export/archive/restore/delete surfaces |
| Performance evidence | About 2.2–2.7 seconds per turn and about $0.0045 per turn | First local turn was much slower; usage was visible but cost and elapsed time were not summarized |

Claude's clearest advantage was intentional tool exclusion plus a compact
latency/cost ledger. Kitsu's clearest advantage was inspectable durable-session
state and a much broader provider-free lifecycle UI.

## Improvement implemented

`kitsu chat --no-tools` now offers an empty model-tool registry for the entire
chat while retaining client-side project context, explicit skill preloads,
attachments, and session continuity. It also omits the on-demand skill-tool hint
from the system prompt. Write and command capability flags conflict at CLI parse
time so grants cannot be silently ignored; `--allow-path` remains available for
client-side project-context and attachment reads without exposing a read tool.

The post-change local smoke test emitted `tools_offered: 0`, returned
`tool-free confirmed` exactly, used 35 total tokens, and completed in about 2.3
seconds. Wire-level integration coverage separately verifies that the provider
request contains no `tools` field even when an authorized `@file` attachment is
expanded into the prompt.

## Next useful comparisons

- Run the observer's four-turn base/child fork-isolation exercise in both CLIs.
- Compare cancellation, retry, and correction/steering behavior during an
  intentionally slow response.
- Add elapsed-time and optional provider cost metadata to Kitsu's final summary.
- Improve hosted-provider diagnostics so catalog reachability and chat-route
  authorization are visually distinct without implying that a paid probe ran.

No commit or push was made during this cycle.
