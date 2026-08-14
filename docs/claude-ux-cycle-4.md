# Claude/Kitsu UX dogfood — cycle 4

Date: 2026-08-12

## Challenge

Exercise provider-backed session fork isolation after rebasing the custom branch
onto current `main`. Seed a tool-free conversation with `CODE=ORCHID-731` and
`STATE=BASE`, fork it, set different state in the source and child, then resume
both across fresh processes. A generic read-only observer independently audited
the same path.

## Results

Kitsu used local `qwen2.5-coder:latest` through Ollama. The successful turns took
1.8–7.1 seconds and every JSONL request reported `tools_offered: 0`. Provider-free
`sessions show --json` proved that the source contained only the ORIGINAL turn
and the child contained only the CHILD turn, so durable transcript isolation was
correct. The small local model followed both state-setting prompts but later
recalled the initial BASE value; this is model behavior, not transcript leakage.

Claude Haiku completed the equivalent four-call flow in 1.0–2.8 seconds of API
time and recalled `ORCHID-731|CHILD` correctly. Its `--fork-session` composes
forking and the child turn in one command. Kitsu takes a provider-free fork
command followed by a resume, but provides stronger host-readable snapshot and
permission-reset evidence.

## Defect and improvement

The source Kitsu session was named `cycle4-fork-0812`. The fork copied that name,
creating two identical selectors; `chat --resume cycle4-fork-0812` then failed
as ambiguous. This contradicted the documented guarantee that a name resolves
to one session. The live failure and observer audit independently identified the
same cause in `SessionStore::fork`.

Forking now clears the child name while retaining its fresh id and copied
transcript. The original name remains a stable selector; users can explicitly
name the child later. Unit and CLI regression tests cover name resolution,
tool-free named-session forking, source/child transcript divergence, omission of
all model tools, and compatibility with the command/runtime capabilities added
upstream.
