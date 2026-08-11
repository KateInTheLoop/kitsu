# `workspace_diff` — a model-callable, host-verified evidence tool

Cycle-10 headline fix for docs/chat-ux-cycle-9.md finding 1: piped (and
interactive, scripted, JSONL) chat has no evidence path for "explain my
uncommitted changes". The model cannot see the diff, flails, spawns agents,
and times out — while `kitsu review` answers the same question in one command.
This doc is the design contract; the implementation follows it.

## What it is

A model-callable tool, `workspace_diff`, registered in chat whenever the
launch directory is inside a Git worktree. It returns the same bounded,
host-verified diff evidence that `/diff` renders and `/review` discloses:

```json
{"scope": "working" | "staged" | "unstaged" | {"base": "REV"} | {"commit": "REV"},
 "path": "optional/repo/relative/filter"}
```

Scope mirrors `/diff`'s scopes exactly (minus `checkpoint <id>`, which replays
Kitsu's own write journal and stays a human surface — the model has no
checkpoint ids and `/checkpoint` already renders them host-side). `scope`
defaults to `working`. The result is a JSON object with `target`,
`resolved_revision`, `merge_base`, `truncated`, and `diff` — byte-identical
evidence to `/diff` for the same scope: produced by Kitsu's own `git`
(`workspace::review_evidence_excluding`), capped at the same 256 KiB with the
in-band `...[diff truncated at 256 KiB]` marker and the `truncated` flag,
with the same private-path exclusions.

## Authority model — a disclosure, not a read

This is the `/review` half of the cycle-7 host-evidence asymmetry, on demand:
a **disclosure of host-gathered evidence to the provider**, never a filesystem
read by the model.

- It reuses `WorkspaceEvidenceAuthority` (main.rs), gaining a Send-safe
  constructor (`for_workspace_tool`) bound at registration time to the launch
  directory's repository root. The root is never caller-nameable; the `path`
  argument can only narrow inside the repository
  (`workspace::pathspec_from_directory` rejects `..` escapes and paths outside
  the root before any I/O).
- **No model read roots are consulted, created, or widened.** The
  `kitsu-tools` policy engine is untouched; `fs_*` registration is unchanged;
  `/permissions` continues to report `read roots none` in an ungranted
  session even after the model has called `workspace_diff`.
- Kitsu-private paths (active config, state dir, offload/artifact
  namespaces) are excluded exactly as `/diff` excludes them today — the tool
  captures a superset snapshot (config path, state dir, and the stateless
  offload base) whose directories cover every per-boundary offload
  generation, so `/new`/`/resume` rotation cannot open a gap.
- A worktree inside Kitsu-private state fails the same
  `is_within_protected_read_root` guard as `/diff`: the tool is simply not
  registered.
- Read-only: no interaction with `/undo`, checkpoints, or artifacts — the
  tool never touches the write journal or artifact storage.

## Consent

The consent basis is the same one that lets `/review` and non-interactive
`kitsu review` disclose a diff without a grant prompt: **the user started a
conversation about their own workspace with this provider**, and the
disclosure is bounded (one diff, capped, private paths excluded) and
**announced at the moment it happens** — never a standing capability.

- **Interactive:** the call surfaces like every other tool receipt
  (`◆ workspace_diff(working)` via the orchestrator's standard narration),
  followed by the standing announcement line, identical to `/review`'s:
  `~ disclosing N bytes of host-verified evidence to <model>; no read root
  granted`. No filesystem prompt is raised — prompting would train users to
  widen *model read authority* to look at their own diff, the exact
  anti-pattern the cycle-7 work removed.
- **Non-interactive (piped/scripted):** allowed by default, for the same
  reason `kitsu review` works non-interactively. The disclosure line prints
  to stderr; JSONL runs get a `workspace_evidence_disclosure` event instead.
- **Opt-out:** `--no-workspace-evidence` (global flag) or
  `workspace_evidence = false` (top-level config key) disable **registration
  entirely** — the model never learns the tool exists. Either surface
  disables it; there is no override flag.
- Not registered for `kitsu review` one-shots (the review turn stays
  tool-free as documented) and not offered to subagents (`sub_tools` /
  profile registries): disclosure announcements are a top-level conversation
  contract, and a spawner that has the evidence can already include it in a
  task brief.

## JSONL framing

The call itself is framed like every tool: `tool_started` /
`tool_finished` with `name: "workspace_diff"`. Between them the handler emits
one additive v1 event, generalizing the `/review` command-frame
`review_disclosure` result to a turn-scoped event:

```json
{"schema":"kitsu.chat.v1","type":"workspace_evidence_disclosure","run_id":"…",
 "source":"workspace_diff","target":"working_tree","resolved_revision":null,
 "merge_base":null,"truncated":false,"evidence_bytes":1699,
 "model_disclosed":true,"model_read_root_granted":false}
```

The field set matches `review_disclosure` exactly (plus `source`), so
consumers audit both disclosure surfaces with one shape. The
`/permissions` payload's `host_evidence.consumers` gains
`"workspace_diff"` and `prompt_disclosure` becomes
`"user_command_or_workspace_diff_tool"` when the tool is registered
(unchanged otherwise). Schema and docs/jsonl-protocol.md are updated.

## System prompt

When (and only when) the tool is registered, one line tells the model it can
fetch verified workspace diffs itself — directly preventing the cycle-9
flail (agent-spawning, asking the user to paste diffs):

> Use workspace_diff to fetch the user's verified git diff (scope working /
> staged / unstaged / {base: REV} / {commit: REV}) when asked about their
> changes — do not ask them to paste diffs or spawn agents to run git.

## Test plan

Unit (main.rs): scope-argument parsing (strings, `{base}`/`{commit}`
objects, defaults, rejection of unknown scopes/extra keys/empty revisions);
system-prompt line present iff `workspace_diff` is registered.

Integration (tests/cli.rs, existing wiremock + git-fixture patterns): a
scripted chat where the mock provider calls `workspace_diff` and the
transcript shows the diff-backed answer plus the disclosure line while
`/permissions` still reports no read roots and the host-evidence section
lists the tool; the same flow in `--jsonl` asserting the
`workspace_evidence_disclosure` event shape between `tool_started` and
`tool_finished`; `--no-workspace-evidence` (and the config knob) unregister
the tool; private-path exclusions honored through the tool
(kitsu-private file changes absent from the returned diff); a non-worktree
directory registers nothing.
