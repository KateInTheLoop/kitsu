# Codex/Claude UX cycle 17: managed gateway authentication

## Challenge

Run three seeded, permission-prompt-free comparisons at UTC boundary
`1786689000` (cycle seed `992605`), verify Cycle 16's incomplete-response
handling, measure Hugging Face authentication parity, and exercise a bounded
multi-file observation task before implementing one product improvement.

The continuous-development profile allowed repository and private-fixture
reads/writes, local builds/tests/models, and agent orchestration without
approval dialogs. Each scored Kitsu run still received only its declared roots,
commands, and tools. No run received arbitrary shell, unrestricted host
filesystem, credential, or new-network authority. This keeps the loop
prompt-free without weakening the workspace boundary described in the
[official OpenAI approval and sandbox documentation](https://learn.chatgpt.com/docs/agent-approvals-security).

The scored pre-fix binary was SHA-256
`1d7cb339ecb42f82eab061355609ff9ff9e7e2ac1b482af439eca34d11ac06dd`.
Every scored launch passed an immediate hash gate, ran once, used a hard outer
bound, and was reaped without a permission request or fallback rerun.

## Seeded comparison

| Task | Kitsu | Comparator | Attribution |
|---|---:|---:|---|
| Complete a tool-effect turn whose terminal model response is whitespace | 98/100 | deterministic Codex-style reducer | Kitsu correctly retained the completed file effect while discarding and rolling back the incomplete conversational turn. The remaining deduction is a non-null final session ID for a fresh session that was never persisted. |
| Inspect and compare Hugging Face authentication lifecycle support | 63/100 | Claude 91/100; standalone Codex not run | Kitsu had truthful, secret-safe missing-credential diagnostics but no login/status/logout lifecycle. Claude exposed all three operations. The standalone Codex executable was absent; official Codex documentation describes the same lifecycle and durable credential storage. |
| Find three matching records across scoped read-only shards | 90/100 | Codex 93/100 provisional | Kitsu safely listed and read all shards, then hit its enforced turn deadline before producing the answer. Only file read/list schemas were offered, whereas the comparator recovered to a text search and returned the exact oracle. |

The incomplete-terminal fixture digest was
`a46b4a5ec11550ba41bcb501d13e9dfe4c964a3625cc368d70670e390a060ba6`.
Its one scored Kitsu process started at `1786689000.000449`, exited `2` in
14.8 ms, and emitted 21 valid `kitsu.chat.v1` events. A malformed write failed,
a corrected write created the exact 24-byte effect, and the provider then
returned `" \n\t"`. Kitsu discarded the assistant, emitted
`error.kind="incomplete_turn"`, retained and reported the completed filesystem
change, counted one raw and zero recovered tool errors, and created no durable
snapshot. All fixture and sentinel hashes remained correct. Evidence is
retained at
`/tmp/kitsu-parity/992605/blank_terminal/evidence/summary.json`.

That run also exposed a separate receipt inconsistency: `final_summary` named a
planned session ID even though `sessions --json` was empty. The run-start event
correctly said `pending_until_first_success`, but terminal automation cannot
tell that the ID never became resumable without issuing a second query.

The authentication fixture digest was
`18df1550aac8f2d2a1138dbc5c4893da855544fc16c0fbf949e86f07d8b62693`.
`HF_TOKEN` was absent from the process and the user service manager was
unavailable, so the one scored probe stopped locally with
`credential_present=false` and `request_sent=false`; no Hugging Face request or
credential disclosure occurred. Kitsu exposed no `auth` command. Claude
2.1.232 exposed login, status, and logout, and its secret-free projected status
reported an active login. A standalone `codex` executable was unavailable; the
[official OpenAI authentication documentation](https://developers.openai.com/codex/auth/)
documents login, status, logout, and durable file/keyring storage. The shipped
configuration was pinned before a later working-tree drift, so the scored
result remained controlled.

The observation-task digest was
`f1168a70648ee1b8a211473ed41aac2ea44baa8b25a9a2ff7ef3438c217ade19`.
Kitsu listed the allowed directory and read all three shards, but the third
provider round reached the real 60-second whole-turn deadline before returning
final text. It exited `2`, discarded the turn, and kept every input and sentinel
unchanged. The embedded comparator recovered from an unavailable `rg` binary
using a bounded text search, found all three oracle records, and made no writes.
Its score is provisional because no standalone Codex CLI was installed. The
product gap is a model-callable, root-confined content-search schema for plain
allowed read roots; forcing complete file reads cost an extra provider round
and consumed the deadline.

## Selected improvement

Kitsu now provides a managed bearer-credential lifecycle:

- `kitsu --gateway <name> auth login` reads from an echo-disabled terminal;
- `auth login --token-stdin` is the explicit non-interactive path, so secrets
  are never accepted in argv;
- `auth status [--json]` is read-only and reports only source, presence, local
  usability, and whether remote validity was checked or a request was sent;
- `auth logout` idempotently removes only Kitsu's managed credential, leaving
  environment and static configuration untouched.

Managed credentials are scoped to the canonical configuration identity and
exact gateway name. Accepted gateway names have collision-free bounded
filenames, so profiles cannot alias one another. Environment authentication
retains precedence: a non-empty configured environment variable overrides the
managed value, an empty variable falls back to it, and malformed non-Unicode
environment data fails closed instead of silently selecting a stored token.

The credential namespace reuses Kitsu's descriptor-relative private-state
primitives. Directories must be owned and mode `0700`; credential files must be
regular, owner-owned, single-link, mode `0600`, and opened without following
symlinks. Writes use a private temporary file, atomic replacement, and directory
synchronization. Reads are bounded, hardlinks and symlinks are rejected, and
credential buffers are zeroized on all owned paths. Login refuses a missing,
empty, or relative home directory instead of falling back to a temporary
location. User-declared command and filesystem roots may not contain or overlap
the protected credential namespace.

Chat, ask, review, model catalog, and doctor now load the managed value through
the same authentication resolver. Login validates that the opaque value can be
encoded as an authorization header but does not contact the provider. Status
likewise never contacts the network. `doctor --inference` remains the explicit
way to validate the selected credential remotely, and its existing
`request_sent` evidence distinguishes local preflight from a provider-reaching
probe.

Help and examples make the security boundary explicit: managed login is
supported only for bearer-environment profiles, the environment wins, status
does not establish remote validity, and a secret is never accepted as a command
line option.

## Verification

New deterministic regressions prove:

- login, status, environment override, atomic rotation, logout, and idempotent
  logout behavior without a remote request;
- the stored credential authenticates an isolated mock-provider request when
  the configured environment variable is absent;
- configuration and gateway isolation, exact environment precedence, and
  malformed-environment fail-closed behavior;
- `0700` directory and `0600` single-link file invariants plus hardlink,
  symlink, and protected-root rejection;
- non-TTY login requires `--token-stdin`, while PTY input is hidden and echo is
  restored after interruption;
- missing-home refusal and truthful behavior for `none` and static
  authentication profiles;
- no credential or private credential path appears in output or diagnostics.

Verification completed without approval prompts:

- complete CLI integration suite: 149 passed;
- complete workspace suite: 715 passed;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- staged and unstaged `git diff --check`;
- fresh debug build and manual `auth --help` discovery check.

The post-fix debug binary is SHA-256
`3f6bb95dfd8bbca67ba7c6c1cee76c4b6900694c20109197f3de1b6f22399cec`.

No real Hugging Face inference was attempted because no user-controlled token
was available. The next live probe can start with managed login and then use
`doctor --inference`; the continuous loop remains fail-closed rather than
prompting for or discovering credentials.

## Next backlog

1. Emit terminal session materialization state, or a null `final_summary`
   session ID for a fresh failed session, while preserving the ID of an existing
   resumable session.
2. Add a root-confined, bounded text-search tool for plain allowed read roots so
   observation tasks do not require whole-file reads and extra provider rounds.
3. Run the three Hugging Face comparison legs after a user-controlled managed
   credential is present; keep token entry outside benchmark transcripts.
4. Add `--no-artifacts` or a strict stateless mode that suppresses private
   command-artifact persistence while retaining truthful bounded inline
   evidence.
5. Label provider-attempt and whole-turn deadline telemetry with explicit scope.
