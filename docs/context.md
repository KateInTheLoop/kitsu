# Context management

Kitsu sends the conversation again on every provider request. Context management
therefore has two jobs: resolve a credible model window, then prove that the
concrete request fits before it crosses the provider boundary.

## Window resolution and discovery

The CLI resolves one context window in this order:

1. `--context-window`;
2. the gateway profile's `context_window`;
3. a fresh discovery-cache entry keyed by endpoint, models path, and model;
4. the conservative default, **8192 tokens**.

Discovery entries remain fresh for 24 hours. A missing entry never delays the
first inference: Kitsu uses 8192 for that run and starts a best-effort catalog
probe in the background. The probe has its own two-second deadline and can warm
the cache for a later process. Within one provider instance, a successful model
catalog is reused for 60 seconds and eligible discovery failures are cached for
five seconds. Authentication failures are not treated as ordinary negative-cache
entries.

Explicit `models` and `/models` commands are user-requested catalog operations,
so they use the configured CLI timeout. `/model` validates against a catalog when
one is available. If the window was set by CLI/config, the switch preserves it;
otherwise Kitsu resolves the selected model's metadata synchronously and falls
back to 8192 when no length is reported.

Discovery is advisory. The window actually enforced for the current request is
always present, including on a cold cache.

## Token accounting and budget

Before Kitsu has provider usage, text is estimated at roughly four characters
per token and tool-call JSON at three characters per token. Tool-schema overhead
is included. Once a response reports `prompt_tokens`, the meter uses that real
count as an anchor and estimates only messages appended afterward. Reshaping the
history or changing models invalidates the anchor.

For a window `W`:

```text
reply reserve = max(max_output_tokens, 512), capped at W / 2
effective     = W - reply reserve
trim target   = effective - min(W / 10, 2048)
```

The reserve leaves room for an answer; the target leaves a buffer below the
hard effective prompt limit. These are estimates, not a gateway-specific
tokenizer, but reported usage continuously corrects their scale.

## Compaction cascade

Kitsu applies cheaper and more recoverable reductions before more lossy ones:

1. **Offload large tool results.** Above the default 8 KiB threshold
   (configurable with `--offload-cap`), a tool result can be written to a private
   artifact when storage is available. The conversation retains the exact path,
   size, re-read guidance, and a 500-character preview. With `--no-artifacts`,
   oversized results become bounded, lossy inline previews with byte counts
   and no recoverable pointer. Existing artifacts remain intact.
2. **Elide old tool bodies.** Under pressure, older tool-result bodies become a
   short marker. An existing offload pointer is retained so the result remains
   re-readable.
3. **Compact whole exchanges.** If needed, Kitsu removes or summarizes the
   oldest complete user-delimited exchanges. It never splits an assistant tool
   call from its tool result, and it preserves the leading system context plus
   the two newest user exchanges.

`--compaction trim` deterministically drops eligible old exchanges. It is the
default and makes no extra provider call.

`--compaction summarize` asks the active model for a lossy continuation note.
The summarizer request is non-streaming, capped at 1024 output tokens (and at
half the actual model window), and locally shortened if its input would not fit.
The returned summary is admitted only when the rebuilt main request fits the
target; it may be shortened locally to do so. Empty, failed, or unfit summaries
fall back to deterministic trimming. Three consecutive summary failures open a
per-session circuit breaker so a doomed summarizer is not called repeatedly.

Every provider request—including the summarizer request and every agent-loop
round—receives a final check against its concrete messages and tool schemas. If
the protected system/recent context still cannot fit after all eligible
compaction, Kitsu stops locally with an actionable context-overflow error.

## Manual compaction and rollback

`/compact` invokes the same cascade without waiting for the automatic pressure
threshold. It compacts all eligible older exchanges while preserving the normal
system/recent boundary. The command reports its before/after estimate, method,
removed exchanges, and tool-result elisions. If the command is cancelled or
fails, both the conversation and its context-management state are restored.

If fewer than three user exchanges exist, there may be nothing safe to compact.
Manual compaction still requires a resolved window at the core API boundary; the
CLI supplies its enforced 8192 fallback when no stronger value exists.

## Observability and session state

Redirected human-mode turns emit stable `kitsu-context: {...}` telemetry on
stderr alongside `kitsu-usage:`. JSONL mode carries context and usage through
versioned events instead.

`/context` reports the window, used/effective tokens, remaining headroom,
per-source provenance and estimates (including conversation roles and tool
schemas), and the compaction ledger. `/status` includes cumulative compaction
runs, tokens saved, exchanges removed, tool results elided, and the last
outcome/fallback.

The in-memory session ledger records automatic versus manual runs, trim versus
summary outcomes, savings, fallback reason, and whether summary input/output was
locally shortened. Its detailed record list is bounded. Conversation snapshots
persist the compacted messages and artifact pointers, but the runtime meter,
circuit-breaker count, and ledger are not serialized into a resumed session.

## Project context (`llms.txt`)

Unless `--no-project-context` or `KITSU_NO_PROJECT_CONTEXT` is set, Kitsu looks
from the launch directory up to the Git/project root for `llms.txt`. If none is
found, it falls back to the nearest `AGENTS.md` or `CLAUDE.md`. A configured
project-context path takes precedence when it exists.

For `llms.txt`, Kitsu parses the H1, leading blockquote summary, required body,
and `## Optional` section. It includes the optional section only when the entire
rendered file fits within one quarter of the window, then caps the final project
context at one third of the window. Fallback instruction files receive the same
final cap. Reads are bounded to 256 KiB.

Kitsu injects the file's contents; markdown links are an index for the model and
are **not followed or fetched automatically**. The exact source is disclosed in
`/status` and `/context`. `--no-project-context` also prevents saved project text
from being silently reintroduced when resuming a session.

On Unix, discovery and reads use descriptor-relative traversal and reject
symlink retargeting. Platforms without that primitive do not load implicit
project files. Context management runs client-side; it does not use a
provider-specific server compaction extension.

Development checks and the local-fixture testing policy are in
[Contributing](../CONTRIBUTING.md).
