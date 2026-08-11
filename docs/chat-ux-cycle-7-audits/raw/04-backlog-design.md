# Cycle 7 — Audit 4: design spike on the top three backlog items

Date: 2026-08-11. Auditor: UX auditor 4 (design spike, no repository edits).
Binary: `target/debug/kitsu` → `kitsu 0.1.0 (6c047a0ee69f, source commit 2026-08-10)`, matches repo
HEAD `6c047a0`. Gateway: local Ollama, `qwen2.5-coder:latest`. Fixtures: `/tmp/cycle7-audit4/`.
Line numbers are against HEAD `6c047a0` (they moved since cycle 6: `authorized_workspace_review`
is now main.rs:3180, not 3031).

All three backlog items were re-reproduced live on this binary before designing. Two new
facts turned up that change the shape of the fixes:

* **`kitsu review` does not "verify with no grant at all"** (cycle-6 audit 4 §A.1 got the
  mechanism wrong, not the conclusion). `run_review` *fabricates* `--allow-path <repo root>`
  (main.rs:396-397) and hands it to `run_chat` as a normal invocation read root. It is inert only
  because `forced_one_shot` suppresses read-tool registration (main.rs:7796-7797). So today the
  two entry points are: chat `/diff` = *demand* model read authority; `kitsu review` = *silently
  self-grant* model read authority. Both are wrong; the fix removes both.
* **`/checkpoint <id>` already prints the same host-owned diff bytes with no permission check at
  all** (main.rs:10174-10211). `/diff checkpoint <id>` per-change `check_read`
  (main.rs:3209) is therefore already inconsistent with a sibling command over identical data.

---

## Item 1 — Decouple host-owned `/diff` and `/review` from model read authority

### 1.1 Live reproduction (this binary)

```
cd /tmp/cycle7-audit4/repo                     # git repo, 1 commit, dirty src/lib.rs
printf '/diff\n/permissions\n/quit\n' > /tmp/cycle7-audit4/diff.txt
kitsu --gateway ollama --model qwen2.5-coder:latest chat --no-session \
      --script /tmp/cycle7-audit4/diff.txt
→ cannot show diff: read permission required for /tmp/cycle7-audit4/repo: policy denied:
  read denied: '…' is outside the allowed read root(s) (none granted) — grant it with
  --allow-path <dir> (or /allow <dir> mid-session)
  /permissions → read roots none         exit 1
```

and from the same directory, with *zero* user grants:

```
kitsu --gateway ollama --model qwen2.5-coder:latest --timeout-secs 8 review
→ ~ verified target working_tree            (evidence gathered; only the model call timed out)
```

### 1.2 Current code path

| Concern | Location |
|---|---|
| Single choke point | `authorized_workspace_review` — crates/kitsu/src/main.rs:3180-3257 |
| Root gate (the bug) | main.rs:3244-3247 `perms.check_read(&root)` |
| Checkpoint per-change gate | main.rs:3209 `perms.check_read(&change.path)` |
| Private-path exclusions | main.rs:3186 `private_workspace_paths` (defined 3100-3108), passed to `workspace::review_evidence_excluding` |
| Callers | main.rs:8340-8367 (`kitsu review`), 10246-10279 (`/diff`), 10281-10310 (`/review`) |
| Self-grant to be deleted | main.rs:388-417 `run_review` (roots built at 396-397) |
| Evidence engine (host `git`) | crates/kitsu/src/workspace.rs:67-99 (`git`/`git_owned`), 665+ `review_evidence_excluding`, 616-653 `checkpoint_evidence` |
| Scope resolution | workspace.rs:102-110 `repository_root`, 132-161 `pathspec_from_directory` |
| `/permissions` renderer | main.rs:3603-3789 `format_permissions`; payload main.rs:3817-3899 |

Everything below `authorized_workspace_review` is already host-owned: `workspace.rs` shells out to
`git` in the Kitsu process, applies private exclusions, caps size, and (per its own tests at
workspace.rs:952/996/1037/1219/1234) refuses to disclose symlink targets, hard-link aliases of
private files, or Kitsu runtime state. `check_read` contributes **no** confidentiality property
that `review_evidence_excluding` does not already enforce — it only couples the host's evidence
gathering to the model's standing authority.

### 1.3 Proposed change

**Replace the model-permission gate with an explicit host-evidence authorization** that is
structurally incapable of widening model roots. This mirrors an existing, reviewed precedent in
the permission crate: `TrustedImplicitReadRoot` /
`Permissions::authorize_trusted_implicit_read_root` (crates/kitsu-tools/src/permissions.rs:218-231,
1218-1278) — "Host-only authorization … intentionally separate from model-visible read roots",
used today for project context and skills. We do **not** need a new `kitsu-tools` API (the diff is
produced by `git`, not by Kitsu opening files), so the security-critical crate stays untouched.

New type in `crates/kitsu/src/main.rs` (or a small `evidence.rs`):

```rust
/// Host-owned authority to gather verified workspace evidence. Never consults,
/// creates, or widens a model read root. Its scope is fixed by the directory the
/// user launched Kitsu in; a path argument can only narrow it.
struct WorkspaceEvidenceAuthority {
    root: PathBuf,            // canonical repository root of session.directory
    private: Vec<PathBuf>,    // private_workspace_paths(session)
}

impl WorkspaceEvidenceAuthority {
    fn for_session(session: &SessionInfo, perms: &Permissions) -> Result<Self, String>;
}
```

`for_session` rules (all host-side, no `Permissions` mutation):

1. `root = canonicalize(workspace::repository_root(&session.directory)?)` — unchanged; the user
   cannot name a root, only the launch directory determines it.
2. **Deny if the root itself is Kitsu-private:** `perms.is_within_protected_read_root(&root)`
   (permissions.rs:757) ⇒ `Err("refusing to gather evidence from inside Kitsu-private state")`.
   Today this case only prints a warning (main.rs:3253-3255) and proceeds — tighten it.
3. `private = private_workspace_paths(session)`; keep the existing "~ active Kitsu config/private
   state omitted from workspace evidence" notice for the *contained* case.
4. No prompt, in any mode. `Mode::Ask` must not raise a filesystem prompt for `/diff`
   (training users to widen model authority for a host-only read is the consent anti-pattern
   this item exists to kill).

`authorized_workspace_review` then becomes: build the authority, resolve the pathspec against
`root`, call `workspace::review_evidence_excluding(...)`. Delete both `check_read` calls.

Checkpoint scope specifically: the per-change `check_read` at main.rs:3209 can be deleted outright
because `checkpoint_evidence` (workspace.rs:616-653) never re-reads the filesystem — it replays
`JournalTurnReport.unified_diff` bytes that Kitsu itself captured at write time under write
authority. Keep `checkpoint_change_included`'s private-path filter (workspace.rs:589-614).

`run_review` (main.rs:388-417): delete the fabricated `allow_paths`; pass `&[]`. The evidence path
no longer needs it, and this removes an undisclosed invocation read root from every
`kitsu review` run.

### 1.4 The `/diff` vs `/review` asymmetry (the one real security question)

`/diff` renders host bytes to the user's terminal only. `/review` and `kitsu review` put those
bytes **into the model prompt** (main.rs:3172-3178 `verified_review_prompt`). The two must not be
treated identically:

* `/diff` — pure host evidence. No authority, no prompt, no disclosure beyond the existing
  `~ verified target …` line. Symmetric with `/checkpoint <id>`, which already does exactly this.
* `/review`, `kitsu review` — a **user-initiated, bounded, one-shot disclosure** of exactly the
  scope the user typed. Still no model read root (a read root would grant the model standing
  authority to read *anything* under it on later turns — strictly broader than showing it one
  diff). Instead, disclose at the moment it happens:

  ```
  ~ verified target working_tree
  ~ disclosing 4,812 bytes of host-verified evidence to qwen2.5-coder:latest; no read root granted
  ```

  and in JSONL, on the existing `/review` submission path and the `kind:"diff"` payload
  (main.rs:10266-10273), add `"model_disclosed": true|false` and `"evidence_bytes": <n>`.

Note the deliberate difference from `@file` attachments (crates/kitsu/src/attachments.rs:441-465),
which *do* consume model read authority via `perms.authorize_read_file`. That stays: `@file`
discloses arbitrary user-named file content, and its grant is the mechanism that bounds *which*
files can be named. `/review`'s scope is not user-nameable beyond narrowing inside the repository
Kitsu was launched in, and its content is `git`-verified rather than raw file bytes. Document this
distinction in the README so the model of consent stays legible.

### 1.5 Edge cases

| Case | Required behavior |
|---|---|
| Launched in a subdirectory of a large repo | Evidence covers the whole repo root (unchanged from `kitsu review` today). `/permissions` must *say so* (§1.6) — this is the only real authority expansion in the change, and it is host-only. |
| Not inside a git worktree | Unchanged error from workspace.rs:106 (`not inside a Git worktree`). |
| `/diff ../outside-the-repo` | Unchanged: `pathspec_from_directory` (workspace.rs:148-154) rejects with "path … is outside repository …"; `lexical_absolute` (112-128) rejects `..` escapes before any I/O. |
| Repo root inside `~/.kitsu` (or any protected root) | New hard denial (rule 2). Add a test. |
| Kitsu private state *inside* the repo (offload dir, sidecars) | Existing exclusion + notice, unchanged; regression-covered by workspace.rs:1219 / main.rs private-path tests. |
| Untracked symlink / hard-linked private file | Unchanged; workspace.rs:996/1037/1234 already cover it. |
| `--dangerously-allow-all-filesystem` | No interaction: evidence authority is independent of `Mode`. Do not let `AutoAllow` widen the evidence root beyond the repository. |
| Background agents | Unchanged: `/diff` and `/review` are foreground slash commands; agents never reach `authorized_workspace_review`. |
| Checkpoint change written outside the repo root (a write root elsewhere) | Included, as today, because it is Kitsu's own verified write record. Worth one line in the `/permissions` host-evidence section. |

### 1.6 `/permissions` disclosure (required — this is what makes the change honest)

Insert a section between "internal reads" and "protected writes" in `format_permissions`
(main.rs:3686-3695):

```
  host evidence
    - /home/you/project (git worktree; /diff, /review, kitsu review)
      host-only: read by Kitsu's own `git`, never a model read root, never a model tool target
      excludes ~/.kitsu (Kitsu-private) and Kitsu runtime artifacts
      write checkpoints are replayed from Kitsu's own verified snapshots
```

When the session is not inside a worktree: `host evidence  none (not inside a Git worktree)`.

`permissions_payload` (main.rs:3817-3899) gains:

```json
"host_evidence": {
  "repository_root": "/home/you/project",
  "consumers": ["diff", "review", "checkpoint"],
  "model_read_root": false,
  "prompt_disclosure": "explicit_user_command_only",
  "excluded_private_paths": ["/home/you/.kitsu"]
}
```

`docs/schemas/kitsu-chat-v1.schema.json` needs no change (`command_result.result` only requires
`kind`, schema line 461-464), but `docs/jsonl-protocol.md` should mention the new key.

### 1.7 Test plan

New/changed tests in `crates/kitsu/tests/cli.rs`:

1. `test_diff_needs_no_model_read_authority` — dirty repo, script `/diff\n/permissions\n/quit\n`,
   **no** `--allow-path`. Assert: exit 0; stderr contains `-before`/`+after`; stderr contains
   `read roots   none`; stderr contains `host evidence` and the repo path; stderr does **not**
   contain `read permission required`.
2. `test_diff_authority_does_not_make_files_model_readable` — same session, add a turn whose mocked
   model response calls `fs_read_file` on a repo file. Assert the tool call is policy-denied and
   the run still reports no read roots. (Extends the existing wiremock pattern at cli.rs:3333.)
3. **Rewrite** `test_diff_from_subdirectory_requires_root_permission_and_resolves_filter`
   (cli.rs:3262-3329) → `test_diff_from_subdirectory_resolves_filter_without_grants`: the "denied"
   half now succeeds. Keep the pathspec assertions (`crates/widget/src/lib.rs`, `+after`).
   **This is the one existing test the change invalidates.**
4. `test_diff_jsonl_emits_no_grant_events` — `--jsonl` scripted `/diff`: exactly one
   `command_result` with `result.kind == "diff"` carrying the diff text, `model_disclosed: false`,
   and no permission/grant event anywhere in the stream.
5. `test_diff_interactive_does_not_prompt_for_filesystem_grant` — `Mode::Ask` (or a `Prompter`
   unit test on the new authority): a `CountPrompts`-style prompter records **0** prompts.
6. `test_review_discloses_evidence_without_granting_read_root` — top-level `kitsu review` against a
   mock provider: stderr has the disclosure line; the request body contains the diff; no read root
   is reported; `fs_*` tools are absent from the advertised tool list (already asserted by
   cli.rs:3434 `test_top_level_review_is_verified_tool_free_and_jsonl_framed` — extend it).
7. `test_evidence_root_inside_private_state_is_refused` — init a git repo under the temp HOME's
   `.kitsu/`; `/diff` must fail closed with the new message.
8. Keep every `crates/kitsu/src/workspace.rs` private-exclusion test green unchanged — they are the
   real confidentiality boundary now and must not be touched by this change.

### 1.8 Blast radius

Small and well-contained. `crates/kitsu-tools` is **not** modified (no new permission API).
Estimated: ~120-150 lines in `crates/kitsu/src/main.rs` (new authority type, two deleted
`check_read` calls, `run_review` grant removal, `/permissions` section + payload), 1 test rewritten,
6-7 tests added, README two spots (the `/diff` and `review` rows plus one paragraph in
"Tools in chat"), `docs/jsonl-protocol.md` one line. No on-disk format change, no session-snapshot
change, no CLI-flag change. Risk concentrates in one place: if `for_session` ever accepted a
caller-supplied root it would become an arbitrary-read primitive — keep the root derived solely
from `session.directory` and assert that in review.

---

## Item 2 — Explicit cwd for `--allow-command` pregrants

### 2.1 Live reproduction (this binary)

```
cd /tmp/cycle7-audit4/launch
kitsu … chat --no-session --command-root /tmp/cycle7-audit4/repo --allow-command '["pwd"]' …
→ ! error: invalid --allow-command: policy denied: command cwd '/tmp/cycle7-audit4/launch'
    is outside the approved command roots                                        exit 1
```

Fails closed at startup, before any turn. The bare form works only when the launch directory is
itself inside a command root (verified: launching in `/tmp/cycle7-audit4/launch` with no
`--command-root` yields `[1] "pwd" (cwd /tmp/cycle7-audit4/launch; invocation/base)` in
`/permissions`).

### 2.2 Current code path

| Concern | Location |
|---|---|
| Flag definition + help | crates/kitsu/src/cli.rs:283-287 (`--allow-command <JSON_ARGV>`) |
| Parse | main.rs:7809-7818 → `Vec<Vec<String>>` |
| Root selection | main.rs:7819-7827 (launch dir when no `--command-root`) |
| Grant | main.rs:7842-7846 `policy.grant_command(argv, &launch_dir)` |
| Re-grant on `/new` and `/resume` | main.rs:2952-2969 `reset_command_session`, called at 9076-9080 and 9203 with `&session.directory` |
| Retained for provenance | `SessionInfo.invocation_command_grants` main.rs:2770, populated 8277 |
| Policy side | crates/kitsu-tools/src/builtins/command.rs:1373-1382 `grant_command`, 1443-1467 `resolve_cwd` (error text at 1461-1464), 1535-1549 `insert_grant`, 544-561 `CommandGrantInfo` |
| `/permissions` render / payload | main.rs:3715-3742 / 3799-3815 + 3844-3862 |
| README row | README.md:505 |

### 2.3 Proposed change

Accept **either** JSON shape for `--allow-command`; keep the array form byte-compatible.

```
--allow-command '["cargo","test"]'                              # legacy: launch cwd
--allow-command '{"argv":["cargo","test"],"cwd":"/tmp/proj"}'   # new: explicit cwd
```

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum PregrantedCommandSpec {
    Argv(Vec<String>),
    Explicit { argv: Vec<String>, cwd: PathBuf },
}

#[derive(Debug, Clone)]
struct PregrantedCommand { argv: Vec<String>, cwd: PathBuf, cwd_source: CwdSource }
enum CwdSource { LaunchDirectory, Explicit }
```

Wiring:

* main.rs:7809-7818 — parse into `Vec<PregrantedCommandSpec>`. Resolve each against the launch dir
  *after* the command roots are known (they are computed at 7823-7827), producing
  `Vec<PregrantedCommand>`.
* main.rs:7842-7846 — `policy.grant_command(&spec.argv, &spec.cwd)`.
* main.rs:2952-2969 — `reset_command_session(policy, &[PregrantedCommand], /* no cwd arg */)`;
  re-grant each with **its own recorded cwd**. This is a required fix, not a nicety: today `/new`
  in a session with an explicit-cwd grant would silently re-grant it against the launch directory.
  Update both call sites (9076-9080, 9203) and the unit test at main.rs:12466.
* `kitsu-tools` needs **no change**: `grant_command`/`resolve_cwd` already take an arbitrary cwd and
  already enforce canonicalize → is-dir → inside-a-command-root → executable-fingerprint.

Error and help text:

* cli.rs:283-287 doc comment → "Pre-approve one exact argv, as a JSON array (launch directory) or
  `{\"argv\":[…],\"cwd\":\"…\"}` for an explicit working directory inside a `--command-root`."
* main.rs:7845 — when the *legacy* form fails because the launch dir is outside the roots, the
  message must name the new syntax:
  `invalid --allow-command ["pwd"]: the launch directory /tmp/x is outside the approved command roots (/tmp/proj); pass {"argv":["pwd"],"cwd":"/tmp/proj"} to bind the grant to a root explicitly`
* Keep `deny_unknown_fields` so `{"argv":[…],"cd":"…"}` is a startup error, not a silent
  launch-cwd grant. Reject an empty `argv` and a relative `cwd` with a named error (a relative cwd
  would resolve against the process cwd and reintroduce exactly the ambiguity being removed).

### 2.4 Security / permission-model implications

Deny-by-default is unchanged and arguably strengthened:

* An explicit cwd cannot escape containment — `resolve_cwd` (command.rs:1456-1465) still requires
  the canonical cwd to be under a `--command-root`, and roots are descriptor-bound
  (`BoundCommandRoot`, command.rs:581-637) and revalidated before every execution.
* The grant remains **exact argv + exact cwd** (`insert_grant` keys on both, command.rs:1537-1540)
  plus the executable-identity binding (`CommandGrantedExecutable`), so this widens nothing:
  `{"argv":["pwd"],"cwd":"/tmp/proj"}` must **not** authorize `pwd` in `/tmp/proj/sub`.
* Startup still fails closed on a cwd outside the roots; the change only makes the *satisfiable*
  configuration expressible without a `cd`.
* Mandatory Linux Bubblewrap preflight is untouched and still runs before any execution.
* Do **not** add an `/allow-command` slash command in the same pass: a runtime grant path for exact
  argv currently exists only through the interactive approval prompt (which binds executable
  identity at approval time). Keeping the pregrant flag-only preserves that property.

### 2.5 Provenance display and JSONL disclosure

`/permissions` (main.rs:3730-3735) — annotate the cwd origin:

```
    exact session grants
      [1] "cargo" "test" (cwd ~/proj; invocation/base, explicit cwd)
      [2] "pwd" (cwd ~/proj; invocation/base, launch directory)
```

`command_grant_payload` (main.rs:3799-3815) gains `"cwd_source": "explicit" | "launch_directory"`
alongside the existing `provenance`/`fingerprint_status`. Revoked-invocation entries
(main.rs:3857-3862) carry it too, so a `/revoke-command` → `/new` round trip is inspectable.

The `SessionInfo.invocation_command_grants` comparison used to classify provenance
(main.rs:3725, 3849) is `Vec<CommandGrantInfo>` equality on `argv`+`cwd`+`executable`; keep the new
field out of `CommandGrantInfo` (it is a CLI-layer fact, not a policy fact) and carry it in a
parallel map keyed by `(argv, cwd)` to avoid perturbing that equality — or the revoked-grant
diffing at 3743-3747/3857-3862 will silently change behavior.

### 2.6 Edge cases

| Case | Required behavior |
|---|---|
| Object form, no `--command-root` | Roots default to the launch dir (main.rs:7823-7824), so an explicit cwd elsewhere fails closed with the roots named. Correct, but the error must list the effective roots. |
| Explicit cwd == launch cwd | Identical grant; `cwd_source` still reports `explicit` (what the user wrote). |
| Two specs, same argv, different cwd | Two distinct grants (`insert_grant` keys on both). `/permissions` numbers them separately; `/revoke-command <n>` removes one. |
| `cwd` is a symlink into a root | `resolve_cwd` canonicalizes first, so the grant records the real path; a later symlink swap cannot redirect it (`AuthorizedCwd` is descriptor-bound). |
| `cwd` does not exist / is a file | Existing typed errors from command.rs:1444-1455, surfaced at startup. |
| Executable not resolvable in the pregrant cwd | Existing `unresolved_executable` failure at command.rs:1377-1379 — startup error, unchanged. |
| `/new` and `/resume` | Re-grant with the recorded cwd (§2.3). Add a regression test. |
| Resumed session | Unchanged: snapshots never restore grants; invocation pregrants are re-applied from the current argv. |

### 2.7 Test plan

1. `test_allow_command_object_form_binds_explicit_cwd` — launch in `/tmp/launch`, run with
   `--command-root /tmp/proj --allow-command '{"argv":["pwd"],"cwd":"/tmp/proj"}'` and a mocked
   model turn that calls the command tool with cwd `/tmp/proj`. Assert: startup succeeds, the
   command runs unprompted, output is `/tmp/proj`.
2. `test_allow_command_grant_is_exact_cwd` — same session, the model requests `["pwd"]` in
   `/tmp/launch` → policy denied; and in `/tmp/proj/sub` → policy denied.
3. `test_allow_command_legacy_array_still_means_launch_cwd` — bare `["pwd"]` with no
   `--command-root`: `/permissions` shows `cwd <launch>` and `launch directory`.
4. `test_allow_command_launch_cwd_outside_roots_names_object_syntax` — the §2.1 repro; assert
   exit 1 and that stderr contains `"cwd"` and the effective root path.
5. `test_allow_command_rejects_unknown_fields_and_relative_cwd` — `{"argv":["pwd"],"cd":"/x"}` and
   `{"argv":["pwd"],"cwd":"proj"}` both exit 1 with a named error.
6. `test_new_session_restores_explicit_cwd_pregrants` — `/new` then re-run the command; assert it
   still runs unprompted from the explicit cwd (unit test around `reset_command_session`,
   extending main.rs:12457-12466).
7. `test_permissions_payload_reports_cwd_source` — `--jsonl` `/permissions`:
   `commands.exact_grants[0].cwd_source == "explicit"`.

### 2.8 Blast radius

Smallest of the three. ~60-90 lines in `crates/kitsu/src/main.rs`, ~5 lines in `cli.rs`, zero lines
in `kitsu-tools`. One signature change (`reset_command_session`) with two call sites plus one unit
test. README row 505 and the `--allow-command` help string. No JSONL breaking change (additive
key). The only sharp edge is the provenance-equality note in §2.5.

---

## Item 3 — Cross-run artifact retention / GC

### 3.1 Live verification (this session, this binary)

Orphaned generations exist across every profile on this machine, from earlier cycles *and* from
runs made during this audit:

```
~/.kitsu/profiles/v1-fnv128-4817…/artifacts/  4 × runtime-*  (2 hold command-output-{0,1}.txt, 1550 B)
~/.kitsu/profiles/v1-fnv128-9a19…/artifacts/  4 × runtime-*  (2 hold command-output-0.txt,     918 B)
~/.kitsu/profiles/v1-fnv128-88f5…/artifacts/  2 × runtime-*  (empty)
~/.kitsu/artifacts/                           2 × runtime-*  ← created by THIS audit
```

A live `--enable-commands` run during this audit wrote
`~/.kitsu/artifacts/runtime-2337193-1786433655908263182-0/command-output-0.txt` (459 B) containing
the full captured stdout of the executed command:

```json
{"event":"started","format":"kitsu.command-output.v1","limit_bytes":16777216}
{"bytes":24,"data":"/tmp/cycle7-audit4/repo\n","encoding":"utf8","event":"chunk","stream":"stdout"}
{"artifact_bytes":459,"captured_bytes":24,…,"success":true,"termination":"exited"}
```

The dir survived process exit; a later `/artifacts` cannot see it; there is no `kitsu artifacts`
subcommand (top level is init/doctor/ask/review/models/sessions/chat). That run was subsequently
killed by an external `SIGTERM` (exit 143) while the model was still generating — the populated
generation directory survived abnormal termination too, so GC cannot assume a clean shutdown path.
This is also why the liveness test in §3.5 must be `flock`, which the kernel releases on process
death (including `SIGKILL`), rather than a marker file that a killed process never removes.

**New finding not in the cycle-6 filing — orphaned session artifact sidecars.** `sessions/` also
accumulates `s-*.artifacts` directories with no matching `s-*.json`:

```
11 of 13 *.artifacts sidecars under ~/.kitsu/profiles/*/sessions/ are orphans (0 bytes, 0 files)
```

These come from sessions that never committed a snapshot (a chat that started and exited before its
first successful turn). Empty today, but the same sidecar is where a durable session's command
output and offloaded tool results land (main.rs:2912-2914 → `SessionStore::artifacts_dir`,
session.rs:1903), so a durable run that dies mid-turn leaves *populated* orphans. Any GC design that
only sweeps `artifacts/runtime-*` misses half the problem.

### 3.2 Why they leak (code)

| Concern | Location |
|---|---|
| Generation naming | main.rs:2138 `runtime-{jsonl_run_id()}-{generation}`, where `jsonl_run_id()` = `"{pid}-{unix_nanos}"` (main.rs:72-81) |
| Allocator | main.rs:2106-2171 `ArtifactDirectoryAllocator` (0700 dirs) |
| Namespace choice | main.rs:8019-8023 — commands ⇒ `state_dir.join("artifacts")`; otherwise a `/tmp` namespace |
| Cleanup, and its hole | main.rs:2093-2100 `OffloadDirGuard`, installed only when `session_store.is_none() && command_policy.is_none()` (main.rs:8044-8048). **Commands enabled ⇒ never cleaned up.** |
| Durable-session base | main.rs:2903-2916 `artifact_directory_for_boundary` → session sidecar |
| In-session view (per-process only) | crates/kitsu/src/command_artifact.rs:138-220 (`CommandArtifactSink::{usage,list,prune,prune_all}`); slash handler main.rs:10422-10427; render/payload main.rs:5913-5990 |
| Sidecar lifecycle | session.rs:27 `ARTIFACTS_SUFFIX`, 820, 1903, 2146 `archive`, 2356 `delete` |
| State namespace resolution | main.rs:4142-4164 `runtime_state_dir_read_only`, 4166-4205 `runtime_state_dir` (note: the **default** config maps to `~/.kitsu` itself, not `~/.kitsu/profiles/*`) |

### 3.3 Recommendation: `kitsu artifacts list|prune` (primary), plus a *narrow* startup sweep

**Recommended: the top-level provider-free subcommand**, modelled exactly on `kitsu sessions`
(main.rs:662-786; `reject_jsonl_for_management` at 438; `runtime_state_dir_read_only` at 4142).
Reasons it fits kitsu's grain better than startup GC:

* It is **provider-free and offline** — no gateway, no config parse required for list (the
  `sessions` precedent explicitly supports a non-parsing config), so a user can audit disk state
  without touching a model.
* Deletion stays **explicit and inspectable**, matching every other destructive surface in Kitsu
  (`/artifacts prune`, `/jobs prune`, `sessions delete` all require an exact target and print what
  they removed). Silent startup deletion of files that may hold the only copy of a command's output
  contradicts that posture and is unauditable.
* It gives cross-run **listing**, which startup GC alone never provides — and the cycle-5/6 filing
  is as much "no listing" as "no pruning".
* It composes with automation (`--json`), which a startup side effect cannot.

**But** pure opt-in leaves litter forever, so pair it with two bounded automatic behaviors that
require no consent because they destroy nothing of value:

1. **Startup sweep of provably empty orphans only.** At `prepare_runtime_state_dir` time
   (main.rs:4207) — when the state dir is being prepared anyway — remove `runtime-*` generation
   dirs and `*.artifacts` sidecars that are (a) not locked by a live process, (b) contain zero
   files, and (c) for sidecars, have no matching `<id>.json`. On this machine that is 11 of 13
   sidecars and 4 of 10 generations. Zero disclosure risk, no output needed beyond `/status`.
2. **A one-line startup notice, not a deletion**, when non-empty orphans exceed a threshold
   (default: >16 generations or >64 MiB):
   `~ 23 retained command-artifact generations (18.4 MiB) from earlier runs; inspect with 'kitsu artifacts list', remove with 'kitsu artifacts prune --older-than 7d'`

### 3.4 Command surface

```
kitsu artifacts [list]                      # default; --json for automation
kitsu artifacts prune <id|all> [--older-than <dur>] [--dry-run] [--json]
```

`list` output (grouped by namespace, oldest first):

```
command artifacts for config ~/.kitsu/config.toml
  namespace runtime (stateless command runs)
    runtime-2337193-1786433655908263182-0   1 file    459 B   2h ago   orphaned
    runtime-2336956-1786433636907534838-0   0 files     0 B   2h ago   orphaned (empty)
  namespace sessions
    s-1786424829314290985-2215983-0         3 files  12.4 KiB  1d ago   session exists
    s-1786425187376539099-2219179-0         0 files     0 B    1d ago   orphaned (no session)
  total 4 generations, 12.9 KiB; 3 prunable, 1 in use by a live process
prune with: kitsu artifacts prune all | kitsu artifacts prune --older-than 7d
```

`prune` prints exactly what was reclaimed (`removed 3 generation(s), 12.9 KiB`), skips live ones
with a named reason, and exits nonzero if the selector matched nothing (consistent with
`sessions delete`).

Retention rules (defaults, all overridable by flag, none applied automatically):
`--older-than 7d` for age; the existing per-sink quota (32 slots / 256 MiB,
command_artifact.rs:23-25) stays the *in-process* bound and is unchanged.

### 3.5 Safety — never touch active session state

This is the crux; get it wrong and a `kitsu artifacts prune` in one terminal corrupts a chat in
another.

* **Liveness by lock, not by pid.** The generation name embeds the pid (main.rs:72-81), but pids are
  reused — do not test `kill -0`. Instead, the owning process creates and `flock(LOCK_EX|LOCK_NB)`s
  a `.lock` file inside each generation directory it allocates (main.rs:2132-2165) and holds it for
  the directory's lifetime. GC/prune acquires `LOCK_EX|LOCK_NB` before removal and skips on
  `EWOULDBLOCK`. Reuse the exact mechanism already proven for sessions:
  `SessionLease` + `PrivateDirectory` (session.rs:115-136, 1147-1215) — they need to become
  `pub(crate)` or move to a shared module.
* **Pre-upgrade generations have no lock file.** Treat "no `.lock`" as prunable only when the
  directory's mtime is older than a grace period (default 1h) — long enough that a live
  pre-upgrade process is implausible, and the risk is bounded to deleting an output file the
  in-process sink may still list.
* **Never touch a session's `.artifacts` sidecar while its session is loadable and its lock is
  held.** Match the existing session lock at `sessions/.locks/<id>.lock` (session.rs:1147-1149)
  before pruning any `<id>.artifacts`.
* **Structural safety:** operate through descriptor-pinned directory handles (`PrivateDirectory`),
  refuse symlinks (`symlink_metadata`, as main.rs:2119-2130 already does), and only remove entries
  whose names match the exact `runtime-<pid>-<nanos>-<gen>` / `s-…-….artifacts` patterns. Never
  `remove_dir_all` the namespace root, never follow a `..` component, never delete `*.json`,
  `history`, `context_window_cache.json`, or `.locks`.
* **Read-only listing.** `list` must use `runtime_state_dir_read_only` (main.rs:4142) and create
  nothing — merely inspecting artifacts must not materialize a profile directory (the same
  discipline `kitsu sessions` follows).
* **Permission model:** these paths are Kitsu-private (`protect_read_path` at main.rs:7782-7787).
  `kitsu artifacts` is a host command outside any chat, so no model authority is involved and none
  is created; the model never learns these paths exist. Note explicitly in the README that
  `kitsu artifacts` is host-only.

### 3.6 In-session `/artifacts` must stop lying

`/artifacts` in a fresh session reports `0 retained` while gigabytes may sit on disk. Extend
`format_command_artifacts` / `command_artifact_payload` (main.rs:5913-5990) with a second line
sourced from the same on-disk scan:

```
command artifacts: 1 retained / 0 in flight | 459 retained bytes | …
  this session: ~/.kitsu/artifacts/runtime-2337193-…-0
  other runs:   3 generation(s), 2.4 KiB retained on disk — `kitsu artifacts list` (host-only)
```

with `"cross_run": {"generation_count": 3, "bytes": 2468, "namespace": "…"}` in the payload.

### 3.7 Test plan

1. `test_stateless_command_run_retains_generation_after_exit` — run a scripted `--enable-commands`
   chat (mock provider, one command tool call), assert `<state>/artifacts/runtime-*/command-output-0.txt`
   exists after exit. (Locks in today's leak as intended behavior until pruned.)
2. `test_artifacts_list_reports_cross_run_generations` — after (1), `kitsu artifacts list --json`
   lists that generation with id, byte size, file count, age, and `state: "orphaned"`.
3. `test_artifacts_prune_all_reclaims_and_reports` — `kitsu artifacts prune all` removes it, prints
   the reclaimed byte count, and the namespace is empty afterwards.
4. `test_artifacts_prune_skips_live_generation` — hold the generation lock from the test process;
   `prune all` must skip it with a named message and exit nonzero (nothing else matched).
5. `test_artifacts_prune_never_touches_sessions_or_config` — a namespace containing `s-*.json`,
   `history`, `.locks`, and a foreign `not-an-artifact/` directory: all survive `prune all`.
6. `test_artifacts_prune_refuses_symlinked_generation` — a symlink named `runtime-x-y-0` pointing
   outside the namespace is refused, and its target is untouched.
7. `test_startup_sweep_removes_only_empty_orphans` — pre-create one empty orphan generation, one
   empty orphan `.artifacts` sidecar, one *non-empty* orphan, and one sidecar with a live `.json`.
   After a chat start: the two empty orphans are gone; the other two remain; the notice is printed
   only when the threshold is exceeded.
8. `test_artifacts_list_is_read_only` — running `kitsu artifacts list` against a HOME with no
   `.kitsu` creates no directories and prints an empty-state line.
9. `test_slash_artifacts_reports_cross_run_total` — in-session `/artifacts` payload contains
   `cross_run.generation_count` matching the on-disk state.

### 3.8 Blast radius

Largest of the three, but almost entirely additive. New `Command::Artifacts` in `cli.rs` (~25
lines) + a `run_artifacts` function in main.rs (~200 lines including the scanner and formatter);
lock-file wiring in `ArtifactDirectoryAllocator` (main.rs:2132-2165, ~30 lines); an empty-orphan
sweep at `prepare_runtime_state_dir` (~50 lines); `pub(crate)` exposure of `SessionLease` /
`PrivateDirectory` from session.rs; `/artifacts` payload extension (~30 lines). Touches on-disk
layout only by *adding* a `.lock` file inside generation directories — old generations without one
remain readable and prunable via the grace-period rule. README: one row in the command table plus a
short "Artifacts on disk" paragraph. Highest-risk area is the deletion path; keep it behind the
pattern match + descriptor pinning + lock acquisition, and land tests 5 and 6 first.

---

## Small items from cycle 6 "Also noted" — which are now cheap

Verified against this binary where a live check was possible.

| Item | Status now | Cheap? | Concrete fix |
|---|---|---|---|
| **`review <path>` clean-tree dead end** (F4) | **Still reproduces**: `kitsu review src/lib.rs` on a clean tree → `! error: nothing to review for verified target working_tree` (exit 1). The message does not even echo the path the user typed. | **Yes — trivial.** | Two string sites: main.rs:8348-8353 (top level) and 10294-10300 (`/review`). Include the pathspec in the label (`review_target_label`, main.rs:3146-3158, currently ignores `request.path`) and append: `use --commit <rev> to review its committed content, --staged/--base for other targets, or attach it in chat with @<path>`. ~15 lines + 1 string-assertion test. **Take it.** |
| **`sessions export` format inference** (F5) | Still true by construction: `format` is `#[arg(default_value = "markdown")]` (cli.rs:346) and main.rs:773-777 maps it blindly. | **Yes — trivial.** | Make it `Option<String>`; when absent, infer from the destination extension (`.json` → Json, else Markdown) and print `~ inferred --format json from the .json destination`. An explicit `--format` always wins. ~20 lines in main.rs:767-786 + cli.rs:346, 2 tests. **Take it.** |
| **Preflight budget-number consistency** (F6) | **Improved but not closed.** Live: `kitsu-context: {"used":15000,"effective":7680,"window":8192}` vs the error's `needs about 15000 tokens, but the configured target is 6861 tokens`. `used` now agrees; `target` (6861) vs `effective` (7680) still differ silently — the gap is the trim buffer `min(window/10, 2048)` (context.rs:158-167). `--context-window` is still unnamed in the remedy list. | **Yes — near-trivial.** | Add `effective: u64` to `AgentError::ContextOverflow` (orchestrator.rs:613-623; populated from `ContextBudget` at 1700-1705) and reword: `…needs about {required} tokens; the window is {window}, effective prompt budget {effective}, trim target {target} (effective minus a {buffer} reserve); raise it with --context-window <n>, lower --max-output-tokens, or /compact`. ~25 lines + 1 test. **Take it.** |
| **`last` retargets to a fresh fork** (F7) | Unverified live this window (no saved sessions under the default profile), unchanged in code: `format_session_summaries` (main.rs:3278-3307) has no fork annotation and `last` is purely most-recently-updated. | Yes, if the fork parent is already stored in the snapshot; otherwise it needs a schema field. **Check first.** | One annotation line in `format_session_summaries`. Low value relative to session search — fold in only if the parent id already exists on `SavedSession`. |
| **Session search** | Still true: `sessions list` takes no filter (cli.rs SessionCommand::List), `/sessions` maps to `SlashResult::ListSessions` with `args: ""` (main.rs:2477-2482, 10399). | **Yes — small, not trivial.** The precedent exists: `kitsu models [FILTER]` (main.rs:4635) shipped in cycle 6 with a guided empty state. | Add `FILTER` to `SessionCommand::List` and an arg to the `/sessions` spec; filter case-insensitively over id + preview + gateway/model + directory in `format_session_summaries`; reuse the models empty-state wording. ~40 lines + 2 tests. **Take it if the pass has room** — it is the last unfixed half of backlog #4. |
| **Screen-reader rendering mode** | Still true: `format_message` (crates/kitsu-render/src/lib.rs:306) returns text verbatim when styling is off; styled mode deliberately retains markers. | **No.** | Needs a real Markdown structure walker (heading verbalization, list/code-boundary announcements, marker stripping) and an a11y design pass. Keep backlogged; do not absorb into a fix pass. |

Cheap-win bundle for the fix pass: **F4 + F5 + F6** (~60 lines, 4 tests, all pure string/argument
work with no permission-model surface), optionally **session search**.

---

## Recommended implementation order

1. **Item 2** (`--allow-command` cwd) — smallest, zero `kitsu-tools` change, unblocks non-interactive
   command automation, and its `reset_command_session` fix is a latent correctness bug today.
2. **Item 1** (`/diff` + `/review` decoupling) — highest daily-UX value; medium and contained;
   requires the `/permissions` disclosure and one existing test rewrite. Do it after item 2 so the
   `/permissions` renderer is touched once, for both the command-grant provenance and the new
   host-evidence section.
3. **Cheap wins F4 + F5 + F6** — ~60 lines total, no shared surface with 1 or 2, safe to land any
   time; good filler while item 3's tests are being written.
4. **Item 3** (artifact retention) — largest and the only one with a deletion path; land its safety
   tests (prune-never-touches-sessions, symlink refusal, live-lock skip) before the sweep.
5. **Session search** — only if the pass still has room; closes backlog #4.

---

## Fixture inventory

* `/tmp/cycle7-audit4/repo` — git repo, one commit, dirty `src/lib.rs` (used for `/diff`, `review`,
  clean-tree and command-artifact probes).
* `/tmp/cycle7-audit4/launch` — launch directory outside the command root (item 2 repro).
* `/tmp/cycle7-audit4/{diff.txt,p.txt,cmd.txt,huge.txt}` — chat scripts and the 12k-word preflight
  prompt.
* Live artifact evidence: `~/.kitsu/artifacts/runtime-2337193-1786433655908263182-0/command-output-0.txt`
  (459 B, written and orphaned during this audit) plus the pre-existing orphans enumerated in §3.1.
* No repository files were modified; every chat run used `--no-session`.
