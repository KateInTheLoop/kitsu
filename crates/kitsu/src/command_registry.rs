//! Slash-command metadata and help.

use crate::presentation::terminal_inline;

/// `/help` section a command belongs to. Grouping is presentation only: the
/// registry below stays the single source for completion and JSONL payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandGroup {
    Session,
    ContextHistory,
    FilesDiffsCheckpoints,
    AgentsJobs,
    Permissions,
    ModelsReasoning,
}

impl CommandGroup {
    pub(crate) const ORDER: [CommandGroup; 6] = [
        CommandGroup::Session,
        CommandGroup::ContextHistory,
        CommandGroup::FilesDiffsCheckpoints,
        CommandGroup::AgentsJobs,
        CommandGroup::Permissions,
        CommandGroup::ModelsReasoning,
    ];

    pub(crate) fn heading(self) -> &'static str {
        match self {
            CommandGroup::Session => "session",
            CommandGroup::ContextHistory => "context & history",
            CommandGroup::FilesDiffsCheckpoints => "files, diffs & checkpoints",
            CommandGroup::AgentsJobs => "agents & jobs",
            CommandGroup::Permissions => "permissions",
            CommandGroup::ModelsReasoning => "models & reasoning",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SlashCommandSpec {
    pub(crate) name: &'static str,
    pub(crate) args: &'static str,
    pub(crate) description: &'static str,
    /// Longer explanation for `/help <command>`: behavior, constraints, and
    /// an example where one helps. Empty falls back to `description`.
    pub(crate) details: &'static str,
    pub(crate) group: CommandGroup,
    /// A backward-compatible alias folds into its parent's `/help` row while
    /// remaining a first-class registry entry for completion and dispatch.
    pub(crate) alias_of: Option<&'static str>,
}

impl SlashCommandSpec {
    pub(crate) fn syntax(self) -> String {
        if self.args.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.args)
        }
    }
}

/// One source of truth for interactive completion and `/help`. Keep aliases in
/// the registry too so discoverability never depends on reading documentation.
pub(crate) const SLASH_COMMANDS: &[SlashCommandSpec] = &[
    SlashCommandSpec {
        name: "agent",
        args: "[list|view|show <id>|explain <id>|steer <id> <text>|close-steering <id>|stop <id>|stop-all|prune <id|all>|close <id>|reply <id> <y|a|n>]",
        description: "inspect and control one background agent thread",
        details: "Per-thread control of one background agent: `show` streams its bounded transcript, `steer` injects a correction, `stop` cancels, `close` acknowledges a finished thread. Ids come from /jobs.",
        group: CommandGroup::AgentsJobs,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "agents",
        args: "[view|<id>|any /jobs subcommand]",
        description: "backward-compatible alias for /jobs",
        details: "",
        group: CommandGroup::AgentsJobs,
        alias_of: Some("jobs"),
    },
    SlashCommandSpec {
        name: "allow",
        args: "<path>",
        description: "grant read access for this session",
        details: "Grants READ access to a path for this session only. Runtime grants are dropped on /new, /resume, and /fork; invocation-flag roots persist. Example: /allow ../other-crate",
        group: CommandGroup::Permissions,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "allow-read-write",
        args: "<path>",
        description: "grant read and write access for this session",
        details: "Grants read AND write access to a path for this session. Writes are journaled into checkpoints (/checkpoints, /undo).",
        group: CommandGroup::Permissions,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "allow-write",
        args: "<path>",
        description: "grant write access for this session",
        details: "Grants WRITE-ONLY access — the model can create/replace files there but cannot read them back; use /allow-read-write when edits need readback.",
        group: CommandGroup::Permissions,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "archive",
        args: "<id>",
        description: "archive an inactive saved conversation",
        details: "Moves an inactive saved session into recoverable archive storage. Restore later with /restore <id>; archived sessions leave /sessions listings.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "artifacts",
        args: "[list|prune <id|all>]",
        description: "inspect or prune retained command-output artifacts",
        details: "",
        group: CommandGroup::FilesDiffsCheckpoints,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "auto",
        args: "[on|off]",
        description: "show or switch command auto mode for this session",
        details: "Auto mode runs every later command without a permission prompt; containment, command roots and argv checks still apply. Answer [u] at a command prompt or use /auto on to start it, /auto off to stop. File access outside the granted roots still asks.",
        group: CommandGroup::Permissions,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "checkpoint",
        args: "<id>",
        description: "preview one recoverable verified file checkpoint",
        details: "Previews one write checkpoint: its files, sizes, and a bounded verified diff. Restore it with /undo <id>.",
        group: CommandGroup::FilesDiffsCheckpoints,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "checkpoints",
        args: "",
        description: "list recoverable verified file checkpoints",
        details: "Lists write checkpoints (grouped successful model writes) with ids — each is restorable via /undo <id> while its files still match.",
        group: CommandGroup::FilesDiffsCheckpoints,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "clear",
        args: "",
        description: "save current history and clear into a fresh session",
        details: "",
        group: CommandGroup::ContextHistory,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "compact",
        args: "",
        description: "compact older conversation context now",
        details: "Reclaims context: trim drops the oldest exchanges; summarize replaces them with an LLM-written summary (falls back to trim on failure). /context shows the savings ledger afterwards.",
        group: CommandGroup::ContextHistory,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "context",
        args: "[window|sources|refresh]",
        description: "show context details/window/sources or refresh discovered metadata",
        details: "Bare `/context` preserves the detailed legacy view. `window` shows focused typed occupancy. `sources` shows coarse Kitsu-owned request contributors with explicit estimates and unavailable sub-buckets; it does not reverse-parse attachments, on-demand skill bodies, memory, or response-schema tokens. `refresh` re-discovers window metadata from the gateway.",
        group: CommandGroup::ContextHistory,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "cost",
        args: "",
        description: "compatibility alias for current-session usage",
        details: "",
        group: CommandGroup::ContextHistory,
        alias_of: Some("usage"),
    },
    SlashCommandSpec {
        name: "delete",
        args: "<id>",
        description: "permanently delete an inactive live session",
        details: "",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "detach",
        args: "",
        description: "return the composer from an attached background agent",
        details: "Leaves the attachment an agent view `Enter` (or the chord `Ctrl+X d`, or left-arrow on an empty composer) created, so the next line you type goes to the model again. It stops nothing: the agent keeps running, and stopping one is `x` in the agent view or `/jobs stop <id>`.",
        group: CommandGroup::AgentsJobs,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "diff",
        args: "[scope] [path]",
        description: "show a verified working/staged/unstaged/base/commit/checkpoint diff",
        details: "Host-owned bounded diff evidence from Git and the write journal: working/staged/unstaged/base/commit/checkpoint scopes. Renders to the terminal only — nothing is sent to the provider (that is /review's job) and no filesystem grant is created or needed.",
        group: CommandGroup::FilesDiffsCheckpoints,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "doctor",
        args: "[online]",
        description: "read-only config/auth/context/state diagnostics without leaving chat",
        details: "Runs the same diagnostics as `kitsu doctor` in-session: config validity, gateway/auth status (credential PRESENCE only, never values), context-window provenance and cache, runtime state, project context, and tool policy. `online` adds the opt-in network probes (endpoint reachability, model catalog). Always read-only.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "exit",
        args: "[wait|cancel]",
        description: "exit chat with an explicit background-job policy",
        details: "",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "export",
        args: "<file> [markdown|json]",
        description: "export the active session without overwriting",
        details: "Writes the session transcript to a new file (never overwrites): markdown for humans, JSON for automation; artifact sidecars may be created next to it.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "fork",
        args: "[id]",
        description: "clone and switch to a fresh session",
        details: "Clones a saved session under a fresh id and switches to it. Permission grants are deliberately NOT carried — a fork starts with invocation roots only.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "help",
        args: "[command|keys]",
        description: "show command help",
        details: "Grouped command list. `/help <command>` shows that command's syntax, explanation, and aliases, and `/help keys` prints the agent view's bindings.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "jobs",
        args: "[list|view|show <id>|explain <id>|steer <id> <text>|stop <id>|stop-all|prune <id|all>|close-steering <id>|close <id>|reply <id> <y|a|n>]",
        description: "inspect, steer, stop, prune, and close background agent threads",
        details: "The agent deck: every background agent thread with state glyphs (filled = running, hollow = spent, half = result awaiting pickup), token/tool telemetry, and ids for /agent.",
        group: CommandGroup::AgentsJobs,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "loop",
        args: "<count> <prompt>",
        description: "queue a prompt to run <count> times in sequence (2-20)",
        details: "Queues <count> copies (2-20) of a plain prompt onto the bounded prompt queue; they run in sequence after the current turn. Inspect with /queue, abort with /queue clear. Interactive chat only — scripted runs repeat lines natively. Example: /loop 3 run the tests and fix one failure",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "model",
        args: "<name>",
        description: "switch model while keeping history",
        details: "Shows or switches the model. Switching re-discovers the context window (unless pinned by --context-window) and applies to subagents spawned afterwards.",
        group: CommandGroup::ModelsReasoning,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "models",
        args: "[filter]",
        description: "list gateway models",
        details: "",
        group: CommandGroup::ModelsReasoning,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "new",
        args: "",
        description: "start and autosave a fresh conversation",
        details: "Starts a fresh session: clean history, runtime grants dropped, invocation roots restored, background registry reset. The old session stays resumable if it was durable.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "permissions",
        args: "[protect <path>]",
        description: "show access policy; protect paths for the session",
        details: "Full authority disclosure: read/write roots split by invocation vs runtime, protected kitsu-private paths, exact command grants (numbered, revocable), containment status, and the workspace-evidence tool's standing disclosure. `/permissions protect <path>` marks a path Kitsu-private (read+write) for the rest of the session — a hard guard that survives every bypass, including --dangerously-skip-permissions, and cannot be revoked in-session.",
        group: CommandGroup::Permissions,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "profiles",
        args: "",
        description: "show available specialist background-agent profiles",
        details: "",
        group: CommandGroup::AgentsJobs,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "queue",
        args: "[list|add|remove|edit|move|promote|clear]",
        description: "inspect or edit the bounded prompt queue",
        details: "Bounded 8-slot FIFO of upcoming prompts: list/add/remove/edit/move/promote/clear. On Unix, complete lines typed during a running turn are collected here automatically.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "quit",
        args: "[wait|cancel]",
        description: "exit, wait for jobs, or cancel and join them",
        details: "",
        group: CommandGroup::Session,
        alias_of: Some("exit"),
    },
    SlashCommandSpec {
        name: "reasoning",
        args: "[default|none|minimal|low|medium|high|xhigh]",
        description: "show or change reasoning effort for subsequent turns",
        details: "Sets the reasoning-effort hint (none..xhigh) sent to compatible gateways; `default` omits the field entirely so strict endpoints never see it. Applies to later turns and spawned subagents.",
        group: CommandGroup::ModelsReasoning,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "restore",
        args: "<id>",
        description: "restore an archived session",
        details: "",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "resume",
        args: "<id|last>",
        description: "resume a saved conversation",
        details: "Bare /resume on a TTY opens a numbered picker over saved sessions (Enter = last, q = cancel). With an argument: id or NAME; `last` picks the newest anywhere, `auto` the newest recorded in this directory. Saved permission grants are never restored; the saved gateway and working directory must match.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "retry",
        args: "",
        description: "retry the last failed or cancelled prompt",
        details: "Resubmits the last failed or cancelled prompt exactly as typed.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "review",
        args: "[scope] [path]",
        description: "review a verified working/staged/unstaged/base/commit/checkpoint diff",
        details: "Sends host-verified diff evidence (same scopes as /diff) to the model in a tool-free turn and prints its review. One bounded disclosure, clearly announced; no filesystem grant involved.",
        group: CommandGroup::FilesDiffsCheckpoints,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "revoke-command",
        args: "<number>",
        description: "revoke a numbered exact command grant",
        details: "",
        group: CommandGroup::Permissions,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "revoke-read",
        args: "<exact-root>",
        description: "revoke an exact read root for this session",
        details: "",
        group: CommandGroup::Permissions,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "revoke-write",
        args: "<exact-root>",
        description: "revoke an exact write root for this session",
        details: "",
        group: CommandGroup::Permissions,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "save",
        args: "<file>",
        description: "save the latest response",
        details: "",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "sessions",
        args: "[filter]",
        description: "list saved conversations by id, name, or preview",
        details: "An optional case-insensitive filter matches the session id, name, or transcript preview.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "skill",
        args: "<name>",
        description: "load a skill into context",
        details: "Loads one named skill's instructions into context (granting no filesystem access); its body then guides the model. Subagents inherit loaded skills.",
        group: CommandGroup::ContextHistory,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "skills",
        args: "[filter]",
        description: "list available skills",
        details: "Lists discovered skills with descriptions and provenance hashes; project skill metadata is untrusted until explicitly loaded.",
        group: CommandGroup::ContextHistory,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "status",
        args: "",
        description: "show the typed current-session telemetry deck",
        details: "Adaptive route, runtime identity, effective authority, context, and observed current-session usage facts, each labeled with truth class and provenance.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "steer",
        args: "<correction>",
        description: "redirect the active response and restart that turn",
        details: "Mid-turn correction: rolls the partial turn back (completed file effects are kept and reported), injects your correction, and restarts the turn ahead of queued prompts.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "theme",
        args: "[name]",
        description: "list color themes or switch the live theme",
        details: "Restyles every styled surface immediately (prompt, banner, receipts, notices); structural glyphs stay put. Bare /theme lists themes with the active one marked. Persist a choice with `theme = \"<name>\"` in the config or --theme. --plain disables styling entirely and wins over any theme.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "tools",
        args: "",
        description: "show enabled tools and effective access",
        details: "Lists the tools advertised to the model this session, with the permission state that gates each.",
        group: CommandGroup::Permissions,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "tui",
        args: "[classic|inline|fullscreen]",
        description: "report or switch the live renderer",
        details: "Switches the renderer in place: `classic` is the line renderer kitsu has always had, `inline` is the pinned bottom viewport, `fullscreen` takes the terminal's alternate screen and repaints the tail of the transcript the inline viewport retained. The conversation is unchanged by a switch — the transcript already on screen stays exactly where it is, and nothing is replayed, except into fullscreen, where the alternate screen starts blank and the retained tail is painted onto it. In fullscreen, `PgUp` and `PgDn` move the frame back and forward by a window, `Ctrl+Home` moves it to the oldest line the transcript ring still retains and `Ctrl+End` brings it back to the newest and resumes following the output; all four move the frame rather than the caret, and only while fullscreen holds the screen. `Ctrl+O` opens the transcript overlay over the transcript the ring retains, from the inline composer and from fullscreen alike: the same four keys scroll it, and `q` or `Esc` closes it and leaves the frame behind it where it was. `Ctrl+X 1..9` attaches to an agent by the position it is displayed at: `Ctrl+X 3` is the third row you can see, whatever its `#id`, so the nine digits reach the first nine rows and anything below them is the agent view and its arrows. It switches agent when one is already attached; `Ctrl+X 0`, and a digit past the last row, do nothing at all, and the line you have typed is kept either way. Inside the view the digits are ordinary characters. Leaving fullscreen gives the screen back untouched. Bare /tui reports the renderer in use. Accepted only between turns; a /tui typed during a turn is refused rather than queued. Persist a choice with `mode = \"inline\"` under `[tui]` in the config or --tui.",
        group: CommandGroup::Session,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "undo",
        args: "[id]",
        description: "undo the latest or selected conflict-free file checkpoint",
        details: "Restores a write checkpoint (default: the latest) after verifying its files still match the recorded state; refuses while background agents run. /checkpoints lists candidates.",
        group: CommandGroup::FilesDiffsCheckpoints,
        alias_of: None,
    },
    SlashCommandSpec {
        name: "usage",
        args: "[session]",
        description: "show observed provider token usage for this session",
        details: "Shows current-process provider usage and exact response coverage. Bare `/usage` means `/usage session`; `/cost` is retained as an empty-args-only compatibility alias for the identical snapshot. Missing usage and cached-token attribution stay unavailable; no prices, credits, rate limits, or account data are inferred.",
        group: CommandGroup::ContextHistory,
        alias_of: None,
    },
];

pub(crate) fn matching_slash_commands(prefix: &str) -> Vec<&'static SlashCommandSpec> {
    SLASH_COMMANDS
        .iter()
        .filter(|spec| spec.name.starts_with(prefix))
        .collect()
}

/// Render `/help` grouped by [`CommandGroup`] instead of one alphabetical
/// wall. Aliases fold into their parent row. Rows carry no color in any mode;
/// `styled` only emboldens the section headers, so the row text is identical
/// under `NO_COLOR`/`--plain` and in the JSONL `command_result` payload
/// (which always receives the unstyled variant).
pub(crate) fn slash_help(model: &str, styled: bool) -> String {
    let model = terminal_inline(model);
    // Cap the first column width to avoid excessive padding when commands have
    // long arguments. Longer commands will have their description on a new line.
    const MAX_SYNTAX_WIDTH: usize = 50;
    let width = std::cmp::min(
        SLASH_COMMANDS
            .iter()
            .filter(|spec| spec.alias_of.is_none())
            .map(|spec| spec.syntax().len())
            .max()
            .unwrap_or(0),
        MAX_SYNTAX_WIDTH,
    );
    let mut out = String::from("commands:\n");
    for group in CommandGroup::ORDER {
        out.push_str(&kitsu_render::section_heading(group.heading(), styled));
        out.push('\n');
        for spec in SLASH_COMMANDS
            .iter()
            .filter(|spec| spec.group == group && spec.alias_of.is_none())
        {
            let syntax = spec.syntax();
            let mut description = spec.description.to_string();
            for alias in SLASH_COMMANDS
                .iter()
                .filter(|alias| alias.alias_of == Some(spec.name))
            {
                description.push_str(&format!(" (alias: /{})", alias.name));
            }
            if syntax.len() <= width {
                out.push_str(&format!("  {syntax:<width$}  {description}\n"));
            } else {
                // Long commands wrap; description goes on the next line indented
                out.push_str(&format!("  {syntax}\n"));
                out.push_str(&format!("    {description}\n"));
            }
        }
    }
    out.push_str(&format!(
        "current model: {model} — /help <command> explains one command in detail"
    ));
    out
}
