# Cycle-8 Review — `EnvBearerAuth::header()`

**Scope:** `crates/kitsu-providers/src/auth.rs` lines 59–95 (primary `header()`), with
context from the trait (lines 14–18), the struct + `new()` (lines 32–56), and
`AuthError` (`crates/kitsu-providers/src/error.rs` lines 62–72). No callers, no
match sites on `AuthError`, and no `Cargo.toml` were read; claims that depend on
those are hedged below.

## Evidence snapshot (exact lines read)

- auth.rs:62 — `        let token = match std::env::var(&self.env) {`
- auth.rs:63 — `            Ok(v) if !v.is_empty() => v,`
- auth.rs:64–69 — the `_` arm: `return Err(AuthError::Missing { env: self.env.clone(), hint: self.refresh_hint.clone() })`
- auth.rs:73 — `        let name = HeaderName::from_bytes(self.header.to_lowercase().as_bytes()).map_err(|_| {`
- auth.rs:74–76 — `            AuthError::Malformed { env: self.env.clone() }`
- auth.rs:81 — `        let raw = format!("{} {}", self.scheme, token);`
- auth.rs:82–86 — trim leading space only when `self.scheme.is_empty()`
- auth.rs:88–90 — `HeaderValue::from_str(trimmed).map_err(|_| AuthError::Malformed { env: self.env.clone() })?;`
- auth.rs:92 — `        value.set_sensitive(true);` (satisfies the trait contract at auth.rs:17)
- error.rs:64 — `Missing` Display: `"auth error: ${env} is not set{}"`
- error.rs:70 — `Malformed` Display: `"auth error: token in ${env} is malformed"`
- error.rs:65 / 71 — `Missing { env, hint }` / `Malformed { env }` both carry only `env`.

## Proposals (ranked)

### 1. `VarError::NotUnicode` is misreported as `Missing` — **highest priority**

At auth.rs:62 the match is on `std::env::var(&self.env)`, whose `Err` is either
`VarError::NotPresent` or `VarError::NotUnicode(_)`. The `_` arm (auth.rs:64–69)
collapses both into `AuthError::Missing`. But `Missing`'s Display (error.rs:64)
says `"${env} is not set"`, which is **false** for `NotUnicode`: the variable *is*
set, it just holds non-UTF-8 bytes (e.g. a token file read with a bad locale, or
a binary value). The user is told to "set" a variable that is already present,
hiding the real fix (encoding / how the env was exported).

**Fix:** split the arm —
`Err(std::env::VarError::NotPresent) => AuthError::Missing { .. }` and
`Err(std::env::VarError::NotUnicode(_)) => AuthError::Malformed { env: self.env.clone() }`
(or a dedicated variant). The empty/`is_empty` guard at auth.rs:63 is unchanged.

**Hedge:** I did not read any caller that matches on `AuthError::Missing`
specifically (e.g. to print "set your token" guidance). Moving the
`NotUnicode` case off `Missing` changes which branch such a caller hits — but
the `Malformed`/dedicated message is strictly more accurate, so it should be an
improvement, not a regression. I also did not confirm `std::env::VarError` is
imported; the fix can qualify it inline as shown.

### 2. `Malformed` blames `env` for failures that are not the env token

`AuthError::Malformed { env }` is emitted at two sites that do **not** fail on
the env token:

- auth.rs:73–77: the failing input is `self.header` (the header **name**), yet
  the error is `Malformed { env: self.env.clone() }` and Display (error.rs:70)
  says `"token in ${env} is malformed"`. A bad `header` (e.g. containing a space)
  is blamed on the bearer token.
- auth.rs:88–90: the failing input is `trimmed` = `"{scheme} {token}"`. The
  failure can be caused by an invalid `self.scheme` (e.g. non-ASCII or control
  bytes), not the token — yet it is reported as the env token being malformed.

**Fix:** add a discriminator to `Malformed` (e.g. `field: &'static str`, or
variants `MalformedHeader { header }` / `MalformedScheme`) and emit the
header-name failure at auth.rs:73 and the value failure at auth.rs:88 with the
correct source. If a token truly is the bad input at auth.rs:88, keep `env`;
otherwise attribute it to `scheme`.

**Hedge:** `AuthError` (error.rs:62–72) is the only part of `error.rs` I read; I
did not read other match arms on `Malformed` elsewhere in the crate, so
adding/renaming variants may require touching those sites. Also, `new()`
(auth.rs:51–52) only ever sets `header = "Authorization"` and `scheme = "Bearer"`
— both valid — so in the default-config path neither site can fire; this only
affects callers that customize the `pub` fields (auth.rs:33–37), whose code I did
not read.

### 3. Token surrounding whitespace is not trimmed → silent 401s

The token is read at auth.rs:62 and only an `is_empty` guard is applied
(auth.rs:63); the value is otherwise used verbatim. The trim logic at
auth.rs:82–86 removes **only the single space inserted by `format!`** when
`scheme` is empty — it never trims the token itself. Consequences:

- A trailing space or tab in the env value passes `is_empty` and is accepted by
  `HeaderValue::from_str` (auth.rs:88, which permits SP and TAB), producing a
  header like `Bearer <token>\t` that gateways reject with 401 — indistinguishable
  from a wrong token.
- A leading tab in the Bearer case yields `Bearer \t<token>`, also accepted and
  sent.
- A trailing `\n`/`\r` is rejected by `from_str` → the (mislabeled) `Malformed`
  of proposal 2, not a clear "whitespace in your token" message.

**Fix:** trim the token *before* the empty check, so an all-whitespace value
still reports `Missing` and a real token loses surrounding whitespace:
`let v = std::env::var(&self.env)?; let v = v.trim(); if v.is_empty() { Missing }
else { v }` (then use `v` in the `format!` at auth.rs:81).

**Hedge:** I did not read the set of auth schemes callers configure via the
`pub` fields. A scheme whose token legitimately carries leading/trailing
whitespace (extremely unlikely for RFC 6750 bearer tokens or typical API keys,
which are whitespace-free) would be altered. The comment at auth.rs:60–61 only
calls out "missing or empty," which does not imply verbatim whitespace must be
preserved; trimming is a robustness improvement, not a contract change.

### 4. (low) Secret hygiene — token-bearing `String`s are not zeroized

`token` (auth.rs:62), `raw` (auth.rs:81), and the `format!` temporaries hold the
secret on the heap and are dropped (not zeroized) on return. The module doc
(auth.rs:1–2) promises "never logged, never persisted" but **not** zeroization,
so this is hardening rather than a contract violation. If desired, wrap these
locals in `zeroize::Zeroizing<String>` (and consider it for the env read).

**Hedge:** I did not read `Cargo.toml`, so I cannot confirm `zeroize` is already
a dependency; adding it has build/license implications I did not verify. Low
priority for a per-request CLI path where the env itself holds the secret for
the process lifetime anyway.

## Notes (not proposals)

- auth.rs:73 `self.header.to_lowercase()` is redundant — `HeaderName` normalizes
  to lowercase and accepts mixed case (the comment at auth.rs:72 says so). It
  costs one allocation and changes nothing behaviorally; left as-is is fine.
- auth.rs:92 `value.set_sensitive(true)` correctly satisfies the trait contract
  at auth.rs:17 ("MUST be marked sensitive"). No change needed.