//! Config model + loading (docs/architecture.md §2). Parses the
//! `[gateways.<name>]` profiles and `default_gateway`, resolves a profile by
//! name, and validates flavor/model per the design's rules.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Top-level config file shape.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default_gateway: Option<String>,
    #[serde(default)]
    pub gateways: BTreeMap<String, Gateway>,
}

/// A single named gateway profile.
#[derive(Debug, Clone, Deserialize)]
pub struct Gateway {
    pub flavor: String,
    pub base_url: String,
    #[serde(default = "default_chat_path")]
    pub chat_path: String,
    #[serde(default)]
    pub models_path: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub auth: AuthConfig,
}

fn default_chat_path() -> String {
    "/v1/chat/completions".to_string()
}

/// Auth block within a gateway profile.
#[derive(Debug, Clone, Deserialize)]
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

fn default_auth_method() -> String {
    "bearer-env".to_string()
}
fn default_auth_env() -> String {
    "ANTHROPIC_AUTH_TOKEN".to_string()
}
fn default_expires_env() -> Option<String> {
    Some("ANTHROPIC_TOKEN_EXPIRES".to_string())
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
    Parse(String),
    UnknownGateway {
        name: String,
        defined: String,
    },
    NoGateway,
    UnsupportedFlavor {
        flavor: String,
    },
    ModelRequired,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read { path, source } => {
                write!(f, "config error: cannot read {path}: {source}")
            }
            ConfigError::Parse(msg) => write!(f, "config error: {msg}"),
            ConfigError::UnknownGateway { name, defined } => write!(
                f,
                "config error: unknown gateway '{name}' — defined gateways: {defined}"
            ),
            ConfigError::NoGateway => write!(
                f,
                "config error: no gateway selected — pass --gateway <name> or set default_gateway"
            ),
            ConfigError::UnsupportedFlavor { flavor } => write!(
                f,
                "config error: unsupported flavor '{flavor}' — expected one of: openai, anthropic, ollama"
            ),
            ConfigError::ModelRequired => {
                write!(f, "provider.model is required — set it with --model <name>")
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
        let home = std::env::var("HOME").unwrap_or_default();
        Path::new(&home).join(".kitsu").join("config.toml")
    }

    /// Load and parse the config file. A missing file yields an empty config
    /// (so `--gateway` validation can still produce a clear error).
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string())),
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
            None => {
                let defined = self.gateways.keys().cloned().collect::<Vec<_>>().join(", ");
                Err(ConfigError::UnknownGateway {
                    name: name.to_string(),
                    defined,
                })
            }
        }
    }
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
