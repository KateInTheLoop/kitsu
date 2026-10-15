# Contributing

Use a current stable Rust toolchain. On Linux, containment integration tests
also need `bubblewrap`, Python 3, and pytest. The host must allow unprivileged
user/network namespaces; see `.github/workflows/rust.yml` for CI setup.

## Development

```bash
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash scripts/check-run-boundary.sh
bash scripts/check-session-boundary.sh
```

CI also checks unused must-use results and runs tests in the release profile:

```bash
RUSTFLAGS=-Dunused-must-use cargo check --workspace --all-targets --locked
cargo test --workspace --release --locked
```

HTTP and PTY tests require local sockets and terminal resources. If a sandbox
blocks them, run with the required permissions rather than weakening the test.
Some session fault-injection tests can interfere in parallel; when diagnosing
those failures, compare with `cargo test --workspace --locked -- --test-threads=1`
and report the parallel failure as well.

For runtime changes, add or update tests that exercise the changed behavior.
Provider tests should use local fixtures; live-provider checks require explicit
credentials and can incur costs. Documentation-only changes need link and
reference checks, not a full rebuild.

## Documentation

Keep the README focused on getting started. Put detailed user instructions in
`docs/usage.md`, protocol details in `docs/jsonl-protocol.md` and its schema,
and implementation orientation in `docs/architecture.md`.

Update an existing document when behavior changes. Keep implementation plans,
audit transcripts, model comparisons, per-commit receipts, and review chatter
in issues or pull requests. Git history preserves removed documents; do not
create an archive directory to keep them in the current tree. Add a new guide
only when it serves an ongoing user or maintainer need, and link it from the
README.

Describe the problem, resulting behavior, and validation in the pull request.
Use [the release guide](docs/releasing.md) when cutting a release.

## Credential checks

Keep tokens in environment variables or Kitsu's private credential store.
Never commit `.env` overrides, cloud credential directories, private keys, or
captured provider traffic. Synthetic security-test credentials must be clearly
identified as fixtures.

With Gitleaks installed, scan current files and reachable history before
publishing changes that touch authentication or logging:

```bash
gitleaks dir --redact .
gitleaks git --redact --log-opts="--all" .
```

Inspect findings locally with redacted output. Do not paste credential values
into issues or PRs. A confirmed published secret needs revocation or rotation;
removing it from a current file does not remove it from Git history.
