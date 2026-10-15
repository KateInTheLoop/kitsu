# Working on Kitsu

Read `README.md` and `docs/architecture.md` for orientation. Use
`CONTRIBUTING.md` for development commands and documentation policy.

- Inspect the relevant source and tests before changing behavior. Preserve
  filesystem permissions, credential redaction, containment, and private-state
  exclusions.
- Keep run/session values independent of terminal and physical storage
  adapters. Run the boundary scripts when touching those modules.
- Use local mock providers for automated tests. Do not send requests to real
  providers unless the user asks for that validation.
- Test behavior changes at the appropriate boundary. Report environment
  restrictions and intermittent failures; do not hide them by deleting tests.
- Update existing user/reference docs when behavior changes. Do not add cycle
  logs, audit transcripts, per-commit receipts, or generated review records.
- Keep documentation claims tied to shipped source behavior. CLI help is the
  authority for available flags; `docs/schemas/kitsu-chat-v1.schema.json` is the
  machine-readable JSONL contract.

`llms.txt` is Kitsu's project-context index. Its links are not automatically
loaded, so keep its summary useful on its own.
