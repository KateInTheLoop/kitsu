# Claude UX cycle 40 — actionable JSONL command errors

Date: 2026-08-14
Seed: `992640`
Boundary: Unix epoch `1786755600`

## Scored evidence

All three lanes gated the same immutable post-Cycle-39 binary: SHA-256
`5a40810cda6f282ade0be9c4539d419a79180d391071e22a9d510079e3b10b53`,
154,670,208 bytes, mode `0555`, source revision `5c1938b4f76e`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.

- The provider-free randomized lane ran three bounded children covering one
  success/one failure, two successes/two failures, and JSONL plural failure
  framing. All exited `0` in 9–12 ms. Singular and plural grammar, human and
  JSONL `partial`, counters, event order, destination preservation, zero
  effects, and process reaping passed. It scored **99/100** because JSONL's
  unknown-command errors discarded the actionable human `/help` guidance.
  Evidence SHA-256:
  `a1ef0181b6729fe8eeb32c393e252538ab2678e08e3a9c4a8565825fecf385e7`.
- The context/session lane exercised the styled PTY path with one committed
  model turn and one accepted `/quit later` failure. Its terminal ended
  `partially completed in 5 ms`; output and the named one-turn durable session
  were exact, the failed command was absent, and a provider-free JSONL reducer
  agreed on `partial`. All 24 checks passed for **100/100**. Evidence SHA-256:
  `d9c518cf53431c4dba3bb48a11e10783f1abe30422f7cabefb0915b67fc84cd6`.
- The Hugging Face-shaped lane verified the same partial contract through an
  owned authenticated local adapter, including bounded artifact-free reads,
  exact output in two resumable sessions, managed/environment auth precedence,
  logout, redaction, and full reaping. Hosted HF remained `not_run`
  infrastructure because process, user-service, and managed credential gates
  were all false and no remote request was sent. It scored **100/100**.
  Evidence SHA-256:
  `05f3791f355c1b98abc52bc07b7d5233283adac34747e0f96ca48a5a3a390548`.

No standalone Codex or Claude score was substituted: neither exposed an
honest isolated equivalent for this accepted-partial scripted lifecycle.

## Selected improvement

The timing and grammar postfix passed every scored branch. The provider-free
JSONL branch then exposed a narrower automation defect: Kitsu's human error
said either `did you mean /...` or `(try /help)`, while the typed event reduced
both to `command failed: /...`. That made the machine surface less actionable
even though the recovery text was host-authored, bounded, and safe.

Unknown commands now return a typed host-owned user failure. Interactive mode
prints the same recovery, human scripts attach it to the source line, and JSONL
places it in the existing `error.message` before `command_finished`. This does
not add a schema field, expose provider text, change stderr framing, or alter
exit status and counters. Other slash-command failures retain their existing
behavior.

## Regression and verification

The existing human typo regression still requires source-line context and the
closest `/status` suggestion. A new provider-free JSONL regression covers both
the closest-command and `/help` branches, exact line/command identity, empty
stderr, user classification, exit code `1`, and the unchanged two-failure
terminal summary. README and protocol documentation now state this recovery
contract.

All **790** workspace tests pass (256 Kitsu unit, 2 provenance, 185 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, published-schema parsing, and diff checks also pass.
