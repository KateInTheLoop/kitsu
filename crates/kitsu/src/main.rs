//! kitsu — CLI entry. MVP target: `kitsu ask --gateway example "hello"` runs the
//! agent loop against the gateway (Mistral). See docs/architecture.md §6.

mod cli;
mod config;

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use cli::{Cli, Command};
use config::{Config, Gateway};
use kitsu_core::{run_agent_turn, AgentConfig};
use kitsu_providers::auth::{AuthProvider, EnvBearerAuth, NoAuth, StaticAuth};
use kitsu_providers::openai_compat::{OpenAICompatConfig, OpenAICompatProvider};
use kitsu_providers::{ChatMessage, ProviderError};
use kitsu_tools::Registry;

/// Exit codes per design §6: 1 = user/config/auth-missing, 2 = provider.
const EXIT_USER: i32 = 1;
const EXIT_PROVIDER: i32 = 2;

fn main() {
    // Logs (never the token: HeaderValue is marked sensitive) go to stderr.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let code = match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            e.exit_code()
        }
    };
    std::process::exit(code);
}

/// Top-level error carrying the right exit code.
struct CliError {
    message: String,
    code: i32,
}

impl CliError {
    fn user(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_USER,
        }
    }
    fn provider(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_PROVIDER,
        }
    }
    fn exit_code(&self) -> i32 {
        self.code
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| CliError::user(format!("failed to start runtime: {e}")))?;

    match &cli.command {
        Command::Ask { prompt } => runtime.block_on(run_ask(&cli, prompt)),
        Command::Chat => Err(CliError::user(
            "chat: not yet implemented (MVP supports `ask`)".to_string(),
        )),
    }
}

async fn run_ask(cli: &Cli, prompt: &str) -> Result<(), CliError> {
    // Load + resolve config.
    let path = Config::resolve_path(cli.config.as_deref());
    let config = Config::load(&path).map_err(|e| CliError::user(e.to_string()))?;

    let (_name, gateway) = config
        .select_gateway(cli.gateway.as_deref())
        .map_err(|e| CliError::user(e.to_string()))?;
    gateway
        .validate_flavor()
        .map_err(|e| CliError::user(e.to_string()))?;

    let model = gateway
        .resolve_model(cli.model.as_deref())
        .map_err(|e| CliError::user(e.to_string()))?;

    // Construct auth + provider from the profile.
    let auth = build_auth(gateway);
    let models_path = gateway.models_path.clone().filter(|p| !p.is_empty());
    let provider = OpenAICompatProvider::new(
        OpenAICompatConfig {
            base_url: gateway.base_url.clone(),
            chat_path: gateway.chat_path.clone(),
            models_path,
            retries: 2,
            timeout: Duration::from_secs(60),
        },
        auth.clone(),
    );

    // Best-effort expiry warning before the request (warn-but-send, design §3).
    if let kitsu_providers::ExpiryStatus::Expired { since } = auth.expiry_status() {
        let hint = gateway.auth.refresh_hint.clone().unwrap_or_default();
        eprintln!("warning: gateway token expired (since {since}) — sending anyway. {hint}");
    }

    let messages = vec![ChatMessage::user(prompt)];
    let tools = Registry::new();
    let agent_cfg = AgentConfig {
        model,
        max_rounds: 10,
        max_context_bytes: 128 * 1024,
        stream: !cli.no_stream,
        temperature: cli.temperature,
    };

    match run_agent_turn(&provider, &tools, messages, &agent_cfg).await {
        Ok(text) => {
            // When streaming, fragments were already printed; ensure a trailing
            // newline. When not streaming, print the full text.
            if cli.no_stream {
                println!("{text}");
            } else {
                println!();
            }
            Ok(())
        }
        Err(e) => Err(map_agent_error(e)),
    }
}

/// Build the appropriate AuthProvider from the profile's auth block.
fn build_auth(gateway: &Gateway) -> Arc<dyn AuthProvider> {
    let auth = &gateway.auth;
    match auth.method.as_str() {
        "none" => Arc::new(NoAuth),
        "static" => {
            let token = auth.token.clone().unwrap_or_default();
            Arc::new(StaticAuth {
                token,
                header: auth.header.clone(),
                scheme: auth.scheme.clone(),
            })
        }
        // "bearer-env" and anything else default to env-bearer.
        _ => Arc::new(EnvBearerAuth {
            env: auth.env.clone(),
            expires_env: auth.expires_env.clone(),
            header: auth.header.clone(),
            scheme: auth.scheme.clone(),
            refresh_hint: auth.refresh_hint.clone(),
        }),
    }
}

/// Map an agent/provider error to the right exit code (design §6).
fn map_agent_error(e: kitsu_core::AgentError) -> CliError {
    match e {
        kitsu_core::AgentError::Provider(pe) => match pe {
            // Missing token is a user/auth-config problem → exit 1.
            ProviderError::Auth(ae) => CliError::user(ae.to_string()),
            // Gateway rejected the token → exit 2.
            ProviderError::Unauthorized { .. } => CliError::provider(pe.to_string()),
            other => CliError::provider(other.to_string()),
        },
        kitsu_core::AgentError::Loop(msg) => CliError::provider(format!("agent loop error: {msg}")),
    }
}
