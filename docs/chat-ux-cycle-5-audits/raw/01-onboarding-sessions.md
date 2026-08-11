# Audit 1 — onboarding and sessions

Baseline: clean `af20c26`. The auditor used disposable homes/projects,
provider-free commands, and the approved localhost no-auth catalog.

## What worked

- `init` created a credential-free mode-0600 Ollama config and refused clobber.
- Offline doctor was read-only and created no runtime state.
- Human/JSON doctor exposed config, model, context, auth, and tool-policy state.
- Missing-config chat exited 1 with explicit recovery commands.
- Local model discovery returned normalized `mock-alt` and `mock-good`.

## Findings

1. **Medium — first-run guidance uses two different config locations.** Top-level
   help says `kitsu init` then `kitsu doctor`; init defaults to `./kitsu.toml`,
   while bare doctor reads `~/.kitsu/config.toml`. Explicit
   `kitsu --config ./kitsu.toml doctor` succeeds. Evidence: `cli.rs:24,146-147`,
   `main.rs:1298-1302`.
2. **Low — no-auth JSON says a credential is present.** Doctor emits
   `method:none`, `credential_present:true`, and `no credential required`.
   Readiness and credential presence should be separate fields
   (`main.rs:1361-1363`).
3. **Low — missing-config recovery says to copy a bundled config without naming
   the bundled path** (`main.rs:4060`).

Top priorities: one canonical first-run path, truthful auth JSON, and an exact
missing-config recovery command.
