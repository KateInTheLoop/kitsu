# Architecture — Multi-Gateway Design

Status: proposed. Targets the architecture in `design.md` (Provider trait §5, tool loop §6)
and the locked dependency table in `development_plan.md`.

> The multi-gateway layer lets one `kitsu` binary plug into
> many model gateways (Anthropic native, OpenAI/OpenAI-compatible, Mistral, the example example
> AI Gateway) selected by a named **gateway profile**.

---

## 0. Why one binary can match all backends

The backends differ along three axes only: **wire shape**, **auth**, and **endpoint/transport**.

| Backend | Wire shape | Auth | Endpoint | Streaming |
|---|---|---|---|---|
| vLLM | OpenAI chat/completions | usually none | `localhost:8000/v1/chat/completions` | SSE `data:`, `[DONE]` |
| Ollama | **own NDJSON shape** | none | `localhost:11434/api/chat` | NDJSON, `"done":true` |
| OpenAI | OpenAI chat/completions | `Authorization: Bearer <key>` | `api.openai.com/v1/chat/completions` | SSE `data:`, `[DONE]` |
| gateway | **OpenAI chat/completions** | `Authorization: Bearer <JWT>` (24h) | `…gateway.example.com/v1/chat/completions` | SSE `data:`, `[DONE]` |
| Anthropic native | **different** (top-level system, content blocks, tool_use/tool_result) | `x-api-key` + `anthropic-version` | `api.anthropic.com/v1/messages` | typed SSE events |

Groupings that follow:

- **OpenAI-compatible family** (vLLM, OpenAI, Mistral-cloud, **gateway**) — same JSON and SSE;
  differ only in `base_url` + endpoint path + which header carries the token → **configuration, not code.**
- **Ollama** — same concepts, different NDJSON envelope → its own small adapter (already in `design.md`).
- **Anthropic native** — genuinely different shape → its own adapter (post-MVP; Claude is also reachable
  through the example OpenAI-compatible route, so MVP can skip it).

A thin internal representation (`ChatMessage`/`ToolCall`) that every backend translates to/from at its
edges is what makes "one binary" real: the agent loop only ever sees the internal types.

---

## 1. How this fits the existing Provider trait

### Decision

Introduce a **gateway profile** config concept layered on top of the existing providers, and
generalize the existing `OpenAICompatProvider` to take a configurable `base_url`, endpoint
path, and an injected `AuthProvider`. Add a separate `AnthropicProvider` only for the
Anthropic-native flavor (post-MVP). Do **not** create one new provider type per gateway.

So the mapping is:

- `flavor = "openai"` → `OpenAICompatProvider` (vLLM, OpenAI, Mistral via OpenAI-compat,
  **and the gateway** — they are all the same wire shape)
- `flavor = "anthropic"` → `AnthropicProvider` (native `/v1/messages`; post-MVP)
- `flavor = "ollama"` → existing `OllamaProvider` (unchanged)

A **gateway profile** is a named bundle of `{ base_url, endpoint, auth, default model, flavor }`.
`build_provider(profile)` reads the profile and constructs the right provider impl with the
right transport config. The `Provider` trait signature is **unchanged** — profiles are a
construction-time concern, not a trait concern.

### Justification

- The gateway, OpenAI, vLLM, and Mistral all speak the same `chat/completions` JSON. The
  only differences are `base_url`, the endpoint path, and the auth header. Those are *transport
  configuration*, not behavioral differences — so they belong as fields on the existing adapter,
  not as new trait implementations.
- Anthropic-native genuinely differs in request/response shape (see §4), so it earns its own
  impl — but it is the same `Provider` trait, selected by flavor.
- The "profile" concept gives the user the `--gateway <name>` UX (one tool, many gateways)
  without coupling that selection to the provider taxonomy, and keeps existing Phase 1/2 work intact.

### Trait shape (reuses design.md §5.1, no signature change)

```rust
// kitsu-providers
pub trait Provider {
    async fn healthcheck(&self) -> Result<(), ProviderError>;
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError>;
    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError>;
}
```

`OpenAICompatProvider` gains construction-time fields (not new trait methods):

```rust
pub struct OpenAICompatProvider {
    http: reqwest::Client,
    base_url: String,            // e.g. https://gateway.example.com
    chat_path: String,           // e.g. /v1/chat/completions  (default /v1/chat/completions)
    models_path: Option<String>, // e.g. /v1/models; None => healthcheck/list_models degrade gracefully
    auth: Arc<dyn AuthProvider>, // see §3
    retries: u32,
    timeout: Duration,
}
```

---

## 2. Config model (TOML)

A new `[gateways.<name>]` set of named profiles, plus a top-level `default_gateway`. This sits
alongside the existing `[provider]` config (which stays for the local-first Ollama/vLLM default).
Gateway precedence is explicit `--gateway`, then `KITSU_GATEWAY`, then
`default_gateway`. Model precedence is explicit `--model`, then `KITSU_MODEL`,
then the selected profile's model; interactive `/model` may replace that
effective model for later turns. A selected profile supplies `base_url`, model,
flavor, endpoint, and auth.

```toml
# ~/.kitsu/config.toml

default_gateway = "example"        # optional; used when --gateway is omitted and a gateway flow is invoked

# ---- AI gateway (OpenAI-compatible, Mistral via Bedrock) ----
[gateways.example]
flavor       = "openai"        # openai | anthropic | ollama
base_url     = "https://gateway.example.com"
chat_path    = "/v1/chat/completions"
models_path  = ""              # empty => no /models endpoint; healthcheck uses a cheap probe (§3)
model        = "example-model-v1"
auth         = { method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN", expires_env = "ANTHROPIC_TOKEN_EXPIRES", refresh_hint = "run `refresh-token` to refresh the gateway JWT" }

# ---- Anthropic native (post-MVP flavor; profile shape shown for completeness) ----
[gateways.anthropic]
flavor       = "anthropic"
base_url     = "https://api.anthropic.com"
chat_path    = "/v1/messages"
model        = "claude-sonnet-4-5"
auth         = { method = "bearer-env", env = "ANTHROPIC_API_KEY", header = "x-api-key", scheme = "" }
# anthropic-native also needs the version header (set by the AnthropicProvider, see §4)

# ---- Public OpenAI ----
[gateways.openai]
flavor       = "openai"
base_url     = "https://api.openai.com"
chat_path    = "/v1/chat/completions"
models_path  = "/v1/models"
model        = "gpt-4o"
auth         = { method = "bearer-env", env = "OPENAI_API_KEY" }

# ---- Mistral native cloud (OpenAI-compatible) ----
[gateways.mistral]
flavor       = "openai"
base_url     = "https://api.mistral.ai"
chat_path    = "/v1/chat/completions"
models_path  = "/v1/models"
model        = "mistral-large-latest"
auth         = { method = "bearer-env", env = "MISTRAL_API_KEY" }
```

### Auth method variants

`auth.method` is one of:

- `"bearer-env"` — use a non-empty token from `auth.env` (default name
  `ANTHROPIC_AUTH_TOKEN`), otherwise the config-scoped managed credential
  installed by `kitsu auth login`. Sent as `Authorization: Bearer <token>`
  unless `auth.header`/`auth.scheme` override it.
  Optional `auth.expires_env` (default `ANTHROPIC_TOKEN_EXPIRES`) and `auth.refresh_hint`.
- `"static"` — `auth.token = "..."` inline. **Discouraged**; loader emits a warning and the
  value is redacted in `--print-config`. Never recommended for the example JWT.
- `"none"` — no auth header (local vLLM/Ollama through a gateway with no auth).

### Header override fields (for non-Bearer schemes, e.g. Anthropic `x-api-key`)

- `auth.header` — header name (default `Authorization`)
- `auth.scheme` — prefix before the token (default `Bearer`; set to `""` for raw `x-api-key`)

### Validation rules (fail fast at invocation, mirroring `development_plan.md` Gap 3)

- `--gateway <name>` referencing an undefined profile → exit 1:
  `config error: unknown gateway 'foo' — defined gateways: example, anthropic, openai, mistral`
- `flavor` not in {`openai`, `anthropic`, `ollama`} → exit 1.
- `base_url` must be absolute `https://` (warn, not error, on plain `http://`).
- `model` resolution order: `--model` flag > `KITSU_MODEL` >
  `gateways.<name>.model` > error
  (`provider.model is required — pass --model <name>, set KITSU_MODEL, or
  configure the gateway model`), consistent with Gap 3.
- `auth.token` (static) is **redacted** as `"***"` in `--print-config`; env-based tokens are
  never read at config-print time.

---

## 3. Auth handling

### `AuthProvider` abstraction (in `kitsu-providers`)

```rust
pub trait AuthProvider: Send + Sync {
    /// Returns (header_name, header_value) to attach, or an actionable error.
    fn header(&self) -> Result<(HeaderName, HeaderValue), AuthError>;
    /// Best-effort expiry check; called before each request for a warning, never fatal on its own.
    fn expiry_status(&self) -> ExpiryStatus; // Valid { until } | Expired { since } | Unknown
}
```

Implementations: `EnvBearerAuth { env, expires_env, header, scheme, refresh_hint }`,
`StaticAuth { token, header, scheme }`, `NoAuth`.

### Reading the token

On each request, `EnvBearerAuth::header()`:

1. `std::env::var(&self.env)` (default `ANTHROPIC_AUTH_TOKEN`).
2. Missing or empty → `AuthError::Missing { env, hint }`:
   `auth error: $ANTHROPIC_AUTH_TOKEN is not set — run ` + "`refresh-token`" + ` to obtain a gateway token`
   (hint text comes from `auth.refresh_hint` when present).
3. Build `HeaderValue` from `format!("{} {}", scheme, token)` (scheme `""` ⇒ raw token).
   The `HeaderValue` is marked **sensitive** (`set_sensitive(true)`) so `reqwest`/`tracing`
   never logs it.

### Expiry awareness

`expiry_status()` uses, in priority order:

1. **`ANTHROPIC_TOKEN_EXPIRES`** (env, default `auth.expires_env`) — an RFC 3339 timestamp
   such as `2026-06-18T10:33:47+0900`. Parse with `chrono::DateTime::parse_from_rfc3339`
   (chrono is already a locked dep). This is the **primary, reliable** source — confirmed from
   the user's `~/.zshrc`, where `refresh-token` writes it.
2. **JWT `exp` claim fallback** — only if `expires_env` is absent *and* the token has three
   dot-separated segments. Base64url-decode the middle segment, read `exp` (Unix seconds).
   **No `jsonwebtoken` dependency** — plain `base64` + `serde_json` parse; signature is never
   verified (the gateway does that). Non-JWT token → returns `Unknown` (do not error).
3. Otherwise `Unknown`.

Behavior on status:

- `Expired { since }` → emit a **non-fatal warning** to stderr before the request, then send
  anyway (clock skew + gateway grace windows mean the local timestamp can be wrong; the gateway
  is the source of truth).
- A `401`/`403` from the gateway → `ProviderError::Unauthorized` with the refresh hint appended.
  Exit code maps to provider error (2).
- `Unknown` / `Valid` → no warning.

### Healthcheck when there is no `/models` endpoint

The example `models_path` is empty. `healthcheck()` degrades to: check the env token is present and
(if known) not expired, and confirm `base_url` resolves/TCP-connects. It does **not** spend a chat
call. `kitsu doctor` shows gateway name, base_url, masked token presence
(`token: present (expires 2026-06-18T10:33Z)` — value never printed), and flavor.

### Security invariants

- Environment credentials are read at request time. Managed credentials are
  bounded owner-only files in config-scoped private state, atomically replaced,
  never written to the repository, and never exposed to model tools or command
  roots. Neither source is written to the audit log, `--json`, config output, or
  tracing (`design.md` §7.3 — redact `Authorization`).
- `--json` output and error messages must never echo the token; tests assert this (§7).

---

## 4. Request/response mapping

### OpenAI-compatible flavor (MVP — covers example + Mistral + OpenAI + vLLM)

Request `POST {base_url}{chat_path}`:

```json
{
  "model": "example-model-v1",
  "messages": [
    {"role": "system",    "content": "..."},
    {"role": "user",      "content": "hello"},
    {"role": "assistant", "content": "...", "tool_calls": [ ... ]},
    {"role": "tool",      "tool_call_id": "call_1", "content": "<json result>"}
  ],
  "tools": [ {"type":"function","function":{"name":"fs.read_file","parameters":{}}} ],
  "response_format": {
    "type": "json_schema",
    "json_schema": {"name":"kitsu_output","strict":true,"schema":{}}
  },
  "stream": true,
  "temperature": 0.2
}
```

Internal `ChatMessage` (`design.md` §4.1) maps directly. Response (streaming SSE, `data:` lines,
`data: [DONE]` terminator — same parser as the Phase 1 vLLM path):
`choices[].delta.content` → text fragments; `choices[].delta.tool_calls[]` (incremental, by index)
→ `ToolCall`; `choices[].finish_reason`: `stop`→`Stop`, `length`→`Length`, `tool_calls`→`ToolCalls`.
Non-streaming fallback (`stream:false`) selected by `--no-stream`.
`response_format` is omitted unless the caller supplied `--output-schema`.
The orchestrator budgets its exact serialized bytes, while the CLI independently
validates the final assistant text before commit; provider enforcement is a
quality/control layer, never the only acceptance check.

### Anthropic-native flavor (post-MVP — documented for honesty)

Differs enough to justify the separate `AnthropicProvider`: endpoint `POST /v1/messages`; headers
`x-api-key` + `anthropic-version`; `system` is a top-level field; content is a list of blocks;
tool results sent back as a `user` message with `tool_result` blocks keyed by `tool_use_id`;
typed SSE event stream. It implements the **same `Provider` trait** and the **same internal types**,
translating at the edges so the orchestrator never sees the difference. MVP does not build it.

---

## 5. Runtime agent loop (the core)

Reuses the orchestrator tool loop in `design.md` §6 — the same loop, now driven through a
gateway-backed `Provider`. No second loop is introduced.

### Termination conditions

1. Model returns `finish_reason = Stop` (or `Length`) with no tool calls → final answer.
2. `max_rounds` cap reached (default **10**, config `agent.max_rounds`) → stop, append
   `[stopped: max tool rounds reached]`.
3. Token/byte **budget** exceeded (`agent.max_context_bytes`, default 128 KiB) → stop with
   `[stopped: context budget reached]`.
4. Non-recoverable `ProviderError` (timeout after retries, `Unauthorized`, 5xx after retries) →
   propagate, exit 2. (Per Gap 4: only `ToolsNotSupported` triggers text-protocol fallback.)

### Error-as-tool-result feedback

A tool that fails (policy denial, not found, malformed args, `rg` missing) does **not** abort the
loop. The error is serialized as the tool result content and appended as a `tool` message, e.g.
`{"error":"PolicyDenied","detail":"path /etc/passwd outside allowed_paths"}`, logged with
`status:"denied"|"error"` (`design.md` §7.3).

### Context growth management

Before each round, sum message byte sizes. If over `max_context_bytes`, truncate oldest
tool-result messages first, inserting `[older tool output elided]`. Never drop the system prompt or
the latest user turn. (Summarization is post-MVP.) `max_rounds` is the hard backstop.

### Pseudocode

```
fn run_agent_turn(provider, tools, mut messages, cfg) -> Result<String> {
    let mut round = 0;
    loop {
        round += 1;
        if round > cfg.max_rounds { messages.note("[stopped: max tool rounds reached]"); break }

        messages = enforce_context_budget(messages, cfg.max_context_bytes); // §5 truncation

        if let Expired{since} = provider.auth_expiry_status() { warn_expired(since, cfg.refresh_hint) }

        let req = ChatRequest { model: cfg.model, messages, tools: Some(tools.descriptors()),
                                stream: !cfg.no_stream, .. };
        let mut stream = provider.chat_stream(req).await?;   // 401/403 => Unauthorized (hard, hint)
        let (assistant_text, tool_calls, finish) = render_and_collect(stream).await?;

        messages.push(assistant(assistant_text, tool_calls.clone()));

        match finish {
            Stop | Length if tool_calls.is_empty() => return Ok(assistant_text),  // (1) done
            ToolCalls | (_ if !tool_calls.is_empty()) => {
                for tc in tool_calls {
                    let result = match tools.validate_and_run(&tc).await {   // policy + exec
                        Ok(out)  => out,                                     // JSON string
                        Err(e)   => json!({ "error": e.kind(), "detail": e.detail() }).to_string(),
                    };
                    audit_log(&tc, &result);                                 // token never logged
                    messages.push(tool_msg(tc.call_id, result));            // error-as-result feedback
                }
                continue;                                                    // next round
            }
            _ => return Ok(assistant_text),
        }
    }
    Ok(messages.last_assistant_text())
}
```

`kitsu ask` runs this once per prompt (single turn, multi-round tool calls within it).
`kitsu chat` runs it per user message, with `messages` persisted across turns.

---

## 6. MVP scope — `kitsu ask --gateway example "hello"`

Smallest thing that proves value: authenticate with the env JWT and get a Mistral completion
through `/v1/chat/completions`. **No tools required for MVP** (tools are Phase 4; the
agent loop degenerates to one round when `tools` is empty).

### Exactly what changes

`crates/kitsu` (binary):
- `cli.rs` — add global flag `--gateway <name>`.
- `config.rs` — parse `[gateways.<name>]` + `default_gateway`; resolution/validation (§2).
- wiring — `build_provider`: when `--gateway` set, select profile → construct
  `OpenAICompatProvider` with `base_url`, `chat_path`, and `EnvBearerAuth`.

`crates/kitsu-providers`:
- `auth.rs` (new) — `AuthProvider` trait, `EnvBearerAuth`, `StaticAuth`, `NoAuth`, `AuthError`,
  `ExpiryStatus`, RFC3339 + optional JWT-`exp` decode.
- `openai_compat.rs` — add `base_url`/`chat_path`/`models_path`/`auth` fields; attach auth header
  per request; degrade `healthcheck` when `models_path` empty; map 401/403 →
  `ProviderError::Unauthorized`.
- `error.rs` — add `ProviderError::Unauthorized`.

`crates/kitsu-core`:
- The agent loop (§5) — for MVP the single-round path needed by `ask`; full multi-round tool loop
  lands with Phase 4 but the function shape is introduced here.

`crates/kitsu-render`: **no change** (reuses streaming SSE renderer from Phase 1).

`Cargo.toml`: add `base64` to `kitsu-providers` only if the JWT-`exp` fallback is kept (RM approval
required). `chrono` already present.

### MVP acceptance

`kitsu ask --gateway example "hello"`:
- reads `ANTHROPIC_AUTH_TOKEN`, warns if `ANTHROPIC_TOKEN_EXPIRES` is past,
- POSTs to `…/v1/chat/completions` with `model = example-model-v1`,
- streams the completion, exit 0.
- Missing token → exit 1 with `refresh-token` hint. Gateway 401 → exit 2 with refresh hint.

---

## 7. Test plan (TDD, per `agent-roles.md`)

### Tester — wiremock unit tests (`kitsu-providers`)

- `test_gateway_openai_chat_stream_ok` — wiremock at fake base_url + `/v1/chat/completions`,
  matcher asserts `Authorization: Bearer test-token`; SSE returns two fragments + `stop` → `"hello world"`.
- `test_gateway_auth_header_attached` — request matcher requires the bearer header.
- `test_gateway_401_maps_unauthorized` — 401 stub → `ProviderError::Unauthorized` with refresh hint.
- `test_gateway_no_models_path_healthcheck` — `models_path=""` → no `/models` hit, succeeds on token present.
- `test_auth_missing_env` — env unset → `AuthError::Missing` with `refresh-token` hint.
- `test_expiry_rfc3339_expired` / `_valid` — past/future `expires_env` → `Expired`/`Valid`.
- `test_expiry_jwt_exp_fallback` — unsigned JWT with past `exp` → `Expired`; UUID token → `Unknown`.
- `test_token_never_logged` — capture `tracing`; assert token absent; audit line redacts `Authorization`.

### Tester — `assert_cmd` CLI tests (`crates/kitsu`)

- `kitsu ask --gateway example "hello"` against wiremock-backed profile (temp config), `ANTHROPIC_AUTH_TOKEN=test-token`
  → prints response, exit 0.
- token unset → exit 1, stderr contains `refresh-token`.
- `kitsu doctor` → `token: present`, never the value. `--print-config` → static tokens `***`.

### Avoiding real-token leakage in tests

- Tests never read the real `ANTHROPIC_AUTH_TOKEN`; each sets a dummy value in a scoped, serialized
  env guard (`#[serial]`) and points `base_url` at local wiremock via a `tempfile` config — no request
  leaves the machine. `predicates::str::contains(token).not()` guards stdout/stderr/audit-log redaction.

### Integration Tester — one live check

- `tests/integration/` script: with a fresh `refresh-token`, run `kitsu ask --gateway example "Say hello in one word."`
  against the real gateway; assert exit 0 and non-empty output. Reads token from live env only; never prints
  or commits it. `#[ignore]`/manually-gated so CI never needs a real token.

---

## Open questions (to resolve before/at build)

1. **`base64` dependency** for the JWT-`exp` fallback (needs RM sign-off), OR rely only on
   `ANTHROPIC_TOKEN_EXPIRES` (zero new deps, since your `refresh-token` always sets it).
2. **Profile vs `[provider]` precedence** — `--gateway` fully overrides (chosen) vs merge.
3. **Expired token** — warn-and-send (chosen) vs hard-fail locally.
4. **Companion docs** (`config-schema.md`, `types.md`, …) cited by `design.md` don't exist yet;
   this doc is self-contained but should be reconciled with them later.
