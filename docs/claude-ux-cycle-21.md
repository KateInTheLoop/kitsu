# Codex/Claude UX cycle 21: atomic committed final output

## Challenge

Run three seeded, permission-prompt-free comparisons at UTC boundary
`1786717800` (cycle seed `992609`), recheck hosted Hugging Face readiness, verify
the new top-level review disclosure, and compare Kitsu's non-interactive final
answer surface with Codex/Claude-style automation.

The scored pre-fix binary was SHA-256
`900777005c54d69c30a597e360c132c77acaf9d0518f2cd31ebcc7635557e88d`.
Every eligible Kitsu launch used a private pin plus a launch-time shared-binary
hash gate, one attempt, least-authority roots, and hard process bounds. No
comparison requested permission expansion, exposed credentials, staged
shared-tree changes, or left a child running.

## Seeded comparison

| Task | Kitsu | Comparator | Attribution |
|---|---:|---:|---|
| Create a seeded manifest and preserve the exact committed final answer as a host-owned artifact | 85/100 | controlled Codex-style runner, 95/100 provisional | Kitsu read and wrote only the granted fixture, returned the exact final text, and truthfully committed the turn. It lacked `-o/--output-last-message`, so automation had to correlate an `assistant` event with its later committed disposition. The manifest omitted one required final LF; that separate strict-oracle miss was model adherence. |
| Recheck missing-auth behavior and a managed local Hugging Face-shaped inference flow | 97/100 local fallback | Claude surface and official Codex contract | Hosted HF remained ineligible because both Boolean credential gates were false; no remote request was sent. The local managed flow failed closed without auth, then authenticated exactly one tool-free request and returned the exact oracle. Its only parity deduction was the same missing final-message file surface. |
| Verify the post-cycle-20 top-level review disclosure on a randomly selected commit | 100/100 | deterministic Codex/Claude-style control, 100/100 provisional | Kitsu emitted one exact pre-provider `review_disclosure`, disclosed the private-filtered 298-byte evidence without a read grant, offered no tools, and committed the exact answer. An initial harness score was corrected without rerunning after inspection found its expected prompt string omitted the product's documented colon. |

The final-output digest was
`8c2a45cf2c9f227d10dd4bf9bb19c49d2b921db82ba832b016c71db6ab5f4cee`.
It selected the create task. Kitsu completed three provider rounds in 68.928
seconds: one 151-byte blueprint read, one manifest write, and one exact 47-byte
assistant answer. All 22 JSONL records parsed, the disposition was committed,
the final summary was `ok`, all counters were zero, and the child was reaped.
The answer SHA-256 was
`614b7c56e9433f8ab9d91d3fc1460fc27f47f9a409d6c50cbc0252ff59e9759b`.
The 125-byte manifest SHA-256 was
`99d9acabbbf5c1cc2a1edccbc57c75a9c84b22f516657dea2674a56ba2807260`;
it differed from the 126-byte oracle only by the requested terminal LF. Kitsu
had no native host-output artifact, while the controlled runner independently
wrote the exact 48-byte answer-plus-LF artifact outside model authority.

The Hugging Face digest was
`552cc4ae61c99e8b713c597ec5e2d5242821c8b4190b8a6c8d027bdd4934932b`.
The process and user-service credential gates were both false, so hosted
inference was recorded as `not_run: infrastructure` and `request_sent=false`.
The local fallback first proved missing auth exits one with a safe `auth login`
recovery and zero requests. Managed auth then sent exactly one authenticated,
tool-free chat request and committed
`FINAL KEY=552cc4ae61c99e8b CODE=0`; logout removed the credential file. No secret,
fingerprint, or store path appeared, and all six children plus the mock were
reaped.

The review digest was
`5c961ef2e8683c282c3f196e0fcf962c26741c88fc15358cd5fc6ec29974bccd`.
It selected commit `b21dd0b542aa82ecb367f4215406fa33087cd812`. The
private-filtered evidence was exactly 298 bytes with SHA-256
`a63b70866b415239260cb328b20f2a0099b6be0d43342e0c5c74f3a57e960b2f`.
The event order was `run_started`, `review_disclosure`, `turn_started`, provider
and assistant frames, committed disposition, `turn_finished`, and
`final_summary`. The receipt and provider prompt agreed exactly; no private,
staged, unstaged, untracked, or outside decoy leaked. The scored process exited
zero in 15.112 ms and was reaped.

## Selected improvement

Kitsu now supports global `-o/--output-last-message <PATH>` for non-interactive
ask, top-level review, piped chat, and scripted chat. It writes the exact bytes
of the last fully committed assistant message without adding a newline or
changing normal stdout/JSONL output. Multi-turn scripts select their last
committed turn; a later failed turn under `--allow-turn-errors` does not erase
that valid candidate. Discarded, blank, cancelled, length-limited, and
`committed_partial` output never qualifies.

Installation is host-owned and atomic. The parent must already exist and is
opened without following symlink components on Unix; an existing destination
must be a regular file. Kitsu creates a same-directory `0600` temporary file,
writes and syncs the exact bytes, performs a descriptor-relative rename, and
syncs the pinned parent. Errors distinguish a pre-install failure (destination
unchanged) from an installed result whose final durability sync could not be
confirmed.

The destination is never granted to filesystem tools. It cannot overlap
Kitsu's protected configuration or state. When command tools are enabled,
startup also rejects a destination visible through a writable command root, a
read-only runtime root, or an existing hard-link alias; this happens before any
provider request. The restriction keeps the machine receipt's
`model_write_authority: false` claim true across both filesystem and command
tool families.

On JSONL success, Kitsu emits one additive `output_last_message` event after the
selected turn's `turn_finished` and before `final_summary`. It contains the
turn ID, requested destination, exact byte count, lowercase SHA-256,
`atomic: true`, `assistant_committed: true`, `content_included: false`, and
`model_write_authority: false`. The assistant body remains only in the ordinary
assistant frame. File-install failures emit a typed `error` and a nonzero
terminal summary without a false success receipt.

## Verification

New regressions prove:

- short `-o` and long `--output-last-message` work across ask, review, piped
  chat, and scripted chat;
- exact bytes, Unix `0600` mode, SHA-256, event order, and one-receipt behavior;
- scripts choose the last commit and retain an earlier commit after a later
  blank turn;
- missing auth, discarded output, and a blank tool-using turn leave an existing
  destination unchanged;
- symlink targets, private-state overlap, filesystem-tool access, writable
  command roots, read-only runtime roots, and hard-link aliases fail closed;
- atomic installation cleans temporary files and truthfully classifies
  pre-install versus post-install durability failures; and
- management and interactive surfaces reject the automation-only flag early.

Verification completed without permission prompts:

- focused atomic writer and CLI regressions: passed;
- ask, review, piped-chat, script, auth, and incomplete-turn regressions: passed;
- published JSON schema regression: passed;
- complete workspace suite: 735 passed;
- `cargo fmt --all -- --check`;
- JSON schema parse validation;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- staged and unstaged `git diff --check`; and
- fresh debug build.

The post-fix debug binary is SHA-256
`e35ca6a6b59f00501ceb2d8d34fd1c91966a42a906eb7ce829600fcfa4c02d3a`.

## Next backlog

1. Add a bounded structured-output schema surface for non-interactive answers,
   complementing the new final-message file and matching useful Codex/Claude
   automation workflows.
2. Run real hosted Hugging Face inference when a user-controlled credential is
   present; continue to fail closed without prompting or discovering one.
3. Re-run native content-search parity with an explicit pre-exec launcher
   receipt and strict one-attempt process ownership.
4. Consider an OS-keyring/automatic credential backend while retaining the
   atomic `0600` file fallback and secret-free receipts.
5. Add strict stateless artifact suppression and explicit timer-scope labels.
