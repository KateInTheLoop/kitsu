//! System-prompt composition.

use crate::command_policy::display_pregrant_argv;
use crate::command_policy::PregrantedCommand;
use crate::presentation::shorten_home;

// --- chat REPL --------------------------------------------------------------

/// Situational grounding injected into the system prompt: the launch working
/// directory plus every granted filesystem root with its authority, so a
/// prompt like "explain this codebase" resolves against the grants instead of
/// making the model ask the user for paths.
#[derive(Debug, Clone, Default)]
pub(crate) struct PromptEnvironment {
    /// Launch working directory (already display-formatted).
    pub(crate) cwd: String,
    /// Granted roots as `(path, authority)` where authority is one of
    /// "read-only", "write-only", or "read-write".
    pub(crate) roots: Vec<(String, &'static str)>,
    /// Exact `--allow-command` pregrants as `(display argv, cwd)`, so the
    /// model can choose to call one directly instead of guessing an
    /// invocation and risking a denied round-trip (cycle-8 audit gap 3).
    /// Capped at [`PROMPT_PREGRANT_CAP`] entries; overflow is summarized
    /// rather than silently dropped.
    pub(crate) command_pregrants: Vec<(String, String)>,
    /// Count of pregrants omitted from `command_pregrants` because the cap
    /// was reached.
    pub(crate) command_pregrants_overflow: usize,
    /// Ask mode: a read or write outside the granted roots prompts the user
    /// instead of failing, so the model should attempt it rather than tell
    /// the user to restart with grant flags.
    pub(crate) access_prompts: bool,
}

/// Maximum number of exact command pregrants named verbatim in the system
/// prompt. Bounded so a script with dozens of `--allow-command` flags
/// doesn't blow the context budget on grant listings alone; the overflow
/// count is still disclosed so the model knows more exist (and can be
/// discovered via `/permissions` provenance).
pub(crate) const PROMPT_PREGRANT_CAP: usize = 20;

impl PromptEnvironment {
    /// Classify the CLI grant lists into deduplicated `(path, authority)`
    /// pairs. Read-write wins over the one-sided lists when a path appears in
    /// both.
    pub(crate) fn from_grants(
        cwd: &std::path::Path,
        read_only: &[String],
        write_only: &[String],
        read_write: &[String],
    ) -> Self {
        let mut roots: Vec<(String, &'static str)> = Vec::new();
        let mut push = |path: &String, authority: &'static str| {
            if !roots.iter().any(|(existing, _)| existing == path) {
                roots.push((path.clone(), authority));
            }
        };
        for path in read_write {
            push(path, "read-write");
        }
        for path in read_only {
            push(path, "read-only");
        }
        for path in write_only {
            push(path, "write-only");
        }
        Self {
            cwd: cwd.display().to_string(),
            roots,
            command_pregrants: Vec::new(),
            command_pregrants_overflow: 0,
            access_prompts: false,
        }
    }

    /// Attach the exact `--allow-command` pregrants for disclosure in the
    /// system prompt. Order is preserved; entries beyond
    /// [`PROMPT_PREGRANT_CAP`] are counted but not listed verbatim.
    pub(crate) fn with_command_pregrants(mut self, pregrants: &[PregrantedCommand]) -> Self {
        let overflow = pregrants.len().saturating_sub(PROMPT_PREGRANT_CAP);
        self.command_pregrants = pregrants
            .iter()
            .take(PROMPT_PREGRANT_CAP)
            .map(|grant| (display_pregrant_argv(&grant.argv), shorten_home(&grant.cwd)))
            .collect();
        self.command_pregrants_overflow = overflow;
        self
    }
}

/// System prompt seeding every chat session.
pub(crate) fn chat_system_prompt(tool_names: &[String], environment: &PromptEnvironment) -> String {
    if tool_names.is_empty() {
        "You are kitsu, a concise, helpful CLI assistant.".to_string()
    } else {
        let has_read = tool_names.iter().any(|name| name == "fs_read_file");
        let has_write = tool_names.iter().any(|name| name == "fs_write_file");
        let has_edit = tool_names.iter().any(|name| name == "fs_edit_file");
        let has_commands = tool_names.iter().any(|name| name == "command_exec");
        let has_git = tool_names.iter().any(|name| name == "git");
        let has_spawn = tool_names.iter().any(|name| name == "spawn_agent_bg");
        let has_workspace_diff = tool_names.iter().any(|name| name == "workspace_diff");
        let mut prompt = format!(
            "You are kitsu, a concise, helpful CLI assistant with these tools: {}. ",
            tool_names.join(", ")
        );
        if !environment.cwd.is_empty() {
            prompt.push_str(&format!("Current working directory: {}. ", environment.cwd));
        }
        if !environment.roots.is_empty() {
            let listed = environment
                .roots
                .iter()
                .map(|(path, authority)| format!("{path} ({authority})"))
                .collect::<Vec<_>>()
                .join(", ");
            prompt.push_str(&format!("Granted filesystem roots: {listed}. "));
        }
        if has_read {
            prompt.push_str(
                "When the user refers to a local file or directory, use the available filesystem \
                 tools to inspect it directly instead of asking them to paste its contents. ",
            );
            if !environment.roots.is_empty() {
                prompt.push_str(
                    "When the user says \"this codebase\", \"this project\", or uses relative \
                     paths, explore the granted roots with your tools (list, then read) instead \
                     of asking the user for paths. ",
                );
            }
        }
        if has_edit {
            prompt.push_str(
                "For code or file changes, use fs_edit_file for exact localized edits or \
                 fs_write_file for full replacements. ",
            );
        } else if has_write {
            prompt.push_str(
                "You have write-only filesystem authority: use fs_write_file to create or \
                 replace content, and do not claim you can inspect existing files. ",
            );
        }
        prompt.push_str(
            "Filesystem access is permission-gated; attempt only tools actually listed above. ",
        );
        if environment.access_prompts && (has_read || has_write) {
            prompt.push_str(
                "Access outside the granted roots is not refused: the user is asked to approve \
                 it when you call the tool, so when a task needs a file change, make it \
                 directly instead of asking the user to grant access first. ",
            );
        }
        if has_commands {
            prompt.push_str(
                "Use command_exec's literal argv/cwd interface for relevant builds, tests, and \
                 linters; command approval is separate from filesystem access. ",
            );
            if !environment.command_pregrants.is_empty() {
                let listed = environment
                    .command_pregrants
                    .iter()
                    .map(|(argv, cwd)| format!("{argv} in {cwd}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                prompt.push_str(&format!(
                    "Pre-approved commands (call command_exec with this exact argv/cwd — no \
                     approval prompt needed): {listed}"
                ));
                if environment.command_pregrants_overflow > 0 {
                    prompt.push_str(&format!(
                        " (+{} more pre-approved; see /permissions)",
                        environment.command_pregrants_overflow
                    ));
                }
                prompt.push_str(". ");
            }
        }
        if has_git {
            prompt.push_str(
                "For Git operations, use the git tool with literal args and cwd, including \
                 status, diff, switch -c, add, and commit when the user requests them. \
                 Git uses separate exact command approvals and OS containment. ",
            );
        }
        if !has_commands && !has_git {
            prompt.push_str(
                "Git and other developer commands are not enabled for this invocation. \
                 If a requested task requires Git, explain that `kitsu chat --enable-commands` \
                 enables guarded Git/command tools with separate exact approvals. ",
            );
        }
        if has_spawn {
            prompt.push_str(
                "Use spawn_agent_bg only for explicitly parallel or long-running work; answer simple requests directly. ",
            );
        }
        if has_workspace_diff {
            prompt.push_str(
                "Use workspace_diff to fetch the user's verified git diff (scope working/staged/unstaged, {\"base\": REV}, or {\"commit\": REV}) whenever they ask about their changes — do not ask them to paste diffs or spawn agents to run git for this. ",
            );
        }
        prompt.trim_end().to_string()
    }
}
