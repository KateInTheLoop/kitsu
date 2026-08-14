# Claude UX cycle 7: model-catalog orientation

## Challenge

Query the authenticated Hugging Face catalog through top-level `kitsu models`
and in-chat `/models`, then compare the result with Claude's `/model` picker.
No chat completion is required for the Kitsu exercise.

## Observation

Hugging Face advertised 130 normalized model IDs. Top-level Kitsu emitted every
ID, while `/models` printed the first 40 alphabetically. The configured active
model, `meta-llama/Llama-3.3-70B-Instruct`, occurred after that cutoff and was
therefore invisible in the interactive result.

Claude displayed five choices, marked Haiku as current, and explained the
selection scope. Hugging Face cannot be reduced to a similarly curated fixed
set, but Kitsu can preserve orientation and keep the first view within one
terminal viewport.

## Improvement

Human model catalogs now:

- show at most 12 model rows;
- pin an exact catalog match for the active model before alphabetical results;
- mark it with `* ... (current)` and repeat it in the count header;
- report the hidden count and the exact substring-filter command;
- retain a direct switch hint.

Top-level output changes only when stdout is a TTY. Redirected stdout remains a
complete newline-delimited catalog, and JSONL continues to contain every match.
Catalog discovery remains available when no model is configured.

## Verification

A formatter unit test uses 20 models to prove the current model is pinned, only
12 rows render, the hidden count is correct, and filtered results do not falsely
mark a current model outside the match set. The provider-backed slash integration
test asserts the configured model marker and switch hint. Existing top-level
integration coverage continues to require the complete raw list when stdout is
captured for automation.
