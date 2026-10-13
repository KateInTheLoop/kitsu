//! Config model + loading (docs/architecture.md §2). Parses the
//! `[gateways.<name>]` profiles and `default_gateway`, resolves a profile by
//! name, and validates flavor/model per the design's rules.

#[allow(dead_code)] // Private local/offline coordinator; product mounting is a later card.
mod local_setup;
#[allow(dead_code)] // Private pure model-picker staging; product mounting is a later card.
mod model_picker;
#[allow(dead_code)] // Staged pure guided-setup transaction; product mounting is a later card.
mod onboarding;
#[allow(dead_code)] // Private create-new adapter; product mounting is a later card.
mod publication;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

/// Top-level config file shape.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub default_gateway: Option<String>,
    /// Register the model-callable `workspace_diff` evidence tool in chat when
    /// the launch directory is a Git worktree (docs/workspace-diff-tool.md).
    /// `false` disables registration entirely, like `--no-workspace-evidence`.
    #[serde(default)]
    pub workspace_evidence: Option<bool>,
    /// Permit Kitsu-created automatic command-output and oversized tool-result
    /// artifacts. `false` is a one-way privacy default; a run may disable the
    /// feature again with `--no-artifacts`, but no CLI flag silently overrides
    /// a config-level disable.
    #[serde(default)]
    pub artifact_storage: Option<bool>,
    /// Per-turn cap on subagent spawns across `spawn_agent`, `spawn_agents`,
    /// and `spawn_agent_bg`. Spawns beyond the cap return a tool error the
    /// model sees. Unset: 8 in interactive chat, 2 in scripted/piped
    /// automation (small local models storm agents on trivial turns).
    /// `--agent-spawn-cap` overrides; 0 disables spawning for the run and
    /// unregisters all multi-agent tools.
    #[serde(default)]
    pub agent_spawn_cap: Option<u32>,
    /// Default color theme for styled output (see `/theme` for the list).
    /// `--theme` overrides per run; `--plain` disables styling entirely.
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub gateways: BTreeMap<String, Gateway>,
    /// `--gateway local` with no `[gateways.local]` profile: the first local
    /// server found on a well-known port and the models it lists, probed once
    /// on first use.
    #[serde(skip)]
    detected_local: OnceLock<Option<(Gateway, Vec<String>)>>,
    /// Optional `[agents]` table. Absent is the same as every key at its
    /// default, which is why callers read it as `agents.unwrap_or_default()`
    /// rather than being handed a silently materialized table.
    #[serde(default)]
    pub agents: Option<AgentsConfig>,
}

/// When the per-process agent state file is written (docs/architecture.md).
/// `Auto` is the default: only a real interactive session writes, so scripted,
/// piped and JSONL runs leave no state behind unless asked to.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StateFileMode {
    #[default]
    Auto,
    On,
    Off,
}

/// The `[agents]` table. Unknown keys are rejected loudly: `agent_spawn_cap`
/// deliberately stays a flat top-level key, so a copy of it in here is a
/// mistake and must not be silently ignored.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    #[serde(default)]
    pub state_file: StateFileMode,
}

/// Well-known loopback ports of OpenAI-compatible local runtimes, probed in
/// order by the zero-config `local` gateway.
const LOCAL_SERVERS: &[(u16, &str)] = &[
    (11434, "Ollama"),
    (1234, "LM Studio"),
    (8080, "llama.cpp"),
    (8000, "vLLM"),
];

/// A single named gateway profile. Unknown keys are rejected loudly: a typo
/// like `token_env` (the real key is `auth.env`) silently changing behavior
/// cost a cycle-14 audit journey its credentials.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayAdapter {
    #[default]
    CompatibleChat,
    AnthropicMessages,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gateway {
    pub flavor: String,
    /// Closed compiled wire selection. Legacy flavor-only profiles retain the
    /// compatible Chat adapter; native Messages requires this explicit value.
    #[serde(default)]
    pub adapter: GatewayAdapter,
    pub base_url: String,
    #[serde(default = "default_chat_path")]
    pub chat_path: String,
    #[serde(default)]
    pub models_path: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// The model's context window in tokens, if known. Lets kitsu budget context
    /// against the real window. Overridden by `--context-window`; falls back to
    /// `/v1/models` discovery then a conservative default when unset.
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub auth: AuthConfig,
}

fn default_chat_path() -> String {
    "/v1/chat/completions".to_string()
}

/// Auth block within a gateway profile.
///
/// `Debug` is implemented manually to redact credential-bearing free-form
/// fields. An inline static token or a token accidentally pasted into a refresh
/// hint must never leak via `{:?}` / `debug!(?config)`. `Gateway` and `Config`
/// derive `Debug` and delegate here.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    #[serde(default = "default_auth_method")]
    pub method: String,
    #[serde(default = "default_auth_env")]
    pub env: String,
    #[serde(default = "default_expires_env")]
    pub expires_env: Option<String>,
    #[serde(default)]
    pub refresh_hint: Option<String>,
    #[serde(default = "default_header")]
    pub header: String,
    #[serde(default = "default_scheme")]
    pub scheme: String,
    /// Inline static token (method = "static"). Discouraged.
    #[serde(default)]
    pub token: Option<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            method: default_auth_method(),
            env: default_auth_env(),
            expires_env: default_expires_env(),
            refresh_hint: None,
            header: default_header(),
            scheme: default_scheme(),
            token: None,
        }
    }
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact the static token: show only whether one is present, never its
        // value. All other fields are non-sensitive (env var *names*, not values).
        let token = match &self.token {
            Some(_) => "Some(\"<redacted>\")",
            None => "None",
        };
        let refresh_hint = match &self.refresh_hint {
            Some(_) => "Some(\"<redacted>\")",
            None => "None",
        };
        f.debug_struct("AuthConfig")
            .field("method", &self.method)
            .field("env", &self.env)
            .field("expires_env", &self.expires_env)
            .field("refresh_hint", &format_args!("{refresh_hint}"))
            .field("header", &self.header)
            .field("scheme", &self.scheme)
            .field("token", &format_args!("{token}"))
            .finish()
    }
}

fn default_auth_method() -> String {
    "bearer-env".to_string()
}
fn default_auth_env() -> String {
    // Neutral, provider-agnostic default. A profile overrides this with its own
    // `env = "..."` (e.g. OPENAI_API_KEY, HF_TOKEN, ANTHROPIC_API_KEY).
    "KITSU_TOKEN".to_string()
}
fn default_expires_env() -> Option<String> {
    Some("KITSU_TOKEN_EXPIRES".to_string())
}
fn default_header() -> String {
    "Authorization".to_string()
}
fn default_scheme() -> String {
    "Bearer".to_string()
}

#[derive(Debug)]
pub enum ConfigError {
    Read {
        path: String,
        source: std::io::Error,
    },
    /// A TOML parse failure. Only location metadata is retained: the parser's
    /// formatted error embeds the offending source line, which may contain a
    /// credential from a malformed assignment.
    Parse {
        line: Option<usize>,
        column: Option<usize>,
        /// Field-name-level detail that is safe to echo (serde's
        /// "unknown field `x`, expected one of …" names keys, never values).
        /// Everything else stays withheld.
        hint: Option<String>,
    },
    UnknownGateway {
        name: String,
        defined: String,
    },
    NoGateway,
    NoLocalServer,
    AmbiguousLocalModel {
        model: String,
        matches: String,
    },
    UnsupportedFlavor {
        flavor: String,
    },
    UnsupportedAuthMethod {
        method: String,
    },
    AuthEnvRequired,
    StaticTokenRequired,
    ModelRequired,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read { path, source } => {
                write!(f, "config error: cannot read {path}: {source}")
            }
            ConfigError::Parse {
                line: Some(line),
                column: Some(column),
                hint,
            } => {
                write!(
                    f,
                    "config error: TOML parse failed at line {line}, column {column}"
                )?;
                match hint {
                    Some(hint) => write!(f, ": {hint}"),
                    None => write!(f, " (source excerpt withheld to avoid echoing credentials)"),
                }
            }
            ConfigError::Parse { hint: Some(hint), .. } => {
                write!(f, "config error: TOML parse failed: {hint}")
            }
            ConfigError::Parse { .. } => write!(
                f,
                "config error: TOML parse failed (source excerpt withheld to avoid echoing credentials)"
            ),
            ConfigError::UnknownGateway { name, defined } => write!(
                f,
                "config error: unknown gateway '{name}' — defined gateways: {defined}"
            ),
            ConfigError::NoGateway => write!(
                f,
                "config error: no gateway selected — pass --gateway <name>, set KITSU_GATEWAY, or set default_gateway"
            ),
            ConfigError::NoLocalServer => {
                let probed = LOCAL_SERVERS
                    .iter()
                    .map(|(port, runtime)| format!("{runtime} :{port}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "config error: gateway 'local' found no running local server with models (probed {probed}) — start one, or define [gateways.local]"
                )
            }
            ConfigError::AmbiguousLocalModel { model, matches } => write!(
                f,
                "config error: model '{model}' matches several local models: {matches} — pass the full id"
            ),
            ConfigError::UnsupportedFlavor { flavor } => write!(
                f,
                "config error: unsupported flavor '{flavor}' — expected one of: openai, anthropic, ollama"
            ),
            ConfigError::UnsupportedAuthMethod { method } => write!(
                f,
                "config error: unsupported auth method '{method}' — expected one of: bearer-env, static, none"
            ),
            ConfigError::AuthEnvRequired => write!(
                f,
                "config error: bearer-env auth requires a non-empty env name"
            ),
            ConfigError::StaticTokenRequired => write!(
                f,
                "config error: static auth requires a non-empty token"
            ),
            ConfigError::ModelRequired => {
                write!(
                    f,
                    "provider.model is required — pass --model <name>, set KITSU_MODEL, or configure the gateway model"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Resolve the config file path: explicit `--config`, then `$KITSU_CONFIG`,
    /// then `$HOME/.kitsu/config.toml`.
    pub fn resolve_path(explicit: Option<&str>) -> PathBuf {
        if let Some(p) = explicit {
            return PathBuf::from(p);
        }
        if let Ok(p) = std::env::var("KITSU_CONFIG") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        // HOME → USERPROFILE via the shared accessor: Windows has no HOME,
        // and the raw read sent the Windows release to a cwd-relative
        // `.kitsu` (issue #26).
        let home = kitsu_tools::host_home_dir().unwrap_or_default();
        Path::new(&home).join(".kitsu").join("config.toml")
    }

    /// Load and parse the config file. A missing file yields an empty config
    /// (so `--gateway` validation can still produce a clear error).
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|error: toml::de::Error| {
                // Never retain `error.to_string()`: toml's Display includes a
                // verbatim excerpt of the source line. Derive only a safe,
                // one-based location from the byte span.
                let location = error.span().and_then(|span| {
                    let prefix = text.get(..span.start.min(text.len()))?;
                    let line = prefix.chars().filter(|ch| *ch == '\n').count() + 1;
                    let column = prefix
                        .rsplit_once('\n')
                        .map_or(prefix, |(_, tail)| tail)
                        .chars()
                        .count()
                        + 1;
                    Some((line, column))
                });
                // serde's unknown-field message enumerates key NAMES only —
                // safe to surface, and exactly what a typo'd key needs.
                let message = error.message().to_string();
                let hint = message.starts_with("unknown field").then_some(message);
                ConfigError::Parse {
                    line: location.map(|(line, _)| line),
                    column: location.map(|(_, column)| column),
                    hint,
                }
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(ConfigError::Read {
                path: path.display().to_string(),
                source: e,
            }),
        }
    }

    /// Select a gateway profile by name, falling back to `default_gateway`.
    pub fn select_gateway<'a>(
        &'a self,
        requested: Option<&'a str>,
    ) -> Result<(&'a str, &'a Gateway), ConfigError> {
        let name = match requested {
            Some(n) => n,
            None => self
                .default_gateway
                .as_deref()
                .ok_or(ConfigError::NoGateway)?,
        };
        match self.gateways.get(name) {
            Some(g) => Ok((name, g)),
            None if name == "local" => self
                .local()
                .map(|(g, _)| (name, g))
                .ok_or(ConfigError::NoLocalServer),
            // `remote` names whatever default_gateway points at, so scripts can
            // say local vs remote without knowing profile names.
            None if name == "remote" => match self.default_gateway.as_deref() {
                Some(default) if default != "remote" => self.select_gateway(Some(default)),
                _ => Err(ConfigError::NoGateway),
            },
            None => {
                let defined = self.gateways.keys().cloned().collect::<Vec<_>>().join(", ");
                Err(ConfigError::UnknownGateway {
                    name: name.to_string(),
                    defined,
                })
            }
        }
    }

    fn local(&self) -> Option<&(Gateway, Vec<String>)> {
        self.detected_local
            .get_or_init(detect_local_gateway)
            .as_ref()
    }

    /// Best-effort routing for `--model` without `--gateway`: a model the
    /// detected local server lists (the exact id, or a unique case-insensitive
    /// substring such as `glm`) resolves to that full local id; anything else
    /// returns `None` and stays with the default gateway. A configured
    /// `[gateways.local]` profile opts out of routing entirely.
    pub fn route_model_to_local(&self, model: &str) -> Result<Option<String>, ConfigError> {
        if self.gateways.contains_key("local") {
            return Ok(None);
        }
        let Some((_, models)) = self.local() else {
            return Ok(None);
        };
        if models.iter().any(|m| m == model) {
            return Ok(Some(model.to_string()));
        }
        let needle = model.to_lowercase();
        let hits: Vec<&String> = models
            .iter()
            .filter(|m| m.to_lowercase().contains(&needle))
            .collect();
        match hits.as_slice() {
            [] => Ok(None),
            [hit] => Ok(Some((*hit).clone())),
            _ => Err(ConfigError::AmbiguousLocalModel {
                model: model.to_string(),
                matches: hits
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }
}

/// The first well-known local runtime that answers `/v1/models` with at least
/// one model, as a no-auth OpenAI-compatible profile defaulting to that first
/// model, plus everything it lists.
fn detect_local_gateway() -> Option<(Gateway, Vec<String>)> {
    LOCAL_SERVERS.iter().find_map(|&(port, _)| {
        let models = probe_local_models(port)?;
        let gateway = Gateway {
            flavor: "openai".to_string(),
            adapter: GatewayAdapter::default(),
            base_url: format!("http://127.0.0.1:{port}"),
            chat_path: default_chat_path(),
            models_path: Some("/v1/models".to_string()),
            model: models.first().cloned(),
            context_window: None,
            auth: AuthConfig {
                method: "none".to_string(),
                ..AuthConfig::default()
            },
        };
        Some((gateway, models))
    })
}

/// Model ids a loopback server lists at `/v1/models`, or `None` if nothing
/// answers or the list is empty. HTTP/1.0 keeps the reply un-chunked so a plain read works.
// ponytail: blocking std probe at startup (≤ ~1s per closed-but-hung port);
// move onto the async provider client if selection ever runs mid-session.
fn probe_local_models(port: u16) -> Option<Vec<String>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(200)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    stream
        .write_all(b"GET /v1/models HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .ok()?;
    let mut reply = Vec::new();
    stream.take(1 << 20).read_to_end(&mut reply).ok()?;
    let reply = String::from_utf8_lossy(&reply);
    let (head, body) = reply.split_once("\r\n\r\n")?;
    if head.split_whitespace().nth(1) != Some("200") {
        return None;
    }
    let catalog: serde_json::Value = serde_json::from_str(body).ok()?;
    let models: Vec<String> = catalog["data"]
        .as_array()?
        .iter()
        .filter_map(|model| model["id"].as_str().map(str::to_string))
        .collect();
    (!models.is_empty()).then_some(models)
}

impl Gateway {
    /// Validate the flavor field (only `openai` is supported in the MVP path).
    pub fn validate_flavor(&self) -> Result<(), ConfigError> {
        match self.flavor.as_str() {
            "openai" | "anthropic" | "ollama" => Ok(()),
            other => Err(ConfigError::UnsupportedFlavor {
                flavor: other.to_string(),
            }),
        }
    }

    /// Validate the closed auth-method vocabulary and its required inputs before
    /// provider construction, so typos and missing static credentials never turn
    /// into a different authentication mode at request time.
    pub fn validate_auth(&self) -> Result<(), ConfigError> {
        match self.auth.method.as_str() {
            "none" => Ok(()),
            "bearer-env" if self.auth.env.trim().is_empty() => Err(ConfigError::AuthEnvRequired),
            "bearer-env" => Ok(()),
            "static"
                if self
                    .auth
                    .token
                    .as_deref()
                    .is_some_and(|token| !token.trim().is_empty()) =>
            {
                Ok(())
            }
            "static" => Err(ConfigError::StaticTokenRequired),
            other => Err(ConfigError::UnsupportedAuthMethod {
                method: other.to_string(),
            }),
        }
    }

    /// Resolve the effective model: `--model` flag > gateway.model > error.
    pub fn resolve_model(&self, flag: Option<&str>) -> Result<String, ConfigError> {
        if let Some(m) = flag {
            if !m.is_empty() {
                return Ok(m.to_string());
            }
        }
        self.model
            .clone()
            .filter(|m| !m.is_empty())
            .ok_or(ConfigError::ModelRequired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a debug-print of config must never expose a static token.
    #[test]
    fn debug_redacts_static_token() {
        const HINT_SECRET: &str = "hf_refresh_hint_secret_value";
        let auth = AuthConfig {
            method: "static".to_string(),
            token: Some("super-secret-token-value".to_string()),
            refresh_hint: Some(HINT_SECRET.to_string()),
            ..AuthConfig::default()
        };
        let dbg = format!("{auth:?}");
        assert!(
            !dbg.contains("super-secret-token-value"),
            "token value leaked in Debug output: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "expected redaction marker: {dbg}"
        );
        assert!(
            !dbg.contains(HINT_SECRET),
            "refresh hint leaked in Debug output: {dbg}"
        );

        // And via the enclosing Gateway/Config (which derive Debug).
        let gw = Gateway {
            flavor: "openai".to_string(),
            adapter: GatewayAdapter::CompatibleChat,
            base_url: "https://x".to_string(),
            chat_path: default_chat_path(),
            models_path: None,
            model: None,
            context_window: None,
            auth,
        };
        assert!(
            !format!("{gw:?}").contains("super-secret-token-value"),
            "token leaked through Gateway Debug"
        );
    }

    /// The zero-config `local` gateway takes the first listed model from a
    /// 200 catalog, and treats an empty catalog or an error status as absent.
    #[test]
    fn local_probe_reads_first_model_and_rejects_empty_or_error_replies() {
        let serve = |reply: &'static str| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = stream.read(&mut [0; 1024]);
                stream.write_all(reply.as_bytes()).unwrap();
            });
            port
        };
        let ok = serve(
            "HTTP/1.0 200 OK\r\n\r\n{\"data\":[{\"id\":\"glm-4.7-flash:q4_K_M\"},{\"id\":\"other\"}]}",
        );
        assert_eq!(
            probe_local_models(ok),
            Some(vec![
                "glm-4.7-flash:q4_K_M".to_string(),
                "other".to_string()
            ])
        );
        let empty = serve("HTTP/1.0 200 OK\r\n\r\n{\"data\":[]}");
        assert_eq!(probe_local_models(empty), None);
        let missing = serve("HTTP/1.0 404 Not Found\r\n\r\n{\"data\":[{\"id\":\"x\"}]}");
        assert_eq!(probe_local_models(missing), None);
    }

    /// `--model` routes to the detected local server on an exact id or a unique
    /// substring, refuses an ambiguous one, and leaves everything else (and any
    /// configured `[gateways.local]`) to the default gateway. `remote` aliases
    /// default_gateway.
    #[test]
    fn model_routing_prefers_unique_local_matches_and_remote_aliases_default() {
        let local = |gateways: &str| {
            let mut config: Config = toml::from_str(gateways).unwrap();
            let models = ["glm-4.7-flash:q4_K_M", "qwen3-coder-next:q4_K_M"];
            config.detected_local = OnceLock::from(Some((
                Gateway {
                    flavor: "openai".to_string(),
                    adapter: GatewayAdapter::default(),
                    base_url: "http://127.0.0.1:11434".to_string(),
                    chat_path: default_chat_path(),
                    models_path: Some("/v1/models".to_string()),
                    model: Some(models[0].to_string()),
                    context_window: None,
                    auth: AuthConfig::default(),
                },
                models.map(str::to_string).to_vec(),
            )));
            config
        };
        let config = local(
            "default_gateway = \"anthropic1m\"\n[gateways.anthropic1m]\nflavor = \"openai\"\nbase_url = \"https://x\"\n",
        );
        let route = |model| config.route_model_to_local(model);
        assert_eq!(
            route("glm").unwrap().as_deref(),
            Some("glm-4.7-flash:q4_K_M")
        );
        assert_eq!(
            route("qwen3-coder-next:q4_K_M").unwrap().as_deref(),
            Some("qwen3-coder-next:q4_K_M")
        );
        assert!(matches!(
            route("q4_k_m"),
            Err(ConfigError::AmbiguousLocalModel { .. })
        ));
        assert_eq!(route("global.anthropic.claude-opus-5-5").unwrap(), None);
        assert_eq!(
            config.select_gateway(Some("remote")).unwrap().0,
            "anthropic1m"
        );
        assert_eq!(
            config.select_gateway(Some("local")).unwrap().1.base_url,
            "http://127.0.0.1:11434"
        );

        let pinned = local("[gateways.local]\nflavor = \"openai\"\nbase_url = \"http://h\"\n");
        assert_eq!(pinned.route_model_to_local("glm").unwrap(), None);
        assert!(matches!(
            pinned.select_gateway(Some("remote")),
            Err(ConfigError::NoGateway)
        ));
    }

    /// A typo'd config key (the cycle-14 audit hit `token_env` for `env`) must
    /// fail loudly with the key names — and never fall back to a silently
    /// different auth behavior.
    #[test]
    fn unknown_auth_key_is_rejected_with_field_names_only() {
        const SECRET: &str = "hf_unknown_field_secret_value";
        let directory = tempfile::tempdir().expect("temp config directory");
        let path = directory.path().join("config.toml");
        std::fs::write(
            &path,
            format!(
                "[gateways.example]\nflavor = \"openai\"\nbase_url = \"https://x\"\nauth = {{ method = \"bearer-env\", token_env = \"{SECRET}\" }}\n"
            ),
        )
        .expect("write config with unknown auth key");

        let error = Config::load(&path).expect_err("unknown key must be rejected");
        let display = error.to_string();
        assert!(display.contains("unknown field"), "{display}");
        assert!(display.contains("token_env"), "{display}");
        assert!(!display.contains(SECRET), "value leaked: {display}");
    }

    #[test]
    fn artifact_storage_false_parses_and_misspelling_fails_closed() {
        let parsed: Config = toml::from_str("artifact_storage = false\n").unwrap();
        assert_eq!(parsed.artifact_storage, Some(false));

        let error = toml::from_str::<Config>("artifact_stroage = false\n")
            .expect_err("privacy-sensitive top-level typos must be rejected");
        assert!(error.message().contains("unknown field"));
        assert!(error.message().contains("artifact_stroage"));
    }

    #[test]
    fn provider_adapter_is_closed_explicit_and_backward_compatible() {
        let legacy: Config = toml::from_str(
            "[gateways.legacy]\nflavor = \"anthropic\"\nbase_url = \"https://example.invalid\"\n",
        )
        .unwrap();
        assert_eq!(
            legacy.gateways["legacy"].adapter,
            GatewayAdapter::CompatibleChat,
            "a flavor label must never silently select a different wire"
        );

        let native: Config = toml::from_str(
            "[gateways.native]\nflavor = \"anthropic\"\nadapter = \"anthropic-messages\"\nbase_url = \"https://example.invalid\"\nchat_path = \"/v1/messages\"\n",
        )
        .unwrap();
        assert_eq!(
            native.gateways["native"].adapter,
            GatewayAdapter::AnthropicMessages
        );

        let error = toml::from_str::<Config>(
            "[gateways.bad]\nflavor = \"anthropic\"\nadapter = \"messages-ish\"\nbase_url = \"https://example.invalid\"\n",
        )
        .expect_err("unknown adapter names must fail closed");
        assert!(error.message().contains("unknown variant"));
        assert!(error.message().contains("messages-ish"));
    }

    /// The `[agents]` table carries the state-file switch. `agent_spawn_cap`
    /// deliberately stays a flat key, so the table must not swallow it.
    #[test]
    fn agents_state_file_mode_parses_all_three_spellings() {
        for (text, expected) in [
            ("[agents]\nstate_file = \"on\"\n", StateFileMode::On),
            ("[agents]\nstate_file = \"off\"\n", StateFileMode::Off),
            ("[agents]\nstate_file = \"auto\"\n", StateFileMode::Auto),
        ] {
            let parsed: Config = toml::from_str(text).expect(text);
            assert_eq!(
                parsed.agents.unwrap_or_default().state_file,
                expected,
                "parsing {text:?}"
            );
        }
    }

    #[test]
    fn agents_table_defaults_to_auto_when_absent_or_empty() {
        let none: Config = toml::from_str("default_gateway = \"example\"\n").unwrap();
        assert_eq!(
            none.agents.unwrap_or_default().state_file,
            StateFileMode::Auto,
            "a config without [agents] must still load and default to auto"
        );

        let empty: Config = toml::from_str("[agents]\n").unwrap();
        assert_eq!(
            empty.agents.unwrap_or_default().state_file,
            StateFileMode::Auto
        );

        assert_eq!(AgentsConfig::default().state_file, StateFileMode::Auto);
    }

    #[test]
    fn agents_table_rejects_unknown_keys_and_values_instead_of_defaulting() {
        let error = toml::from_str::<Config>("[agents]\nstate_fle = \"on\"\n")
            .expect_err("a typo inside [agents] must be rejected, not defaulted");
        assert!(error.message().contains("unknown field"), "{error}");
        assert!(error.message().contains("state_fle"), "{error}");

        let error = toml::from_str::<Config>("[agents]\nstate_file = \"sometimes\"\n")
            .expect_err("an unknown mode must fail closed rather than default to auto");
        assert!(error.message().contains("unknown variant"), "{error}");
        assert!(error.message().contains("sometimes"), "{error}");
    }

    /// The spawn cap is not part of the new table: putting it there must be a
    /// loud error, and the flat key must keep working alongside `[agents]`.
    #[test]
    fn agent_spawn_cap_stays_a_flat_key_beside_the_agents_table() {
        let parsed: Config =
            toml::from_str("agent_spawn_cap = 3\n\n[agents]\nstate_file = \"on\"\n").unwrap();
        assert_eq!(parsed.agent_spawn_cap, Some(3));
        assert_eq!(
            parsed.agents.unwrap_or_default().state_file,
            StateFileMode::On
        );

        toml::from_str::<Config>("[agents]\nagent_spawn_cap = 3\n")
            .expect_err("the spawn cap must not be accepted inside [agents]");
    }

    #[test]
    fn parse_error_retains_location_but_never_source_excerpt() {
        const SECRET: &str = "hf_parse_error_secret_value";
        let directory = tempfile::tempdir().expect("temp config directory");
        let path = directory.path().join("config.toml");
        std::fs::write(
            &path,
            format!("default_gateway = \"example\"\ncredential = \"{SECRET}\" trailing\n"),
        )
        .expect("write malformed config");

        let error = Config::load(&path).expect_err("config must be malformed");
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(display.contains("TOML parse failed at line 2"), "{display}");
        assert!(display.contains("source excerpt withheld"), "{display}");
        assert!(
            !display.contains(SECRET),
            "source leaked in Display: {display}"
        );
        assert!(!debug.contains(SECRET), "source leaked in Debug: {debug}");
    }
}
