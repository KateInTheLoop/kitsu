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
use kitsu_providers::{ChatMessage, Provider, ProviderError};
use kitsu_tools::{builtins, Decision, Mode, PermissionRequest, Permissions, Prompter, Registry};

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
            allow_write,
            script,
        } => runtime.block_on(run_chat(
            &cli,
            prompt.as_deref(),
            allow_paths,
            allow_write,
            script.as_deref(),
        )),
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

    Ok(ChatSetup { provider, model })
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
        max_tokens: cli.max_output_tokens,
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
            "You are kitsu, a concise, helpful CLI assistant with these filesystem tools: {}. \
             When the user refers to a local file or directory, USE these tools to inspect it \
             directly — call fs_list_dir to see a directory's contents and fs_read_file to read a \
             file. Do NOT ask the user to paste file contents or list filenames; read them \
             yourself. Access is permission-gated, so simply attempt the tool call — the user is \
             asked to approve paths as needed. When asked to generate or edit code or files, use \
             fs_write_file.",
            tool_names.join(", ")
        )
    }
}

/// Run turns from a script file non-interactively, threading multi-turn history
/// and printing a readable transcript (`user> …` then the response). Each line
/// is one turn; blank lines and `#` comments are skipped; `/slash` commands work.
///
/// A `<<<` line begins a MULTI-LINE turn (a pasted code block or stack trace),
/// preserved verbatim with its newlines until a closing `>>>` line — the scripted
/// equivalent of a bracketed paste in the interactive REPL.
async fn run_scripted(
    setup: &ChatSetup,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    model: &mut String,
    cli: &Cli,
    path: &str,
    perms: &Permissions,
) -> Result<(), CliError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| CliError::user(format!("cannot read script '{path}': {e}")))?;
    let lines: Vec<&str> = content.lines().collect();
    let mut idx = 0;
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        idx += 1;

        if trimmed == "<<<" {
            // Collect a verbatim multi-line block until the closing ">>>".
            let mut block: Vec<&str> = Vec::new();
            while idx < lines.len() && lines[idx].trim() != ">>>" {
                block.push(lines[idx]);
                idx += 1;
            }
            idx += 1; // consume the closing ">>>" (if present)
            run_scripted_turn(setup, tools, messages, model, cli, &block.join("\n")).await;
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('/') {
            match handle_slash(rest, messages, model, &setup.provider, perms).await {
                SlashResult::Continue => continue,
                SlashResult::Quit => break,
            }
        }
        run_scripted_turn(setup, tools, messages, model, cli, trimmed).await;
    }
    Ok(())
}

/// Send one scripted user turn and render the response (errors are printed, not
/// propagated — a bad turn shouldn't abort the rest of the script).
async fn run_scripted_turn(
    setup: &ChatSetup,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    model: &str,
    cli: &Cli,
    input: &str,
) {
    println!("user> {input}");
    messages.push(ChatMessage::user(input));
    let cfg = AgentConfig {
        model: model.to_string(),
        max_rounds: cli.max_rounds,
        max_context_bytes: 128 * 1024,
        stream: !cli.no_stream,
        temperature: cli.temperature,
        max_tokens: cli.max_output_tokens,
    };
    match run_agent_turn(&setup.provider, tools, messages, &cfg).await {
        Ok(text) => {
            if cli.no_stream {
                println!("{text}");
            } else {
                kitsu_render::finish_line();
            }
        }
        Err(e) => {
            kitsu_render::finish_line();
            eprintln!("{}", chat_error_message(e, model));
        }
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
    allow_write: &[String],
    script: Option<&str>,
) -> Result<(), CliError> {
    let setup = build_setup(cli)?;

    // Permissions gate the fs tools. Interactive TTY chat uses "ask" mode (prompt
    // on demand) with read access to the launch directory by default; scripted /
    // piped runs are deny-by-default (or --yes to auto-approve). --allow-path /
    // --allow-write pre-grant paths so those never prompt.
    let scripted = script.is_some();
    let interactive = kitsu_render::stdin_is_tty() && !scripted;
    let mode = if cli.yes {
        Mode::AutoAllow
    } else if interactive {
        Mode::Ask
    } else {
        Mode::Deny
    };
    let mut read_roots: Vec<String> = allow_paths.to_vec();
    if interactive {
        if let Ok(cwd) = std::env::current_dir() {
            read_roots.push(cwd.to_string_lossy().to_string());
        }
    }
    let mut perms = Permissions::new(&read_roots, allow_write, mode)
        .map_err(|e| CliError::user(e.to_string()))?;
    if mode == Mode::Ask {
        perms = perms.with_prompter(Arc::new(CliPrompter));
    }
    let perms = Arc::new(perms);

    // In interactive chat the tools are always available (permissions gate access
    // at call time). Non-interactively, register a tool only when it can actually
    // succeed — so scripted runs with no flags advertise nothing, and a read-only
    // grant doesn't advertise fs_write_file only to always deny it.
    let can_read = interactive || cli.yes || !allow_paths.is_empty();
    let can_write = interactive || cli.yes || !allow_write.is_empty();
    let mut tools = Registry::new();
    if can_read || can_write {
        builtins::register_fs_tools(&mut tools, perms.clone());
    }
    if can_write {
        builtins::register_write_tools(&mut tools, perms.clone());
    }
    let tool_names: Vec<String> = tools.descriptors().iter().map(|d| d.name.clone()).collect();

    let mut model = setup.model.clone();
    let base_cfg = |model: String, cli: &Cli| AgentConfig {
        model,
        max_rounds: cli.max_rounds,
        max_context_bytes: 128 * 1024,
        stream: !cli.no_stream,
        temperature: cli.temperature,
        max_tokens: cli.max_output_tokens,
    };

    let mut messages = vec![ChatMessage::system(chat_system_prompt(&tool_names))];

    // Scripted mode: run turns from a file non-interactively, then exit. Works
    // regardless of TTY, so it's the deterministic multi-turn driver.
    if let Some(path) = script {
        return run_scripted(&setup, &tools, &mut messages, &mut model, cli, path, &perms).await;
    }

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

    // Interactive TTY loop. Explicitly enable bracketed paste so a pasted
    // multi-line block (a code snippet or stack trace — the bread and butter of
    // coding chats) is captured as ONE turn instead of each line submitting
    // separately. It defaults on in rustyline 14, but we set it deliberately.
    let rl_config = rustyline::Config::builder().bracketed_paste(true).build();
    let mut rl = rustyline::DefaultEditor::with_config(rl_config)
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
        // A multi-line line came from a bracketed paste — acknowledge it so the
        // user sees the block landed as one turn.
        if line.contains('\n') {
            eprintln!("[pasted {} lines]", line.lines().count());
        }
        let _ = rl.add_history_entry(line);

        if let Some(rest) = line.strip_prefix('/') {
            match handle_slash(rest, &mut messages, &mut model, &setup.provider, &perms).await {
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
                eprintln!("{}", chat_error_message(e, &model));
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
async fn handle_slash(
    cmd: &str,
    messages: &mut Vec<ChatMessage>,
    model: &mut String,
    provider: &OpenAICompatProvider,
    perms: &Permissions,
) -> SlashResult {
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match name {
        "quit" | "exit" => return SlashResult::Quit,
        "help" => {
            eprintln!(
                "commands:\n  /model <name>  switch model (keeps history)\n  /models [filter]  \
                 list models the gateway offers (optionally filtered)\n  /allow <path>  grant \
                 read access to a path (no more prompts there)\n  /allow-write <path>  grant \
                 write access to a path\n  /save <file>   write the last response to <file> \
                 (code blocks if present)\n  /clear         clear conversation (keeps system \
                 prompt)\n  /help          this help\n  /quit          exit\ncurrent model: {model}"
            );
        }
        "models" => list_models_and_print(provider, arg).await,
        "allow" => {
            if arg.is_empty() {
                eprintln!("usage: /allow <path> (grant read access to a directory/file)");
            } else {
                match perms.grant_read_root(arg) {
                    Ok(p) => eprintln!("~ read access granted: {}", p.display()),
                    Err(e) => eprintln!("{e}"),
                }
            }
        }
        "allow-write" => {
            if arg.is_empty() {
                eprintln!("usage: /allow-write <path> (grant write access to a directory)");
            } else {
                match perms.grant_write_root(arg) {
                    Ok(p) => eprintln!("~ write access granted: {}", p.display()),
                    Err(e) => eprintln!("{e}"),
                }
            }
        }
        "save" => {
            if arg.is_empty() {
                eprintln!("usage: /save <file> (writes the last response; code blocks if present)");
            } else {
                match messages
                    .iter()
                    .rev()
                    .find(|m| matches!(m.role, kitsu_providers::Role::Assistant))
                {
                    Some(m) => {
                        let body = extract_code_or_text(&m.content);
                        match std::fs::write(arg, body.as_bytes()) {
                            Ok(()) => eprintln!("~ saved {} bytes to {arg}", body.len()),
                            Err(e) => eprintln!("save failed: {e}"),
                        }
                    }
                    None => eprintln!("nothing to save yet"),
                }
            }
        }
        "clear" => {
            // Keep only a leading system message, if present.
            let system = messages
                .first()
                .filter(|m| matches!(m.role, kitsu_providers::Role::System))
                .cloned();
            messages.clear();
            if let Some(s) = system {
                messages.push(s);
            }
            eprintln!("~ conversation cleared");
        }
        "model" => {
            if arg.is_empty() {
                eprintln!("usage: /model <name> (current: {model}); /models to list available");
            } else if arg == "list" || arg.starts_with("list ") {
                // Alias: `/model list [filter]` → same as `/models [filter]`.
                let filter = arg.strip_prefix("list").unwrap_or("").trim();
                list_models_and_print(provider, filter).await;
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

/// For `/save`: if the text contains fenced ``` code blocks, return the
/// concatenation of their contents (so a `.rs`/`.py` file gets just the code);
/// otherwise return the whole text. Useful for the "ask for code, then save it"
/// coding workflow — a lightweight bridge between paste and file generation.
fn extract_code_or_text(text: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_block = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_block {
                blocks.push(std::mem::take(&mut cur));
            }
            in_block = !in_block;
            continue;
        }
        if in_block {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if blocks.is_empty() {
        text.to_string()
    } else {
        blocks.join("\n")
    }
}

/// Fetch and print the models the gateway advertises (via its `models_path`),
/// optionally filtered by a case-insensitive substring. Output goes to stderr
/// like other slash commands. The listed IDs are what you pass to `/model <id>`
/// or `--model` — handy for exploring a big catalog (e.g. Hugging Face).
async fn list_models_and_print(provider: &OpenAICompatProvider, filter: &str) {
    match provider.list_models().await {
        Ok(Some(models)) => {
            let filter = filter.trim().to_lowercase();
            let matched: Vec<&String> = models
                .iter()
                .filter(|m| filter.is_empty() || m.to_lowercase().contains(&filter))
                .collect();
            let total = matched.len();
            if filter.is_empty() {
                eprintln!("~ {total} model(s) available:");
            } else {
                eprintln!("~ {total} model(s) matching '{filter}':");
            }
            const CAP: usize = 40;
            for m in matched.iter().take(CAP) {
                eprintln!("  {m}");
            }
            if total > CAP {
                eprintln!(
                    "  … and {} more — narrow with `/models <substring>`",
                    total - CAP
                );
            }
            if total > 0 {
                eprintln!("switch with: /model <id>");
            }
        }
        Ok(None) => {
            eprintln!("this gateway does not expose a model list (its models_path is empty)")
        }
        Err(e) => eprintln!("could not fetch models: {e}"),
    }
}

/// Prints a permission prompt to stderr and reads the user's choice from stdin.
/// Used only in interactive `Mode::Ask`; the prompt fires mid-turn while
/// rustyline is idle. Bracketed paste is disabled around the read so a stray
/// paste can't inject `ESC[200~` markers, and any extra buffered input is
/// drained afterward so leftover pasted lines don't leak into the next turn.
/// Anything other than `y`/`a` (including empty input or EOF) denies.
struct CliPrompter;

impl Prompter for CliPrompter {
    fn ask(&self, req: &PermissionRequest) -> Decision {
        use std::io::Write;
        // Disable bracketed paste (ESC[?2004l) for the raw read; rustyline
        // re-enables it on the next prompt.
        eprint!(
            "\x1b[?2004l\n\u{23fa} kitsu wants to {} {}\n  [y] allow once   [a] always ({})   [n] deny\n> ",
            req.action.verb(),
            req.path,
            req.grant_dir
        );
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let read = std::io::stdin().read_line(&mut line);
        // Drop any remaining buffered input (e.g. the rest of a pasted block) so
        // it isn't picked up as the next turn.
        drain_stdin();
        if read.is_err() {
            return Decision::Deny;
        }
        // Take the first meaningful character, tolerating a stray paste marker.
        let ans = line.trim().trim_start_matches("\u{1b}[200~").trim_start();
        match ans.chars().next().map(|c| c.to_ascii_lowercase()) {
            Some('y') => Decision::AllowOnce,
            Some('a') => Decision::AllowAlways,
            _ => Decision::Deny,
        }
    }
}

/// Discard any input currently buffered on stdin without blocking. Used after a
/// permission prompt so leftover bytes from a multi-line paste don't spill into
/// the next `readline`.
#[cfg(unix)]
fn drain_stdin() {
    use std::io::Read;
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return;
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return;
        }
        let mut buf = [0u8; 4096];
        let mut stdin = std::io::stdin();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 {
                break;
            }
        }
        libc::fcntl(fd, libc::F_SETFL, flags); // restore blocking
    }
}

#[cfg(not(unix))]
fn drain_stdin() {}

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

/// Render an agent error for the interactive/scripted chat loop. When the
/// gateway rejected the model id (ISSUE-004: a `/model` typo, or a model the
/// gateway doesn't serve), add an actionable hint naming the model and pointing
/// at `/model`, instead of dumping the raw HTTP 404 JSON.
fn chat_error_message(e: kitsu_core::AgentError, model: &str) -> String {
    if let kitsu_core::AgentError::Provider(ProviderError::Http { status, body }) = &e {
        let looks_like_bad_model =
            *status == 404 || (body.contains("model") && body.to_lowercase().contains("not found"));
        if looks_like_bad_model {
            return format!(
                "gateway rejected model '{model}' (HTTP {status}) — run /model <valid-id> and \
                 check the gateway's available models.\n  detail: {body}"
            );
        }
    }
    map_agent_error(e).to_string()
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
