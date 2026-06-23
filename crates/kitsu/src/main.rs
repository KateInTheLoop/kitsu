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
use kitsu_tools::{builtins, Policy, Registry};

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
        Command::Chat {
            prompt,
            allow_paths,
        } => runtime.block_on(run_chat(&cli, prompt.as_deref(), allow_paths)),
    }
}

/// Provider + model + auth resolved from config and CLI flags. Shared by the
/// `ask` and `chat` commands so they construct the gateway connection identically.
struct ChatSetup {
    provider: OpenAICompatProvider,
    model: String,
}

/// Resolve config + gateway profile and build the provider/auth/model. Emits the
/// best-effort token-expiry warning (warn-but-send, design §3).
fn build_setup(cli: &Cli) -> Result<ChatSetup, CliError> {
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

    let auth = build_auth(gateway);
    let models_path = gateway.models_path.clone().filter(|p| !p.is_empty());
    let provider = OpenAICompatProvider::new(
        OpenAICompatConfig {
            base_url: gateway.base_url.clone(),
            chat_path: gateway.chat_path.clone(),
            models_path,
            retries: 2,
            timeout: Duration::from_secs(cli.timeout_secs),
        },
        auth.clone(),
    );

    if let kitsu_providers::ExpiryStatus::Expired { since } = auth.expiry_status() {
        let hint = gateway.auth.refresh_hint.clone().unwrap_or_default();
        eprintln!("warning: gateway token expired (since {since}) — sending anyway. {hint}");
    }

    Ok(ChatSetup {
        provider,
        model,
    })
}

async fn run_ask(cli: &Cli, prompt: &str) -> Result<(), CliError> {
    let setup = build_setup(cli)?;

    let mut messages = vec![ChatMessage::user(prompt)];
    let tools = Registry::new();
    let agent_cfg = AgentConfig {
        model: setup.model.clone(),
        max_rounds: cli.max_rounds,
        max_context_bytes: 128 * 1024,
        stream: !cli.no_stream,
        temperature: cli.temperature,
    };

    match run_agent_turn(&setup.provider, &tools, &mut messages, &agent_cfg).await {
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

// --- chat REPL --------------------------------------------------------------

/// System prompt seeding every chat session.
fn chat_system_prompt(tool_names: &[String]) -> String {
    if tool_names.is_empty() {
        "You are kitsu, a concise, helpful CLI assistant.".to_string()
    } else {
        format!(
            "You are kitsu, a concise, helpful CLI assistant. You can call these read-only \
             tools when useful: {}. Only read paths the user has allowed.",
            tool_names.join(", ")
        )
    }
}

/// Outcome of a single (cancellable) turn.
enum TurnOutcome {
    Completed(Result<String, kitsu_core::AgentError>),
    Cancelled,
}

/// Run one turn, racing it against Ctrl+C. The turn future is awaited directly
/// (never spawned), so a Ctrl+C drops it — and with it the in-flight HTTP
/// request — with no leaked task (design §10).
async fn run_turn_cancellable(
    provider: &OpenAICompatProvider,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
) -> TurnOutcome {
    let turn = run_agent_turn(provider, tools, messages, cfg);
    tokio::pin!(turn);
    tokio::select! {
        res = &mut turn => TurnOutcome::Completed(res),
        _ = tokio::signal::ctrl_c() => TurnOutcome::Cancelled,
    }
}

async fn run_chat(
    cli: &Cli,
    first_prompt: Option<&str>,
    allow_paths: &[String],
) -> Result<(), CliError> {
    let setup = build_setup(cli)?;

    // Build the tool registry from the allowlist. No --allow-path ⇒ no tools.
    let mut tools = Registry::new();
    if !allow_paths.is_empty() {
        let policy =
            Policy::from_roots(allow_paths).map_err(|e| CliError::user(e.to_string()))?;
        builtins::register_fs_tools(&mut tools, Arc::new(policy));
    }
    let tool_names: Vec<String> = tools.descriptors().iter().map(|d| d.name.clone()).collect();

    let mut model = setup.model.clone();
    let base_cfg = |model: String, cli: &Cli| AgentConfig {
        model,
        max_rounds: cli.max_rounds,
        max_context_bytes: 128 * 1024,
        stream: !cli.no_stream,
        temperature: cli.temperature,
    };

    let mut messages = vec![ChatMessage::system(chat_system_prompt(&tool_names))];

    // Non-TTY stdin → one-shot, no interactive loop (development_plan.md).
    if !kitsu_render::stdin_is_tty() {
        let prompt = match first_prompt {
            Some(p) => p.to_string(),
            None => {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| CliError::user(format!("failed to read stdin: {e}")))?;
                buf.trim().to_string()
            }
        };
        if prompt.is_empty() {
            return Err(CliError::user(
                "chat: no input (provide a prompt or pipe text on stdin)".to_string(),
            ));
        }
        messages.push(ChatMessage::user(prompt));
        let cfg = base_cfg(model, cli);
        return match run_agent_turn(&setup.provider, &tools, &mut messages, &cfg).await {
            Ok(text) => {
                if cli.no_stream {
                    println!("{text}");
                } else {
                    kitsu_render::finish_line();
                }
                Ok(())
            }
            Err(e) => Err(map_agent_error(e)),
        };
    }

    // Interactive TTY loop.
    let mut rl = rustyline::DefaultEditor::new()
        .map_err(|e| CliError::user(format!("failed to start input editor: {e}")))?;
    let history_path = Config::resolve_path(cli.config.as_deref())
        .parent()
        .map(|p| p.join("history"))
        .unwrap_or_else(|| std::path::PathBuf::from(".kitsu_history"));
    let _ = rl.load_history(&history_path);

    if !tool_names.is_empty() {
        eprintln!("kitsu chat — tools enabled: {}", tool_names.join(", "));
    } else {
        eprintln!("kitsu chat — type /help for commands, Ctrl+D to exit");
    }

    // If a first prompt was given, queue it as the first turn.
    let mut queued = first_prompt.map(|s| s.to_string());
    let prompt = kitsu_render::prompt_string();

    loop {
        let line = match queued.take() {
            Some(p) => p,
            None => match rl.readline(&prompt) {
                Ok(l) => l,
                Err(rustyline::error::ReadlineError::Eof) => break,
                Err(rustyline::error::ReadlineError::Interrupted) => break, // Ctrl+C at idle prompt exits
                Err(e) => {
                    eprintln!("input error: {e}");
                    break;
                }
            },
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(line);

        if let Some(rest) = line.strip_prefix('/') {
            match handle_slash(rest, &mut messages, &mut model) {
                SlashResult::Continue => continue,
                SlashResult::Quit => break,
            }
        }

        messages.push(ChatMessage::user(line));
        let cfg = base_cfg(model.clone(), cli);
        match run_turn_cancellable(&setup.provider, &tools, &mut messages, &cfg).await {
            TurnOutcome::Completed(Ok(_)) => kitsu_render::finish_line(),
            TurnOutcome::Completed(Err(e)) => {
                kitsu_render::finish_line();
                eprintln!("{}", map_agent_error(e));
            }
            TurnOutcome::Cancelled => {
                kitsu_render::finish_line();
                eprintln!("^C (response cancelled)");
            }
        }
    }

    let _ = rl.save_history(&history_path);
    Ok(())
}

enum SlashResult {
    Continue,
    Quit,
}

/// Handle a slash command (the leading `/` already stripped).
fn handle_slash(cmd: &str, messages: &mut Vec<ChatMessage>, model: &mut String) -> SlashResult {
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match name {
        "quit" | "exit" => return SlashResult::Quit,
        "help" => {
            eprintln!(
                "commands:\n  /model <name>  switch model (keeps history)\n  /clear         \
                 clear conversation (keeps system prompt)\n  /help          this help\n  \
                 /quit          exit\ncurrent model: {model}"
            );
        }
        "clear" => {
            // Keep only a leading system message, if present.
            let system = messages.first().filter(|m| {
                matches!(m.role, kitsu_providers::Role::System)
            }).cloned();
            messages.clear();
            if let Some(s) = system {
                messages.push(s);
            }
            eprintln!("~ conversation cleared");
        }
        "model" => {
            if arg.is_empty() {
                eprintln!("usage: /model <name> (current: {model})");
            } else {
                // Model validity is gateway-defined; accept any non-empty id.
                *model = arg.to_string();
                eprintln!("~ switched to {model}");
            }
        }
        other => {
            eprintln!("unknown command: /{other} (try /help)");
        }
    }
    SlashResult::Continue
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
