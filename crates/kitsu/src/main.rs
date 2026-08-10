//! kitsu — CLI entry. MVP target: `kitsu ask --gateway example "hello"` runs the
//! agent loop against the gateway (Mistral). See docs/architecture.md §6.

mod cli;
mod config;
mod llms;
mod skills;
mod subagent;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use cli::{Cli, Command};
use config::{Config, ConfigError, Gateway};
use kitsu_core::context::{estimate_str, ContextBudget, ContextMeter, DEFAULT_CONTEXT_WINDOW};
use kitsu_core::{run_agent_turn, run_agent_turn_with_state, AgentConfig, CompactionMode};
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
    /// `Arc` so the main loop and spawned subagents can share one connection.
    provider: Arc<OpenAICompatProvider>,
    model: String,
    /// Resolved model context window in tokens, if known (design §3.2 order:
    /// `--context-window` → profile `context_window` → `/v1/models` → unset).
    context_window: Option<u64>,
    /// True when `context_window` came from an explicit `--context-window` flag
    /// or the gateway profile's `context_window`, rather than `/v1/models`
    /// discovery. A pinned window is a user/config declaration that overrides
    /// "any value discovered from `/v1/models`" (see the flag's doc comment) —
    /// `/model` must not silently replace it with fresh auto-discovery.
    context_window_pinned: bool,
}

/// Concatenate the bodies of the skills named via `--skill` into one context
/// block (or `None` if none/unknown). Unknown names warn and are skipped.
fn preload_skills(cli: &Cli, skillset: &skills::SkillSet) -> Option<String> {
    if cli.skill.is_empty() {
        return None;
    }
    let mut out = String::new();
    for name in &cli.skill {
        match skillset.get(name) {
            Some(s) => out.push_str(&format!("# Skill: {}\n{}\n\n", s.name, s.body)),
            None => eprintln!("~ no skill named '{name}' (skipped)"),
        }
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        None
    } else {
        eprintln!("~ preloaded {} skill(s)", cli.skill.len());
        Some(out)
    }
}

/// Register the base tool set (fs read/write, grep, skill) shared by the main
/// agent and by subagents. `spawn_agent` is registered separately and only for
/// the main agent, so subagents can't spawn further (depth-1).
fn register_base_tools(
    tools: &mut Registry,
    perms: &Arc<Permissions>,
    skillset: &Arc<skills::SkillSet>,
    can_read: bool,
    can_write: bool,
) {
    if can_read || can_write {
        builtins::register_fs_tools(tools, perms.clone());
    }
    if can_write {
        builtins::register_write_tools(tools, perms.clone());
    }
    if !skillset.is_empty() && (can_read || can_write) {
        let (d, h) = skills::skill_tool(skillset.clone(), perms.clone());
        tools.register_with_descriptor(d, h);
    }
}

/// Parse the `--compaction` flag into the orchestrator's mode.
fn compaction_mode(cli: &Cli) -> CompactionMode {
    if cli.compaction == "summarize" {
        CompactionMode::Summarize
    } else {
        CompactionMode::Trim
    }
}

/// Build the per-turn `AgentConfig` shared by `ask`, scripted runs, and
/// interactive chat, from a single place — so a future field/default change
/// can't silently apply to only some call sites (as `ask`'s hardcoded
/// `offload_dir: None` very nearly diverged from the other two paths).
fn build_agent_config(
    model: String,
    cli: &Cli,
    context_window: Option<u64>,
    offload_dir: Option<std::path::PathBuf>,
) -> AgentConfig {
    AgentConfig {
        model,
        max_rounds: cli.max_rounds,
        max_context_bytes: 128 * 1024,
        context_window,
        offload_dir,
        stream: !cli.no_stream,
        temperature: cli.temperature,
        max_tokens: cli.max_output_tokens,
        compaction: compaction_mode(cli),
        quiet: false,
    }
}

/// Removes the per-session offload scratch dir (and its files) when the chat
/// session ends, so we don't leak temp dirs into `/tmp`.
struct OffloadDirGuard(std::path::PathBuf);
impl Drop for OffloadDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// rustyline helper that collapses a pasted MULTI-LINE input to a compact chip
/// (`<first line>… [pasted N lines]`) in the editor display — Codex-style — so a
/// big paste doesn't flood the prompt. The full text is untouched in the buffer
/// and returned verbatim on Enter; only the on-screen rendering is collapsed.
struct PasteCollapse;

impl rustyline::completion::Completer for PasteCollapse {
    type Candidate = String;
}
impl rustyline::hint::Hinter for PasteCollapse {
    type Hint = String;
}
impl rustyline::validate::Validator for PasteCollapse {}
impl rustyline::Helper for PasteCollapse {}

impl rustyline::highlight::Highlighter for PasteCollapse {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> std::borrow::Cow<'l, str> {
        match line.split_once('\n') {
            Some((first, _)) => {
                let n = line.split('\n').count();
                let preview: String = first.chars().take(48).collect();
                let ell = if first.chars().count() > 48 {
                    "…"
                } else {
                    ""
                };
                std::borrow::Cow::Owned(format!("{preview}{ell} [pasted {n} lines]"))
            }
            None => std::borrow::Cow::Borrowed(line),
        }
    }
    /// Only re-render through `highlight` when the buffer is multi-line (a paste);
    /// normal single-line typing is shown as-is.
    fn highlight_char(&self, line: &str, _pos: usize, _forced: bool) -> bool {
        line.contains('\n')
    }
}

/// A provenance entry: a static context source, why it's loaded, its estimated
/// token cost, and the governing policy — honest accounting (design §3).
struct CtxEntry {
    label: String,
    tokens: u64,
    policy: &'static str,
    why: String,
}

impl CtxEntry {
    fn new(
        label: impl Into<String>,
        tokens: u64,
        policy: &'static str,
        why: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            tokens,
            policy,
            why: why.into(),
        }
    }
}

/// Context-window info for `/context`, assembled once per session. `provenance`
/// records how the static context (system prompt sources + tool schemas) was
/// built, so `/context` can show WHAT loaded, WHY, its tokens, and the policy.
/// `window` is shared with the `AgentConfig` builder (see `build_agent_config`
/// call sites) via a `Cell` so `/model` can update it in place for both.
struct CtxInfo {
    window: Rc<Cell<Option<u64>>>,
    max_tokens: Option<u32>,
    provenance: Vec<CtxEntry>,
}

fn ctx_info(
    context_window: Rc<Cell<Option<u64>>>,
    cli: &Cli,
    provenance: Vec<CtxEntry>,
) -> CtxInfo {
    CtxInfo {
        window: context_window,
        max_tokens: cli.max_output_tokens,
        provenance,
    }
}

/// Render `/context`: the static provenance rows (what loaded and why) plus the
/// live conversation, with token cost and policy per source. The per-turn
/// `kitsu-context:` line carries the usage-anchored number; this is an estimate.
fn render_context_command(model: &str, messages: &[ChatMessage], ctx: &CtxInfo) {
    let b = ContextMeter::new().breakdown(messages);
    let ctx_window = ctx.window.get();
    let window = ctx_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let budget = ContextBudget::new(window, ctx.max_tokens);
    let note = if ctx_window.is_some() {
        ""
    } else {
        "default — set context_window or --context-window"
    };
    // Static provenance (system prompt sources + tool schemas) then the live
    // conversation. breakdown.system is already covered by the provenance rows.
    let rows: Vec<(&str, u64, &str, &str)> = ctx
        .provenance
        .iter()
        .map(|e| (e.label.as_str(), e.tokens, e.policy, e.why.as_str()))
        .chain([
            ("user turns", b.user, "admitted", "your messages"),
            ("assistant turns", b.assistant, "admitted", "model replies"),
            (
                "tool results",
                b.tool_results,
                "admitted",
                "tool outputs (offloaded → pointers)",
            ),
        ])
        .collect();
    let prov_total: u64 = ctx.provenance.iter().map(|e| e.tokens).sum();
    let used = prov_total + b.user + b.assistant + b.tool_results;
    kitsu_render::render_context(model, window, note, budget.effective, used, &rows);
}

/// How long a discovered context window is trusted before re-discovering. A
/// model's window essentially never changes, so this just spares every later
/// invocation of the same gateway+model a `/v1/models` round-trip purely to
/// re-learn a number we already know.
const CONTEXT_WINDOW_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// On-disk cache for discovered context windows, keyed by `"<base_url>|<model>"`.
/// Without this, `build_setup` paid a blocking `/v1/models` round-trip on
/// *every single* `ask`/`chat` invocation whenever `context_window` wasn't
/// pre-configured — doubling perceived startup latency on a slow gateway, even
/// for one-off `ask` calls where the result is immediately thrown away.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ContextWindowCache {
    #[serde(default)]
    entries: std::collections::BTreeMap<String, (u64, u64)>,
}

fn context_window_cache_path(cli: &Cli) -> Option<std::path::PathBuf> {
    Config::resolve_path(cli.config.as_deref())
        .parent()
        .map(|p| p.join("context_window_cache.json"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_cached_context_window(path: &std::path::Path, key: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let cache: ContextWindowCache = serde_json::from_str(&text).ok()?;
    let (window, fetched_at) = cache.entries.get(key)?;
    (unix_now().saturating_sub(*fetched_at) < CONTEXT_WINDOW_CACHE_TTL_SECS).then_some(*window)
}

fn write_cached_context_window(path: &std::path::Path, key: &str, window: u64) {
    let mut cache: ContextWindowCache = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    cache.entries.insert(key.to_string(), (window, unix_now()));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = std::fs::write(path, json);
    }
}

/// Resolve config + gateway profile and build the provider/auth/model. Emits the
/// best-effort token-expiry warning (warn-but-send, design §3).
async fn build_setup(cli: &Cli) -> Result<ChatSetup, CliError> {
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

    let auth = build_auth(gateway).map_err(|e| CliError::user(e.to_string()))?;
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

    // Resolve the context window: explicit flag → profile → cached discovery →
    // fresh /v1/models discovery. Left None when nothing reports it (the
    // orchestrator then falls back to the coarse byte budget).
    let context_window_pinned = cli.context_window.or(gateway.context_window).is_some();
    let context_window = match cli.context_window.or(gateway.context_window) {
        Some(w) => Some(w),
        None => {
            let cache_key = format!("{}|{model}", gateway.base_url);
            let cache_path = context_window_cache_path(cli);
            let cached = cache_path
                .as_deref()
                .and_then(|p| read_cached_context_window(p, &cache_key));
            match cached {
                Some(w) => Some(w),
                None => {
                    let discovered = provider.model_context_length(&model).await;
                    if let (Some(w), Some(path)) = (discovered, cache_path.as_deref()) {
                        write_cached_context_window(path, &cache_key, w);
                    }
                    discovered
                }
            }
        }
    };

    Ok(ChatSetup {
        provider: Arc::new(provider),
        model,
        context_window,
        context_window_pinned,
    })
}

async fn run_ask(cli: &Cli, prompt: &str) -> Result<(), CliError> {
    let setup = build_setup(cli).await?;

    let mut messages = Vec::new();
    // Preloaded skills (--skill) work even in the tool-less `ask` path: inject the
    // bodies as a leading system message.
    if !cli.skill.is_empty() {
        let no_project = std::env::var_os("KITSU_NO_PROJECT_CONTEXT").is_some();
        let skillset = std::env::current_dir()
            .map(|cwd| skills::SkillSet::discover(&cwd, no_project))
            .unwrap_or_default();
        if let Some(pre) = preload_skills(cli, &skillset) {
            messages.push(ChatMessage::system(format!(
                "Follow these skill instructions:\n\n{pre}"
            )));
        }
    }
    messages.push(ChatMessage::user(prompt));
    let tools = Registry::new();
    let agent_cfg = build_agent_config(setup.model.clone(), cli, setup.context_window, None);

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
#[allow(clippy::too_many_arguments)]
async fn run_scripted(
    setup: &ChatSetup,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    model: &mut String,
    cli: &Cli,
    path: &str,
    perms: &Permissions,
    offload_dir: &Option<std::path::PathBuf>,
    skillset: &skills::SkillSet,
    ctx: &CtxInfo,
    context_window: &Rc<Cell<Option<u64>>>,
    live_mw: &subagent::LiveModelWindow,
    registry: &Arc<subagent::AgentRegistry>,
) -> Result<(), CliError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| CliError::user(format!("cannot read script '{path}': {e}")))?;
    // One session state for the whole script — the context-meter anchor and
    // compaction circuit breaker persist across turns, matching interactive chat
    // (a script is a multi-turn session, not a series of independent one-shots).
    let mut state = kitsu_core::AgentSessionState::default();
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
            run_scripted_turn(
                setup,
                tools,
                messages,
                model,
                cli,
                &block.join("\n"),
                offload_dir,
                &mut state,
                context_window,
            )
            .await;
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('/') {
            let result = handle_slash(
                rest,
                messages,
                model,
                &setup.provider,
                perms,
                ctx,
                skillset,
                context_window,
                setup.context_window_pinned,
                &mut state,
                registry,
            )
            .await;
            // Keep subagents in sync with any `/model` change in the script.
            *live_mw.lock().expect("live model/window") = (model.clone(), context_window.get());
            match result {
                SlashResult::Continue => continue,
                SlashResult::Quit => break,
            }
        }
        run_scripted_turn(
            setup,
            tools,
            messages,
            model,
            cli,
            trimmed,
            offload_dir,
            &mut state,
            context_window,
        )
        .await;
    }
    Ok(())
}

/// Send one scripted user turn and render the response (errors are printed, not
/// propagated — a bad turn shouldn't abort the rest of the script).
#[allow(clippy::too_many_arguments)]
async fn run_scripted_turn(
    setup: &ChatSetup,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    model: &str,
    cli: &Cli,
    input: &str,
    offload_dir: &Option<std::path::PathBuf>,
    state: &mut kitsu_core::AgentSessionState,
    context_window: &Rc<Cell<Option<u64>>>,
) {
    println!("user> {input}");
    messages.push(ChatMessage::user(input));
    let cfg = build_agent_config(
        model.to_string(),
        cli,
        context_window.get(),
        offload_dir.clone(),
    );
    match run_agent_turn_with_state(&setup.provider, tools, messages, &cfg, state).await {
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
/// request — with no leaked task (design §10). `state` persists the context-meter
/// anchor and compaction circuit breaker across the session's turns.
async fn run_turn_cancellable(
    provider: &OpenAICompatProvider,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
    state: &mut kitsu_core::AgentSessionState,
) -> TurnOutcome {
    let turn = run_agent_turn_with_state(provider, tools, messages, cfg, state);
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
    let setup = build_setup(cli).await?;

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

    // Discover LOCAL skills before building tool sets (they register the `skill`
    // tool). Skills are budget-gated context, not privileged — suggested actions
    // still go through the permission model. Disabled (with the project-context
    // loader) via KITSU_NO_PROJECT_CONTEXT.
    let no_project = std::env::var_os("KITSU_NO_PROJECT_CONTEXT").is_some();
    let skillset = Arc::new(
        std::env::current_dir()
            .map(|cwd| skills::SkillSet::discover(&cwd, no_project))
            .unwrap_or_default(),
    );

    // The subagent tool set: base tools ONLY (no `spawn_agent` → depth-1).
    let sub_tools = {
        let mut t = Registry::new();
        register_base_tools(&mut t, &perms, &skillset, can_read, can_write);
        Arc::new(t)
    };

    // The live (model, window) subagents should use, kept in sync with `/model`
    // (see the loop below) so a subagent never runs the session's initial model
    // against a stale window.
    let live_mw: subagent::LiveModelWindow = Arc::new(std::sync::Mutex::new((
        setup.model.clone(),
        setup.context_window,
    )));
    // Registry of BACKGROUND subagents (Stage C), shared with the spawn_agent_bg /
    // agent_result tools and the `/agents` command.
    let agent_registry = subagent::AgentRegistry::new();

    // The main tool set: base tools + the spawn/agent tools, which delegate to
    // subagents running the base `sub_tools` in their own isolated context.
    let mut tools = Registry::new();
    register_base_tools(&mut tools, &perms, &skillset, can_read, can_write);
    if can_read || can_write {
        let mut sub_cfg = build_agent_config(setup.model.clone(), cli, setup.context_window, None);
        sub_cfg.stream = false; // the subagent's output is a returned result, not streamed
        let sub_system = if skillset.is_empty() {
            String::new()
        } else {
            format!(
                "You have these skills (call the `skill` tool to load one when a task matches):\n{}",
                skillset.index("")
            )
        };
        // Stage A: synchronous single delegation.
        let (d, h) = subagent::spawn_agent_tool(
            setup.provider.clone(),
            sub_tools.clone(),
            sub_cfg.clone(),
            live_mw.clone(),
            sub_system.clone(),
        );
        tools.register_with_descriptor(d, h);
        // Stage B: fan several isolated subagents out in parallel.
        let (d, h) = subagent::spawn_agents_tool(
            setup.provider.clone(),
            sub_tools.clone(),
            sub_cfg.clone(),
            live_mw.clone(),
            sub_system.clone(),
        );
        tools.register_with_descriptor(d, h);
        // Stage C: detached background subagent (runs quiet so it doesn't corrupt
        // the prompt) + a tool to fetch its result by id.
        let mut bg_cfg = sub_cfg;
        bg_cfg.quiet = true;
        let (d, h) = subagent::spawn_agent_bg_tool(
            setup.provider.clone(),
            sub_tools.clone(),
            bg_cfg,
            live_mw.clone(),
            sub_system,
            agent_registry.clone(),
        );
        tools.register_with_descriptor(d, h);
        let (d, h) = subagent::agent_result_tool(agent_registry.clone());
        tools.register_with_descriptor(d, h);
    }
    let tool_names: Vec<String> = tools.descriptors().iter().map(|d| d.name.clone()).collect();

    // A per-session scratch dir for offloading large tool results (Stage 2). Only
    // when tools can run; granted read access so the model can re-read offloaded
    // results. If the grant fails the dir would yield dead pointers, so skip
    // offloading entirely. A guard removes the dir (and its files) on exit.
    let offload_dir = if tool_names.is_empty() {
        None
    } else {
        let dir = std::env::temp_dir().join(format!("kitsu-offload-{}", std::process::id()));
        match std::fs::create_dir_all(&dir).map(|_| perms.grant_read_root(&dir.to_string_lossy())) {
            Ok(Ok(_)) => Some(dir),
            _ => {
                let _ = std::fs::remove_dir_all(&dir);
                None
            }
        }
    };
    let _offload_guard = offload_dir.clone().map(OffloadDirGuard);

    let mut model = setup.model.clone();
    // Shared with `ctx` (for `/context`) and updated in place by `/model`, so
    // switching models doesn't leave every subsequent turn budgeting against
    // the OLD model's window — a stale, larger window would under-trim and the
    // gateway would reject the oversized request instead of kitsu proactively
    // trimming to fit the new model's real limit.
    let context_window = Rc::new(Cell::new(setup.context_window));
    let base_offload = offload_dir.clone();
    let base_cfg = {
        let context_window = context_window.clone();
        move |model: String, cli: &Cli| {
            build_agent_config(model, cli, context_window.get(), base_offload.clone())
        }
    };
    // Persists the context-meter anchor and compaction circuit breaker across
    // every turn of this interactive/one-shot chat session.
    let mut session_state = kitsu_core::AgentSessionState::default();

    // Assemble the system prompt while recording provenance (design §3): each
    // static context source with its token cost, policy, and reason, so `/context`
    // is honest about what loaded and why. Each source is pushed as a chunk so its
    // estimate (headers included) sums to the whole system-message estimate.
    let mut provenance: Vec<CtxEntry> = Vec::new();
    let base = chat_system_prompt(&tool_names);
    provenance.push(CtxEntry::new(
        "base prompt",
        estimate_str(&base),
        "always",
        "core assistant instructions",
    ));
    let mut system = base;

    // Project context (llms.txt / AGENTS.md), budget-gated (Stage 4).
    if std::env::var_os("KITSU_NO_PROJECT_CONTEXT").is_none() {
        if let Some((ctx, _)) = std::env::current_dir()
            .ok()
            .and_then(|cwd| llms::load(&cwd, None, setup.context_window))
        {
            let chunk = format!("\n\n---\nProject context (from llms.txt):\n{ctx}");
            let tokens = estimate_str(&chunk);
            system.push_str(&chunk);
            eprintln!("~ loaded project context ({tokens} tokens)");
            provenance.push(CtxEntry::new(
                "project (llms.txt)",
                tokens,
                "loaded",
                "curated project context",
            ));
        }
    }
    // Skill index (always-on): names + descriptions; bodies load on demand.
    if !skillset.is_empty() {
        let mut chunk = String::from(
            "\n\n---\nAvailable skills — call the `skill` tool with the name to load full \
             instructions when a task matches:\n",
        );
        chunk.push_str(&skillset.index(""));
        let always = skillset.always_bodies();
        if !always.is_empty() {
            chunk.push('\n');
            chunk.push_str(&always);
        }
        system.push_str(&chunk);
        eprintln!("~ {} skill(s) available", skillset.names().len());
        provenance.push(CtxEntry::new(
            format!("skill index ({})", skillset.names().len()),
            estimate_str(&chunk),
            "always",
            "names + descriptions (bodies load on demand)",
        ));
    }
    // --skill preloads named skills up front (in addition to on-demand loading).
    if let Some(pre) = preload_skills(cli, &skillset) {
        // Grant read to each preloaded skill's dir so its bundled files are reachable.
        for name in &cli.skill {
            if let Some(s) = skillset.get(name) {
                let _ = perms.grant_read_root(&s.dir.to_string_lossy());
            }
        }
        let chunk = format!("\n\n---\nPreloaded skills (follow these):\n{pre}");
        system.push_str(&chunk);
        provenance.push(CtxEntry::new(
            format!("skills (--skill ×{})", cli.skill.len()),
            estimate_str(&chunk),
            "preloaded",
            "--skill flag",
        ));
    }
    // Tool schemas ride the request's `tools` field, not the system message.
    if !tool_names.is_empty() {
        let schema_tokens =
            estimate_str(&serde_json::to_string(tools.descriptors()).unwrap_or_default());
        provenance.push(CtxEntry::new(
            format!("tool schemas ({})", tool_names.len()),
            schema_tokens,
            "admitted",
            tool_names.join(", "),
        ));
    }
    let ctx = ctx_info(context_window.clone(), cli, provenance);

    let mut messages = vec![ChatMessage::system(system)];

    // Scripted mode: run turns from a file non-interactively, then exit. Works
    // regardless of TTY, so it's the deterministic multi-turn driver.
    if let Some(path) = script {
        return run_scripted(
            &setup,
            &tools,
            &mut messages,
            &mut model,
            cli,
            path,
            &perms,
            &offload_dir,
            &skillset,
            &ctx,
            &context_window,
            &live_mw,
            &agent_registry,
        )
        .await;
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
    let mut rl: rustyline::Editor<PasteCollapse, rustyline::history::DefaultHistory> =
        rustyline::Editor::with_config(rl_config)
            .map_err(|e| CliError::user(format!("failed to start input editor: {e}")))?;
    // Collapse pasted multi-line blocks to a chip in the editor display (#16).
    rl.set_helper(Some(PasteCollapse));
    let history_path = Config::resolve_path(cli.config.as_deref())
        .parent()
        .map(|p| p.join("history"))
        .unwrap_or_else(|| std::path::PathBuf::from(".kitsu_history"));
    let _ = rl.load_history(&history_path);

    let gateway_name = cli
        .gateway
        .clone()
        .unwrap_or_else(|| "(default)".to_string());
    let dir = std::env::current_dir()
        .ok()
        .map(|p| shorten_home(&p))
        .unwrap_or_default();
    kitsu_render::chat_banner(&gateway_name, &model, &dir, &tool_names);

    // If a first prompt was given, queue it as the first turn.
    let mut queued = first_prompt.map(|s| s.to_string());
    let prompt = kitsu_render::prompt_string();
    // Report background-agent activity only when the running count changes, so
    // the user notices work starting/finishing without a per-prompt spam line.
    let mut last_running = 0usize;

    loop {
        let running = agent_registry.running();
        if running != last_running {
            if running > 0 {
                eprintln!("~ {running} background agent(s) running — /agents to check");
            } else {
                eprintln!("~ background agents finished — /agents for results");
            }
            last_running = running;
        }
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
        // A multi-line paste is already shown collapsed as a chip by the
        // PasteCollapse highlighter, so no extra acknowledgement is needed here.
        let _ = rl.add_history_entry(line);

        if let Some(rest) = line.strip_prefix('/') {
            let result = handle_slash(
                rest,
                &mut messages,
                &mut model,
                &setup.provider,
                &perms,
                &ctx,
                &skillset,
                &context_window,
                setup.context_window_pinned,
                &mut session_state,
                &agent_registry,
            )
            .await;
            // Keep subagents in sync with any `/model` change (which may have
            // swapped the model and re-discovered the window).
            *live_mw.lock().expect("live model/window") = (model.clone(), context_window.get());
            match result {
                SlashResult::Continue => continue,
                SlashResult::Quit => break,
            }
        }

        messages.push(ChatMessage::user(line));
        let cfg = base_cfg(model.clone(), cli);
        match run_turn_cancellable(
            &setup.provider,
            &tools,
            &mut messages,
            &cfg,
            &mut session_state,
        )
        .await
        {
            TurnOutcome::Completed(Ok(text)) => {
                kitsu_render::finish_line();
                // The model can return an empty completion for vague inputs;
                // say so instead of looking hung.
                if text.trim().is_empty() {
                    eprintln!("(the model returned an empty response — try rephrasing)");
                }
            }
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
#[allow(clippy::too_many_arguments)]
async fn handle_slash(
    cmd: &str,
    messages: &mut Vec<ChatMessage>,
    model: &mut String,
    provider: &OpenAICompatProvider,
    perms: &Permissions,
    ctx: &CtxInfo,
    skillset: &skills::SkillSet,
    context_window: &Rc<Cell<Option<u64>>>,
    context_window_pinned: bool,
    state: &mut kitsu_core::AgentSessionState,
    registry: &Arc<subagent::AgentRegistry>,
) -> SlashResult {
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match name {
        "quit" | "exit" => return SlashResult::Quit,
        "help" => {
            eprintln!(
                "commands:\n  /model <name>  switch model (keeps history)\n  /models [filter]  \
                 list models the gateway offers (optionally filtered)\n  /skills [filter]  list \
                 available skills\n  /skill <name>  load a skill's instructions into context\n  \
                 /allow <path>  grant read access to a path (no more prompts there)\n  \
                 /allow-write <path>  grant write access to a path\n  /save <file>   write the \
                 last response to <file> (code blocks if present)\n  /context       show \
                 context-window utilization\n  /agents [id]   list background agents (or one \
                 result)\n  /clear         clear conversation (keeps system \
                 prompt)\n  /help          this help\n  /quit          exit\ncurrent model: {model}"
            );
        }
        "context" => render_context_command(model, messages, ctx),
        "agents" => {
            if arg.is_empty() {
                eprintln!("{}", registry.list());
            } else if let Ok(id) = arg.parse::<u64>() {
                match registry.result(id) {
                    Some(text) => eprintln!("#{id}:\n{text}"),
                    None => eprintln!("no background agent #{id}"),
                }
            } else {
                eprintln!("usage: /agents [id]");
            }
        }
        "skills" => {
            if skillset.is_empty() {
                eprintln!(
                    "no skills found — add SKILL.md dirs under .kitsu/skills/ or ~/.kitsu/skills/"
                );
            } else {
                eprintln!("skills:\n{}", skillset.index(arg));
                eprintln!("load one with /skill <name>, or the model can call the `skill` tool");
            }
        }
        "skill" => {
            if arg.is_empty() {
                eprintln!("usage: /skill <name> (loads the skill's instructions into context)");
            } else {
                match skillset.get(arg) {
                    Some(s) => {
                        // Grants read to the skill's dir + lists bundled files/links.
                        let loaded = skills::load_skill(s, perms);
                        if !s.links.is_empty() {
                            eprintln!("~ {} reference link(s) (not fetched)", s.links.len());
                        }
                        messages.push(ChatMessage::user(format!("[Loaded skill]\n\n{loaded}")));
                        eprintln!("~ loaded skill '{}'", s.name);
                    }
                    None => eprintln!("no skill named '{arg}' — try /skills"),
                }
            }
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
            // The context-meter anchor describes token cost for the messages we
            // just wiped, and the summarize circuit breaker belongs to that old
            // conversation too — starting fresh means both must reset, not carry
            // over and silently mis-budget (or permanently skip Summarize for) an
            // unrelated new conversation.
            state.meter.invalidate();
            state.compact_failures = 0;
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
                // The meter's anchor is a real `prompt_tokens` count measured
                // under the OLD model's tokenizer; a different model can tokenize
                // the same text differently, so that anchor must not be trusted
                // for the new one (same reasoning as /clear, just below in this
                // match). The summarize circuit breaker is also model-specific —
                // don't let a different model inherit "summarization is broken".
                state.meter.invalidate();
                state.compact_failures = 0;
                // An explicit --context-window flag or gateway-profile
                // context_window is a user/config declaration that overrides
                // discovery entirely (see the flag's doc comment) — switching
                // models must not silently replace it with fresh auto-discovery.
                // Only re-resolve when the window was never pinned in the first
                // place: carrying over the OLD model's discovered window (or
                // lack thereof) would otherwise budget against the wrong limit.
                let note = if context_window_pinned {
                    " (context window pinned by --context-window / config; unchanged)".to_string()
                } else {
                    let discovered = provider.model_context_length(model).await;
                    context_window.set(discovered);
                    match discovered {
                        Some(w) => format!(" (context window: {w} tokens)"),
                        None => " (context window unknown — falling back to the byte budget)"
                            .to_string(),
                    }
                };
                eprintln!("~ switched to {model}{note}");
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

/// Replace the home-directory prefix with `~` for compact display in the banner.
fn shorten_home(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().to_string();
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        let home = home.trim_end_matches('/');
        if !home.is_empty() {
            // Require a path-segment boundary after the home prefix (exact match,
            // or followed by '/') so a sibling directory that merely SHARES the
            // home dir as a string prefix (e.g. HOME=/home/mmpg and cwd
            // /home/mmpg2/project) isn't corrupted into a bogus `~2/project`.
            if let Some(rest) = s.strip_prefix(home) {
                if rest.is_empty() {
                    return "~".to_string();
                }
                if let Some(rest) = rest.strip_prefix('/') {
                    return format!("~/{rest}");
                }
            }
        }
    }
    s
}

/// Build the appropriate AuthProvider from the profile's auth block.
fn build_auth(gateway: &Gateway) -> Result<Arc<dyn AuthProvider>, ConfigError> {
    gateway.validate_auth()?;
    let auth = &gateway.auth;
    let provider: Arc<dyn AuthProvider> = match auth.method.as_str() {
        "none" => Arc::new(NoAuth),
        "static" => {
            let token = auth.token.clone().ok_or(ConfigError::StaticTokenRequired)?;
            Arc::new(StaticAuth {
                token,
                header: auth.header.clone(),
                scheme: auth.scheme.clone(),
            })
        }
        "bearer-env" => Arc::new(EnvBearerAuth {
            env: auth.env.clone(),
            expires_env: auth.expires_env.clone(),
            header: auth.header.clone(),
            scheme: auth.scheme.clone(),
            refresh_hint: auth.refresh_hint.clone(),
        }),
        other => {
            return Err(ConfigError::UnsupportedAuthMethod {
                method: other.to_string(),
            })
        }
    };
    Ok(provider)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn paste_collapse_chips_multiline_only() {
        use rustyline::highlight::Highlighter;
        let h = PasteCollapse;
        // Single line: unchanged, and not re-highlighted.
        assert_eq!(h.highlight("just one line", 0), "just one line");
        assert!(!h.highlight_char("just one line", 0, false));
        // Multi-line paste: collapsed to a chip; middle lines hidden.
        let out = h.highlight("first line here\nsecond\nthird", 0);
        assert!(out.contains("first line here"), "keeps a preview of line 1");
        assert!(out.contains("[pasted 3 lines]"), "shows the count");
        assert!(!out.contains("second"), "hides the pasted body");
        assert!(
            h.highlight_char("a\nb", 0, false),
            "re-highlights when multi-line"
        );
    }

    /// Regression: a sibling directory that merely SHARES the home dir as a
    /// string prefix (no path-segment boundary) must NOT be corrupted by
    /// `strip_prefix` — it must be left as the full, unshortened path.
    #[test]
    #[serial]
    fn shorten_home_requires_a_path_segment_boundary() {
        let _g_home = EnvVarGuard::set("HOME", "/home/mmpg");
        assert_eq!(
            shorten_home(std::path::Path::new("/home/mmpg2/project")),
            "/home/mmpg2/project",
            "a sibling dir sharing HOME as a bare string prefix must not become ~2/project"
        );
    }

    #[test]
    #[serial]
    fn shorten_home_shortens_a_real_subdirectory() {
        let _g_home = EnvVarGuard::set("HOME", "/home/mmpg");
        assert_eq!(
            shorten_home(std::path::Path::new("/home/mmpg/project")),
            "~/project"
        );
    }

    #[test]
    #[serial]
    fn shorten_home_shortens_the_home_dir_itself() {
        let _g_home = EnvVarGuard::set("HOME", "/home/mmpg");
        assert_eq!(shorten_home(std::path::Path::new("/home/mmpg")), "~");
    }

    /// Scoped env-var guard; restores the prior value (or unsets) on drop.
    struct EnvVarGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
