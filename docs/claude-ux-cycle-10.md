# Claude/Codex UX cycle 10: Hugging Face auth preflight

## Challenge

Bootstrap a recurring three-agent Kitsu-versus-Codex/Claude comparison through
the Hugging Face gateway after a laptop restart, without reading, printing, or
persisting credential values.

## Observation

The restart removed `HF_TOKEN` from the process and user-service environments.
Kitsu classified the missing credential correctly and retained its safe
Inference Providers permission hint, but one-shot chat emitted context/setup
telemetry before the provider auth path finally rejected the run. Offline
`doctor` reported the missing variable without the configured recovery hint.
That made readiness troubleshooting inconsistent and allowed unrelated setup
work before a failure that was already knowable locally.

Claude exposes an explicit persisted authentication lifecycle. Kitsu continues
to use deliberately explicit gateway configuration and environment credentials,
but its local readiness check should be just as immediate and coherent.

## Improvement

- Chat, review, and model-catalog provider work now exercises the same auth
  provider path before project context, tools, run-start telemetry, or network
  work.
- Missing and malformed credentials retain user/config exit status `1` and use
  the existing secret-safe provider error formatter.
- `doctor` includes the same sanitized, bounded recovery hint in its blocking
  issue and additive JSON `auth.recovery_hint` field.
- Provider-free management commands remain credential-independent, and Kitsu
  still never writes environment credentials to the repository.

## Verification

Integration coverage proves a missing bearer variable produces no assistant
stdout, context meter, provider-attempt notice, or thinking state; `models`
fails through the same early path; and offline doctor exposes only the safe
configured recovery prose. The full workspace test suite, formatting check,
Clippy with warnings denied, and both staged and unstaged diff checks passed at
the checkpoint. The reusable three-task rotation and scoring rules are recorded
in [dogfood-rotation.md](dogfood-rotation.md).

Live Hugging Face inference still requires the user to restore `HF_TOKEN`
through a secret-safe local channel.
