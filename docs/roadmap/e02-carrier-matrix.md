# E02 Provider-Free Carrier Matrix

Status: fixture contract for E02-S01; test-only, not a public or persisted API.

Implementation owner:
`crates/kitsu/tests/support/carrier_fixture.rs`. Executable coverage:
`crates/kitsu/tests/carrier_matrix.rs`.

## Purpose and non-goals

The fixture asks whether existing Kitsu entrypoints preserve one causal run
result across presentation and failure. It does not introduce a second host,
provider, network server, public `RunSpec`, JSONL schema field, or session
format. Production types are authorized only after real carrier observations
show duplicated or divergent classification.

The fixture itself creates no provider, socket, configuration, credential,
tool, child process, or session store. Later cards adapt actual process output
and fault injection into this oracle.

## Carrier shapes

| Entry | Human | JSONL | Notes |
|---|---:|---:|---|
| `ask` | required | required | One prompt, noninteractive |
| piped `chat` | required | required | Input EOF is part of the carrier contract |
| scripted `chat` | required | required | Multiple turns and same-session sequencing |
| interactive `chat` | required | not a current fixture carrier | PTY input, confirmation EOF, signal handling |

Seven carrier shapes are therefore in v1. A named independent host/principal is
still absent, so X003 cannot pass from this fixture.

## Required observation dimensions

Every applicable cell records these independently:

1. run identity assigned before acceptance and unchanged across the carrier;
2. semantic class: success, user/preflight fault, provider fault, cancelled, or
   interrupted;
3. assistant disposition: committed, discarded, or absent;
4. cleanup: joined or typed residue;
5. persistence: committed, preserved, failed, or not requested;
6. delivery: delivered, failed, or not attempted;
7. exactly one terminal publication;
8. exact process exit `0`, `1`, `2`, or `130`;
9. zero work after terminal.

Delivery/process failure must not rewrite semantic settlement. For example, a
committed successful answer followed by EPIPE remains semantically successful
and committed, while delivery is failed and process exit is nonzero.

## Scenario matrix

| Scenario | Required trigger/evidence | Carrier applicability |
|---|---|---|
| Success | One ordered identity/accept/settle/join/persist/deliver/terminal/exit trace | All seven |
| Sequential same-session | Two runs keep distinct run IDs while the requested session identity continues | All seven via fixture injection |
| Preflight fault | Invalid config/auth/authority makes zero provider/tool sends | All seven |
| Provider fault | Typed provider failure, no committed assistant, exit 2 | All seven |
| Tool fault | Recovered and unrecovered tool outcomes cannot be inferred from presentation | All seven |
| Confirmation EOF | Interactive prompt closes without granting; no effect/send after EOF | Interactive required; six noninteractive carriers must fail closed without prompting |
| Pre-terminal EOF | EOF/channel close before terminal is interruption/loss, never success | All seven |
| Cancel before active | No accepted work starts; one cancelled/no-run outcome | All seven |
| Cancel active | Active owned work is cancelled and joined before terminal | All seven |
| Cancel queued | Queued work never becomes active after cancellation | All seven |
| Blocked sink | Bounded writer policy; no later provider/tool work | Six noninteractive carriers; interactive PTY excluded |
| EPIPE sink | Checked failed delivery and exact nonzero exit | Six noninteractive carriers; interactive PTY excluded |
| Cleanup residue | Typed residue blocks a clean-success terminal claim | All seven |
| Duplicate terminal | Second publication for one run is rejected | All seven |
| Cross-run terminal | Event/terminal with another run ID is rejected | All seven |
| Identity stripped | Missing/reassigned identity is a fixture failure | All seven |
| Version mismatch | Unsupported `kitsu.chat.v1` projection is typed, not inferred | JSONL carriers only |

This produces 119 declared cells: required, explicitly fail-closed without a
prompt, or explicitly not applicable. No cell is silently omitted.

## Oracle rules

- Identity must be assigned before `Accepted`.
- Required phase order is `settled -> joined -> persisted -> delivered`, then
  one terminal publication and process exit.
- A phase regression, identity reassignment/cross-run event, duplicate terminal,
  missing terminal/exit, or post-terminal work is an explicit fixture violation.
- Semantic class, assistant state, persistence, delivery, cleanup, terminal
  count, and exit are compared as separate fields.
- The test-only oracle never infers success from EOF, idle, prose, UI state, or
  exit code alone.

## Delivery cards

- Cycle 011 defines and self-tests this matrix/oracle.
- Cycle 012 maps success plus provider/tool/preflight faults and exact exits
  from existing carriers.
- Cycle 013 maps cancellation, EOF, blocked/EPIPE delivery, duplicate/cross-run
  terminals, and cleanup residue.
- E02-S02 may introduce private production run types only for mismatches proven
  by those mappings.
