---
name: code-review
description: Review a file or diff for bugs and report findings by severity.
when_to_use: When the user asks to review code, a diff, or a file for issues.
always: false
---
# Code review

1. Read the target file(s) with fs_read_file (ask for the path if unclear).
2. Look for: correctness bugs, edge cases, resource/error handling, security, and
   unclear naming. Prefer concrete failure scenarios over style nits.
3. Report each finding as: `file:line — [High|Med|Low] one-line description` then a
   one-line fix suggestion.
4. End with a short overall verdict. If nothing is wrong, say so plainly.
