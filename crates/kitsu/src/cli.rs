//! CLI surface (clap derive). Bare `kitsu` opens chat; global gateway/model
//! selectors and `-p` cover the common launch paths without a subcommand.

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
    after_help = "GET STARTED:\n  kitsu                              Open interactive chat\n  kitsu -p 'Explain this project'    Run one prompt and exit\n  kitsu init                         Create a starter config\n  kitsu doctor                       Check configuration\n\nDISCOVER:\n  kitsu chat --help       Chat, permissions, and continuity\n  kitsu review --help     Verified one-shot code review\n  kitsu models --help     Model discovery\n  kitsu auth --help       Managed credential login/status/logout\n  kitsu sessions --help   Provider-free session management\n  kitsu artifacts --help  Host-only command-artifact storage\n\nSelection precedence: --gateway/--model, then $KITSU_GATEWAY/$KITSU_MODEL, then config defaults.\nConfiguration lookup: --config, then $KITSU_CONFIG, then ~/.kitsu/config.toml"
)]
pub struct Cli {
    /// Gateway profile to use (from [gateways.<name>] in the config). Overrides
    /// KITSU_GATEWAY, which overrides default_gateway.
    #[arg(long, global = true, env = "KITSU_GATEWAY", hide_env_values = true)]
    pub gateway: Option<String>,

    /// Override the model id. Precedence: --model, KITSU_MODEL, then the
    /// selected gateway's configured model; interactive /model wins afterward.
    #[arg(long, global = true, env = "KITSU_MODEL", hide_env_values = true)]
    pub model: Option<String>,

    /// Run one prompt non-interactively and exit (shorthand for `kitsu ask
    /// <PROMPT>`). This top-level mode does not combine with a subcommand.
    #[arg(short = 'p', long = "prompt", global = true, value_name = "PROMPT")]
    pub print_prompt: Option<String>,

    /// Atomically write the last fully committed assistant message to this
    /// host-owned file. Supported by non-interactive ask, review, piped chat,
    /// and scripted chat; normal stdout or JSONL output is unchanged.
    #[arg(
        short = 'o',
        long = "output-last-message",
        global = true,
        value_name = "PATH"
    )]
    pub output_last_message: Option<std::path::PathBuf>,

    /// Constrain and locally validate each non-interactive final assistant
    /// message with this bounded JSON Schema. The root must be a strict object
    /// schema supported by the configured OpenAI-compatible gateway. When
    /// paired with a chat required-write contract, tool rounds are
    /// unconstrained and one tool-free schema finalization round is reserved.
    #[arg(long = "output-schema", global = true, value_name = "PATH")]
    pub output_schema: Option<std::path::PathBuf>,

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

    /// Use stable ASCII terminal markers, disable color/animation, and select
    /// literal line-oriented input. KITSU_PLAIN remains supported. NO_COLOR
    /// selects the same rendering without changing the input editor.
    #[arg(long, global = true)]
    pub plain: bool,

    /// Use escape-free, literal line-oriented input and plain rendering for
    /// assistive technology. KITSU_SCREEN_READER remains supported.
    #[arg(long, global = true)]
    pub screen_reader: bool,

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
    /// response-body streaming, and every provider/tool round in one scripted
    /// turn. A heavy tool-aware turn on a slower model (local Ollama/vLLM in
    /// particular) routinely exceeds even this default and fails; raise it
    /// further (e.g. 300) for tool-heavy review work.
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

    /// DANGER: run fully unattended — never prompt for anything. Implies the
    /// filesystem bypass above, registers read/write tools even in
    /// non-interactive runs, and auto-approves `--command` invocations inside
    /// the command roots. Built for loop automation, not interactive chat.
    /// Hard guards survive: Kitsu config/state stay protected, commands stay
    /// confined to their roots and containment, and `/permissions protect
    /// <path>` can fence off more paths for the session.
    #[arg(
        long = "dangerously-skip-permissions",
        global = true,
        conflicts_with_all = ["yes", "dangerously_allow_all_filesystem"]
    )]
    pub dangerously_skip_permissions: bool,

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

    /// Color theme for styled output (kitsune, sakura, ocean, forest, ember,
    /// mono — see `/theme`). Overrides the config key `theme`; `--plain`
    /// disables styling entirely and wins over any theme.
    #[arg(long, global = true, value_name = "NAME")]
    pub theme: Option<String>,

    /// Disable Kitsu-created automatic command-output and oversized tool-result
    /// artifacts for this run. Commands keep bounded previews and oversized
    /// generic results become explicitly lossy head/tail previews. Existing
    /// artifacts are untouched; sessions, credentials, -o, schemas, required
    /// writes, and other explicit destinations keep their normal behavior.
    #[arg(long, global = true)]
    pub no_artifacts: bool,

    /// Per-turn cap on subagent spawns (`spawn_agent`, `spawn_agents`,
    /// `spawn_agent_bg` combined). Spawns beyond the cap return a tool error
    /// the model sees, stopping agent storms from models that ignore prompt
    /// guidance. Overrides the config key `agent_spawn_cap`. Defaults: 8 in
    /// interactive chat, 2 in scripted/piped automation. 0 disables spawning
    /// and unregisters every multi-agent tool, including `agent_result`.
    #[arg(long, global = true, value_name = "N")]
    pub agent_spawn_cap: Option<u32>,

    /// Preload a named skill's instructions for this run (repeatable). Works for
    /// `ask` and `chat` — the skill body is injected up front so it's available
    /// immediately (and to spawned agents), not just discovered on demand. See
    /// `/skills` for available names.
    #[arg(long = "skill", value_name = "NAME", global = true)]
    pub skill: Vec<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
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
    /// Run offline configuration, auth, cache, and runtime diagnostics.
    Doctor {
        /// Also contact and validate the configured model-catalog endpoint.
        /// This verifies catalog reachability/model presence, not a billable
        /// chat completion; offline checks are the default.
        #[arg(long, conflicts_with = "inference")]
        online: bool,

        /// Send one minimal, tool-free chat completion to verify inference
        /// access. This is an explicit online, potentially billable probe and
        /// cannot be combined with the catalog-only --online check.
        #[arg(long, conflicts_with = "online")]
        inference: bool,

        /// Emit one JSON report instead of human text.
        #[arg(long)]
        json: bool,
    },
    /// Manage one config-scoped gateway credential without contacting it.
    #[command(
        after_help = "EXAMPLES:\n  kitsu --gateway huggingface auth login\n  printf '%s\\n' \"$HF_TOKEN\" | kitsu --gateway huggingface auth login --token-stdin\n  kitsu --gateway huggingface auth status --json\n  kitsu --gateway huggingface auth logout\n\nOnly bearer-env profiles support managed credentials. A non-empty configured environment variable always takes precedence. Login stores an opaque token locally; use `doctor --inference` for an explicit remote validity check. Secrets are never accepted as command-line values."
    )]
    Auth {
        /// Emit one secret-free JSON result instead of human text.
        #[arg(long, global = true)]
        json: bool,

        #[command(subcommand)]
        command: AuthCommand,
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
        after_help = "CONTINUITY:\n  Interactive chat autosaves by default.\n  kitsu chat --session --script turns.txt     Autosave a new scripted session\n  kitsu chat --resume last --script more.txt  Resume and autosave explicitly\n  echo 'next' | kitsu chat --resume <id>      Resume from piped input\n\nTOOL CONTROL:\n  kitsu chat --no-tools 'Draft a concise summary'\n  --no-tools keeps client-side context and attachments but offers no model tools. Use --allow-path for client-side reads in scripted or piped chat.\n\nFILESYSTEM CAPABILITIES:\n  --allow-path is read-only, --allow-write is write-only, and --allow-read-write is explicit combined authority. --yes suppresses prompts but preserves roots."
    )]
    Chat {
        /// Optional first prompt; queued as the first turn. In non-TTY mode
        /// (piped stdin) this (or stdin) is the single turn.
        prompt: Option<String>,

        /// Offer no tools to the model for this chat. Client-side project
        /// context, explicit skill preloads, attachments, and session
        /// continuity remain available.
        #[arg(
            long,
            conflicts_with_all = [
                "allow_write",
                "allow_read_write",
                "enable_commands",
                "command_roots",
                "command_runtime_roots",
                "command_env",
                "allow_commands"
            ]
        )]
        no_tools: bool,

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

        /// Fail the automation run unless this file receives a verified model
        /// filesystem mutation and its final contents differ from launch.
        /// Repeatable; does not grant write authority, so every path must also
        /// be covered by --allow-write or --allow-read-write. An existing target
        /// also needs read authority so the model can replace it safely; use
        /// --allow-read-write (or combine --allow-path with --allow-write).
        #[arg(long = "require-write", value_name = "PATH")]
        require_writes: Vec<String>,

        /// Fail the automation run unless this file receives the verified
        /// model mutation/net change required by --require-write and its exact
        /// final raw bytes have this SHA-256. Repeatable; split on the final
        /// '=' so PATH may contain '='. This grants no authority.
        #[arg(long = "require-write-sha256", value_name = "PATH=SHA256")]
        require_write_sha256: Vec<String>,

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

        /// Resume and autosave a saved session by id, by NAME (see --name /
        /// `kitsu sessions rename`), with `last` (newest anywhere), or with
        /// `auto` (newest recorded in the current directory). Works in
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

        /// Name this durable session (1-40 chars: letters, digits, '-', '_').
        /// Names show in listings and resolve in `--resume <name>`. Implies
        /// --session; also renames when combined with --resume.
        #[arg(long, value_name = "NAME", conflicts_with = "no_session")]
        name: Option<String>,

        /// Preserve the legacy script behavior of exiting 0 even when one or
        /// more scripted turns or slash commands fail.
        #[arg(long)]
        allow_turn_errors: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Read and atomically store a managed bearer credential.
    Login {
        /// Read the credential from stdin for secret-manager pipelines. Without
        /// this flag, login requires a terminal and disables input echo.
        #[arg(long)]
        token_stdin: bool,
    },
    /// Report the effective local credential source without revealing it.
    Status,
    /// Remove only Kitsu's managed credential; environment/config auth remains.
    Logout,
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
        /// Session id or name; also `last` (newest anywhere, the default) or
        /// `auto` (newest recorded in the current directory).
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
    /// Name or rename a saved session (names resolve in `--resume <name>`).
    Rename {
        /// Session id or current name; `last` picks the newest session.
        id: String,
        /// New name: 1-40 chars of letters, digits, '-' or '_'. Omit to clear.
        name: Option<String>,
    },
    /// Move an exact inactive session into recoverable archive storage.
    Archive { id: String },
    /// Restore an exact archived session id.
    Restore { id: String },
    /// Permanently delete an exact inactive live session id.
    Delete { id: String },
}
