# ISSUE-004 — Invalid model name yields repeated raw HTTP 404s with no guidance

- **Status:** OPEN
- **Severity:** Medium (bad UX; easy to hit via a `/model` typo)
- **Found by:** dogfooding, Stage 1 (persona `novice_03`)
- **Area:** `kitsu` CLI (`/model` + error rendering)

## Symptom
After `/model tiny-fast-thing` (a name the gateway doesn't have), the switch is
accepted silently and then **every** subsequent turn fails with the raw provider
error, repeatedly:

```
~ switched to tiny-fast-thing
user> hi
provider returned HTTP 404: {"error":{"message":"model 'tiny-fast-thing' not found",...}}
user> ?
provider returned HTTP 404: {"error":{"message":"model 'tiny-fast-thing' not found",...}}
...
```

The user gets a wall of JSON and no hint that the **model name** is the cause or
how to fix it.

## Root cause / notes
- `/model` accepts any non-empty string by design (model validity is
  gateway-defined) — reasonable, but there's no feedback loop when it's wrong.
- The provider `Http { status, body }` error is rendered verbatim, so a 404 with
  a JSON body is dumped as-is.

## Proposed fix
- When a request fails with 404 (or the body mentions "model" + "not found"),
  render a friendly hint: `model '<name>' was rejected by the gateway — run
  /model <valid-id> (see the gateway's available models)`.
- Optionally: on `/model <name>`, if the gateway exposes `models_path`, validate
  the name against the model list and warn immediately (still allow it — some
  gateways don't list all served models).

## Verification
- Switch to a bogus model, send a turn → one clear, actionable error (not raw
  JSON); the hint names the model and points at `/model`.
