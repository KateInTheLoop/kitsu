//! kitsu — CLI entry. MVP target: `kitsu ask --gateway example "hello"` runs the
//! agent loop against the gateway (Mistral). See docs/architecture.md §6.

mod cli;
mod config;
mod llms;
mod session;
mod skills;
mod subagent;
mod workspace;

use std::cell::{Cell, RefCell};
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
            resume,
            no_session,
            allow_turn_errors,
        } => runtime.block_on(run_chat(
            &cli,
            ChatInvocation {
                first_prompt: prompt.as_deref(),
                allow_paths,
                allow_write,
                script: script.as_deref(),
                resume: resume.as_deref(),
                no_session: *no_session,
                allow_turn_errors: *allow_turn_errors,
            },
        )),
    }
}

struct ChatInvocation<'a> {
    first_prompt: Option<&'a str>,
    allow_paths: &'a [String],
    allow_write: &'a [String],
    script: Option<&'a str>,
    resume: Option<&'a str>,
    no_session: bool,
    allow_turn_errors: bool,
}

/// Provider + model + auth resolved from config and CLI flags. Shared by the
/// `ask` and `chat` commands so they construct the gateway connection identically.
struct ChatSetup {
    /// `Arc` so the main loop and spawned subagents can share one connection.
    provider: Arc<OpenAICompatProvider>,
    gateway_name: String,
    config_path: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    endpoint: String,
    auth_status: String,
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

fn project_context_disabled(cli: &Cli) -> bool {
    cli.no_project_context || std::env::var_os("KITSU_NO_PROJECT_CONTEXT").is_some()
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

#[derive(Debug, Clone, Copy)]
struct SlashCommandSpec {
    name: &'static str,
    args: &'static str,
    description: &'static str,
}

impl SlashCommandSpec {
    fn syntax(self) -> String {
        if self.args.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.args)
        }
    }
}

/// One source of truth for interactive completion and `/help`. Keep aliases in
/// the registry too so discoverability never depends on reading documentation.
const SLASH_COMMANDS: &[SlashCommandSpec] = &[
    SlashCommandSpec {
        name: "agents",
        args: "[id]",
        description: "list background agents or show one result",
    },
    SlashCommandSpec {
        name: "allow",
        args: "<path>",
        description: "grant read access for this session",
    },
    SlashCommandSpec {
        name: "allow-write",
        args: "<path>",
        description: "grant write access for this session",
    },
    SlashCommandSpec {
        name: "clear",
        args: "",
        description: "clear conversation history",
    },
    SlashCommandSpec {
        name: "context",
        args: "",
        description: "show detailed context utilization",
    },
    SlashCommandSpec {
        name: "diff",
        args: "[path]",
        description: "show the verified Git and filesystem diff",
    },
    SlashCommandSpec {
        name: "exit",
        args: "",
        description: "exit chat (alias for /quit)",
    },
    SlashCommandSpec {
        name: "help",
        args: "",
        description: "show command help",
    },
    SlashCommandSpec {
        name: "model",
        args: "<name>",
        description: "switch model while keeping history",
    },
    SlashCommandSpec {
        name: "models",
        args: "[filter]",
        description: "list gateway models",
    },
    SlashCommandSpec {
        name: "new",
        args: "",
        description: "start and autosave a fresh conversation",
    },
    SlashCommandSpec {
        name: "permissions",
        args: "",
        description: "show filesystem access policy",
    },
    SlashCommandSpec {
        name: "quit",
        args: "",
        description: "exit chat",
    },
    SlashCommandSpec {
        name: "resume",
        args: "<id|last>",
        description: "resume a saved conversation",
    },
    SlashCommandSpec {
        name: "retry",
        args: "",
        description: "retry the last failed or cancelled prompt",
    },
    SlashCommandSpec {
        name: "review",
        args: "[path]",
        description: "ask the model to review the verified diff",
    },
    SlashCommandSpec {
        name: "save",
        args: "<file>",
        description: "save the latest response",
    },
    SlashCommandSpec {
        name: "sessions",
        args: "",
        description: "list locally saved conversations",
    },
    SlashCommandSpec {
        name: "skill",
        args: "<name>",
        description: "load a skill into context",
    },
    SlashCommandSpec {
        name: "skills",
        args: "[filter]",
        description: "list available skills",
    },
    SlashCommandSpec {
        name: "status",
        args: "",
        description: "show model, context, tools, and session state",
    },
    SlashCommandSpec {
        name: "tools",
        args: "",
        description: "show enabled tools and effective access",
    },
];

fn matching_slash_commands(prefix: &str) -> Vec<&'static SlashCommandSpec> {
    SLASH_COMMANDS
        .iter()
        .filter(|spec| spec.name.starts_with(prefix))
        .collect()
}

fn slash_help(model: &str) -> String {
    let width = SLASH_COMMANDS
        .iter()
        .map(|spec| spec.syntax().len())
        .max()
        .unwrap_or(0);
    let mut out = String::from("commands:\n");
    for spec in SLASH_COMMANDS {
        let syntax = spec.syntax();
        out.push_str(&format!(
            "  {syntax:<width$}  {}\n",
            spec.description,
            width = width
        ));
    }
    out.push_str(&format!("current model: {model}"));
    out
}

/// Rustyline helper for slash-command completion. Multi-line paste is kept
/// verbatim: shortening it from a highlighter corrupts Rustyline's cursor-width
/// accounting (especially for Unicode), so visual collapsing belongs in a
/// future editor renderer that can preserve buffer geometry.
struct ChatHelper;

impl rustyline::completion::Completer for ChatHelper {
    type Candidate = rustyline::completion::Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let Some(typed) = line.get(..pos).and_then(|text| text.strip_prefix('/')) else {
            return Ok((0, Vec::new()));
        };
        if typed.chars().any(char::is_whitespace) {
            return Ok((0, Vec::new()));
        }
        let candidates = matching_slash_commands(typed)
            .into_iter()
            .map(|spec| rustyline::completion::Pair {
                display: format!("{:<24} {}", spec.syntax(), spec.description),
                replacement: format!(
                    "/{}{}",
                    spec.name,
                    if spec.args.is_empty() { "" } else { " " }
                ),
            })
            .collect();
        Ok((0, candidates))
    }
}
impl rustyline::hint::Hinter for ChatHelper {
    type Hint = String;
}
impl rustyline::validate::Validator for ChatHelper {}
impl rustyline::Helper for ChatHelper {}

impl rustyline::highlight::Highlighter for ChatHelper {}

/// A provenance entry: a static context source, why it's loaded, its estimated
/// token cost, and the governing policy — honest accounting (design §3).
#[derive(Clone)]
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

/// Stable UI metadata plus context-window provenance for the running chat.
/// `window` is shared with the `AgentConfig` builder via a `Cell` so `/model`
/// updates both request budgeting and `/status`/`/context` immediately.
struct SessionInfo {
    gateway: String,
    directory: std::path::PathBuf,
    config_path: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    endpoint: String,
    auth_status: String,
    project_context: RefCell<Option<std::path::PathBuf>>,
    offload_dir: Rc<RefCell<Option<std::path::PathBuf>>>,
    session_id: Rc<RefCell<Option<String>>>,
    tool_names: Vec<String>,
    window: Rc<Cell<Option<u64>>>,
    max_tokens: Option<u32>,
    provenance: RefCell<Vec<CtxEntry>>,
}

fn persist_session(
    store: &Option<session::SessionStore>,
    session: &SessionInfo,
    model: &str,
    messages: &[ChatMessage],
) -> bool {
    let Some(store) = store else {
        return true;
    };
    let project_context = session.project_context.borrow();
    if let Err(error) = store.save(
        &session.directory,
        &session.gateway,
        model,
        project_context.as_deref(),
        messages,
    ) {
        eprintln!("warning: session was not saved: {error}");
        false
    } else {
        true
    }
}

fn activate_offload_dir(
    perms: &Permissions,
    state: &Rc<RefCell<Option<std::path::PathBuf>>>,
    desired: Option<std::path::PathBuf>,
) -> Result<(), String> {
    // Drop the previous session's destination before doing any fallible work.
    // Otherwise a failed directory creation could leave direct orchestrator
    // writes pointed at the old session even after its permission was revoked.
    *state.borrow_mut() = None;
    if let Some(dir) = desired.as_ref() {
        std::fs::create_dir_all(dir).map_err(|error| {
            format!(
                "cannot create artifact directory {}: {error}",
                dir.display()
            )
        })?;
        perms
            .grant_read_root(&dir.to_string_lossy())
            .map_err(|error| error.to_string())?;
    }
    *state.borrow_mut() = desired;
    Ok(())
}

fn grant_preloaded_skill_roots(cli: &Cli, skillset: &skills::SkillSet, perms: &Permissions) {
    for name in &cli.skill {
        if let Some(skill) = skillset.get(name) {
            let _ = perms.grant_read_root(&skill.dir.to_string_lossy());
        }
    }
}

fn resumed_provenance(
    saved: &session::SavedSession,
    current_sources: &[CtxEntry],
) -> Vec<CtxEntry> {
    let mut provenance = vec![CtxEntry::new(
        "resumed system context",
        saved
            .messages
            .first()
            .map(|message| estimate_str(&message.content))
            .unwrap_or(0),
        "resumed",
        format!("saved session {}", saved.id),
    )];
    provenance.extend(
        current_sources
            .iter()
            .filter(|entry| entry.label.starts_with("tool schemas"))
            .cloned(),
    );
    provenance
}

fn restored_messages(
    saved: &session::SavedSession,
    fresh_system: &ChatMessage,
    omit_saved_project_context: bool,
) -> Vec<ChatMessage> {
    let mut messages = saved.messages.clone();
    if omit_saved_project_context && !messages.is_empty() {
        // A saved system message may contain llms.txt/AGENTS.md text. Replacing
        // it (rather than merely hiding its provenance) makes the opt-out a
        // real request boundary on resumed sessions too.
        messages[0] = fresh_system.clone();
    }
    messages
}

fn same_directory(saved: &str, current: &std::path::Path) -> bool {
    let saved = std::path::Path::new(saved);
    match (std::fs::canonicalize(saved), std::fs::canonicalize(current)) {
        (Ok(saved), Ok(current)) => saved == current,
        _ => saved == current,
    }
}

async fn authorized_workspace_diff(
    session: &SessionInfo,
    perms: &Permissions,
    arg: &str,
) -> Result<String, String> {
    let root = workspace::repository_root(&session.directory)?;
    let root = perms
        .check_read(&root)
        .await
        .map_err(|error| format!("read permission required for {}: {error}", root.display()))?;
    let pathspec = workspace::pathspec_from_directory(&root, &session.directory, arg)?;
    if [&session.config_path, &session.state_dir]
        .iter()
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .any(|path| path.starts_with(&root))
    {
        eprintln!("~ active Kitsu config/private state omitted from workspace evidence");
    }
    workspace::diff_excluding(
        &root,
        pathspec.as_deref(),
        &[session.config_path.clone(), session.state_dir.clone()],
    )
}

fn format_sessions(dir: &std::path::Path, current: Option<&str>) -> Result<String, String> {
    let sessions = session::SessionStore::list(dir)?;
    if sessions.is_empty() {
        return Ok("no saved sessions".to_string());
    }
    let mut out = String::from("saved sessions:\n");
    for saved in sessions {
        let marker = if current == Some(saved.id.as_str()) {
            "*"
        } else {
            " "
        };
        out.push_str(&format!(
            " {marker} {}  {}  {} turn(s)  {}/{}\n     {}\n     {}\n",
            saved.id,
            saved.updated_at,
            saved.turns,
            saved.gateway,
            saved.model,
            shorten_home(std::path::Path::new(&saved.directory)),
            saved.preview
        ));
    }
    out.push_str("resume with /resume <id> or `kitsu chat --resume last`");
    Ok(out)
}

fn permission_mode_label(mode: Mode) -> (&'static str, &'static str) {
    match mode {
        Mode::Ask => ("ask", "prompt for unlisted paths"),
        Mode::AutoAllow => ("auto-allow", "allow unlisted paths"),
        Mode::Deny => ("deny", "reject unlisted paths"),
    }
}

fn context_totals(
    messages: &[ChatMessage],
    session: &SessionInfo,
    state: &kitsu_core::AgentSessionState,
) -> (u64, u64, u64) {
    let b = ContextMeter::new().breakdown(messages);
    let window = session.window.get().unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let effective = ContextBudget::new(window, session.max_tokens).effective;
    let static_tokens: u64 = session
        .provenance
        .borrow()
        .iter()
        .map(|entry| entry.tokens)
        .sum();
    let estimated = static_tokens + b.user + b.assistant + b.tool_results;
    let used = if state.meter.anchored(messages) {
        state.meter.estimate(messages)
    } else {
        estimated
    };
    (window, effective, used)
}

fn format_session_status(
    model: &str,
    messages: &[ChatMessage],
    session: &SessionInfo,
    perms: &Permissions,
    registry: &subagent::AgentRegistry,
    state: &kitsu_core::AgentSessionState,
) -> String {
    let (window, effective, used) = context_totals(messages, session, state);
    let pct = if effective == 0 {
        0
    } else {
        used.saturating_mul(100).saturating_div(effective).min(999)
    };
    let user_turns = messages
        .iter()
        .filter(|message| matches!(message.role, kitsu_providers::Role::User))
        .count();
    let assistant_turns = messages
        .iter()
        .filter(|message| matches!(message.role, kitsu_providers::Role::Assistant))
        .count();
    let tool_results = messages
        .iter()
        .filter(|message| matches!(message.role, kitsu_providers::Role::Tool))
        .count();
    let (mode, _) = permission_mode_label(perms.mode());
    let window_note = if session.window.get().is_some() {
        window.to_string()
    } else {
        format!("{window} default")
    };
    let session_id = session
        .session_id
        .borrow()
        .clone()
        .unwrap_or_else(|| "disabled".to_string());
    let project_context = session
        .project_context
        .borrow()
        .as_deref()
        .map(shorten_home)
        .unwrap_or_else(|| "disabled / not found".to_string());
    let git = workspace::status(&session.directory)
        .map(|status| {
            format!(
                "{} · {} changed ({} untracked)",
                status.branch, status.changed, status.untracked
            )
        })
        .unwrap_or_else(|| "not a Git worktree".to_string());

    format!(
        "session:\n  id           {session_id}\n  gateway      {}\n  model        {model}\n  directory    {}\n  git          {git}\n  config       {}\n  private state {}\n  endpoint     {}\n  auth         {}\n  project ctx  {project_context}\n  context      {used} / {effective} used ({pct}%, {} left; window {window_note})\n  conversation {user_turns} user · {assistant_turns} assistant · {tool_results} tool\n  tools        {} enabled\n  permissions  {mode} · {} read root(s) · {} write root(s)\n  agents       {} running",
        session.gateway,
        shorten_home(&session.directory),
        shorten_home(&session.config_path),
        shorten_home(&session.state_dir),
        session.endpoint,
        session.auth_status,
        effective.saturating_sub(used),
        session.tool_names.len(),
        perms.read_roots().len(),
        perms.write_roots().len(),
        registry.running(),
    )
}

fn format_permissions(perms: &Permissions, session: &SessionInfo) -> String {
    let (mode, behavior) = permission_mode_label(perms.mode());
    let read_roots = perms.read_roots();
    let write_roots = perms.write_roots();
    let mut out = format!("permissions:\n  mode         {mode} ({behavior})\n  read roots");
    if read_roots.is_empty() {
        out.push_str("   none\n");
    } else {
        out.push('\n');
        for root in read_roots {
            let offload_dir = session.offload_dir.borrow();
            let internal = offload_dir.as_deref().is_some_and(|path| root == path);
            out.push_str(&format!(
                "    - {}{}\n",
                shorten_home(&root),
                if internal {
                    " (internal session artifacts)"
                } else {
                    ""
                }
            ));
        }
    }
    out.push_str("  write roots");
    if write_roots.is_empty() {
        out.push_str("  none");
    } else {
        out.push('\n');
        for (index, root) in write_roots.iter().enumerate() {
            out.push_str(&format!("    - {}", shorten_home(root)));
            if index + 1 < write_roots.len() {
                out.push('\n');
            }
        }
    }
    out
}

/// Render `/context`: the static provenance rows (what loaded and why) plus the
/// live conversation, with token cost and policy per source. The per-turn
/// `kitsu-context:` line carries the usage-anchored number; this is an estimate.
fn render_context_command(
    model: &str,
    messages: &[ChatMessage],
    session: &SessionInfo,
    state: &kitsu_core::AgentSessionState,
) {
    let b = ContextMeter::new().breakdown(messages);
    let ctx_window = session.window.get();
    let window = ctx_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let budget = ContextBudget::new(window, session.max_tokens);
    let note = if ctx_window.is_some() {
        ""
    } else {
        "default — set context_window or --context-window"
    };
    // Static provenance (system prompt sources + tool schemas) then the live
    // conversation. breakdown.system is already covered by the provenance rows.
    let provenance = session.provenance.borrow();
    let rows: Vec<(&str, u64, &str, &str)> = provenance
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
    let prov_total: u64 = provenance.iter().map(|e| e.tokens).sum();
    let estimated = prov_total + b.user + b.assistant + b.tool_results;
    let used = if state.meter.anchored(messages) {
        state.meter.estimate(messages)
    } else {
        estimated
    };
    kitsu_render::render_context(model, window, note, budget.effective, used, &rows);
}

/// How long a discovered context window is trusted before re-discovering. A
/// model's window essentially never changes, so this just spares every later
/// invocation of the same gateway+model a `/v1/models` round-trip purely to
/// re-learn a number we already know.
const CONTEXT_WINDOW_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// On-disk cache for discovered context windows, keyed by an opaque local hash
/// of the endpoint and model so URL credentials never appear in the file.
/// Without this, `build_setup` paid a blocking `/v1/models` round-trip on
/// *every single* `ask`/`chat` invocation whenever `context_window` wasn't
/// pre-configured — doubling perceived startup latency on a slow gateway, even
/// for one-off `ask` calls where the result is immediately thrown away.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ContextWindowCache {
    #[serde(default)]
    entries: std::collections::BTreeMap<String, (u64, u64)>,
}

fn runtime_state_base() -> std::path::PathBuf {
    let base = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".kitsu"))
        .unwrap_or_else(|| {
            #[cfg(unix)]
            let owner = unsafe { libc::geteuid() }.to_string();
            #[cfg(not(unix))]
            let owner = std::process::id().to_string();
            std::env::temp_dir().join(format!("kitsu-state-{owner}"))
        });
    base
}

fn stable_profile_key(config_path: &std::path::Path) -> String {
    // FNV-1a/128 is deliberately spelled out and versioned: unlike
    // `DefaultHasher`, its output is stable across Rust releases. This is an
    // on-disk identity, not a security primitive; 128 bits keeps accidental
    // profile collisions impractical without adding a crypto dependency.
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    #[cfg(unix)]
    let encoded = {
        use std::os::unix::ffi::OsStrExt;
        config_path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let encoded = {
        use std::os::windows::ffi::OsStrExt;
        config_path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    #[cfg(not(any(unix, windows)))]
    let encoded = config_path.to_string_lossy().as_bytes().to_vec();

    let digest = encoded.into_iter().fold(OFFSET, |hash, byte| {
        (hash ^ u128::from(byte)).wrapping_mul(PRIME)
    });
    format!("v1-fnv128-{digest:032x}")
}

/// Location used by builds before profile keys became a stable on-disk
/// contract. Kept only so this release can migrate existing development data.
fn legacy_profile_dir(base: &std::path::Path, config_path: &std::path::Path) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config_path.hash(&mut hasher);
    base.join("profiles")
        .join(format!("{:016x}", hasher.finish()))
}

fn runtime_state_dir(config_path: &std::path::Path) -> std::path::PathBuf {
    let base = runtime_state_base();
    let config = std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let default_config = base.join("config.toml");
    let default_config = std::fs::canonicalize(&default_config).unwrap_or(default_config);
    if config == default_config {
        return base;
    }

    let stable = base.join("profiles").join(stable_profile_key(&config));
    if std::fs::symlink_metadata(&stable).is_ok() {
        return stable;
    }

    // Best-effort, same-parent migration. If HOME is read-only, retain the
    // usable legacy location; stateless commands must not fail merely because
    // migration cannot be performed.
    let legacy = legacy_profile_dir(&base, &config);
    let legacy_is_safe_dir = std::fs::symlink_metadata(&legacy)
        .map(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false);
    if legacy_is_safe_dir {
        if std::fs::rename(&legacy, &stable).is_ok() {
            return stable;
        }
        // Another Kitsu process may have won the migration race between our
        // metadata check and rename. Follow it instead of recreating a fresh,
        // empty legacy profile beside the migrated state.
        if std::fs::symlink_metadata(&stable).is_ok() {
            return stable;
        }
        if std::fs::symlink_metadata(&legacy)
            .map(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return legacy;
        }
    }
    stable
}

fn prepare_runtime_state_dir(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create runtime state {}: {error}", dir.display()))?;
    let metadata = std::fs::symlink_metadata(dir)
        .map_err(|error| format!("cannot inspect runtime state {}: {error}", dir.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(format!(
            "runtime state '{}' is not a regular directory",
            dir.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure runtime state {}: {error}", dir.display()))?;
    }
    Ok(())
}

fn context_window_cache_path(state_dir: &std::path::Path) -> std::path::PathBuf {
    state_dir.join("context_window_cache.json")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn context_window_cache_key(base_url: &str, model: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    base_url.hash(&mut hasher);
    model.hash(&mut hasher);
    format!("v1-{:016x}", hasher.finish())
}

/// Status output should identify the endpoint without echoing credentials that
/// a user may have embedded in URL user-info or a query string.
fn redact_endpoint(endpoint: &str) -> String {
    let mut display = endpoint.to_string();
    if let Some(query) = display.find('?') {
        display.truncate(query);
        display.push_str("?<redacted>");
    }
    if let Some(authority_start) = display.find("://").map(|index| index + 3) {
        let authority_end = display[authority_start..]
            .find('/')
            .map(|index| authority_start + index)
            .unwrap_or(display.len());
        if let Some(at) = display[authority_start..authority_end].rfind('@') {
            let at = authority_start + at;
            display.replace_range(authority_start..at, "<redacted>");
        }
    }
    display
}

fn read_cached_context_window(path: &std::path::Path, key: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let cache: ContextWindowCache = serde_json::from_str(&text).ok()?;
    let (window, fetched_at) = cache.entries.get(key)?;
    (unix_now().saturating_sub(*fetched_at) < CONTEXT_WINDOW_CACHE_TTL_SECS).then_some(*window)
}

fn write_cached_context_window(path: &std::path::Path, key: &str, window: u64) {
    use std::io::Write;
    let mut cache: ContextWindowCache = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // Migrate away from the previous plaintext `<base_url>|<model>` keys.
    cache.entries.retain(|key, _| key.starts_with("v1-"));
    cache.entries.insert(key.to_string(), (window, unix_now()));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&cache) {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        if let Ok(mut file) = options.open(path) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            let _ = file.write_all(json.as_bytes());
        }
    }
}

/// Resolve config + gateway profile and build the provider/auth/model. Emits the
/// best-effort token-expiry warning (warn-but-send, design §3).
async fn build_setup(cli: &Cli) -> Result<ChatSetup, CliError> {
    let path = Config::resolve_path(cli.config.as_deref());
    let selected_config = cli.config.is_some()
        || std::env::var_os("KITSU_CONFIG").is_some_and(|value| !value.is_empty());
    if !path.exists() {
        let message = if selected_config {
            format!(
                "config error: requested config '{}' does not exist",
                path.display()
            )
        } else {
            format!(
                "config error: no config found at {}\n  start with: kitsu --config ./kitsu.toml chat\n  or copy the bundled config to ~/.kitsu/config.toml",
                shorten_home(&path)
            )
        };
        return Err(CliError::user(message));
    }
    let config = Config::load(&path).map_err(|e| CliError::user(e.to_string()))?;
    // Resolve the location now, but create it only when a command actually
    // needs durable state. In particular, provider-free/empty scripts should
    // continue to work in containers with a read-only HOME.
    let state_dir = runtime_state_dir(&path);

    let (gateway_name, gateway) = config
        .select_gateway(cli.gateway.as_deref())
        .map_err(|e| CliError::user(e.to_string()))?;
    let gateway_name = gateway_name.to_string();
    gateway
        .validate_flavor()
        .map_err(|e| CliError::user(e.to_string()))?;
    let model = gateway
        .resolve_model(cli.model.as_deref())
        .map_err(|e| CliError::user(e.to_string()))?;

    let auth_status = match gateway.auth.method.as_str() {
        "none" => "none (no credential required)".to_string(),
        "static" => "static credential configured".to_string(),
        "bearer-env" => format!(
            "bearer-env {} ({})",
            gateway.auth.env,
            if std::env::var_os(&gateway.auth.env).is_some_and(|value| !value.is_empty()) {
                "present"
            } else {
                "missing"
            }
        ),
        other => other.to_string(),
    };
    let endpoint = redact_endpoint(&format!("{}{}", gateway.base_url, gateway.chat_path));

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
            let cache_key = context_window_cache_key(&gateway.base_url, &model);
            let cache_path = context_window_cache_path(&state_dir);
            let cached = read_cached_context_window(&cache_path, &cache_key);
            match cached {
                Some(w) => Some(w),
                None => {
                    let discovered = provider.model_context_length(&model).await;
                    if let Some(w) = discovered {
                        write_cached_context_window(&cache_path, &cache_key, w);
                    }
                    discovered
                }
            }
        }
    };

    Ok(ChatSetup {
        provider: Arc::new(provider),
        gateway_name,
        config_path: path,
        state_dir,
        endpoint,
        auth_status,
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
        let no_project = project_context_disabled(cli);
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
        Ok(_) => Ok(()),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptFailureKind {
    User,
    Provider,
}

fn script_failure_kind(error: &kitsu_core::AgentError) -> ScriptFailureKind {
    match error {
        kitsu_core::AgentError::Provider(ProviderError::Auth(_)) => ScriptFailureKind::User,
        _ => ScriptFailureKind::Provider,
    }
}

fn transcript_line(prefix: &str, text: &str) {
    use std::io::Write;
    // Echoes are metadata, not assistant output. Keeping them with slash
    // diagnostics on stderr preserves clean pipeable stdout and deterministic
    // command/result ordering in captured scripts.
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{prefix}> {text}");
    let _ = stderr.flush();
}

fn strict_turn_issue(status: &kitsu_core::AgentTurnStatus) -> Option<String> {
    use kitsu_core::TurnCompletion;
    match status.completion {
        TurnCompletion::LengthLimited => {
            Some("model output was truncated by its length limit".to_string())
        }
        TurnCompletion::MaxRounds => Some("agent exhausted the maximum tool rounds".to_string()),
        TurnCompletion::Complete if status.tool_errors > 0 => Some(format!(
            "{} tool call(s) failed during the turn",
            status.tool_errors
        )),
        TurnCompletion::Complete => None,
    }
}

fn report_cancelled_agents(ids: Vec<u64>) {
    if !ids.is_empty() {
        let ids = ids
            .iter()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!("~ cancelled background agent(s) started by the rolled-back turn: {ids}");
    }
}

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
    session: &SessionInfo,
    context_window: &Rc<Cell<Option<u64>>>,
    live_mw: &subagent::LiveModelWindow,
    registry: &Arc<subagent::AgentRegistry>,
    allow_turn_errors: bool,
) -> Result<(), CliError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| CliError::user(format!("cannot read script '{path}': {e}")))?;
    // One session state for the whole script — the context-meter anchor and
    // compaction circuit breaker persist across turns, matching interactive chat
    // (a script is a multi-turn session, not a series of independent one-shots).
    let mut state = kitsu_core::AgentSessionState::default();
    let lines: Vec<&str> = content.lines().collect();
    let mut idx = 0;
    let mut failures = 0usize;
    let mut provider_failures = 0usize;
    let mut last_failed: Option<SubmittedPrompt> = None;
    // Review prompts receive verified evidence directly and deliberately run
    // without the mutation-capable chat registry.
    let no_tools = Registry::new();
    while idx < lines.len() {
        let line_number = idx + 1;
        let trimmed = lines[idx].trim();
        idx += 1;

        if trimmed == "<<<" {
            // Collect a verbatim multi-line block until the closing ">>>".
            let mut block: Vec<&str> = Vec::new();
            while idx < lines.len() && lines[idx].trim() != ">>>" {
                block.push(lines[idx]);
                idx += 1;
            }
            if idx == lines.len() {
                return Err(CliError::user(format!(
                    "{path}:{line_number}: unterminated <<< block (missing >>>)"
                )));
            }
            idx += 1; // consume the closing ">>>"
            let prompt = SubmittedPrompt::full(block.join("\n"));
            let result = run_scripted_turn(
                setup,
                tools,
                messages,
                model,
                cli,
                &prompt.text,
                &prompt.text,
                offload_dir,
                &mut state,
                context_window,
                registry,
                &format!("{path}:{line_number}"),
            )
            .await;
            match result {
                Ok(()) => last_failed = None,
                Err(kind) => {
                    failures += 1;
                    provider_failures += usize::from(kind == ScriptFailureKind::Provider);
                    last_failed = Some(prompt);
                }
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('/') {
            transcript_line("command", trimmed);
            let result = handle_slash(
                rest,
                messages,
                model,
                &setup.provider,
                perms,
                session,
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
                SlashResult::Error => {
                    failures += 1;
                    eprintln!("{path}:{line_number}: command failed: {trimmed}");
                    continue;
                }
                SlashResult::SubmitPrompt(prompt) => {
                    let turn_tools = match prompt.tool_mode {
                        TurnToolMode::Full => tools,
                        TurnToolMode::None => &no_tools,
                    };
                    let result = run_scripted_turn(
                        setup,
                        turn_tools,
                        messages,
                        model,
                        cli,
                        &prompt.text,
                        trimmed,
                        offload_dir,
                        &mut state,
                        context_window,
                        registry,
                        &format!("{path}:{line_number}"),
                    )
                    .await;
                    match result {
                        Ok(()) => last_failed = None,
                        Err(kind) => {
                            failures += 1;
                            provider_failures += usize::from(kind == ScriptFailureKind::Provider);
                            last_failed = Some(prompt);
                        }
                    }
                    continue;
                }
                SlashResult::Retry => {
                    let Some(prompt) = last_failed.clone() else {
                        failures += 1;
                        eprintln!("{path}:{line_number}: nothing to retry");
                        continue;
                    };
                    let turn_tools = match prompt.tool_mode {
                        TurnToolMode::Full => tools,
                        TurnToolMode::None => &no_tools,
                    };
                    let result = run_scripted_turn(
                        setup,
                        turn_tools,
                        messages,
                        model,
                        cli,
                        &prompt.text,
                        "/retry",
                        offload_dir,
                        &mut state,
                        context_window,
                        registry,
                        &format!("{path}:{line_number}"),
                    )
                    .await;
                    match result {
                        Ok(()) => last_failed = None,
                        Err(kind) => {
                            failures += 1;
                            provider_failures += usize::from(kind == ScriptFailureKind::Provider);
                        }
                    }
                    continue;
                }
                SlashResult::NewSession
                | SlashResult::ResumeSession(_)
                | SlashResult::ListSessions => {
                    failures += 1;
                    eprintln!("{path}:{line_number}: session commands require interactive chat");
                    continue;
                }
            }
        }
        let prompt = SubmittedPrompt::full(trimmed);
        let result = run_scripted_turn(
            setup,
            tools,
            messages,
            model,
            cli,
            &prompt.text,
            trimmed,
            offload_dir,
            &mut state,
            context_window,
            registry,
            &format!("{path}:{line_number}"),
        )
        .await;
        match result {
            Ok(()) => last_failed = None,
            Err(kind) => {
                failures += 1;
                provider_failures += usize::from(kind == ScriptFailureKind::Provider);
                last_failed = Some(prompt);
            }
        }
    }
    let unfinished_agents = registry.running();
    if unfinished_agents > 0 {
        failures += 1;
        provider_failures += 1;
        eprintln!(
            "script ended with {unfinished_agents} unfinished background agent(s); detached work is not considered successful automation"
        );
        report_cancelled_agents(registry.cancel_started_after(0));
    }
    if failures > 0 {
        eprintln!("script summary: {failures} failure(s)");
        if !allow_turn_errors {
            let message = format!(
                "script failed: {failures} turn(s) or command(s) failed (use --allow-turn-errors to accept partial success)"
            );
            return Err(if provider_failures > 0 {
                CliError::provider(message)
            } else {
                CliError::user(message)
            });
        }
    }
    Ok(())
}

/// Send one scripted user turn. Failed turns roll conversation/meter state back
/// transactionally while completed filesystem side effects remain inspectable.
#[allow(clippy::too_many_arguments)]
async fn run_scripted_turn(
    setup: &ChatSetup,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    model: &str,
    cli: &Cli,
    input: &str,
    display_input: &str,
    offload_dir: &Option<std::path::PathBuf>,
    state: &mut kitsu_core::AgentSessionState,
    context_window: &Rc<Cell<Option<u64>>>,
    registry: &Arc<subagent::AgentRegistry>,
    location: &str,
) -> Result<(), ScriptFailureKind> {
    transcript_line("user", display_input);
    let messages_before = messages.clone();
    let state_before = state.clone();
    let agent_watermark = registry.watermark();
    messages.push(ChatMessage::user(input));
    let cfg = build_agent_config(
        model.to_string(),
        cli,
        context_window.get(),
        offload_dir.clone(),
    );
    match run_agent_turn_with_state(&setup.provider, tools, messages, &cfg, state).await {
        Ok(_) => {
            if let Some(issue) = strict_turn_issue(&state.last_turn) {
                *messages = messages_before;
                *state = state_before;
                report_cancelled_agents(registry.cancel_started_after(agent_watermark));
                eprintln!("{location}: strict scripted turn failed: {issue}");
                eprintln!(
                    "kitsu-context-rollback: incomplete turn removed from conversation state"
                );
                eprintln!(
                    "~ failed turn rolled back; completed file changes, if any, remain on disk"
                );
                Err(ScriptFailureKind::Provider)
            } else {
                Ok(())
            }
        }
        Err(e) => {
            let kind = script_failure_kind(&e);
            *messages = messages_before;
            *state = state_before;
            report_cancelled_agents(registry.cancel_started_after(agent_watermark));
            eprintln!("{location}: {}", chat_error_message(e, model));
            eprintln!("kitsu-context-rollback: failed turn removed from conversation state");
            eprintln!("~ failed turn rolled back; completed file changes, if any, remain on disk");
            Err(kind)
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

async fn run_chat(cli: &Cli, invocation: ChatInvocation<'_>) -> Result<(), CliError> {
    let ChatInvocation {
        first_prompt,
        allow_paths,
        allow_write,
        script,
        resume,
        no_session,
        allow_turn_errors,
    } = invocation;
    let setup = build_setup(cli).await?;

    // Permissions gate the fs tools. Interactive TTY chat uses "ask" mode (prompt
    // on demand) with read access to the launch directory by default; scripted /
    // piped runs are deny-by-default (or --yes to auto-approve). --allow-path /
    // --allow-write pre-grant paths so those never prompt.
    let scripted = script.is_some();
    let interactive = kitsu_render::stdin_is_tty() && !scripted;
    if resume.is_some() && !interactive {
        return Err(CliError::user(
            "--resume requires an interactive TTY chat (it cannot be combined with --script or piped input)"
                .to_string(),
        ));
    }
    if interactive {
        prepare_runtime_state_dir(&setup.state_dir).map_err(CliError::user)?;
    }
    let sessions_dir = setup.state_dir.join("sessions");
    let resumed = match resume {
        Some(selector) => Some(
            session::SessionStore::load(&sessions_dir, selector)
                .map_err(|error| CliError::user(format!("cannot resume chat: {error}")))?,
        ),
        None => None,
    };
    if let Some(saved) = resumed.as_ref() {
        let current = std::env::current_dir().map_err(|error| {
            CliError::user(format!("cannot resolve current directory: {error}"))
        })?;
        if !same_directory(&saved.directory, &current) {
            return Err(CliError::user(format!(
                "cannot resume session '{}' from {} while Kitsu is running in {}; change to the saved directory first",
                saved.id,
                shorten_home(std::path::Path::new(&saved.directory)),
                shorten_home(&current)
            )));
        }
    }
    let mut session_store = if interactive && !no_session {
        Some(match resumed.as_ref() {
            Some(saved) => session::SessionStore::from_saved(sessions_dir.clone(), saved)
                .map_err(CliError::user)?,
            None => session::SessionStore::create(sessions_dir.clone()).map_err(CliError::user)?,
        })
    } else {
        None
    };
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
    perms
        .protect_read_path(&setup.config_path.to_string_lossy())
        .map_err(|error| CliError::user(format!("cannot protect Kitsu config: {error}")))?;
    // Stateless runs intentionally leave this directory absent. If it exists,
    // protect its canonical location from model file tools; interactive runs
    // created and secured it just above.
    if setup.state_dir.exists() {
        perms
            .protect_read_path(&setup.state_dir.to_string_lossy())
            .map_err(|error| CliError::user(format!("cannot protect Kitsu state: {error}")))?;
    }

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
    let no_project = project_context_disabled(cli);
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

    // A per-session dir for offloading large tool results (Stage 2). Persistent
    // interactive sessions keep artifacts beside the snapshot so `/resume`
    // never reloads dead /tmp pointers; ephemeral runs remove their scratch dir.
    let persistent_artifacts = session_store
        .as_ref()
        .map(session::SessionStore::artifacts_dir);
    let offload_dir = if tool_names.is_empty() {
        None
    } else {
        let dir = persistent_artifacts.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("kitsu-offload-{}", std::process::id()))
        });
        match std::fs::create_dir_all(&dir).map(|_| perms.grant_read_root(&dir.to_string_lossy())) {
            Ok(Ok(_)) => Some(dir),
            _ => {
                let _ = std::fs::remove_dir_all(&dir);
                None
            }
        }
    };
    let _offload_guard = if persistent_artifacts.is_none() {
        offload_dir.clone().map(OffloadDirGuard)
    } else {
        None
    };
    let offload_state = Rc::new(RefCell::new(offload_dir.clone()));

    let mut model = setup.model.clone();
    // Shared with session UI state and updated in place by `/model`, so
    // switching models doesn't leave every subsequent turn budgeting against
    // the OLD model's window — a stale, larger window would under-trim and the
    // gateway would reject the oversized request instead of kitsu proactively
    // trimming to fit the new model's real limit.
    let context_window = Rc::new(Cell::new(setup.context_window));
    let base_cfg = {
        let context_window = context_window.clone();
        let offload_state = offload_state.clone();
        move |model: String, cli: &Cli| {
            build_agent_config(
                model,
                cli,
                context_window.get(),
                offload_state.borrow().clone(),
            )
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
    let mut project_context_source = None;

    // Project context (llms.txt / AGENTS.md), budget-gated (Stage 4).
    if !project_context_disabled(cli) {
        if let Some((ctx, _, source)) = std::env::current_dir()
            .ok()
            .and_then(|cwd| llms::load(&cwd, None, setup.context_window))
        {
            project_context_source = Some(source.clone());
            let label = source
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("project context");
            let chunk = format!("\n\n---\nProject context (from {label}):\n{ctx}");
            let tokens = estimate_str(&chunk);
            system.push_str(&chunk);
            if resumed.is_none() {
                eprintln!(
                    "~ loaded project context from {} ({tokens} tokens; disable with --no-project-context)",
                    shorten_home(&source)
                );
            }
            provenance.push(CtxEntry::new(
                format!("project ({label})"),
                tokens,
                "loaded",
                format!("curated context from {}", shorten_home(&source)),
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
        grant_preloaded_skill_roots(cli, &skillset, &perms);
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
    let fresh_project_context_source = project_context_source.clone();
    let fresh_provenance = provenance.clone();
    let directory = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let session_id = Rc::new(RefCell::new(
        session_store.as_ref().map(|store| store.id().to_string()),
    ));

    let mut messages = vec![ChatMessage::system(system)];
    // `/new` starts from the context assembled for this invocation, rather than
    // inheriting an older resumed session's system prompt.
    let fresh_system_message = messages[0].clone();
    if let Some(saved) = resumed {
        if saved.gateway != setup.gateway_name {
            return Err(CliError::user(format!(
                "saved session '{}' uses gateway '{}', but '{}' is selected",
                saved.id, saved.gateway, setup.gateway_name
            )));
        }
        if !matches!(
            saved.messages.first().map(|message| message.role),
            Some(kitsu_providers::Role::System)
        ) {
            return Err(CliError::user(format!(
                "saved session '{}' has no leading system message",
                saved.id
            )));
        }
        if cli.model.is_none() && saved.model != model {
            model = saved.model.clone();
            if !setup.context_window_pinned {
                context_window.set(setup.provider.model_context_length(&model).await);
            }
        }
        if no_project {
            project_context_source = None;
            provenance = fresh_provenance.clone();
            if saved.project_context.is_some() {
                eprintln!("~ omitted saved project context because --no-project-context is active");
            }
        } else {
            project_context_source = saved
                .project_context
                .as_deref()
                .map(std::path::PathBuf::from);
            provenance = resumed_provenance(&saved, &fresh_provenance);
        }
        eprintln!(
            "~ resumed session {} (filesystem permissions start fresh)",
            saved.id
        );
        messages = restored_messages(&saved, &fresh_system_message, no_project);
        session_state.meter.invalidate();
    }
    *live_mw.lock().expect("live model/window") = (model.clone(), context_window.get());

    let session = SessionInfo {
        gateway: setup.gateway_name.clone(),
        directory,
        config_path: setup.config_path.clone(),
        state_dir: setup.state_dir.clone(),
        endpoint: setup.endpoint.clone(),
        auth_status: setup.auth_status.clone(),
        project_context: RefCell::new(project_context_source),
        offload_dir: offload_state.clone(),
        session_id,
        tool_names: tool_names.clone(),
        window: context_window.clone(),
        max_tokens: cli.max_output_tokens,
        provenance: RefCell::new(provenance),
    };

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
            &session,
            &context_window,
            &live_mw,
            &agent_registry,
            allow_turn_errors,
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
            Ok(_) => Ok(()),
            Err(e) => Err(map_agent_error(e)),
        };
    }

    // Interactive TTY loop. Explicitly enable bracketed paste so a pasted
    // multi-line block (a code snippet or stack trace — the bread and butter of
    // coding chats) is captured as ONE turn instead of each line submitting
    // separately. It defaults on in rustyline 14, but we set it deliberately.
    let rl_config = rustyline::Config::builder()
        .bracketed_paste(true)
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut rl: rustyline::Editor<ChatHelper, rustyline::history::DefaultHistory> =
        rustyline::Editor::with_config(rl_config)
            .map_err(|e| CliError::user(format!("failed to start input editor: {e}")))?;
    // Complete slash commands without changing the visible input-buffer width.
    rl.set_helper(Some(ChatHelper));
    let history_path = setup.state_dir.join("history");
    let _ = rl.load_history(&history_path);

    kitsu_render::chat_banner(
        &session.gateway,
        &model,
        &shorten_home(&session.directory),
        &session.tool_names,
    );
    persist_session(&session_store, &session, &model, &messages);

    // If a first prompt was given, queue it as the first turn.
    let mut queued = first_prompt.map(|s| s.to_string());
    let prompt = kitsu_render::prompt_string();
    // Report background-agent activity only when the running count changes, so
    // the user notices work starting/finishing without a per-prompt spam line.
    let mut last_running = 0usize;
    let mut last_failed: Option<SubmittedPrompt> = None;
    let no_tools = Registry::new();

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
                Err(rustyline::error::ReadlineError::Interrupted) => {
                    eprintln!("^C (input cleared; Ctrl+D or /quit exits)");
                    continue;
                }
                Err(e) => {
                    eprintln!("input error: {e}");
                    break;
                }
            },
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(&line);
        let mut submitted = SubmittedPrompt::full(line);

        if let Some(rest) = submitted.text.strip_prefix('/') {
            let slash_name = rest.split_whitespace().next().unwrap_or("").to_string();
            let result = handle_slash(
                rest,
                &mut messages,
                &mut model,
                &setup.provider,
                &perms,
                &session,
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
                SlashResult::Continue => {
                    if slash_name == "clear" {
                        last_failed = None;
                    }
                    persist_session(&session_store, &session, &model, &messages);
                    continue;
                }
                SlashResult::Error => continue,
                SlashResult::Quit => {
                    persist_session(&session_store, &session, &model, &messages);
                    break;
                }
                SlashResult::SubmitPrompt(prompt) => submitted = prompt,
                SlashResult::Retry => {
                    let Some(prompt) = last_failed.clone() else {
                        eprintln!("nothing to retry");
                        continue;
                    };
                    eprintln!("~ retrying the last failed prompt");
                    submitted = prompt;
                }
                SlashResult::NewSession => {
                    if agent_registry.running() > 0 {
                        eprintln!(
                            "new session blocked while background agents are running; use /agents and wait for them to finish"
                        );
                        continue;
                    }
                    if !persist_session(&session_store, &session, &model, &messages) {
                        eprintln!(
                            "new session cancelled because the current session could not be saved"
                        );
                        continue;
                    }
                    messages = vec![fresh_system_message.clone()];
                    session_state = kitsu_core::AgentSessionState::default();
                    last_failed = None;
                    agent_registry.reset();
                    *session.project_context.borrow_mut() = fresh_project_context_source.clone();
                    *session.provenance.borrow_mut() = fresh_provenance.clone();
                    if let Some(store) = session_store.as_mut() {
                        store.reset();
                        *session.session_id.borrow_mut() = Some(store.id().to_string());
                    }
                    perms.reset_runtime_grants();
                    grant_preloaded_skill_roots(cli, &skillset, &perms);
                    let desired_offload = if session.tool_names.is_empty() {
                        None
                    } else if let Some(store) = session_store.as_ref() {
                        Some(store.artifacts_dir())
                    } else {
                        // `--no-session` keeps one private scratch directory,
                        // but erases its old-session contents at the boundary.
                        let current = session.offload_dir.borrow().clone();
                        if let Some(dir) = current.as_ref() {
                            let _ = std::fs::remove_dir_all(dir);
                        }
                        current
                    };
                    if let Err(error) =
                        activate_offload_dir(&perms, &session.offload_dir, desired_offload)
                    {
                        eprintln!("warning: large tool-result offloading disabled: {error}");
                    }
                    persist_session(&session_store, &session, &model, &messages);
                    eprintln!(
                        "~ started new session {}",
                        session
                            .session_id
                            .borrow()
                            .as_deref()
                            .unwrap_or("(autosave disabled)")
                    );
                    continue;
                }
                SlashResult::ResumeSession(selector) => {
                    if agent_registry.running() > 0 {
                        eprintln!(
                            "resume blocked while background agents are running; use /agents and wait for them to finish"
                        );
                        continue;
                    }
                    if !persist_session(&session_store, &session, &model, &messages) {
                        eprintln!(
                            "resume cancelled because the current session could not be saved"
                        );
                        continue;
                    }
                    let saved = match session::SessionStore::load(&sessions_dir, &selector) {
                        Ok(saved) => saved,
                        Err(error) => {
                            eprintln!("cannot resume chat: {error}");
                            continue;
                        }
                    };
                    if saved.gateway != session.gateway {
                        eprintln!(
                            "cannot resume session '{}': it uses gateway '{}', not '{}'",
                            saved.id, saved.gateway, session.gateway
                        );
                        continue;
                    }
                    if !same_directory(&saved.directory, &session.directory) {
                        eprintln!(
                            "cannot resume session '{}' from {} while Kitsu is running in {}; change to the saved directory first",
                            saved.id,
                            shorten_home(std::path::Path::new(&saved.directory)),
                            shorten_home(&session.directory)
                        );
                        continue;
                    }
                    if !matches!(
                        saved.messages.first().map(|message| message.role),
                        Some(kitsu_providers::Role::System)
                    ) {
                        eprintln!(
                            "cannot resume session '{}': missing system context",
                            saved.id
                        );
                        continue;
                    }
                    let resumed_store = if no_session {
                        None
                    } else {
                        match session::SessionStore::from_saved(sessions_dir.clone(), &saved) {
                            Ok(store) => Some(store),
                            Err(error) => {
                                eprintln!("cannot autosave resumed session: {error}");
                                continue;
                            }
                        }
                    };
                    model = saved.model.clone();
                    if !setup.context_window_pinned {
                        context_window.set(setup.provider.model_context_length(&model).await);
                    }
                    if let Some(store) = resumed_store {
                        session_store = Some(store);
                        *session.session_id.borrow_mut() = Some(saved.id.clone());
                    }
                    perms.reset_runtime_grants();
                    grant_preloaded_skill_roots(cli, &skillset, &perms);
                    let desired_offload = if session.tool_names.is_empty() {
                        None
                    } else if let Some(store) = session_store.as_ref() {
                        Some(store.artifacts_dir())
                    } else {
                        let current = session.offload_dir.borrow().clone();
                        if let Some(dir) = current.as_ref() {
                            let _ = std::fs::remove_dir_all(dir);
                        }
                        current
                    };
                    if let Err(error) =
                        activate_offload_dir(&perms, &session.offload_dir, desired_offload)
                    {
                        eprintln!("warning: large tool-result offloading disabled: {error}");
                    }
                    if no_project {
                        *session.project_context.borrow_mut() = None;
                        *session.provenance.borrow_mut() = fresh_provenance.clone();
                        if saved.project_context.is_some() {
                            eprintln!(
                                "~ omitted saved project context because --no-project-context is active"
                            );
                        }
                    } else {
                        *session.project_context.borrow_mut() = saved
                            .project_context
                            .as_deref()
                            .map(std::path::PathBuf::from);
                        *session.provenance.borrow_mut() =
                            resumed_provenance(&saved, &fresh_provenance);
                    }
                    messages = restored_messages(&saved, &fresh_system_message, no_project);
                    session_state = kitsu_core::AgentSessionState::default();
                    last_failed = None;
                    agent_registry.reset();
                    *live_mw.lock().expect("live model/window") =
                        (model.clone(), context_window.get());
                    eprintln!(
                        "~ resumed {} (runtime permissions reset; saved permissions are never restored)",
                        saved.id
                    );
                    continue;
                }
                SlashResult::ListSessions => {
                    let current = session.session_id.borrow().clone();
                    match format_sessions(&sessions_dir, current.as_deref()) {
                        Ok(list) => eprintln!("{list}"),
                        Err(error) => eprintln!("cannot list sessions: {error}"),
                    }
                    continue;
                }
            }
        }

        let messages_before = messages.clone();
        let state_before = session_state.clone();
        let agent_watermark = agent_registry.watermark();
        messages.push(ChatMessage::user(&submitted.text));
        let cfg = base_cfg(model.clone(), cli);
        let turn_tools = match submitted.tool_mode {
            TurnToolMode::Full => &tools,
            TurnToolMode::None => &no_tools,
        };
        match run_turn_cancellable(
            &setup.provider,
            turn_tools,
            &mut messages,
            &cfg,
            &mut session_state,
        )
        .await
        {
            TurnOutcome::Completed(Ok(text)) => {
                last_failed = None;
                // The model can return an empty completion for vague inputs;
                // say so instead of looking hung.
                if text.trim().is_empty() {
                    eprintln!("(the model returned an empty response — try rephrasing)");
                }
                persist_session(&session_store, &session, &model, &messages);
            }
            TurnOutcome::Completed(Err(e)) => {
                messages = messages_before;
                session_state = state_before;
                report_cancelled_agents(agent_registry.cancel_started_after(agent_watermark));
                last_failed = Some(submitted);
                eprintln!("{}", chat_error_message(e, &model));
                eprintln!(
                    "~ failed turn rolled back; completed file changes, if any, remain on disk"
                );
            }
            TurnOutcome::Cancelled => {
                messages = messages_before;
                session_state = state_before;
                report_cancelled_agents(agent_registry.cancel_started_after(agent_watermark));
                last_failed = Some(submitted);
                kitsu_render::finish_line();
                eprintln!("^C (response cancelled; turn rolled back — /retry to try again)");
                eprintln!("~ completed file changes, if any, remain on disk");
            }
        }
    }

    let _ = rl.save_history(&history_path);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnToolMode {
    Full,
    None,
}

#[derive(Debug, Clone)]
struct SubmittedPrompt {
    text: String,
    tool_mode: TurnToolMode,
}

impl SubmittedPrompt {
    fn full(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_mode: TurnToolMode::Full,
        }
    }

    fn review(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_mode: TurnToolMode::None,
        }
    }
}

enum SlashResult {
    Continue,
    Quit,
    Error,
    SubmitPrompt(SubmittedPrompt),
    Retry,
    NewSession,
    ResumeSession(String),
    ListSessions,
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (left_index, left_byte) in left.bytes().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_byte) in right.bytes().enumerate() {
            let replace = previous[right_index] + usize::from(left_byte != right_byte);
            let insert = current[right_index] + 1;
            let delete = previous[right_index + 1] + 1;
            current.push(replace.min(insert).min(delete));
        }
        previous = current;
    }
    previous[right.len()]
}

fn closest_slash_command(name: &str) -> Option<&'static str> {
    let prefix: Vec<_> = matching_slash_commands(name);
    if prefix.len() == 1 {
        return Some(prefix[0].name);
    }
    SLASH_COMMANDS
        .iter()
        .map(|spec| (edit_distance(name, spec.name), spec.name))
        .min()
        .filter(|(distance, _)| *distance <= 3)
        .map(|(_, name)| name)
}

fn slash_syntax(name: &str) -> String {
    SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == name)
        .map(|spec| spec.syntax())
        .unwrap_or_else(|| format!("/{name}"))
}

/// Handle a slash command (the leading `/` already stripped).
#[allow(clippy::too_many_arguments)]
async fn handle_slash(
    cmd: &str,
    messages: &mut Vec<ChatMessage>,
    model: &mut String,
    provider: &OpenAICompatProvider,
    perms: &Permissions,
    session: &SessionInfo,
    skillset: &skills::SkillSet,
    context_window: &Rc<Cell<Option<u64>>>,
    context_window_pinned: bool,
    state: &mut kitsu_core::AgentSessionState,
    registry: &Arc<subagent::AgentRegistry>,
) -> SlashResult {
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    if !arg.is_empty()
        && matches!(
            name,
            "clear"
                | "context"
                | "exit"
                | "help"
                | "new"
                | "permissions"
                | "quit"
                | "retry"
                | "sessions"
                | "status"
                | "tools"
        )
    {
        eprintln!("usage: {}", slash_syntax(name));
        return SlashResult::Error;
    }

    match name {
        "quit" | "exit" => return SlashResult::Quit,
        "" | "help" => {
            eprintln!("{}", slash_help(model));
        }
        "status" => eprintln!(
            "{}",
            format_session_status(model, messages, session, perms, registry, state)
        ),
        "permissions" => eprintln!("{}", format_permissions(perms, session)),
        "context" => render_context_command(model, messages, session, state),
        "diff" => match authorized_workspace_diff(session, perms, arg).await {
            Ok(diff) => kitsu_render::render_diff(&diff),
            Err(error) => {
                eprintln!("cannot show diff: {error}");
                return SlashResult::Error;
            }
        },
        "review" => match authorized_workspace_diff(session, perms, arg).await {
            Ok(diff) if diff == "(working tree clean)" => {
                eprintln!("nothing to review: working tree clean");
                return SlashResult::Error;
            }
            Ok(diff) => {
                return SlashResult::SubmitPrompt(SubmittedPrompt::review(format!(
                    "Review the following verified working-tree diff. Focus on correctness, security, regressions, and missing tests. Report findings by severity with file/line references. Do not claim anything outside this exact diff.\n\n```diff\n{diff}\n```"
                )));
            }
            Err(error) => {
                eprintln!("cannot review diff: {error}");
                return SlashResult::Error;
            }
        },
        "new" => return SlashResult::NewSession,
        "resume" => {
            return SlashResult::ResumeSession(if arg.is_empty() {
                "last".to_string()
            } else {
                arg.to_string()
            });
        }
        "retry" => return SlashResult::Retry,
        "sessions" => return SlashResult::ListSessions,
        "tools" => {
            let (mode, behavior) = permission_mode_label(perms.mode());
            eprintln!(
                "tools ({} enabled; permissions: {mode} — {behavior}):",
                session.tool_names.len()
            );
            for name in &session.tool_names {
                eprintln!("  - {name}");
            }
            if session.tool_names.is_empty() {
                eprintln!("  none — pre-grant --allow-path/--allow-write for scripted runs");
            }
        }
        "agents" => {
            if arg.is_empty() {
                eprintln!("{}", registry.list());
            } else if let Ok(id) = arg.parse::<u64>() {
                match registry.result(id) {
                    Some(text) => eprintln!("#{id}:\n{text}"),
                    None => {
                        eprintln!("no background agent #{id}");
                        return SlashResult::Error;
                    }
                }
            } else {
                eprintln!("usage: /agents [id]");
                return SlashResult::Error;
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
                return SlashResult::Error;
            } else {
                match skillset.get(arg) {
                    Some(s) => {
                        let marker = format!("[Loaded skill: {}]", s.name);
                        if messages
                            .iter()
                            .any(|message| message.content.starts_with(&marker))
                        {
                            eprintln!("~ skill '{}' is already loaded", s.name);
                            return SlashResult::Continue;
                        }
                        // Grants read to the skill's dir + lists bundled files/links.
                        let loaded = skills::load_skill(s, perms);
                        if !s.links.is_empty() {
                            eprintln!("~ {} reference link(s) (not fetched)", s.links.len());
                        }
                        messages.push(ChatMessage::user(format!("{marker}\n\n{loaded}")));
                        eprintln!("~ loaded skill '{}'", s.name);
                    }
                    None => {
                        eprintln!("no skill named '{arg}' — try /skills");
                        return SlashResult::Error;
                    }
                }
            }
        }
        "models" => {
            if !list_models_and_print(provider, arg).await {
                return SlashResult::Error;
            }
        }
        "allow" => {
            if arg.is_empty() {
                eprintln!("usage: /allow <path> (grant read access to a directory/file)");
                return SlashResult::Error;
            } else if !session.tool_names.iter().any(|name| name == "fs_read_file") {
                eprintln!(
                    "read tools are disabled in this run — restart with --allow-path <path> (scripted/piped chat cannot add tools after startup)"
                );
                return SlashResult::Error;
            } else {
                match perms.grant_read_root(arg) {
                    Ok(p) => eprintln!("~ read access granted: {}", p.display()),
                    Err(e) => {
                        eprintln!("{e}");
                        return SlashResult::Error;
                    }
                }
            }
        }
        "allow-write" => {
            if arg.is_empty() {
                eprintln!("usage: /allow-write <path> (grant write access to a file or directory)");
                return SlashResult::Error;
            } else if !session
                .tool_names
                .iter()
                .any(|name| name == "fs_write_file")
            {
                eprintln!(
                    "write tools are disabled in this run — restart with --allow-write <path> (scripted/piped chat cannot add tools after startup)"
                );
                return SlashResult::Error;
            } else {
                match perms.grant_write_root(arg) {
                    Ok(p) => eprintln!("~ write access granted: {}", p.display()),
                    Err(e) => {
                        eprintln!("{e}");
                        return SlashResult::Error;
                    }
                }
            }
        }
        "save" => {
            if arg.is_empty() {
                eprintln!("usage: /save <file> (writes the last response; code blocks if present)");
                return SlashResult::Error;
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
                            Err(e) => {
                                eprintln!("save failed: {e}");
                                return SlashResult::Error;
                            }
                        }
                    }
                    None => {
                        eprintln!("nothing to save yet");
                        return SlashResult::Error;
                    }
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
                return SlashResult::Error;
            } else if arg == "list" || arg.starts_with("list ") {
                // Alias: `/model list [filter]` → same as `/models [filter]`.
                let filter = arg.strip_prefix("list").unwrap_or("").trim();
                if !list_models_and_print(provider, filter).await {
                    return SlashResult::Error;
                }
            } else {
                if let Ok(Some(models)) = provider.list_models().await {
                    if !models.iter().any(|candidate| candidate == arg) {
                        eprintln!("model '{arg}' is not in the gateway catalog — run /models");
                        return SlashResult::Error;
                    }
                }
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
            if let Some(suggestion) = closest_slash_command(other) {
                eprintln!("unknown command: /{other} — did you mean /{suggestion}?");
            } else {
                eprintln!("unknown command: /{other} (try /help)");
            }
            return SlashResult::Error;
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
async fn list_models_and_print(provider: &OpenAICompatProvider, filter: &str) -> bool {
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
                true
            } else {
                eprintln!("no matches — run /models without a filter to see the full catalog");
                false
            }
        }
        Ok(None) => {
            eprintln!("this gateway does not expose a model list (its models_path is empty)");
            false
        }
        Err(e) => {
            eprintln!("could not fetch models: {e}");
            false
        }
    }
}

/// Prints a permission prompt to stderr and reads the user's choice from stdin.
/// Used only in interactive `Mode::Ask`; the prompt fires mid-turn while
/// rustyline is idle. Bracketed paste is disabled around the read so a stray
/// paste can't inject `ESC[200~` markers. Exactly one line is considered:
/// invalid input, EOF, or an input error denies without letting a later pasted
/// line become an authorization decision.
struct CliPrompter;

fn parse_permission_answer(answer: &str) -> Option<Decision> {
    let answer = answer
        .trim()
        .trim_start_matches("\u{1b}[200~")
        .trim_end_matches("\u{1b}[201~")
        .trim()
        .to_ascii_lowercase();
    match answer.as_str() {
        "y" | "yes" => Some(Decision::AllowOnce),
        "a" | "always" => Some(Decision::AllowAlways),
        "n" | "no" => Some(Decision::Deny),
        _ => None,
    }
}

impl Prompter for CliPrompter {
    fn ask(&self, req: &PermissionRequest) -> Decision {
        use std::io::Write;
        // Disable bracketed paste (ESC[?2004l) for the raw read; rustyline
        // re-enables it on the next prompt.
        if kitsu_render::plain_mode() {
            eprint!(
                "\x1b[?2004l\npermission> kitsu wants to {} {}\n  [y] allow once   [a] always ({})   [n] deny\n> ",
                req.action.verb(),
                req.path,
                req.grant_dir
            );
        } else {
            eprint!(
                "\x1b[?2004l\n\u{23fa} kitsu wants to {} {}\n  [y] allow once   [a] always ({})   [n] deny\n> ",
                req.action.verb(),
                req.path,
                req.grant_dir
            );
        }
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let Ok(read) = std::io::stdin().read_line(&mut line) else {
            return Decision::Deny;
        };
        if read == 0 {
            return Decision::Deny;
        }
        parse_permission_answer(&line).unwrap_or_else(|| {
            eprintln!("invalid permission response; denied (enter y, a/always, or n next time)");
            Decision::Deny
        })
    }
}

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
    fn multiline_highlighting_preserves_buffer_geometry() {
        use rustyline::highlight::Highlighter;
        let h = ChatHelper;
        assert_eq!(h.highlight("just one line", 0), "just one line");
        assert!(!h.highlight_char("just one line", 0, false));
        let pasted = "first line here\nsecond\nthird";
        assert_eq!(h.highlight(pasted, 0), pasted);
        assert!(!h.highlight_char(pasted, 0, false));
    }

    #[test]
    fn slash_completion_discovers_commands_and_inserts_argument_space() {
        use rustyline::completion::Completer;
        let history = rustyline::history::DefaultHistory::new();
        let context = rustyline::Context::new(&history);

        let (start, candidates) = ChatHelper.complete("/mod", 4, &context).unwrap();
        assert_eq!(start, 0);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.replacement.as_str())
                .collect::<Vec<_>>(),
            vec!["/model ", "/models "]
        );
        assert!(candidates[0].display.contains("switch model"));

        let (_, candidates) = ChatHelper.complete("/status", 7, &context).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "/status");

        let (_, candidates) = ChatHelper.complete("/model qwen", 12, &context).unwrap();
        assert!(candidates.is_empty(), "arguments are not command names");
    }

    #[test]
    fn slash_registry_is_sorted_unique_and_drives_help() {
        let names: Vec<_> = SLASH_COMMANDS.iter().map(|spec| spec.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "registry must remain sorted and unique");

        let help = slash_help("test-model");
        for spec in SLASH_COMMANDS {
            assert!(help.contains(&spec.syntax()));
            assert!(help.contains(spec.description));
        }
        assert!(help.ends_with("current model: test-model"));
    }

    #[test]
    fn endpoint_display_redacts_userinfo_and_query_values() {
        assert_eq!(
            redact_endpoint("https://user:secret@example.test/v1/chat?api_key=secret"),
            "https://<redacted>@example.test/v1/chat?<redacted>"
        );
        assert_eq!(
            redact_endpoint("http://localhost:11434/v1/chat/completions"),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn permission_answers_require_exact_tokens() {
        assert_eq!(parse_permission_answer("y\n"), Some(Decision::AllowOnce));
        assert_eq!(parse_permission_answer("YES"), Some(Decision::AllowOnce));
        assert_eq!(
            parse_permission_answer("always"),
            Some(Decision::AllowAlways)
        );
        assert_eq!(parse_permission_answer("a"), Some(Decision::AllowAlways));
        assert_eq!(parse_permission_answer("no"), Some(Decision::Deny));
        assert_eq!(parse_permission_answer("another pasted line"), None);
        assert_eq!(parse_permission_answer("yes please"), None);
    }

    #[test]
    fn context_cache_key_does_not_embed_endpoint_credentials() {
        let key =
            context_window_cache_key("https://user:secret@example.test/token/path", "model-a");
        assert!(key.starts_with("v1-"));
        assert!(!key.contains("secret"));
        assert!(!key.contains("token"));
    }

    #[test]
    #[cfg(unix)]
    fn profile_key_is_versioned_and_stable() {
        assert_eq!(
            stable_profile_key(std::path::Path::new("/workspace/kitsu/kitsu.toml")),
            "v1-fnv128-9f1392a8a96c8bcf6b4866d572b912eb"
        );
    }

    #[test]
    #[serial]
    fn project_local_config_uses_private_home_state() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join("kitsu.toml");
        std::fs::write(&config, "[gateways]\n").unwrap();
        let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

        let state = runtime_state_dir(&config);
        assert!(state.starts_with(home.path().join(".kitsu/profiles")));
        assert!(!state.starts_with(project.path()));
        assert!(state
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("v1-fnv128-"));
        prepare_runtime_state_dir(&state).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(state).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    #[serial]
    fn project_profile_migrates_the_legacy_unstable_key() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join("kitsu.toml");
        std::fs::write(&config, "[gateways]\n").unwrap();
        let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
        let canonical = std::fs::canonicalize(&config).unwrap();
        let base = home.path().join(".kitsu");
        let legacy = legacy_profile_dir(&base, &canonical);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("history"), "preserved").unwrap();

        let state = runtime_state_dir(&config);

        assert!(state
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("v1-fnv128-"));
        assert_eq!(
            std::fs::read_to_string(state.join("history")).unwrap(),
            "preserved"
        );
        assert!(!legacy.exists());
    }

    #[test]
    fn session_artifact_switch_drops_old_runtime_authority() {
        let old = tempfile::tempdir().unwrap();
        let new_root = tempfile::tempdir().unwrap();
        let new = new_root.path().join("new-artifacts");
        let perms = Permissions::for_roots(&[], &[]).unwrap();
        perms
            .grant_read_root(&old.path().to_string_lossy())
            .unwrap();
        let state = Rc::new(RefCell::new(Some(old.path().to_path_buf())));

        perms.reset_runtime_grants();
        activate_offload_dir(&perms, &state, Some(new.clone())).unwrap();

        assert_eq!(*state.borrow(), Some(new.clone()));
        assert!(perms.read_roots().iter().any(|root| root == &new));
        assert!(!perms.read_roots().iter().any(|root| root == old.path()));
    }

    #[test]
    fn resume_directory_comparison_uses_canonical_paths() {
        let dir = tempfile::tempdir().unwrap();
        assert!(same_directory(
            &dir.path().join(".").to_string_lossy(),
            dir.path()
        ));
        let other = tempfile::tempdir().unwrap();
        assert!(!same_directory(&dir.path().to_string_lossy(), other.path()));
    }

    #[test]
    fn resume_project_context_opt_out_replaces_saved_system_message() {
        let saved = session::SavedSession {
            version: 1,
            id: "s-test".to_string(),
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
            directory: "/tmp/project".to_string(),
            gateway: "local".to_string(),
            model: "model".to_string(),
            project_context: Some("/tmp/project/AGENTS.md".to_string()),
            messages: vec![
                ChatMessage::system("BASE\nSECRET_PROJECT_CONTEXT"),
                ChatMessage::user("keep this turn"),
            ],
        };
        let fresh = ChatMessage::system("FRESH_WITHOUT_PROJECT_CONTEXT");

        let restored = restored_messages(&saved, &fresh, true);
        assert_eq!(restored[0], fresh);
        assert_eq!(restored[1], saved.messages[1]);
        assert!(!restored[0].content.contains("SECRET_PROJECT_CONTEXT"));
    }

    #[test]
    fn global_skill_flag_parses_after_the_subcommand() {
        let parsed = Cli::try_parse_from(["kitsu", "ask", "hello", "--skill", "review"])
            .expect("global flag after subcommand");
        assert_eq!(parsed.skill, vec!["review"]);
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
