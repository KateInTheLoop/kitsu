# Audit #1 — onboarding at `dead112`

## Severity-ranked findings

1. **High — No onboarding/diagnostic command**
   - **Observed:** Root commands are only `ask`, `models`, `chat`, and `help`.
   - A missing config returns only: `config error: no config found at /tmp/kitsu-audit-no-such-config.toml`.
   - The quick start says “after installing `~/.kitsu/config.toml`” but does not explain how to create or validate it.
   - **Improvement:** Add `kitsu init` and `kitsu doctor`, with provider reachability, auth-variable presence, selected gateway/model, config search paths, session storage, and tool-policy checks.

2. **High — Chat capabilities are undiscoverable before entering the REPL**
   - **Observed:** `kitsu chat --help` says script lines may be slash commands, but does not list `/help` or any slash-command groups.
   - Root help mentions “See `/skills`” even though users cannot invoke `/skills` at the shell.
   - **Inferred:** New users cannot discover sessions, agents, tools, permissions, review, model switching, or other chat facilities without already knowing `/help`.
   - **Improvement:** Add a concise “Interactive commands” section, `kitsu chat commands`, or top-level cross-links.

3. **Medium-high — Help is dominated by advanced and repeated global flags**
   - **Observed:** `kitsu models --help` includes long explanations for `--yes`, compaction, max tool rounds, filesystem/project context, and skill preloading, despite model listing not using most of those concepts directly.
   - `chat --help` is similarly long; the actual first-run path is visually buried.
   - **Improvement:** Group basic/provider, automation, context, and dangerous tool-policy options; keep root and `models` help focused.

4. **Medium-high — Missing-config diagnostics do not offer recovery**
   - **Observed:** The missing-config error has no searched-path list, example command, template, documentation pointer, or suggestion to use an environment-only/local gateway.
   - **Improvement:** Include an actionable next command such as `kitsu init`, plus `kitsu doctor --config ...`.

5. **Medium — Model discovery errors lack troubleshooting context**
   - **Observed in sandbox:** `models` returned `connection error: error sending request for url (http://127.0.0.1:18181/v1/models)`.
   - The message omits selected gateway, timeout, likely causes, and a diagnostic suggestion.
   - The mock port was occupied, but loopback access required approval; the approved probe was aborted.
   - **Label:** End-to-end model listing remains unverified by this auditor, not proven broken.

6. **Medium — Version provenance is correct at this snapshot but not self-validating**
   - **Observed:** `git rev-parse --short HEAD` and `kitsu --version` both reported `dead112`.
   - Binary output: `kitsu 0.1.0 (dead112, built 2026-08-10)`.
   - Binary mtime: `2026-08-10 12:09:25 -0700`.
   - **Unverified by this auditor:** Dirty rebuild detection and stale-binary invalidation.
   - **Inferred:** The CLI provides no explicit stale-binary warning; users must compare the embedded hash to Git themselves.

7. **Low-positive — Unknown gateway error is actionable**
   - **Observed:** `unknown gateway 'no-such-gateway' — defined gateways: mock`.
   - Preserve and extend this pattern.

## Top three urgent improvements

1. Implement `kitsu init` and `kitsu doctor`.
2. Expose and group chat/slash-command capabilities outside the REPL.
3. Redesign help around progressive disclosure, especially removing irrelevant inherited options from `models --help`.

## Commands and evidence

```text
git rev-parse --short HEAD
# dead112

git status --short --branch
# ## dogfood/top10-improvements
# ?? docs/chat-ux-cycle-3-audits/

target/debug/kitsu --version
# kitsu 0.1.0 (dead112, built 2026-08-10)

target/debug/kitsu --help
target/debug/kitsu chat --help
target/debug/kitsu models --help

target/debug/kitsu --config /tmp/kitsu-audit-no-such-config.toml models
# exit 1; no config found

target/debug/kitsu --config /tmp/kitsu-ux-live-config.toml \
  --gateway no-such-gateway models
# exit 1; lists defined gateway: mock

target/debug/kitsu --config /tmp/kitsu-ux-live-config.toml models
# sandboxed probe exit 2; loopback connection error

stat -c '%n | size=%s | mtime=%y' target/debug/kitsu
```

No repository files were edited or committed by the auditor.
