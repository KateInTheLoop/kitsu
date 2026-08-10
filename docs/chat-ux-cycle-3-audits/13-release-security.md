# Cycle 3 release security audit

Date: 2026-08-10

Scope: filesystem authority, command authority, private state, secret-safe
diagnostics, background agents, and verified review.

## Findings and disposition

| Severity | Finding | Disposition |
| --- | --- | --- |
| High | `fs_edit_file` could combine write authority with revoked or absent read authority. | Fixed: edit requires live read and write checks that resolve to the same target identity. |
| High | TOML parse diagnostics could echo the source line containing a token. | Fixed: configuration errors retain location-only metadata; credential-like refresh hints are suppressed across human and JSONL modes. |
| High | Background agents shared interactive filesystem/command prompters and could race the foreground for stdin. | Fixed: detached agent tool work is task-locally scoped to explicit grants; both filesystem and command policies refuse interactive prompting. |
| High | A hard-link alias could bypass private-state path protection and verified-review exclusions. | Fixed on Unix: protected regular files are tracked by device/inode identity and all live review aliases are excluded. |
| High | A session command grant authorized argv/cwd but did not bind the executable observed through PATH. | Fixed: grants store the canonical executable observation/fingerprint, revoke on replacement or `PATH` retargeting, revalidate immediately before spawn, and execute the observed target. |
| Medium | `/new` or resume could reuse the prior command-output artifact authority. | Fixed: every initial/new/resumed/no-session activation uses an exclusive generation, clears the old sink/read grant, and retains old evidence only on disk. |
| Medium | `/revoke-read` or `/revoke-write` can sound effective while the explicit global dangerous bypass remains active. | Fixed: revocation refuses under the dangerous global mode and tells the user to restart without it. |
| Medium | Untracked review collection remains path-based on portable platforms during concurrent namespace mutation. | Unix review exclusions now use identity discovery; descriptor-relative collection remains a defense-in-depth follow-up. |

## Established invariants

- Private configuration, session, history, cache, skill, offload, and command
  artifact roots are protected independently of broad allow roots.
- Filesystem editing requires read and write authority; write-only mode exposes
  whole-file creation/replacement but not edit/read tools.
- Command authority remains independent from filesystem `--yes`; it binds
  literal argv and canonical cwd, never a reconstructed shell string.
- Host-owned artifacts use no-follow, create-new, mode-0600 allocation and are
  published only after successful completion; abort/drop removes partials.
- Malformed config, provider failures, model IDs, session previews, paths, and
  terminal notices cross explicit sanitization/redaction boundaries.

## Portability residuals

Device/inode hard-link protection and descriptor-relative namespace hardening
are Unix-specific. Non-Unix builds retain lexical/canonical containment and
fail-closed final-component checks but cannot promise the same concurrent
namespace guarantees using the Rust standard library alone.

Command grants are fingerprint-bound but do not claim immutable-handle
execution: a narrow final metadata-check-to-spawn pathname race remains. The
public telemetry therefore keeps `pinned: false` and explicitly discloses that
ambient command authority is not a sandbox.

## Release recommendation

GO. The complete workspace test, strict Clippy, formatting, secret scan, and
independent post-fix review are green. The disclosed executable
metadata-check-to-spawn race remains a nonblocking hardening target.
