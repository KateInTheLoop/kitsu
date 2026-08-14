# Claude/Codex UX cycle 12: honest cap-zero tool surfaces

## Challenge

Run three seeded Kitsu-versus-Codex tasks with subagents explicitly disabled,
then verify that the model, the user, and automation all see the same effective
tool authority. The Hugging Face inference gate remained closed because
`HF_TOKEN` was absent from both the current process and the user-service
environment. No remote request was sent and no credential value was read or
printed. The workers therefore used the documented local Ollama fallback with
`qwen2.5-coder`.

The comparison side used isolated in-process Codex workers because a standalone
`codex` executable was not available on the host command path. Those lifecycle
scores remain provisional. The official Codex contract says that setting
[`agents.enabled = false`](https://learn.chatgpt.com/docs/agent-configuration/subagents)
disables its multi-agent tools, which is the user-visible behavior Kitsu should
match when its spawn cap is zero.

Future recurring cycles are permission-prompt free: fixtures and exact commands
must be pre-granted, Kitsu runs with `--yes`, and a leg that would need a new
host or sandbox approval is recorded as not run and replaced by an
already-authorized local fallback.

## Seeded comparison

Cycle seed `992600` selected search, file-creation, and exact-command
observation tasks in isolated fixtures under `/tmp/kitsu-parity/992600`. Each
worker recorded its oracle before execution and checked its fixture and an
outside sentinel afterward.

| Task | Kitsu | Codex runner | Attribution |
|---|---:|---:|---|
| Search one seeded record | 65/100; safely cancelled without an answer | 93/100; exact line and value | Pre-fix Kitsu advertised four disabled agent schemas. Qwen spent almost 90 seconds on the first response; the scripted turn then gave its second provider request a fresh deadline. |
| Create one exact JSON file | 35/100; harness-contaminated, wrong placeholder file | 94/100; exact 64-byte file | Pre-fix Kitsu advertised the same four schemas; placeholder content was model behavior. The capture wrapper failed to own its descendant process group, so its child wrote after the wrapper was stopped. |
| Run one exact command | 100/100; exact command, output, and completion | 91/100; exact result | This post-fix run exposed only `command_exec` in `/tools` and both provider requests. The Codex runner's ambient authority was broader than Kitsu's explicit command grant. |

The search oracle came from
`abca64a72ec5059cd1526bef7df150666edad270b9928f6b0b2d9a6206903dc8`;
the create oracle from
`c1eda923ce0295cc790a9b6d706d62299f91a2c4a29bff383267a9b3802c788e`;
and the observation oracle from
`e961733c2bdfaae67b57615d038895785ccf953a948a4123199661c458219c30`.
No outside-fixture write, unauthorized command, credential disclosure, or
untruthful committed Kitsu answer occurred.

The create score is not a clean Kitsu product score: stopping the Python
capture parent did not terminate the Kitsu child, and the post-fix JSONL stream
was lost. The late wrong file is retained as harness/cancellation evidence, not
as proof of a Kitsu lifecycle defect.

## Selected improvement

`--agent-spawn-cap 0` already prevented a spawn from succeeding, but Kitsu still
registered `spawn_agent`, `spawn_agents`, `spawn_agent_bg`, and `agent_result`.
They appeared in `/tools`, the system prompt, `provider_request.tools_offered`,
and the actual provider body. That wasted model context and told users and
models that unusable capabilities existed.

Kitsu now registers those four schemas only when the resolved spawn cap is
greater than zero. A zero value from either `--agent-spawn-cap` or
`agent_spawn_cap` therefore disables the entire model-callable multi-agent
surface while leaving unrelated filesystem, exact-command, skill, and
workspace-evidence tools unchanged. CLI help, the README, and the functionality
contract describe the same behavior.

## Verification

A wiremock CLI regression starts a cap-zero run with read/write filesystem
authority, invokes `/tools`, and inspects the exact POST body. It proves that:

- ordinary authorized filesystem tools remain available;
- none of the four multi-agent names appears in `/tools`;
- `provider_request.tools_offered` equals the visible tool count;
- the outbound provider schemas exactly equal the `/tools` list; and
- the system prompt does not advertise a disabled multi-agent tool.

The independent post-fix observation run used binary SHA-256
`31911431db9aa945eed7e45e2413b408881b3e537d9544ad3be8be3a7ba1c373`.
It exposed only `command_exec`, ran exactly `./emit.sh` once, returned the exact
seeded output, emitted 17 parseable JSONL records, and exited zero with an `ok`
summary. Its fixture and outside sentinel remained byte-identical.

The full workspace test suite, focused CLI integration suite, formatting,
Clippy with warnings denied, and staged and unstaged diff checks pass.

## Next backlog

1. Restore `HF_TOKEN` through a user-controlled secret channel, pass
   `doctor --inference`, and run the three real Hugging Face legs. Add a durable
   login/status/logout lifecycle so a shell-only export is not lost on reboot.
2. Give every scripted turn one end-to-end deadline; currently each provider
   request after a tool call receives a fresh `--timeout-secs` budget.
3. Count failed tool calls in `final_summary` instead of requiring consumers to
   reduce the entire JSONL event stream.
4. Make `/clear` preserve the prior durable session and start a new context
   instead of overwriting the stored transcript.
5. Make the comparison harness terminate process groups, and locate or install
   a standalone Codex CLI for symmetric exit and JSONL comparisons.
