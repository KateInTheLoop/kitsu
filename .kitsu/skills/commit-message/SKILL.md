---
name: commit-message
description: Write a clear Conventional Commits message from a set of changes.
when_to_use: When the user asks for a commit message or to summarize a diff into one.
always: false
---
# Commit message

Write a [Conventional Commits](https://www.conventionalcommits.org/) message.

1. Read the diff or changed files the user points to (fs_read_file / repo_grep).
2. Pick the type: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `style`, `ci`.
3. Subject line: `type(scope): imperative summary`, ≤ 72 chars, no trailing period.
4. Body (optional): what changed and *why*, wrapped at ~72 cols. Reference issues.
5. Output ONLY the commit message, no preamble.
