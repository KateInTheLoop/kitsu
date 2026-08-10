//! CLI surface (clap derive). MVP: global `--gateway`, `--model`, `--config`,
//! and the `ask` subcommand (docs/architecture.md §6).

use clap::{Parser, Subcommand};

/// Commit + date this binary was built from — `-V` shows the plain semver,
/// `--version` shows this too, so a stale binary vs. the docs/source next to
/// it is self-diagnosing instead of surfacing as an unexplained CLI error.
pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("KITSU_GIT_SHA"),
    ", built ",
    env!("KITSU_BUILD_DATE"),
    ")"
);

#[derive(Debug, Parser)]
#[command(
    name = "kitsu",
    version,
    long_version = LONG_VERSION,
    about = "kitsu — multi-gateway model CLI",
    after_help = "QUICK START:\n  kitsu --config ./kitsu.toml chat\n  kitsu --gateway ollama chat\n  kitsu chat  # after installing ~/.kitsu/config.toml\n\nConfiguration: --config, then $KITSU_CONFIG, then ~/.kitsu/config.toml"
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

    /// Sampling temperature. Omitted from the request unless set — some models
    /// (e.g. newer Anthropic models on Bedrock) reject a `temperature` field.
    #[arg(long, global = true)]
    pub temperature: Option<f32>,

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
    #[arg(long, global = true, default_value_t = 120)]
    pub timeout_secs: u64,

    /// Max output tokens per response. Unset lets the gateway apply its own cap
    /// (often 4096), which silently truncates long outputs like code reviews.
    /// Raise it (e.g. 16000) for long-form generation.
    #[arg(long, global = true)]
    pub max_output_tokens: Option<u32>,

    /// Auto-approve filesystem tool access without prompting (for
    /// non-interactive or scripted runs). This never enables filesystem tools
    /// and never enables or approves developer commands.
    #[arg(long, global = true)]
    pub yes: bool,

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
    /// Ask a single question and stream the answer (one user turn).
    Ask {
        /// The prompt to send.
        prompt: String,
    },
    /// List models advertised by the selected gateway without requiring a
    /// configured or guessed model id.
    Models {
        /// Optional case-insensitive substring filter.
        filter: Option<String>,
    },
    /// Interactive chat REPL with multi-turn history and optional tools.
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

        /// Explicitly enable the guarded developer-command tool. Commands have
        /// an independent approval policy and are never authorized by --yes or
        /// filesystem grants.
        #[arg(long)]
        enable_commands: bool,

        /// Constrain command working directories to this existing directory
        /// (repeatable). Implies --enable-commands. With no explicit root,
        /// --enable-commands uses the launch directory.
        #[arg(long = "command-root", value_name = "PATH")]
        command_roots: Vec<String>,

        /// Pre-approve one exact argv in the launch directory, encoded as a
        /// JSON string array, for example '["cargo","test"]'. Repeatable and
        /// intended for non-interactive scripts.
        #[arg(long = "allow-command", value_name = "JSON_ARGV")]
        allow_commands: Vec<String>,

        /// Run turns from a FILE non-interactively (one turn per line; blank
        /// lines and `#` comments are skipped; lines starting with `/` are slash
        /// commands). Threads multi-turn history and prints a transcript. Works
        /// with or without a TTY — used for scripted/replayable sessions.
        #[arg(long, value_name = "FILE")]
        script: Option<String>,

        /// Resume a saved interactive session by id, or use `last`. Permission
        /// grants are intentionally not restored.
        #[arg(long, value_name = "ID", default_missing_value = "last", num_args = 0..=1)]
        resume: Option<String>,

        /// Disable automatic local session snapshots for this chat.
        #[arg(long)]
        no_session: bool,

        /// Preserve the legacy script behavior of exiting 0 even when one or
        /// more scripted turns or slash commands fail.
        #[arg(long)]
        allow_turn_errors: bool,
    },
}
