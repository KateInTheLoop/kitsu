//! CLI surface (clap derive). MVP: global `--gateway`, `--model`, `--config`,
//! and the `ask` subcommand (docs/architecture.md §6).

use clap::{Parser, Subcommand};

/// Commit + source commit date embedded in this binary — `-V` shows the plain
/// semver, while `--version` shows this provenance too. The date is deliberately
/// not called a build date: reproducible builds derive it from Git history.
pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("KITSU_GIT_SHA"),
    ", source commit ",
    env!("KITSU_BUILD_DATE"),
    ")"
);

#[derive(Debug, Parser)]
#[command(
    name = "kitsu",
    version,
    long_version = LONG_VERSION,
    about = "kitsu — multi-gateway model CLI",
    after_help = "GET STARTED:\n  kitsu init\n  kitsu --config ./kitsu.toml doctor\n  kitsu --config ./kitsu.toml chat\n\nDISCOVER:\n  kitsu chat --help       Chat, permissions, and continuity\n  kitsu review --help     Verified one-shot code review\n  kitsu models --help     Model discovery\n  kitsu sessions --help   Provider-free session management\n  kitsu artifacts --help  Host-only command-artifact storage\n\nConfiguration lookup: --config, then $KITSU_CONFIG, then ~/.kitsu/config.toml"
)]
pub struct Cli {
    /// Gateway profile to use (from [gateways.<name>] in the config).
    #[arg(long, global = true)]
    pub gateway: Option<String>,

    /// Override the model id (takes precedence over the gateway's default).
    #[arg(long, global = true)]
    pub model: Option<String>,

    /// Path to the config file. Falls back to $KITSU_CONFIG, then
    /// ~/.kitsu/config.toml.
    #[arg(long, global = true)]
    pub config: Option<String>,

    /// Disable streaming (request a single non-streamed completion).
    #[arg(long, global = true)]
    pub no_stream: bool,

    /// Emit versioned JSON Lines automation events on stdout. Human diagnostics
    /// may still appear on stderr; interactive TTY chat is not supported in
    /// this mode.
    #[arg(long, global = true)]
    pub jsonl: bool,

    /// Use stable ASCII terminal markers and disable color/animation. The
    /// KITSU_PLAIN and NO_COLOR environment variables remain supported.
    #[arg(long, global = true)]
    pub plain: bool,

    /// Keep styled output but replace animated waiting indicators with a
    /// visible static status. KITSU_REDUCED_MOTION remains supported.
    #[arg(long, global = true)]
    pub reduced_motion: bool,

    /// Sampling temperature. Omitted from the request unless set — some models
    /// (e.g. newer Anthropic models on Bedrock) reject a `temperature` field.
    #[arg(long, global = true)]
    pub temperature: Option<f32>,

    /// Reasoning effort for compatible models/gateways. Omitted by default so
    /// non-reasoning endpoints receive no extra request field.
    #[arg(
        long,
        global = true,
        value_name = "LEVEL",
        value_parser = ["none", "minimal", "low", "medium", "high", "xhigh"]
    )]
    pub reasoning_effort: Option<String>,

    /// Maximum agent tool-call rounds per turn before the loop stops. Tool-aware
    /// reviews of large repos need many rounds to read files THEN write the
    /// answer; the default 10 truncates them ("[stopped: max tool rounds
    /// reached]"). Raise it (e.g. 40) for tool-heavy work.
    #[arg(
        long,
        global = true,
        default_value_t = 10,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub max_rounds: u32,

    /// Total inference deadline in seconds, including retries, retry backoff,
    /// and response-body streaming. A heavy tool-aware round on a slower model
    /// (local Ollama/vLLM in particular) routinely exceeds even this default
    /// and fails the turn; raise it further (e.g. 300) for tool-heavy review
    /// work.
    #[arg(
        long,
        global = true,
        default_value_t = 120,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub timeout_secs: u64,

    /// How long a non-interactive exit (scripted or piped chat) waits for live
    /// background agents to finish before cancelling them. Scripted runs join
    /// detached work instead of discarding it; this is the bound on that wait.
    /// Defaults to `--timeout-secs`.
    #[arg(
        long,
        global = true,
        value_name = "SECONDS",
        value_parser = clap::value_parser!(u64).range(0..)
    )]
    pub agent_join_timeout: Option<u64>,

    /// Max output tokens per response. Unset lets the gateway apply its own cap
    /// (often 4096), which silently truncates long outputs like code reviews.
    /// Raise it (e.g. 16000) for long-form generation.
    #[arg(long, global = true)]
    pub max_output_tokens: Option<u32>,

    /// Never prompt for filesystem access. Enabled filesystem tools remain
    /// confined to explicit/current read and write roots; requests outside
    /// those roots are denied. This never registers a tool or approves a
    /// developer command.
    #[arg(
        long,
        global = true,
        conflicts_with = "dangerously_allow_all_filesystem"
    )]
    pub yes: bool,

    /// DANGER: let enabled filesystem tools access every non-protected path
    /// without prompting. This is a global host-filesystem bypass, not root
    /// confinement. It does not register tools or approve developer commands.
    #[arg(
        long = "dangerously-allow-all-filesystem",
        global = true,
        conflicts_with = "yes"
    )]
    pub dangerously_allow_all_filesystem: bool,

    /// The model's context window in tokens. Overrides the gateway profile's
    /// `context_window` and any value discovered from `/v1/models`. Set this for
    /// small local models (e.g. Ollama's default 4096) so kitsu budgets context
    /// against the real window instead of hard-failing.
    #[arg(long, global = true, value_name = "TOKENS")]
    pub context_window: Option<u64>,

    /// How to reclaim context when trimming tool results isn't enough: `trim`
    /// drops the oldest exchanges; `summarize` replaces them with an LLM-written
    /// summary (opt-in — quality depends on the model; falls back to trim on
    /// failure).
    #[arg(long, global = true, default_value = "trim", value_parser = ["trim", "summarize"])]
    pub compaction: String,

    /// Do not discover or send llms.txt/AGENTS.md/CLAUDE.md project context for
    /// this run. Useful for privacy-sensitive or isolated automation.
    #[arg(long, global = true)]
    pub no_project_context: bool,

    /// Do not register the model-callable `workspace_diff` tool for this run.
    /// The tool discloses bounded host-verified diff evidence (like /review)
    /// and never grants filesystem access; with this flag the model is not
    /// told the tool exists. The config key `workspace_evidence = false`
    /// disables it permanently.
    #[arg(long, global = true)]
    pub no_workspace_evidence: bool,

    /// Per-turn cap on subagent spawns (`spawn_agent`, `spawn_agents`,
    /// `spawn_agent_bg` combined). Spawns beyond the cap return a tool error
    /// the model sees, stopping agent storms from models that ignore prompt
    /// guidance. Overrides the config key `agent_spawn_cap`. Defaults: 8 in
    /// interactive chat, 2 in scripted/piped automation. 0 disables spawning.
    #[arg(long, global = true, value_name = "N")]
    pub agent_spawn_cap: Option<u32>,

    /// Preload a named skill's instructions for this run (repeatable). Works for
    /// `ask` and `chat` — the skill body is injected up front so it's available
    /// immediately (and to spawned agents), not just discovered on demand. See
    /// `/skills` for available names.
    #[arg(long = "skill", value_name = "NAME", global = true)]
    pub skill: Vec<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a no-secret starter configuration without overwriting a file.
    Init {
        /// Destination. Defaults to --config when supplied, otherwise
        /// ./kitsu.toml.
        #[arg(long, value_name = "PATH")]
        path: Option<std::path::PathBuf>,

        /// Emit one JSON result instead of human text.
        #[arg(long)]
        json: bool,
    },
    /// Run read-only configuration, auth, cache, and runtime diagnostics.
    Doctor {
        /// Also contact and validate the configured model-catalog endpoint.
        /// This verifies catalog reachability/model presence, not a billable
        /// chat completion; offline checks are the default.
        #[arg(long)]
        online: bool,

        /// Emit one JSON report instead of human text.
        #[arg(long)]
        json: bool,
    },
    /// Ask a single question and stream the answer (one user turn).
    Ask {
        /// The prompt to send.
        prompt: String,
    },
    /// Review host-verified repository evidence in a tool-free model turn.
    #[command(
        after_help = "EXAMPLES:\n  kitsu review\n  kitsu review --staged\n  kitsu review --base main crates/kitsu\n  kitsu --jsonl review --commit HEAD --instructions 'Focus on API compatibility'\n\nExactly one scope flag may be used. Private Kitsu state is excluded, revision arguments are validated, and the model receives no tools during the review turn."
    )]
    Review {
        /// Optional literal repository-relative path filter.
        path: Option<String>,

        /// Review only staged changes.
        #[arg(long, conflicts_with_all = ["unstaged", "base", "commit"])]
        staged: bool,

        /// Review unstaged changes plus untracked text files.
        #[arg(long, conflicts_with_all = ["staged", "base", "commit"])]
        unstaged: bool,

        /// Review changes from the merge base with this revision.
        #[arg(long, value_name = "REV", conflicts_with_all = ["staged", "unstaged", "commit"])]
        base: Option<String>,

        /// Review one commit's verified diff.
        #[arg(long, value_name = "REV", conflicts_with_all = ["staged", "unstaged", "base"])]
        commit: Option<String>,

        /// Append user-owned focus instructions after the verified evidence.
        #[arg(long, value_name = "TEXT")]
        instructions: Option<String>,

        /// Resume and autosave a saved review conversation by id or `last`.
        #[arg(long, value_name = "ID", default_missing_value = "last", num_args = 0..=1)]
        resume: Option<String>,

        /// Persist this one-shot review as a new saved session.
        #[arg(long, conflicts_with = "resume")]
        session: bool,
    },
    /// List models advertised by the selected gateway.
    #[command(
        after_help = "EXAMPLES:\n  kitsu models\n  kitsu models qwen\n  kitsu --gateway huggingface models coder\n\nThe optional filter is a case-insensitive substring. Model IDs printed here can be passed to --model or /model."
    )]
    Models {
        /// Optional case-insensitive substring filter.
        filter: Option<String>,
    },
    /// Manage saved conversations without contacting a provider.
    #[command(
        after_help = "EXAMPLES:\n  kitsu sessions\n  kitsu sessions --json\n  kitsu sessions show <id>\n  kitsu sessions resume last\n  kitsu sessions export <id> chat.md\n\nThe config path selects the isolated session namespace; the config file does not need to parse or even exist for list/show operations."
    )]
    Sessions {
        /// Emit one JSON result instead of human text.
        #[arg(long, global = true)]
        json: bool,

        #[command(subcommand)]
        command: Option<SessionCommand>,
    },
    /// Inspect or reclaim command-artifact storage left by earlier runs.
    #[command(
        after_help = "EXAMPLES:\n  kitsu artifacts\n  kitsu artifacts list --json\n  kitsu artifacts prune all --dry-run\n  kitsu artifacts prune all --older-than 7d\n\nHost-only: these paths are Kitsu-private, no model authority is involved, and the config path only selects the isolated namespace. Generations held by a live process are never removed."
    )]
    Artifacts {
        /// Emit one JSON result instead of human text.
        #[arg(long, global = true)]
        json: bool,

        #[command(subcommand)]
        command: Option<ArtifactCommand>,
    },
    /// Interactive chat, scripted multi-turn work, or one piped turn.
    #[command(
        after_help = "CONTINUITY:\n  Interactive chat autosaves by default.\n  kitsu chat --session --script turns.txt     Autosave a new scripted session\n  kitsu chat --resume last --script more.txt  Resume and autosave explicitly\n  echo 'next' | kitsu chat --resume <id>      Resume from piped input\n\nFILESYSTEM CAPABILITIES:\n  --allow-path is read-only, --allow-write is write-only, and --allow-read-write is explicit combined authority. --yes suppresses prompts but preserves roots."
    )]
    Chat {
        /// Optional first prompt; queued as the first turn. In non-TTY mode
        /// (piped stdin) this (or stdin) is the single turn.
        prompt: Option<String>,

        /// Pre-grant filesystem reads under this path (repeatable). Interactive
        /// chat also offers filesystem tools and asks before any unlisted path;
        /// scripted/piped chat is deny-by-default unless paths are pre-granted.
        #[arg(long = "allow-path", value_name = "PATH")]
        allow_paths: Vec<String>,

        /// Pre-grant model writes under this path (repeatable). Interactive chat
        /// can ask for unlisted writes; scripted/piped chat cannot prompt.
        #[arg(long = "allow-write", value_name = "PATH")]
        allow_write: Vec<String>,

        /// Pre-grant both model reads and writes under this path (repeatable).
        /// Use this explicit combined capability for read-modify-write work;
        /// --allow-write alone remains write-only.
        #[arg(long = "allow-read-write", value_name = "PATH")]
        allow_read_write: Vec<String>,

        /// Explicitly enable the guarded developer-command tool. Commands have
        /// an independent approval policy and are never authorized by --yes or
        /// filesystem grants.
        #[arg(long)]
        enable_commands: bool,

        /// Constrain command working directories to this existing directory
        /// (repeatable). These become writable mounts inside mandatory Linux
        /// OS containment; exact argv/cwd approval remains a separate gate.
        /// Implies --enable-commands. With no explicit root, --enable-commands
        /// uses the launch directory. If the approved executable itself lives
        /// outside every command root, containment denies it — pass its own
        /// directory too (the denial names the exact directory to add).
        #[arg(long = "command-root", value_name = "PATH")]
        command_roots: Vec<String>,

        /// Mount one extra directory read-only inside command containment
        /// (repeatable), for toolchains that live outside /usr — e.g.
        /// `--command-runtime-root ~/.cargo --command-runtime-root ~/.rustup`
        /// makes `cargo test` resolvable in the sandbox (each root's `bin/`
        /// joins PATH; `.cargo`/`.rustup` roots also set CARGO_HOME /
        /// RUSTUP_HOME). Read-only: commands can execute the toolchain but
        /// never mutate it. Never pass a credential or whole-home directory —
        /// every configured root is disclosed at startup. Implies
        /// --enable-commands.
        #[arg(long = "command-runtime-root", value_name = "PATH")]
        command_runtime_roots: Vec<String>,

        /// Opt-in, explicit pass-through of one environment variable into
        /// contained commands (repeatable), layered on top of the fixed
        /// non-secret base set every contained command already receives
        /// (PATH, HOME, LANG, LC_ALL, TERM, TMPDIR, USER, LOGNAME) — needed
        /// for interpreter-based toolchains (pytest, npm test, ...) whose
        /// module search path is env-driven (e.g. PYTHONPATH, NODE_PATH).
        /// Bare `NAME` reads the value from kitsu's own environment and is
        /// refused for names that look secret-bearing (case-insensitive
        /// substring match on KEY/TOKEN/SECRET/PASSWORD); use `NAME=value`
        /// to pass an explicit literal value regardless of name. Nothing
        /// else is inherited from the host — deny-by-default is unchanged.
        #[arg(long = "command-env", value_name = "NAME[=VALUE]")]
        command_env: Vec<String>,

        /// Pre-approve one exact argv, encoded either as a JSON string array
        /// run in the launch directory, for example '["cargo","test"]', or as
        /// '{"argv":["cargo","test"],"cwd":"/abs/dir"}' to bind it to an
        /// explicit working directory inside a --command-root. Repeatable and
        /// intended for non-interactive scripts.
        #[arg(long = "allow-command", value_name = "JSON_ARGV")]
        allow_commands: Vec<String>,

        /// Run turns from a FILE non-interactively (one turn per line; blank
        /// lines and `#` comments are skipped; lines starting with `/` are slash
        /// commands). Threads multi-turn history and prints a transcript. Works
        /// with or without a TTY — used for scripted/replayable sessions.
        #[arg(long, value_name = "FILE")]
        script: Option<String>,

        /// Resume and autosave a saved session by id, or use `last`. Works in
        /// interactive, scripted, and piped chat. Saved permission grants are
        /// intentionally never restored.
        #[arg(long, value_name = "ID", default_missing_value = "last", num_args = 0..=1)]
        resume: Option<String>,

        /// Persist a new scripted or piped chat and autosave successful turns.
        /// Interactive chat already persists by default; --resume implies this,
        /// so passing both is a harmless no-op confirmation rather than an
        /// error.
        #[arg(long, conflicts_with = "no_session")]
        session: bool,

        /// Disable automatic local session snapshots for this chat.
        #[arg(long, conflicts_with = "resume")]
        no_session: bool,

        /// Preserve the legacy script behavior of exiting 0 even when one or
        /// more scripted turns or slash commands fail.
        #[arg(long)]
        allow_turn_errors: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    /// List retained generations across runs (the default when no action is given).
    List,
    /// Remove exact generations. Live and still-owned generations are skipped.
    Prune {
        /// Exact generation id, or `all`.
        target: String,
        /// Only remove generations at least this old (for example `7d`, `12h`).
        #[arg(long, value_name = "DURATION")]
        older_than: Option<String>,
        /// Report what would be removed without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// List saved sessions (the default when no action is given). Optional filter is a case-insensitive substring matched against session id and preview text.
    List {
        /// Optional case-insensitive substring filter over session id and preview text.
        filter: Option<String>,
    },
    /// Show one saved session and its bounded transcript.
    Show {
        /// Exact session id, or `last`.
        id: String,
    },
    /// Resume in interactive chat, or print an exact command outside a TTY.
    Resume {
        /// Session id; defaults to `last`.
        #[arg(default_value = "last")]
        id: String,
    },
    /// Clone a saved session under a fresh id without carrying permissions.
    Fork {
        /// Session id; defaults to `last`.
        #[arg(default_value = "last")]
        id: String,
    },
    /// Export a saved session without overwriting the destination.
    Export {
        /// Exact session id, or `last`.
        id: String,
        /// New output file; an artifact sidecar may be created beside it.
        destination: std::path::PathBuf,
        /// Human-readable Markdown or the complete JSON snapshot. When omitted,
        /// a `.json` destination selects JSON and anything else Markdown.
        #[arg(long, value_parser = ["markdown", "json"])]
        format: Option<String>,
    },
    /// Move an exact inactive session into recoverable archive storage.
    Archive { id: String },
    /// Restore an exact archived session id.
    Restore { id: String },
    /// Permanently delete an exact inactive live session id.
    Delete { id: String },
}
