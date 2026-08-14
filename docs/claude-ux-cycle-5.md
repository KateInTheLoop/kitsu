# Claude UX cycle 5: honest provider readiness

## Challenge

Run Kitsu's authenticated Hugging Face model-catalog diagnostic, then make a
small chat request with the same configured credential. Compare the terminal
wording with a tool-free Claude CLI review of that wording.

## Observation

The Hugging Face catalog request succeeded and Kitsu correctly disclosed
`model catalog only; chat/inference access was not tested`. The old terminal
line nevertheless concluded `you're ready`. The immediately following chat
request failed with HTTP 401 and Kitsu's safe configured remedy, proving that
catalog authorization does not imply inference authorization.

Claude's review identified the same issue: the final line conveyed false
confidence after explicitly admitting that inference was unverified. Claude
recommended turning the next command into a verification step.

## Improvement

Successful doctor output now ends:

```text
doctor: no blocking issues found — run `kitsu` to verify chat/inference access
```

This preserves an actionable next step, reinforces bare `kitsu` as the default
chat entry point, and no longer overstates what the read-only diagnostic proved.
The online JSON contract remains unchanged: `scope` is
`model_catalog_only`, `chat_route_probed` is `false`, and `status` remains
`reachable` for a successful catalog request.

## Verification

The online-doctor integration test asserts the scope disclosure, the honest
terminal state, absence of `you're ready`, and the existing GET-only mock
expectation. No credential values or provider response bodies are recorded.
