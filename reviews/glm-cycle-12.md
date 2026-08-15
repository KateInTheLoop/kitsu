# Cycle 12 review — `get_models_response` (openai_compat.rs:1220-1270)

Scope: only `get_models_response` (lines 1220-1270). Read and verified in-file:
the `MODELS_CACHE_TTL`/`MODELS_NEGATIVE_CACHE_TTL` consts (lines 68-69), the
`OpenAICompatProvider` struct and `ModelsCacheEntry` (lines 26-53), and
`validate_models_catalog` (lines 1393-1410).

**Not read — claims below are hedged, not asserted:** `fetch_models_response`
(only its call site at line 1243), `self.auth.header()` and the `AuthProvider`
trait (only its call at line 1231), and `model_context_length` (the consumer of
`ModelsCacheEntry.context_lengths`, referenced only in the struct doc lines
48-52). Where a proposal turns on behavior I could not read, it says so.

---

## P1 — On refresh, the memoized `context_lengths` map is wiped every cache write (line 1267)

```rust
1263	        *cache = ttl.map(|ttl| ModelsCacheEntry {
1264	            stored_at: tokio::time::Instant::now(),
1265	            ttl,
1266	            outcome: outcome.clone(),
1267	            context_lengths: std::sync::Mutex::new(HashMap::new()),
1268	        });
```

Every cache replacement — including the negative-cache refreshes that fire
every 5s (line 1254) — constructs a fresh empty `HashMap`. The struct doc
(lines 48-52) frames `context_lengths` as a per-model memoization that is
"Fresh on every cache invalidation, so it inherits the existing TTL contract,"
which sounds intentional. But within a *positive* entry's 60s lifetime the map
is never wiped, and across a positive→positive refresh at 60s the whole map is
discarded; across the far more frequent negative refreshes at 5s an entry that
will only ever yield errors still allocates a fresh empty `HashMap` each time.

Two concerns, in priority order:

1. **Negative-cache churn:** each 5s negative refresh allocates an empty
   `HashMap` solely to be thrown away, because `ttl` is `Some(...)` and so the
   `Some(ModelsCacheEntry{...})` branch runs (line 1263) even when `outcome` is
   `Err`. The outcome is an error, so `context_lengths` can never be populated
   for that entry — the allocation is pure waste on the hot negative path.
2. **Positive→positive discard:** the doc's claim of "fresh on every
   invalidation" is only strictly true of *positive* invalidation. A consumer
   reading that doc (see hedge) could reasonably expect a positive entry's
   parsed lengths to be reused across a refresh if the catalog is unchanged;
   they are not.

**Hedge:** I did not read `model_context_length`, so I cannot confirm how often
the map is read, whether it is populated lazily or eagerly, or whether the
discard actually causes observable re-parsing cost. If `model_context_length`
is called rarely (e.g. once at startup), this drops to P3 cosmetics.

**Proposal:** construct `context_lengths` only for the positive case, and
preserve it across positive refreshes where the catalog text is unchanged:

```rust
let prev_context_lengths = cache
    .as_ref()
    .and_then(|e| e.outcome.as_ref().ok())
    .and_then(|(prev_status, prev_text)| {
        (prev_status == &status && prev_text == &text).then(|| {
            e.context_lengths
                .lock()
                .ok()
                .map(|m| m.clone()) // cheap if empty; preserves warm cache otherwise
        })
    })
    .flatten()
    .unwrap_or_default();
```

At minimum, avoid the empty-map allocation on the negative path by returning
the error outcome without a fresh `ModelsCacheEntry` map (or by storing a
`None`-typed placeholder). Ranked P1 because the negative path is the hot one
(5s cadence) and the waste is unconditional.

---

## P2 — Auth is validated before the cache lock, so a stale positive entry can satisfy a request whose auth has since rotated (lines 1231-1239)

```rust
1231	        self.auth.header().map_err(ProviderError::Auth)?;
1232
...
1236	        let mut cache = self.models_cache.lock().await;
1237	        if let Some(entry) = cache.as_ref() {
1238	            if entry.stored_at.elapsed() < entry.ttl {
1239	                return entry.outcome.clone();
1240	            }
1241	        }
```

The comment at lines 1228-1230 states the invariant exactly: *"A cached catalog
must never turn a now-missing credential into an apparent healthcheck success.
Validate current auth before consulting the cache; auth failures themselves
are deliberately never cached."* Line 1231 enforces it by calling
`self.auth.header()` and returning `Err(ProviderError::Auth)` *before* the
cache is touched.

This is correct **provided** `auth.header()` (not read — hedged) actually
validates the credential and fails fast when it is gone/expired, rather than
merely formatting a header string. Two residual risks remain even under that
assumption:

1. **Ordering is load-bearing but undocumented at the call site.** The cache
   hit at line 1239 returns a previously-stored `(StatusCode, text)` pair that
   was captured *with a prior valid auth*. If a future refactor swaps lines
   1231 and 1236 (cache first, auth second), the invariant silently breaks and
   a rotated credential would surface as a healthy 200 from a 60s-old entry.
   The block comment protects the intent, but nothing in the types enforces
   it.
2. **`auth.header()` cost on every cache hit.** If `header()` is non-trivial
   (token refresh, clock check, network — all hedged, since I did not read
   `AuthProvider`), then the "cache hit" path is not actually cheap: it pays
   the auth cost on every call. That may be intentional (the comment
   prioritizes correctness over speed), but it should be stated so a
   performance-minded reader doesn't assume the cache short-circuits auth.

**Proposal (low-risk):** keep the ordering, and make it self-documenting by
binding the auth result so its evaluation cannot be accidentally deferred or
reordered:

```rust
// Evaluating auth here (before any cache read) is the correctness gate:
// a cached catalog must never mask a now-missing credential.
let _header = self.auth.header().map_err(ProviderError::Auth)?;
```

If `header()` is genuinely expensive (verify against `AuthProvider` — hedged),
consider validating auth only when the entry is *stale* or absent, accepting
the (documented) trade-off that a hit within TTL can briefly mask a rotated
credential. That trade-off is exactly what the current comment forbids, so the
present code is the safer choice; the proposal here is only to surface the
cost. Ranked P2 because the current behavior is correct; the risk is
regression under future edits.

---

## P3 — A 200 with a structurally invalid catalog is negatively cached as an error, but the error text is not the server's (lines 1244-1248, 1256-1260)

```rust
1243	        let outcome = match self.fetch_models_response(&url).await {
1244	            Ok((status, text)) if status.is_success() => {
1245	                validate_models_catalog(&text).map(|()| (status, text))
1246	            }
1247	            other => other,
1248	        };
...
1256	            Err(
1257	                ProviderError::Connection(_)
1258	                | ProviderError::Timeout
1259	                | ProviderError::InvalidResponse(_),
1260	            ) => Some(Self::MODELS_NEGATIVE_CACHE_TTL),
```

`validate_models_catalog` (read at lines 1393-1410) returns only
`ProviderError::InvalidResponse` for malformed JSON, a missing `data` array,
or entries lacking a string `id`. When a server returns 200 with such a body,
line 1245 converts the success into `Err(ProviderError::InvalidResponse(...))`,
and line 1256-1260 then caches that error for 5s.

The mapping itself is reasonable — a malformed-but-200 catalog is not usable,
so short-negative-caching it avoids hammering a broken endpoint. The only
nuance: the cached `outcome` is `Err(InvalidResponse("malformed /models JSON:
..."))` carrying *kitsu's* diagnostic, not the server's original `text`. A
caller that logs the outcome loses the raw body that would help debug the
upstream. This is a deliberate "validate then cache the verdict" shape, and I
cannot tell from this function alone whether any caller wants the raw text on
the validation-failure path (hedged: I did not read the callers).

Additionally, the `_` wildcard at line 1261 maps to `None` (no caching). From
the arms above it, the only `Err` variants not enumerated are
`ProviderError::Auth` and any other `ProviderError` variant not listed; the
`Ok` arms not matched are 2xx-non-success (none, since `is_success()` covers
2xx) and non-2xx other than 401/403. So a 429 / 500 / 502 / 503 from
`fetch_models_response` (hedged — I did not read it, so I cannot confirm which
`Err` variant a transport-level 5xx becomes, if any) lands in the
`status != UNAUTHORIZED && != FORBIDDEN` arm at lines 1251-1254 and is
negatively cached for 5s. That is fine, but note it means a flaky 503 is
treated identically to a 400 — no distinction is made for retriable server
errors vs. permanent client errors. This is a reasonable simplification but
worth a one-line comment.

**Proposal (lowest-risk):** add a comment at line 1249 documenting the
two-tier intent — "success or transient/structural failure: negative-cache;
auth (401/403) and unmatched: never cache" — so the `match` reads as a
deliberate policy rather than a list of cases. Optionally, if any caller
needs the raw body on validation failure (verify — hedged), consider
returning the original `text` alongside the `InvalidResponse` via a typed
carrier rather than discarding it. Ranked P3 because the current behavior is
defensible; the gap is legibility and a possible caller need I cannot verify.

---

### Ranking

1. **P1** — unconditional empty-`HashMap` allocation on every negative-cache
   refresh (line 1267); positive-cache memo discard across refresh. Hot path
   (5s cadence). *Hedged on `model_context_length` read frequency.*
2. **P2** — auth-before-cache ordering is correct but fragile under refactor;
   cache-hit path pays `auth.header()` cost every call. *Hedged on
   `AuthProvider::header` semantics and cost.*
3. **P3** — 200-but-invalid catalog is cached as kitsu's error verdict, not
   the server body; non-2xx (except 401/403) all map to a flat 5s negative TTL
   with no retriable/permanent distinction. *Hedged on caller needs and on
   `fetch_models_response`'s error variants.*