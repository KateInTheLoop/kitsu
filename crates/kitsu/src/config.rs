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
    /// Optional `[hooks]` table. Absent is the same as every key unset, which
    /// is why callers read it as `hooks.unwrap_or_default()` rather than being
    /// handed a silently materialized table.
    #[serde(default)]
    pub hooks: Option<HooksConfig>,
    /// Optional `[tui]` table (design §4.8). Absent is the same as every key
    /// at its default, which is why callers read it as
    /// `tui.unwrap_or_default()` rather than being handed a silently
    /// materialized table.
    #[serde(default)]
    pub tui: Option<TuiConfig>,
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

/// How a background agent's ungranted permission request is handled
/// (design B1). `FailClosed` is today's behaviour and the default everywhere:
/// a detached agent's gate returns an error instead of waiting for a human, so
/// a headless run cannot stall. `Ask` parks the agent in the registry so the
/// user can answer it.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentBlockingMode {
    /// Park the request and mark the agent blocked until it is answered or
    /// `block_timeout_secs` elapses.
    Ask,
    /// Return the fail-closed error immediately; nothing is ever parked.
    #[default]
    FailClosed,
}

/// B3's default blocking timeout in seconds (30 minutes). The unit lives in
/// the key name deliberately: a bare `block_timeout = 30` is exactly where a
/// reader guesses wrong between seconds and minutes.
fn default_block_timeout_secs() -> u64 {
    crate::subagent::DEFAULT_AGENT_BLOCK_TIMEOUT.as_secs()
}

/// The `[agents]` table. Unknown keys are rejected loudly: `agent_spawn_cap`
/// deliberately stays a flat top-level key, so a copy of it in here is a
/// mistake and must not be silently ignored.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    #[serde(default)]
    pub state_file: StateFileMode,
    /// How an ungranted permission request from a background agent is handled.
    #[serde(default)]
    pub blocking: AgentBlockingMode,
    /// How long a parked request waits before it resolves `Interrupt` (B3).
    #[serde(default = "default_block_timeout_secs")]
    pub block_timeout_secs: u64,
}

/// Well-known loopback ports of OpenAI-compatible local runtimes, probed in
/// order by the zero-config `local` gateway.
const LOCAL_SERVERS: &[(u16, &str)] = &[
    (11434, "Ollama"),
    (1234, "LM Studio"),
    (8080, "llama.cpp"),
    (8000, "vLLM"),
];

// Hand-written rather than derived: every caller reads the table as
// `agents.unwrap_or_default()`, so a derived `Default` would hand a config with
// no `[agents]` table a ZERO-second block timeout while `#[serde(default =
// ...)]` only covers the key's absence inside a table that is present.
impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            state_file: StateFileMode::default(),
            blocking: AgentBlockingMode::default(),
            block_timeout_secs: default_block_timeout_secs(),
        }
    }
}

impl AgentsConfig {
    /// The configured blocking timeout as a `Duration`.
    pub fn block_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.block_timeout_secs)
    }
}

/// The `[hooks]` table. Unknown keys are rejected loudly, so a typo in a hook
/// name is an error rather than a hook that silently never runs.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HooksConfig {
    /// A local command run when a background agent changes state
    /// (requirement H3). Stored verbatim; splitting into argv is
    /// `agent_hook`'s job. An empty or whitespace-only string means no hook.
    #[serde(default)]
    pub on_agent_state: Option<String>,
}

/// Which renderer a chat session runs (`[tui] mode`, design §4.8).
///
/// All three spellings of the design's grammar parse. `fullscreen` was held
/// back while there was nothing to select — a value that parsed and silently
/// behaved as `inline` is the quiet coercion this table exists to prevent —
/// and it became a variant when its renderer became reachable: the switch
/// takes the terminal's alternate screen and paints the inline cell ring's
/// tail through `kitsu_tui::fullscreen::FullscreenView`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TuiMode {
    /// The line renderer kitsu has always had, and the default: the inline
    /// viewport is a preview until the terminal matrix
    /// (docs/roadmap/cycles/e11/terminal-matrix.md) has been run by a human.
    #[default]
    Classic,
    /// The pinned inline viewport from `kitsu-tui`.
    Inline,
    /// The fullscreen transcript renderer: the terminal's alternate screen,
    /// painted from the cell ring the inline viewport retains. A session can
    /// start in it (`--tui fullscreen`, `mode = "fullscreen"`) and `/tui
    /// fullscreen` switches into it, because a mode a user can switch to but
    /// cannot start in is incoherent.
    Fullscreen,
}

/// Whether a session takes the terminal's alternate screen (`[tui]
/// alternate_screen`).
///
/// A tri-state rather than a boolean, because `auto` ("whatever this terminal
/// and this renderer can do") is not expressible as one, and a boolean would
/// make the two explicit answers indistinguishable from the inferred one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AlternateScreen {
    /// Let the renderer decide. The default.
    #[default]
    Auto,
    /// Always take the alternate screen.
    Always,
    /// Never take it; `--no-alt-screen` forces this.
    Never,
}

/// The prefix every `[tui]` value error carries.
///
/// It is also the credential boundary: each message bearing this prefix is
/// written in this file and interpolates the KEY NAME and the permitted values
/// only — never the offending value. That is what lets `Config::load` surface
/// these while it still withholds serde's own `invalid type` message, which
/// quotes the input and so could echo a token out of a mistyped line.
const TUI_VALUE_ERROR: &str = "invalid value for `[tui]` key ";

/// The accepted `mode` spellings, as the error messages list them.
const TUI_MODES: &str = "`classic`, `inline`, `fullscreen`";
/// The accepted `alternate_screen` spellings.
const TUI_ALTERNATE_SCREENS: &str = "`auto`, `always`, `never`";

/// Deserialize `mode` loudly.
///
/// Both failures — a wrong type and an unknown spelling — become the same
/// `TUI_VALUE_ERROR` message, which names the key and lists the permitted
/// values and interpolates **nothing the user wrote**. That is deliberate and
/// it is the reason this key gets the same help as `title` or `prefix`:
/// serde's own `unknown variant` message would echo the offending value, so
/// it could not cross the credential boundary in `Config::load` and a
/// misspelled `mode` would reach `kitsu doctor` as nothing but a line number.
///
/// The match is byte-exact: `"Inline"`, `"INLINE"` and `"inline "` are typos,
/// and a typo in a key the user wrote on purpose must be a loud error rather
/// than the fail-closed fallback the retired `KITSU_TUI` gate used.
fn deserialize_tui_mode<'de, D>(deserializer: D) -> Result<TuiMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let invalid = || {
        D::Error::custom(format!(
            "{TUI_VALUE_ERROR}`mode`: expected one of {TUI_MODES}"
        ))
    };
    let raw = String::deserialize(deserializer).map_err(|_| invalid())?;
    match raw.as_str() {
        "classic" => Ok(TuiMode::Classic),
        "inline" => Ok(TuiMode::Inline),
        "fullscreen" => Ok(TuiMode::Fullscreen),
        _ => Err(invalid()),
    }
}

/// Deserialize `alternate_screen` loudly.
///
/// A boolean is the mistake the key's name invites, and `"sometimes"` is the
/// mistake its meaning invites; both fail with the one `TUI_VALUE_ERROR`
/// message naming the key and the tri-state, for the same reason as
/// [`deserialize_tui_mode`] — the message must be safe to print, so it never
/// repeats the value.
fn deserialize_alternate_screen<'de, D>(deserializer: D) -> Result<AlternateScreen, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let invalid = || {
        D::Error::custom(format!(
            "{TUI_VALUE_ERROR}`alternate_screen`: expected one of {TUI_ALTERNATE_SCREENS}"
        ))
    };
    let raw = String::deserialize(deserializer).map_err(|_| invalid())?;
    match raw.as_str() {
        "auto" => Ok(AlternateScreen::Auto),
        "always" => Ok(AlternateScreen::Always),
        "never" => Ok(AlternateScreen::Never),
        _ => Err(invalid()),
    }
}

/// Deserialize one `[tui]` boolean, naming the key and nothing else.
///
/// serde's own message for `title = "true"` is `invalid type: string "true",
/// expected a boolean` — it quotes the value, so it cannot cross the
/// suppression boundary in `Config::load`. This one is value-free by
/// construction.
fn deserialize_tui_bool<'de, D>(deserializer: D, key: &str) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    bool::deserialize(deserializer).map_err(|_| {
        D::Error::custom(format!(
            "{TUI_VALUE_ERROR}`{key}`: expected a boolean (true or false)"
        ))
    })
}

fn deserialize_tui_title<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_tui_bool(deserializer, "title")
}

fn deserialize_tui_left_arrow<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_tui_bool(deserializer, "left_arrow_opens_agents")
}

/// Deserialize `prefix`, rejecting a blank value.
///
/// An empty prefix is not a prefix: the chord model needs a key to hang the
/// chords on, and a blank value would otherwise buy a silently chord-less
/// session. The message names the key and shows the default spelling; it never
/// echoes what was written.
fn deserialize_tui_prefix<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let invalid = || {
        D::Error::custom(format!(
            "{TUI_VALUE_ERROR}`prefix`: expected a non-empty key name such as `{}`",
            TuiConfig::DEFAULT_PREFIX
        ))
    };
    let raw = String::deserialize(deserializer).map_err(|_| invalid())?;
    if raw.trim().is_empty() {
        return Err(invalid());
    }
    Ok(raw)
}

/// Deserialize `cell_ring_lines` inside its bound.
///
/// The bound is checked at the parse, not where the ring is built: `0` is not
/// "disabled" and `u64::MAX` is not "unbounded", and a memory bound the user
/// cannot read is one they trip over at runtime instead. The message states
/// the key and the range, and never the rejected number — the range is the
/// fix, and a value echoed back out of a config file is how a token leaks.
fn deserialize_cell_ring_lines<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let invalid = || {
        D::Error::custom(format!(
            "{TUI_VALUE_ERROR}`cell_ring_lines`: expected an integer between {} and {}",
            TuiConfig::MIN_CELL_RING_LINES,
            TuiConfig::MAX_CELL_RING_LINES
        ))
    };
    let lines = usize::try_from(u64::deserialize(deserializer).map_err(|_| invalid())?)
        .map_err(|_| invalid())?;
    if !(TuiConfig::MIN_CELL_RING_LINES..=TuiConfig::MAX_CELL_RING_LINES).contains(&lines) {
        return Err(invalid());
    }
    Ok(lines)
}

/// The `[tui]` table (design §4.8). Unknown keys are rejected loudly, so a
/// misspelled key is an error naming the key rather than a setting that
/// silently never applies.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TuiConfig {
    /// Which renderer the session runs. `--tui <MODE>` overrides.
    #[serde(default, deserialize_with = "deserialize_tui_mode")]
    pub mode: TuiMode,
    /// Whether the session takes the alternate screen. `--no-alt-screen`
    /// forces `never`.
    #[serde(default, deserialize_with = "deserialize_alternate_screen")]
    pub alternate_screen: AlternateScreen,
    /// The chord prefix key.
    #[serde(
        default = "default_tui_prefix",
        deserialize_with = "deserialize_tui_prefix"
    )]
    pub prefix: String,
    /// Whether the viewport paints its title row.
    #[serde(default = "default_true", deserialize_with = "deserialize_tui_title")]
    pub title: bool,
    /// Whether Left opens the agents pane.
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_tui_left_arrow"
    )]
    pub left_arrow_opens_agents: bool,
    /// How many lines of cell scrollback the viewport retains.
    #[serde(
        default = "default_cell_ring_lines",
        deserialize_with = "deserialize_cell_ring_lines"
    )]
    pub cell_ring_lines: usize,
}

fn default_tui_prefix() -> String {
    TuiConfig::DEFAULT_PREFIX.to_string()
}
fn default_true() -> bool {
    true
}
fn default_cell_ring_lines() -> usize {
    TuiConfig::DEFAULT_CELL_RING_LINES
}

// Hand-written rather than derived, for the reason `AgentsConfig` carries the
// same note: every caller reads the table as `tui.unwrap_or_default()`, which
// goes through *this* impl and never through serde, so a derived `Default`
// would hand a config with no `[tui]` table an empty chord prefix and a
// ZERO-line cell ring while the `#[serde(default = "...")]` attributes above
// only cover a key's absence from a table that is present.
impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            mode: TuiMode::default(),
            alternate_screen: AlternateScreen::default(),
            prefix: default_tui_prefix(),
            title: true,
            left_arrow_opens_agents: true,
            cell_ring_lines: default_cell_ring_lines(),
        }
    }
}

impl TuiConfig {
    /// The default chord prefix.
    pub const DEFAULT_PREFIX: &'static str = "ctrl+x";
    /// The default cell scrollback depth in lines.
    pub const DEFAULT_CELL_RING_LINES: usize = 10_000;
    /// The smallest accepted `cell_ring_lines`. One line, not zero: zero would
    /// make "no scrollback" indistinguishable from a typo.
    pub const MIN_CELL_RING_LINES: usize = 1;
    /// The largest accepted `cell_ring_lines`.
    pub const MAX_CELL_RING_LINES: usize = 1_000_000;

    /// The renderer this run uses: `--tui` beats `[tui] mode` beats the
    /// default. There is no fourth source — the `KITSU_TUI` preview gate was
    /// retired with this card, so nothing here reads the environment.
    pub fn effective_mode(&self, flag: Option<TuiMode>) -> TuiMode {
        flag.unwrap_or(self.mode)
    }

    /// The alternate-screen decision this run uses. `--no-alt-screen` is
    /// one-way: it forces `never` over any configured value, and its absence
    /// forces nothing.
    pub fn effective_alternate_screen(&self, no_alt_screen_flag: bool) -> AlternateScreen {
        if no_alt_screen_flag {
            AlternateScreen::Never
        } else {
            self.alternate_screen
        }
    }
}

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
                // Two safe classes, and nothing else:
                //   * serde's unknown-field message enumerates key NAMES only
                //     — safe to surface, and exactly what a typo'd key needs.
                //   * `[tui]` value errors written in this file, which
                //     interpolate the key name and the permitted values and
                //     never the offending value (see `TUI_VALUE_ERROR`).
                // Everything else stays withheld — serde's `invalid type`
                // above all, which quotes the input, and its `unknown
                // variant`, which echoes the word the user wrote. A config
                // file holds gateway tokens, and a mistyped line can put one
                // where the parser would quote it back. No `[tui]` key relies
                // on either: every one of the six fails with a message written
                // above, so the table names the key the user got wrong
                // without the allow-list ever having to admit a value.
                let hint = (message.starts_with("unknown field")
                    || message.starts_with(TUI_VALUE_ERROR))
                .then_some(message);
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

    // -----------------------------------------------------------------
    // T2-S5a: `agents.blocking` and `agents.block_timeout_secs`
    // (design B1/B3, plan row T2-S5). FAILING-FIRST: neither key exists
    // yet, so these do not compile until the implementer adds
    // `AgentBlockingMode`, `AgentsConfig::blocking` and
    // `AgentsConfig::block_timeout_secs` / `block_timeout()`.
    // -----------------------------------------------------------------

    /// B1 + plan row T2-S5: the key exists, follows the `StateFileMode`
    /// enum-key precedent, and its default is `fail_closed` EVERYWHERE.
    /// Nothing about the surface, the mode or the tier moves it in this card,
    /// so an absent `[agents]`, an empty one, the struct default and an
    /// explicit `fail_closed` must all be the same value.
    #[test]
    fn agents_blocking_defaults_to_fail_closed_everywhere() {
        for text in [
            "default_gateway = \"example\"\n",
            "[agents]\n",
            "[agents]\nstate_file = \"on\"\n",
            "[agents]\nblocking = \"fail_closed\"\n",
        ] {
            let parsed: Config = toml::from_str(text).expect(text);
            assert_eq!(
                parsed.agents.unwrap_or_default().blocking,
                AgentBlockingMode::FailClosed,
                "the default must stay fail_closed until T3 flips the TUI default: {text:?}"
            );
        }
        assert_eq!(
            AgentsConfig::default().blocking,
            AgentBlockingMode::FailClosed
        );

        let parsed: Config = toml::from_str("[agents]\nblocking = \"ask\"\n").unwrap();
        assert_eq!(
            parsed.agents.unwrap_or_default().blocking,
            AgentBlockingMode::Ask,
            "explicit `ask` is T2's standalone value and must parse"
        );
    }

    /// The `StateFileMode` precedent again: an unknown mode or a typo'd key
    /// must fail loudly rather than silently leave the run fail-closed.
    #[test]
    fn agents_blocking_rejects_unknown_modes_and_typos() {
        let error = toml::from_str::<Config>("[agents]\nblocking = \"sometimes\"\n")
            .expect_err("an unknown blocking mode must fail closed loudly");
        assert!(error.message().contains("unknown variant"), "{error}");
        assert!(error.message().contains("sometimes"), "{error}");

        let error = toml::from_str::<Config>("[agents]\nblockign = \"ask\"\n")
            .expect_err("a typo inside [agents] must be rejected, not defaulted");
        assert!(error.message().contains("unknown field"), "{error}");
        assert!(error.message().contains("blockign"), "{error}");

        // `ask`/`fail_closed` are the only two spellings: the dash and the
        // camel spelling are typos, not aliases.
        toml::from_str::<Config>("[agents]\nblocking = \"fail-closed\"\n")
            .expect_err("only the snake_case spelling is accepted");
    }

    /// B3: the 30-minute default survives the move out of
    /// `DEFAULT_AGENT_BLOCK_TIMEOUT` and into config, and a short value
    /// parses (which is what makes an end-to-end block test possible).
    ///
    /// Spelling: `block_timeout_secs`. There is no duration key in this
    /// config today — no humantime, no `*_secs` precedent — and the card
    /// forbids a new dependency, so the unit goes in the name where it
    /// cannot be misread as milliseconds or as "30" meaning 30 seconds.
    #[test]
    fn agents_block_timeout_secs_defaults_to_thirty_minutes() {
        for text in [
            "default_gateway = \"example\"\n",
            "[agents]\n",
            "[agents]\nblocking = \"ask\"\n",
        ] {
            let parsed: Config = toml::from_str(text).expect(text);
            let agents = parsed.agents.unwrap_or_default();
            assert_eq!(
                agents.block_timeout_secs, 1_800,
                "B3's 30-minute default must survive becoming a config key: {text:?}"
            );
            assert_eq!(
                agents.block_timeout(),
                std::time::Duration::from_secs(30 * 60),
                "the typed accessor and the raw key agree: {text:?}"
            );
        }
        assert_eq!(AgentsConfig::default().block_timeout_secs, 1_800);

        let parsed: Config =
            toml::from_str("[agents]\nblocking = \"ask\"\nblock_timeout_secs = 2\n").unwrap();
        let agents = parsed.agents.unwrap_or_default();
        assert_eq!(agents.block_timeout_secs, 2);
        assert_eq!(
            agents.block_timeout(),
            std::time::Duration::from_secs(2),
            "a short timeout is what lets a test observe the whole gate->registry->timer path"
        );
    }

    /// A duration key is a place where a wrong type silently becomes a
    /// default. It must not: a string, a float and a negative number are all
    /// errors, and the unit must be in the name rather than in the value.
    #[test]
    fn agents_block_timeout_secs_rejects_non_integer_and_negative_values() {
        for text in [
            "[agents]\nblock_timeout_secs = \"30m\"\n",
            "[agents]\nblock_timeout_secs = 1.5\n",
            "[agents]\nblock_timeout_secs = -1\n",
            "[agents]\nblock_timeout = 30\n",
        ] {
            toml::from_str::<Config>(text)
                .expect_err(&format!("{text:?} must be an error, not a silent default"));
        }
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

    /// The `[hooks]` table carries the agent state hook. An absent table must
    /// keep an existing config loading, and an empty command means no hook at
    /// all rather than an empty argv anyone might try to execute.
    #[test]
    fn hooks_table_parses_the_on_agent_state_command() {
        let parsed: Config = toml::from_str("[hooks]\non_agent_state = \"notify\"\n").unwrap();
        assert_eq!(
            parsed.hooks.unwrap_or_default().on_agent_state.as_deref(),
            Some("notify")
        );

        let parsed: Config =
            toml::from_str("[hooks]\non_agent_state = \"/opt/my tools/notify --now\"\n").unwrap();
        assert_eq!(
            parsed.hooks.unwrap_or_default().on_agent_state.as_deref(),
            Some("/opt/my tools/notify --now"),
            "the command is stored verbatim; splitting is the hook module's job"
        );
    }

    #[test]
    fn hooks_table_is_optional_and_defaults_to_no_hook() {
        let none: Config = toml::from_str("default_gateway = \"example\"\n").unwrap();
        assert!(
            none.hooks.is_none(),
            "an absent [hooks] table must not be materialized"
        );
        assert!(
            none.hooks.unwrap_or_default().on_agent_state.is_none(),
            "a config without [hooks] must still load with no hook"
        );

        let empty: Config = toml::from_str("[hooks]\n").unwrap();
        assert!(
            empty.hooks.unwrap_or_default().on_agent_state.is_none(),
            "a bare [hooks] table must load and mean no hook"
        );

        assert!(HooksConfig::default().on_agent_state.is_none());
    }

    /// An empty string is the explicit "disabled" spelling, and it must not
    /// become a command: it loads, and it carries no hook worth firing.
    #[test]
    fn hooks_empty_command_means_no_hook() {
        let parsed: Config = toml::from_str("[hooks]\non_agent_state = \"\"\n").unwrap();
        let hooks = parsed.hooks.unwrap_or_default();
        assert!(
            hooks
                .on_agent_state
                .as_deref()
                .is_none_or(|command| command.trim().is_empty()),
            "an empty command must not be a runnable hook: {:?}",
            hooks.on_agent_state
        );
    }

    #[test]
    fn hooks_table_rejects_unknown_keys_instead_of_defaulting() {
        let error = toml::from_str::<Config>("[hooks]\non_agent_stat = \"notify\"\n")
            .expect_err("a typo inside [hooks] must be rejected, not defaulted");
        assert!(error.message().contains("unknown field"), "{error}");
        assert!(error.message().contains("on_agent_stat"), "{error}");
    }

    /// The two optional tables are independent: one present must not require
    /// or disturb the other.
    #[test]
    fn hooks_and_agents_tables_coexist() {
        let parsed: Config = toml::from_str(
            "[agents]\nstate_file = \"on\"\n\n[hooks]\non_agent_state = \"notify\"\n",
        )
        .unwrap();
        assert_eq!(
            parsed.agents.unwrap_or_default().state_file,
            StateFileMode::On
        );
        assert_eq!(
            parsed.hooks.unwrap_or_default().on_agent_state.as_deref(),
            Some("notify")
        );
    }
    // -----------------------------------------------------------------
    // T3-S2: the `[tui]` table (design §4.8, plan row T3-S2).
    // FAILING-FIRST: none of `Config::tui`, `TuiConfig`, `TuiMode`,
    // `AlternateScreen` exists yet, so this block does not compile until the
    // implementer adds them.
    //
    // Scope decisions pinned here, with their reasons:
    //
    // * `mode` accepted `classic` and `inline` only, while the design's
    //   grammar also listed `fullscreen`: a name that parsed and silently
    //   behaved as `inline` is exactly the kind of quiet coercion this card's
    //   risk gate exists to forbid. **That boundary has since been retired**,
    //   deliberately and in the card that made the third renderer reachable.
    //   All three spellings parse, and
    //   `tui_mode_accepts_fullscreen_now_that_the_t5_renderer_exists` is
    //   where the third one is pinned. (The test this note used to name,
    //   `tui_mode_rejects_fullscreen_until_the_t5_renderer_exists`, is the
    //   one it replaced and no longer exists.)
    // * `cell_ring_lines` is range-checked HERE, not in T3-S6. The ring is
    //   built in T3-S6; if the parser admits `0` or `4_000_000_000`, T3-S6 has
    //   to invent a silent fallback, and a memory bound you cannot state is
    //   not a bound. The checked range is
    //   `MIN_CELL_RING_LINES..=MAX_CELL_RING_LINES`.
    // * The range error must reach the USER. `ConfigError::Parse` only keeps
    //   its hint when the serde message starts with "unknown field", so a
    //   custom serde range message would be withheld as a possible credential
    //   excerpt. The tests below therefore assert on `Config::load`'s
    //   rendered error, which forces either a dedicated `ConfigError` variant
    //   or a widened hint allow-list.
    // -----------------------------------------------------------------

    /// Parse a config from text the way the product does: through a real file
    /// and `Config::load`, so `ConfigError`'s own rendering is what is pinned
    /// rather than the raw serde message.
    fn load_config_text(text: &str) -> Result<Config, ConfigError> {
        let directory = tempfile::tempdir().expect("temp config directory");
        let path = directory.path().join("config.toml");
        std::fs::write(&path, text).expect("write config text");
        Config::load(&path)
    }

    /// All six keys, every one of them set away from its default, so a test
    /// that passed by accident on a default cannot pass here.
    #[test]
    fn tui_table_parses_all_six_keys() {
        let parsed: Config = toml::from_str(
            "[tui]\n\
             mode = \"inline\"\n\
             alternate_screen = \"never\"\n\
             prefix = \"ctrl+b\"\n\
             title = false\n\
             left_arrow_opens_agents = false\n\
             cell_ring_lines = 250\n",
        )
        .expect("the full [tui] table must parse");
        let tui = parsed.tui.expect("[tui] must be present when written");
        assert_eq!(tui.mode, TuiMode::Inline);
        assert_eq!(tui.alternate_screen, AlternateScreen::Never);
        assert_eq!(tui.prefix, "ctrl+b");
        assert!(!tui.title);
        assert!(!tui.left_arrow_opens_agents);
        assert_eq!(tui.cell_ring_lines, 250);

        // ...and each value above differs from the default, which is what
        // makes the assertions non-vacuous.
        let default = TuiConfig::default();
        assert_ne!(default.mode, TuiMode::Inline);
        assert_ne!(default.alternate_screen, AlternateScreen::Never);
        assert_ne!(default.prefix, "ctrl+b");
        assert!(default.title);
        assert!(default.left_arrow_opens_agents);
        assert_ne!(default.cell_ring_lines, 250);
    }

    /// The defaults, read the way every caller reads them.
    ///
    /// This is the T2-S5a trap restated: callers reach the table through
    /// `tui.unwrap_or_default()`, which goes through the **derived** `Default`
    /// and never through serde, so `#[serde(default = "...")]` alone would
    /// hand a config with no `[tui]` table an empty prefix and a ZERO-line
    /// cell ring. Absent table, empty table and `TuiConfig::default()` must
    /// all be the same six values.
    #[test]
    fn tui_table_defaults_are_identical_absent_empty_and_through_unwrap_or_default() {
        let absent: Config = toml::from_str("default_gateway = \"example\"\n").unwrap();
        let empty: Config = toml::from_str("[tui]\n").unwrap();
        let tables = [
            ("absent", absent.tui.unwrap_or_default()),
            ("empty", empty.tui.unwrap_or_default()),
            ("struct default", TuiConfig::default()),
        ];
        for (label, tui) in tables {
            // `mode` is classic until a human has run the Q1 terminal matrix
            // (docs/roadmap/cycles/e11/terminal-matrix.md). Nothing in this
            // card may make the preview renderer the default.
            assert_eq!(tui.mode, TuiMode::Classic, "{label}: mode");
            assert_eq!(
                tui.alternate_screen,
                AlternateScreen::Auto,
                "{label}: alternate_screen"
            );
            assert_eq!(tui.prefix, TuiConfig::DEFAULT_PREFIX, "{label}: prefix");
            assert_eq!(tui.prefix, "ctrl+x", "{label}: prefix spelling");
            assert!(tui.title, "{label}: title");
            assert!(
                tui.left_arrow_opens_agents,
                "{label}: left_arrow_opens_agents"
            );
            assert_eq!(
                tui.cell_ring_lines,
                TuiConfig::DEFAULT_CELL_RING_LINES,
                "{label}: cell_ring_lines"
            );
            assert_eq!(
                tui.cell_ring_lines, 10_000,
                "{label}: cell_ring_lines value"
            );
        }
    }

    /// Every key defaults independently: one key set must not drag the other
    /// five off their defaults (the failure mode of a `#[serde(default)]` on
    /// the struct rather than on each field).
    #[test]
    fn tui_keys_default_independently_of_one_another() {
        let parsed: Config = toml::from_str("[tui]\nmode = \"inline\"\n").unwrap();
        let tui = parsed.tui.unwrap_or_default();
        assert_eq!(tui.mode, TuiMode::Inline);
        assert_eq!(tui.alternate_screen, AlternateScreen::Auto);
        assert_eq!(tui.prefix, "ctrl+x");
        assert!(tui.title);
        assert!(tui.left_arrow_opens_agents);
        assert_eq!(tui.cell_ring_lines, 10_000);
    }

    /// Precedence, written down once: `--tui` beats `[tui] mode` beats the
    /// default, and there is no fourth source — `KITSU_TUI` is gone (plan row
    /// T3-S2, "env gate removed"), so nothing reads the environment here.
    #[test]
    fn tui_mode_precedence_is_flag_then_config_then_default() {
        let classic = TuiConfig::default();
        let inline = TuiConfig {
            mode: TuiMode::Inline,
            ..TuiConfig::default()
        };

        // Flag wins, in both directions, so "the flag wins" is not just
        // "inline wins".
        assert_eq!(
            classic.effective_mode(Some(TuiMode::Inline)),
            TuiMode::Inline
        );
        assert_eq!(
            inline.effective_mode(Some(TuiMode::Classic)),
            TuiMode::Classic
        );
        // No flag: the config decides.
        assert_eq!(inline.effective_mode(None), TuiMode::Inline);
        // Neither: the default.
        assert_eq!(classic.effective_mode(None), TuiMode::Classic);
        assert_eq!(
            TuiConfig::default().effective_mode(None),
            TuiMode::Classic,
            "a config with no [tui] table must still be classic"
        );
    }

    /// `--no-alt-screen` is a one-way flag: it forces `never` over any
    /// configured value, and its absence never forces anything.
    #[test]
    fn no_alt_screen_flag_forces_never_over_any_configured_value() {
        for configured in [
            AlternateScreen::Auto,
            AlternateScreen::Always,
            AlternateScreen::Never,
        ] {
            let tui = TuiConfig {
                alternate_screen: configured,
                ..TuiConfig::default()
            };
            assert_eq!(
                tui.effective_alternate_screen(true),
                AlternateScreen::Never,
                "--no-alt-screen must beat alternate_screen = {configured:?}"
            );
            assert_eq!(
                tui.effective_alternate_screen(false),
                configured,
                "without the flag the configured value stands"
            );
        }
    }

    /// The risk gate (design §4.8): a misspelled key inside `[tui]` is an
    /// error that NAMES the key. Today `[tui]` is itself an unknown field on
    /// `Config`, so the message names `tui` and the user is told nothing about
    /// their typo — which is why the assertion is on the inner key's name and
    /// on the enumeration that proves the table is known.
    #[test]
    fn tui_table_rejects_unknown_keys_and_names_the_key() {
        let error = toml::from_str::<Config>("[tui]\ncell_ring_line = 500\n")
            .expect_err("a typo inside [tui] must be rejected, not ignored");
        let message = error.message();
        assert!(
            message.starts_with("unknown field `cell_ring_line`"),
            "the message must name the misspelled key first: {message}"
        );
        assert!(
            message.contains("cell_ring_lines"),
            "and enumerate the real keys, which is the user's fix: {message}"
        );

        // The same typo through the product's own loader, which is what
        // `kitsu doctor` prints.
        let rendered = load_config_text("[tui]\ncell_ring_line = 500\n")
            .expect_err("Config::load must reject it too")
            .to_string();
        assert!(
            rendered.contains("unknown field `cell_ring_line`"),
            "ConfigError must carry the key name, not withhold it: {rendered}"
        );
    }

    /// A `[tui]` key copied to the top level is a mistake, not a synonym.
    #[test]
    fn tui_keys_are_not_accepted_at_the_top_level() {
        let error = toml::from_str::<Config>("mode = \"inline\"\n")
            .expect_err("`mode` is a [tui] key and must not be accepted flat");
        assert!(
            error.message().starts_with("unknown field `mode`"),
            "{error}"
        );
    }

    /// An unknown `mode` fails loudly with the offending value named — the
    /// `StateFileMode` precedent. It must NOT fail closed to classic the way
    /// the retired `KITSU_TUI` parser did: an env preview gate can swallow a
    /// typo, a config key the user wrote on purpose cannot.
    #[test]
    fn tui_mode_rejects_unknown_values_instead_of_falling_back_to_classic() {
        for value in ["Inline", "INLINE", "inline ", "1", "true", "", "viewport"] {
            let text = format!("[tui]\nmode = \"{value}\"\n");
            let error = toml::from_str::<Config>(&text)
                .expect_err(&format!("mode = {value:?} must be rejected, not defaulted"));
            let message = error.message().to_string();
            // Authorised change (coordinator, T3-S2): the message is this
            // file's own key-naming one rather than serde's `unknown
            // variant`, which would echo `value` back and so could not be
            // surfaced by `Config::load` at all. The assertion is still that
            // the value is REJECTED, loudly and by name.
            //
            // **Strengthened in T5-S2 (finding F90).** This was two
            // `contains` checks, one of them for `` `classic`, `inline` ``.
            // That substring is still present once a third mode is appended,
            // so the assertion could not fail when its own subject changed —
            // it survived this card by luck. F41 banned substring *bans* for
            // over-constraining the product; this is the inverse hazard, a
            // substring *require* that under-constrains it to nothing. So the
            // whole message is pinned: what is asserted is what the user
            // reads, and appending, removing or reordering a mode fails here.
            assert_eq!(
                message,
                "invalid value for `[tui]` key `mode`: expected one of \
                 `classic`, `inline`, `fullscreen`",
                "mode = {value:?} must fail with the one key-naming message, \
                 listing exactly the modes that parse"
            );
            // And the credential boundary this file exists to hold: the
            // rejected value is never echoed back. Kept as a ban because
            // here the product claim *is* an absence.
            assert!(
                value.is_empty() || !message.contains(value),
                "the message must never repeat what the user wrote: {message}"
            );
        }

        // The accepted spelling, so the loop above is not rejecting everything.
        let parsed: Config = toml::from_str("[tui]\nmode = \"inline\"\n").unwrap();
        assert_eq!(parsed.tui.unwrap_or_default().mode, TuiMode::Inline);
    }

    /// Fullscreen is T5's renderer, and **T5-S2 is where it arrives**, so
    /// `[tui] mode = "fullscreen"` parses.
    ///
    /// **This is the deliberate change the test asked for.** Its own comment
    /// said "**Change this test deliberately in T5**, not incidentally", and
    /// this card is that T5: the switch, the renderer request, the alternate
    /// screen and the ring reader land together. The intent that survives is
    /// the one it was written for — **no silent coercion**: before, a third
    /// name must not quietly parse as `inline`; now it must parse as itself,
    /// `TuiMode::Fullscreen`, and the test still fails if `fullscreen` is
    /// mapped onto either of the other two.
    ///
    /// Decision 1 of this card is why a config test changes at all: `/tui`,
    /// `--tui` and `[tui] mode` cross together, because a mode a user can
    /// switch to but cannot start in is incoherent.
    #[test]
    fn tui_mode_accepts_fullscreen_now_that_the_t5_renderer_exists() {
        let parsed: Config = toml::from_str("[tui]\nmode = \"fullscreen\"\n")
            .expect("fullscreen parses now that the T5 renderer is reachable");
        let mode = parsed.tui.unwrap_or_default().mode;
        // **Tightened as the test author instructed.** This was an
        // `assert_ne!(mode, TuiMode::Inline)` / `assert_ne!(mode,
        // TuiMode::Classic)` pair, which the author wrote only because
        // `TuiMode::Fullscreen` did not exist yet: naming it would have been
        // a **compile** failure and every other test in the crate would have
        // stopped reporting. The variant exists now, so the pair is replaced
        // by the equality it was standing in for. "Neither classic nor
        // inline" is "fullscreen" only while there are exactly three modes —
        // a fourth would let it pass for the wrong mode, which is the F41
        // shape. Both of the intents it carried survive here: no coercion
        // onto either of the other two renderers, and `fullscreen` parsing as
        // itself.
        assert_eq!(
            mode,
            TuiMode::Fullscreen,
            "`fullscreen` must parse as itself and must not be coerced onto \
             the inline viewport or onto classic — the silent fallback this \
             test has guarded since T3-S2"
        );

        // The byte-exact discipline is unchanged by the new variant: a near
        // miss on the new name is still a loud error, not a third coercion.
        for typo in [
            "Fullscreen",
            "FULLSCREEN",
            "fullscreen ",
            "full-screen",
            "full",
        ] {
            let text = format!("[tui]\nmode = \"{typo}\"\n");
            toml::from_str::<Config>(&text)
                .expect_err(&format!("mode = {typo:?} must be rejected, not defaulted"));
        }
    }

    /// `alternate_screen` is the tri-state of design §4.8, not a boolean. A
    /// boolean is the mistake a reader makes from the key's name, so it is an
    /// error naming the key rather than a coercion.
    #[test]
    fn alternate_screen_is_a_tri_state_and_rejects_booleans() {
        for (text, expected) in [
            (
                "[tui]\nalternate_screen = \"auto\"\n",
                AlternateScreen::Auto,
            ),
            (
                "[tui]\nalternate_screen = \"always\"\n",
                AlternateScreen::Always,
            ),
            (
                "[tui]\nalternate_screen = \"never\"\n",
                AlternateScreen::Never,
            ),
        ] {
            let parsed: Config = toml::from_str(text).expect(text);
            assert_eq!(parsed.tui.unwrap_or_default().alternate_screen, expected);
        }

        let rendered = load_config_text("[tui]\nalternate_screen = true\n")
            .expect_err("a boolean must not be coerced to auto/always")
            .to_string();
        assert!(
            rendered.contains("alternate_screen"),
            "the error must name the key the user got wrong: {rendered}"
        );

        let error = toml::from_str::<Config>("[tui]\nalternate_screen = \"sometimes\"\n")
            .expect_err("an unknown tri-state value must fail closed loudly");
        // Authorised change (coordinator, T3-S2): same message as the boolean
        // above, naming the key and the tri-state and never the value, so all
        // six keys of this table fail with one vocabulary.
        assert!(
            error
                .message()
                .contains("invalid value for `[tui]` key `alternate_screen`")
                && error.message().contains("`auto`, `always`, `never`"),
            "{error}"
        );
    }

    /// `title` and `left_arrow_opens_agents` are booleans and stay booleans:
    /// a string `"true"` is a typo, not a value.
    #[test]
    fn tui_boolean_keys_reject_strings_naming_the_key() {
        for key in ["title", "left_arrow_opens_agents"] {
            let rendered = load_config_text(&format!("[tui]\n{key} = \"true\"\n"))
                .expect_err(&format!("{key} = \"true\" must be rejected"))
                .to_string();
            assert!(
                rendered.contains(key),
                "the error must name the key the user got wrong: {rendered}"
            );
        }

        // Real booleans still parse, both ways round.
        let off: Config =
            toml::from_str("[tui]\ntitle = false\nleft_arrow_opens_agents = false\n").unwrap();
        let off = off.tui.unwrap_or_default();
        assert!(!off.title);
        assert!(!off.left_arrow_opens_agents);
    }

    /// The memory bound (T3-S6's risk gate N4) is stated at the parse. Zero is
    /// not "disabled" and `u64::MAX` is not "unbounded": both are rejected
    /// with the key named and the accepted range shown, because a bound the
    /// user cannot read is a bound they will trip over at runtime instead.
    #[test]
    fn cell_ring_lines_rejects_zero_and_absurd_values_naming_the_key() {
        assert_eq!(TuiConfig::MIN_CELL_RING_LINES, 1);
        assert_eq!(TuiConfig::MAX_CELL_RING_LINES, 1_000_000);

        for value in ["0", "1000001", "4000000000", "-1", "1.5", "\"10000\""] {
            let text = format!("[tui]\ncell_ring_lines = {value}\n");
            let rendered = load_config_text(&text)
                .expect_err(&format!(
                    "cell_ring_lines = {value} must be rejected, not clamped"
                ))
                .to_string();
            assert!(
                rendered.contains("cell_ring_lines"),
                "the error must name the key for {value}: {rendered}"
            );
        }

        // ...and the bounds themselves are accepted, so the range check is
        // not simply rejecting everything.
        for value in [
            TuiConfig::MIN_CELL_RING_LINES,
            TuiConfig::DEFAULT_CELL_RING_LINES,
            TuiConfig::MAX_CELL_RING_LINES,
        ] {
            let parsed = load_config_text(&format!("[tui]\ncell_ring_lines = {value}\n"))
                .unwrap_or_else(|error| panic!("{value} must be accepted: {error}"));
            assert_eq!(parsed.tui.unwrap_or_default().cell_ring_lines, value);
        }
    }

    /// The configured bound reaches the ring, at each documented edge, with
    /// the constants read from [`TuiConfig`] so the test and the config cannot
    /// drift (plan row T3-S6, requirement **N4**).
    ///
    /// `MIN` (1) retains exactly one line; `DEFAULT` (10 000) and `MAX`
    /// (1 000 000) are above any plausible push count here, so the ring keeps
    /// everything pushed and evicts nothing — which is what proves the bound
    /// is the *configured* number and not a hardcoded one.
    ///
    /// Not vacuous: at each bound the retained contents are asserted, not just
    /// a count, and the `MIN` case evicts while the others do not, so a ring
    /// that ignored its argument fails one half or the other.
    #[test]
    fn the_configured_cell_ring_bound_reaches_the_ring_at_its_edges() {
        use kitsu_render::surface::Fd;
        use kitsu_tui::cells::CellRing;

        let mut minimum = CellRing::new(TuiConfig::MIN_CELL_RING_LINES);
        for text in ["a", "b", "c"] {
            minimum.push(Fd::Stdout, text);
        }
        assert_eq!(minimum.limit(), TuiConfig::MIN_CELL_RING_LINES);
        assert_eq!(minimum.len(), 1);
        assert_eq!(
            minimum
                .lines()
                .iter()
                .map(|l| l.plain())
                .collect::<Vec<_>>(),
            vec!["c".to_string()]
        );
        assert_eq!(minimum.evicted(), 2);

        for limit in [
            TuiConfig::DEFAULT_CELL_RING_LINES,
            TuiConfig::MAX_CELL_RING_LINES,
        ] {
            let mut ring = CellRing::new(limit);
            for n in 0..256 {
                ring.push(Fd::Stderr, &format!("line {n}"));
            }
            assert_eq!(ring.limit(), limit);
            assert_eq!(ring.len(), 256, "a bound of {limit} evicts nothing here");
            assert_eq!(ring.evicted(), 0);
            assert_eq!(
                ring.lines().last().expect("a line").plain(),
                "line 255",
                "the newest line is last"
            );
        }
    }

    /// An empty prefix is not a prefix. The chord model (design §642) needs a
    /// key to hang the chords on, so a blank value is an error naming the key
    /// rather than a silently chord-less session.
    #[test]
    fn tui_prefix_rejects_an_empty_value_naming_the_key() {
        let rendered = load_config_text("[tui]\nprefix = \"\"\n")
            .expect_err("an empty prefix must be rejected")
            .to_string();
        assert!(rendered.contains("prefix"), "{rendered}");
    }

    /// The three optional tables are independent: `[tui]` must not disturb
    /// `[agents]` or `[hooks]`, and the flat keys keep working beside it.
    #[test]
    fn tui_agents_and_hooks_tables_coexist() {
        let parsed: Config = toml::from_str(
            "agent_spawn_cap = 3\n\n\
             [tui]\nmode = \"inline\"\n\n\
             [agents]\nstate_file = \"on\"\n\n\
             [hooks]\non_agent_state = \"notify\"\n",
        )
        .expect("all three tables must coexist");
        assert_eq!(parsed.agent_spawn_cap, Some(3));
        assert_eq!(parsed.tui.unwrap_or_default().mode, TuiMode::Inline);
        assert_eq!(
            parsed.agents.unwrap_or_default().state_file,
            StateFileMode::On
        );
        assert_eq!(
            parsed.hooks.unwrap_or_default().on_agent_state.as_deref(),
            Some("notify")
        );
    }
}
