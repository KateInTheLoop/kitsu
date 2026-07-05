//! CLI surface (clap derive). MVP: global `--gateway`, `--model`, `--config`,
//! and the `ask` subcommand (docs/architecture.md §6).

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "kitsu", version, about = "kitsu — multi-gateway model CLI")]
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

    /// Sampling temperature. Omitted from the request unless set — some models
    /// (e.g. newer Anthropic models on Bedrock) reject a `temperature` field.
    #[arg(long, global = true)]
    pub temperature: Option<f32>,

    /// Maximum agent tool-call rounds per turn before the loop stops. Tool-aware
    /// reviews of large repos need many rounds to read files THEN write the
    /// answer; the default 10 truncates them ("[stopped: max tool rounds
    /// reached]"). Raise it (e.g. 40) for tool-heavy work.
    #[arg(long, global = true, default_value_t = 10)]
    pub max_rounds: u32,

    /// Per-request HTTP timeout in seconds. A heavy tool-aware round on a slower
    /// model can exceed the default 60s and fail the whole turn; raise it (e.g.
    /// 300) for tool-heavy review work.
    #[arg(long, global = true, default_value_t = 60)]
    pub timeout_secs: u64,

    /// Max output tokens per response. Unset lets the gateway apply its own cap
    /// (often 4096), which silently truncates long outputs like code reviews.
    /// Raise it (e.g. 16000) for long-form generation.
    #[arg(long, global = true)]
    pub max_output_tokens: Option<u32>,

    /// Auto-approve all tool file access without prompting (for non-interactive
    /// or scripted runs). Equivalent to answering "yes" to every permission
    /// prompt — use with care.
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
    /// Interactive chat REPL with multi-turn history and optional tools.
    Chat {
        /// Optional first prompt; queued as the first turn. In non-TTY mode
        /// (piped stdin) this (or stdin) is the single turn.
        prompt: Option<String>,

        /// Allow filesystem tools to read under this path (repeatable). Passing
        /// at least one implicitly enables the read-only fs tool set; without
        /// it, fs tools are not registered or advertised.
        #[arg(long = "allow-path", value_name = "PATH")]
        allow_paths: Vec<String>,

        /// Allow the model to GENERATE/write files under this path (repeatable).
        /// Enables the `fs_write_file` tool, gated by a deny-by-default write
        /// allowlist separate from --allow-path. Without it, no write tool is
        /// registered or advertised.
        #[arg(long = "allow-write", value_name = "PATH")]
        allow_write: Vec<String>,

        /// Run turns from a FILE non-interactively (one turn per line; blank
        /// lines and `#` comments are skipped; lines starting with `/` are slash
        /// commands). Threads multi-turn history and prints a transcript. Works
        /// with or without a TTY — used for scripted/replayable sessions.
        #[arg(long, value_name = "FILE")]
        script: Option<String>,
    },
}
