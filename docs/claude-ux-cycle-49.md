# Claude UX cycle 49 — typed slash-command aliases

Date: 2026-08-14
Seed: `992649`
Boundary: Unix epoch `1786771800`

## Frozen release

All lanes independently gated the immutable post-Cycle-48 binary at
`target/cycle49-pin/kitsu`: SHA-256
`2610a19656cb6e1f480b97b7c9ab2f4aa084764e2ec8c439302679e627e40f35`,
154,686,160 bytes, mode `0555`, source revision `d74b664dd39b`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
Each lane froze its oracle before the boundary, launched once from the common
pin, captured raw evidence before adjudication, reaped every child, and made no
shared-tree change or rerun.

## Evidence and attribution

- The provider-free help-registry lane discovered the selected gap for
  **96/100**. `/help` completed in a seven-event JSONL lifecycle with 42 unique
  typed commands, one successful operation, no failures, no session, and no
  provider, tool, assistant, or filesystem activity. Compatibility text
  correctly folded `(alias: /agents)` under `/jobs` and `(alias: /quit)` under
  `/exit`, but each typed command object exposed only name, syntax, and
  description. Automation therefore had to parse prose to recover relationships
  already present in the registry. Raw stdout SHA-256:
  `e46225c43b56fbc91b9ec66ed0391ff43ffc15a362c7737d0f9642c147cad3ed`;
  adjudication SHA-256:
  `9e9f7719a38db3d224086724ff6bf8ebc27619df29dfe0d41e20ef6cee50da9a`.
- The blank-response retry lane passed its frozen behavior for **99/100**.
  The empty assistant was discarded with an incomplete-turn provider failure;
  `/retry` then sent a byte-identical copy of the original request and committed
  the exact reply. The final summary was partial with one provider failure and
  one successful operation, while the named durable session retained only the
  original user turn and successful reply. Its separate low-severity gap is
  that JSONL does not explicitly link the `/retry` turn to the failed source
  turn; that larger receipt change was not selected here. Evidence SHA-256:
  `1fe91b0e6b083ebf0a4a141ddc16d06972b555f662061529037e0917a5b1fe89`;
  raw JSONL SHA-256:
  `dd5422cd121e4569c3bb69fbf2d76668607db0c0146ca6fc1b48eec04ff9c487`.
- The artifact-free read/readiness lane passed for **100/100**. Exactly two
  authenticated local requests exposed only read/list/grep tools. One
  12,080-byte input read retained a bounded 7,936-byte body plus receipt,
  removed the seeded center, emitted `suppressed_by_policy`, and created no
  artifact. Atomic output, resumable session, auth precedence/logout, private
  modes, redaction, counters, sentinel, and process cleanup all passed. Report
  SHA-256:
  `313d89a70c90adc94234f17456c9a4a7cd5ad7a53b60f8a05097f0297b709fd3`;
  raw JSONL SHA-256:
  `0411ecd6d5e85ae30401a107840d980afbc95c62a3de38f0845c7afdd2a0a5a4`.
  Hosted Hugging Face remained `not_run: infrastructure`: process, service,
  managed credential, and approved exact-pin route were absent and
  `request_sent=false`.

No standalone comparator was substituted without an equivalent authorized
slash-command JSON registry or retry lifecycle.

## Selected improvement

`SlashCommandSpec` already stores each alias's canonical command, completion
uses every registry entry, and human help folds aliases into canonical rows.
The JSONL help result discarded `alias_of`, so a typed consumer could not tell
that `/agents` maps to `/jobs` or `/quit` maps to `/exit` without interpreting
human prose. The latter alias's typed description did not otherwise identify
its canonical target.

Every typed help command object now includes nullable `alias_of`: the canonical
command name for an alias, or `null` for a canonical command. Existing name,
syntax, description, text, ordering, dispatch, and completion behavior are
unchanged. Functionality and JSONL protocol documentation define the additive
field; no durable or top-level event schema changed.

## Regression and verification

The new provider-free JSONL regression requires the exact seven-event help and
quit sequence, clean stderr, one successful operation, zero failures, no
provider request, and no session namespace. It asserts `/agents -> /jobs` and
`/quit -> /exit`, canonical `null` values for `/jobs` and `/exit`, presence of
the field on every command, and unchanged folded alias text.

All **795** workspace tests pass (256 Kitsu unit, 2 provenance, 190 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, schema parsing, and diff checks also pass.
