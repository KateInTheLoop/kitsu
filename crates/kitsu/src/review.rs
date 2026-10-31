//! Workspace-bound evidence and review disclosure.

use crate::cli::Cli;
use kitsu_tools::Permissions;
use std::sync::Arc;

use crate::chat::run_chat;
use crate::chat::state::SessionInfo;
use crate::chat::ChatInvocation;
use crate::chat::InitialReview;
use crate::cli_error::CliError;
use crate::cli_runtime::emit_jsonl;
use crate::cli_runtime::jsonl_requested;
use crate::presentation::terminal_inline;
use crate::subagent;
use crate::workspace;

pub(crate) async fn run_review(
    cli: &Cli,
    review: InitialReview,
    resume: Option<&str>,
    persist_session: bool,
) -> Result<(), CliError> {
    // No fabricated read root. `kitsu review` gathers its evidence through the
    // host-owned WorkspaceEvidenceAuthority, so granting the model the whole
    // repository would be an undisclosed invocation read root that buys nothing.
    run_chat(
        cli,
        ChatInvocation {
            first_prompt: None,
            no_tools: false,
            allow_paths: &[],
            allow_write: &[],
            allow_read_write: &[],
            require_writes: &[],
            require_write_sha256: &[],
            enable_commands: false,
            command_roots: &[],
            command_runtime_roots: &[],
            command_env: &[],
            allow_commands: &[],
            allow_network: false,
            script: None,
            resume,
            persist_session,
            no_session: false,
            session_name: None,
            allow_turn_errors: false,
            initial_review: Some(review),
        },
    )
    .await
}

pub(crate) const REVIEW_SCOPE_USAGE: &str =
    "[working|staged|unstaged|base <revision>|commit <revision>|checkpoint <id>] [path]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewScope {
    Working,
    Staged,
    Unstaged,
    Base(String),
    Commit(String),
    Checkpoint(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewRequest {
    pub(crate) scope: ReviewScope,
    /// Exact literal path as entered after the scope. A path without an
    /// explicit scope remains the backwards-compatible working-tree form.
    pub(crate) path: Option<String>,
}

pub(crate) fn split_review_argument(input: &str) -> (&str, &str) {
    let mut parts = input.trim().splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let tail = parts.next().unwrap_or("").trim();
    (head, tail)
}

pub(crate) fn review_path(input: &str) -> Option<String> {
    (!input.is_empty()).then(|| input.to_string())
}

pub(crate) fn parse_review_request(input: &str) -> Result<ReviewRequest, String> {
    let input = input.trim();
    let (head, tail) = split_review_argument(input);
    let request = match head {
        "" => ReviewRequest {
            scope: ReviewScope::Working,
            path: None,
        },
        "working" => ReviewRequest {
            scope: ReviewScope::Working,
            path: review_path(tail),
        },
        "staged" => ReviewRequest {
            scope: ReviewScope::Staged,
            path: review_path(tail),
        },
        "unstaged" => ReviewRequest {
            scope: ReviewScope::Unstaged,
            path: review_path(tail),
        },
        "base" | "commit" | "checkpoint" => {
            let (value, path) = split_review_argument(tail);
            if value.is_empty() {
                return Err(format!(
                    "missing value after '{head}'; expected {REVIEW_SCOPE_USAGE}"
                ));
            }
            let scope = match head {
                "base" => ReviewScope::Base(value.to_string()),
                "commit" => ReviewScope::Commit(value.to_string()),
                "checkpoint" => ReviewScope::Checkpoint(parse_checkpoint_id(value)?),
                _ => unreachable!("matched review scope"),
            };
            ReviewRequest {
                scope,
                path: review_path(path),
            }
        }
        // Preserve `/diff path` and `/review path`: only documented scope
        // keywords change the interpretation of the first argument.
        _ => ReviewRequest {
            scope: ReviewScope::Working,
            path: Some(input.to_string()),
        },
    };
    Ok(request)
}

pub(crate) fn parse_checkpoint_id(input: &str) -> Result<u64, String> {
    let id = input
        .parse::<u64>()
        .map_err(|_| "invalid checkpoint id; expected a positive integer".to_string())?;
    if id == 0 {
        Err("invalid checkpoint id; expected a positive integer".to_string())
    } else {
        Ok(id)
    }
}

pub(crate) fn parse_exact_checkpoint_id(input: &str) -> Result<u64, String> {
    let (id, extra) = split_review_argument(input);
    if id.is_empty() || !extra.is_empty() {
        return Err("expected exactly one checkpoint id".to_string());
    }
    parse_checkpoint_id(id)
}

pub(crate) fn parse_optional_checkpoint_id(input: &str) -> Result<Option<u64>, String> {
    if input.trim().is_empty() {
        Ok(None)
    } else {
        parse_exact_checkpoint_id(input).map(Some)
    }
}

pub(crate) fn private_workspace_paths(session: &SessionInfo) -> Vec<std::path::PathBuf> {
    let mut paths = vec![session.config_path.clone(), session.state_dir.clone()];
    if let Some(path) = session.offload_dir.borrow().clone() {
        paths.push(path);
    }
    paths.sort_unstable();
    paths.dedup();
    paths
}

pub(crate) fn path_identities(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut identities = vec![path.to_path_buf()];
    if let Ok(canonical) = std::fs::canonicalize(path) {
        if !identities.contains(&canonical) {
            identities.push(canonical);
        }
    }
    identities
}

pub(crate) fn path_is_within(path: &std::path::Path, directory: &std::path::Path) -> bool {
    let paths = path_identities(path);
    let directories = path_identities(directory);
    paths.iter().any(|path| {
        directories
            .iter()
            .any(|directory| path == directory || path.starts_with(directory))
    })
}

pub(crate) fn review_target_label(evidence: &workspace::ReviewEvidence) -> String {
    let mut label = format!("target {}", terminal_inline(&evidence.target));
    if let Some(revision) = &evidence.resolved_revision {
        label.push_str(&format!(", resolved {}", terminal_inline(revision)));
    }
    if let Some(merge_base) = &evidence.merge_base {
        label.push_str(&format!(", merge base {}", terminal_inline(merge_base)));
    }
    if evidence.truncated {
        label.push_str(", truncated");
    }
    label
}

/// "Nothing to review" is a dead end only when it withholds what to do next.
/// Echo the path the user actually typed and name the reachable targets.
pub(crate) fn empty_review_message(
    evidence: &workspace::ReviewEvidence,
    path: Option<&str>,
    top_level: bool,
) -> String {
    let mut message = format!(
        "nothing to review for verified {}",
        review_target_label(evidence)
    );
    if let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) {
        message.push_str(&format!(" at {}", terminal_inline(path)));
    }
    let path_arg = path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or("<path>");
    if top_level {
        message.push_str(&format!(
            "; review its committed content with --commit HEAD {path_arg}, pick another target \
             with --staged/--unstaged/--base <rev>, or discuss the file in chat with @{path_arg}"
        ));
    } else {
        message.push_str(&format!(
            "; review its committed content with /review commit HEAD {path_arg}, pick another \
             target with /review staged|unstaged|base <rev>, or attach it with @{path_arg}"
        ));
    }
    message
}

pub(crate) fn review_evidence_is_empty(evidence: &workspace::ReviewEvidence) -> bool {
    matches!(
        evidence.diff.as_str(),
        "(working tree clean)"
            | "(no staged changes)"
            | "(no unstaged or untracked changes)"
            | "(no changes from merge base)"
            | "(commit has no changes for this path)"
            | "(checkpoint has no reviewable changes)"
    )
}

/// Disclosing host-gathered evidence to the provider is a bounded, one-shot
/// event, not a standing capability: say so at the moment it happens so the
/// absence of a read root is not mistaken for the absence of disclosure.
pub(crate) fn evidence_disclosure_line(
    evidence: &workspace::ReviewEvidence,
    model: &str,
) -> String {
    format!(
        "~ disclosing {} bytes of host-verified evidence to {}; no read root granted",
        evidence.diff.len(),
        terminal_inline(model),
    )
}

/// The shared disclosure record: one field set for every surface that puts
/// host-verified evidence into the model prompt, so consumers audit `/review`
/// and `workspace_diff` disclosures with one shape.
pub(crate) fn evidence_disclosure_fields(
    evidence: &workspace::ReviewEvidence,
) -> serde_json::Map<String, serde_json::Value> {
    let serde_json::Value::Object(fields) = serde_json::json!({
        "target": evidence.target,
        "resolved_revision": evidence.resolved_revision,
        "merge_base": evidence.merge_base,
        "truncated": evidence.truncated,
        "evidence_bytes": evidence.diff.len(),
        "model_disclosed": true,
        "model_read_root_granted": false,
    }) else {
        unreachable!("json! object literal");
    };
    fields
}

pub(crate) fn evidence_disclosure_payload(
    evidence: &workspace::ReviewEvidence,
) -> serde_json::Value {
    let mut fields = evidence_disclosure_fields(evidence);
    fields.insert("kind".to_string(), "review_disclosure".into());
    serde_json::Value::Object(fields)
}

/// Process-scoped JSONL receipt for the top-level `kitsu review` surface.
/// Unlike scripted `/review`, there is no command frame to carry a
/// `command_result`, so the disclosure gets its own event between
/// `run_started` and `turn_started`.
pub(crate) fn top_level_review_disclosure_event(
    evidence: &workspace::ReviewEvidence,
) -> serde_json::Value {
    let mut fields = evidence_disclosure_fields(evidence);
    fields.insert("source".to_string(), "top_level_review".into());
    serde_json::Value::Object(fields)
}

/// Turn-scoped JSONL event for a `workspace_diff` tool disclosure — the same
/// record `/review` emits as a command-frame result, generalized to the tool
/// surface (docs/verified-review.md).
pub(crate) fn workspace_evidence_disclosure_event(
    evidence: &workspace::ReviewEvidence,
) -> serde_json::Value {
    let mut fields = evidence_disclosure_fields(evidence);
    fields.insert("source".to_string(), "workspace_diff".into());
    serde_json::Value::Object(fields)
}

/// Parse `workspace_diff` arguments, accepting `/diff` scopes except the
/// host-only checkpoint scope.
pub(crate) fn parse_workspace_diff_request(args: &str) -> Result<ReviewRequest, String> {
    const SCOPE_USAGE: &str = "expected \"working\", \"staged\", \"unstaged\", \
                               {\"base\": REV}, or {\"commit\": REV}";
    let args = args.trim();
    let parsed: serde_json::Value = if args.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(args).map_err(|error| format!("invalid JSON arguments: {error}"))?
    };
    let serde_json::Value::Object(parsed) = parsed else {
        return Err("arguments must be a JSON object".to_string());
    };
    for key in parsed.keys() {
        if key != "scope" && key != "path" {
            return Err(format!(
                "unknown argument '{key}'; expected scope and/or path"
            ));
        }
    }
    let path = match parsed.get("path") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(path)) => {
            let path = path.trim();
            (!path.is_empty()).then(|| path.to_string())
        }
        Some(_) => return Err("path must be a string".to_string()),
    };
    let scope = match parsed.get("scope") {
        None | Some(serde_json::Value::Null) => ReviewScope::Working,
        Some(serde_json::Value::String(scope)) => match scope.as_str() {
            "working" => ReviewScope::Working,
            "staged" => ReviewScope::Staged,
            "unstaged" => ReviewScope::Unstaged,
            other => return Err(format!("unknown scope \"{other}\"; {SCOPE_USAGE}")),
        },
        Some(serde_json::Value::Object(scope)) => {
            if scope.len() != 1 {
                return Err(format!(
                    "scope object must have exactly one key; {SCOPE_USAGE}"
                ));
            }
            let (key, value) = scope.iter().next().expect("checked non-empty");
            let revision = value
                .as_str()
                .map(str::trim)
                .filter(|revision| !revision.is_empty())
                .ok_or_else(|| format!("scope.{key} must be a non-empty revision string"))?;
            match key.as_str() {
                "base" => ReviewScope::Base(revision.to_string()),
                "commit" => ReviewScope::Commit(revision.to_string()),
                other => return Err(format!("unknown scope key \"{other}\"; {SCOPE_USAGE}")),
            }
        }
        Some(_) => return Err(format!("invalid scope; {SCOPE_USAGE}")),
    };
    Ok(ReviewRequest { scope, path })
}

/// Disclose bounded Git evidence without consulting or granting filesystem
/// read roots. Every call excludes private paths and announces disclosure.
/// Returns `None` outside a worktree or inside Kitsu-private state.
/// See docs/verified-review.md.
pub(crate) fn workspace_diff_tool(
    directory: &std::path::Path,
    private: Vec<std::path::PathBuf>,
    perms: &Permissions,
    live_mw: &subagent::LiveModelWindow,
) -> Option<(kitsu_providers::ToolDescriptor, kitsu_tools::HandlerFn)> {
    let authority =
        Arc::new(WorkspaceEvidenceAuthority::for_workspace_tool(directory, private, perms).ok()?);
    let descriptor = kitsu_providers::ToolDescriptor {
        name: "workspace_diff".to_string(),
        description: "Fetch the user's git diff for this workspace as host-verified evidence. \
                      Kitsu's own git gathers it (this is not filesystem access and needs no \
                      read permission), bounds it to 256 KiB (`truncated` discloses the cap), \
                      excludes Kitsu-private files, and announces the disclosure to the user. \
                      Use it whenever the user asks about their changes."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "scope": {
                    "description": "Diff scope: \"working\" (default: working tree vs HEAD, \
                                    including untracked files), \"staged\", \"unstaged\", \
                                    {\"base\": REV} (changes since the merge base with REV), or \
                                    {\"commit\": REV} (one commit's diff).",
                    "anyOf": [
                        {"type": "string", "enum": ["working", "staged", "unstaged"]},
                        {
                            "type": "object",
                            "properties": {"base": {"type": "string"}},
                            "required": ["base"],
                            "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "properties": {"commit": {"type": "string"}},
                            "required": ["commit"],
                            "additionalProperties": false
                        }
                    ]
                },
                "path": {
                    "type": "string",
                    "description": "Optional repository-relative path filter."
                }
            }
        }),
    };
    let live_mw = live_mw.clone();
    let handler: kitsu_tools::HandlerFn = Box::new(move |args: String| {
        let authority = authority.clone();
        let live_mw = live_mw.clone();
        Box::pin(async move {
            let request =
                parse_workspace_diff_request(&args).map_err(kitsu_tools::ToolError::InvalidArgs)?;
            let target = match request.scope {
                ReviewScope::Working => workspace::ReviewTarget::WorkingTree,
                ReviewScope::Staged => workspace::ReviewTarget::Staged,
                ReviewScope::Unstaged => workspace::ReviewTarget::Unstaged,
                ReviewScope::Base(revision) => workspace::ReviewTarget::Base(revision),
                ReviewScope::Commit(revision) => workspace::ReviewTarget::Commit(revision),
                ReviewScope::Checkpoint(_) => {
                    unreachable!("workspace_diff has no checkpoint scope")
                }
            };
            let pathspec = authority
                .pathspec(request.path.as_deref())
                .map_err(kitsu_tools::ToolError::InvalidArgs)?;
            authority.notice_private_exclusions();
            let evidence = workspace::review_evidence_excluding(
                &authority.root,
                target,
                pathspec.as_deref(),
                &authority.private,
            )
            .map_err(kitsu_tools::ToolError::ExecError)?;
            // The standing rule: evidence disclosure is announced at the moment
            // it happens, on every surface. Returning the diff as this tool's
            // result puts it into the model prompt, so this call IS the
            // disclosure.
            let model = live_mw
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0
                .clone();
            eprintln!("{}", evidence_disclosure_line(&evidence, &model));
            if jsonl_requested() {
                // A failed emit is retained by the process-long emitter and
                // converted to the required nonzero exit at the turn boundary.
                let _ = emit_jsonl(
                    "workspace_evidence_disclosure",
                    workspace_evidence_disclosure_event(&evidence),
                );
            }
            Ok(serde_json::json!({
                "target": evidence.target,
                "resolved_revision": evidence.resolved_revision,
                "merge_base": evidence.merge_base,
                "truncated": evidence.truncated,
                "diff": evidence.diff,
            })
            .to_string())
        })
    });
    Some((descriptor, handler))
}

pub(crate) fn verified_review_prompt(evidence: &workspace::ReviewEvidence) -> String {
    format!(
        "Review the following host-verified diff evidence ({scope}). Focus on correctness, security, regressions, and missing tests. Report findings by severity with file/line references. Do not claim anything outside this exact evidence. This is a review-only turn with no tools.\n\n```diff\n{diff}\n```",
        scope = review_target_label(evidence),
        diff = evidence.diff,
    )
}

/// Host-owned evidence authority for review commands and `workspace_diff`.
/// Private-path exclusions enforce confidentiality independently of model read
/// roots, which this authority never consults or changes. Bind the repository
/// at construction from the launch directory; caller-supplied paths may only
/// narrow it. Accepting a caller-named root would permit arbitrary reads.
pub(crate) struct WorkspaceEvidenceAuthority {
    pub(crate) root: std::path::PathBuf,
    pub(crate) directory: std::path::PathBuf,
    pub(crate) private: Vec<std::path::PathBuf>,
}

impl WorkspaceEvidenceAuthority {
    /// Live git evidence: requires a worktree, since that is what `git` reads.
    pub(crate) fn for_session(session: &SessionInfo, perms: &Permissions) -> Result<Self, String> {
        Self::bind(
            &session.directory,
            private_workspace_paths(session),
            perms,
            workspace::repository_root(&session.directory)?,
        )
    }

    /// Checkpoint replay: `checkpoint_evidence` never re-reads the filesystem,
    /// it replays diff bytes Kitsu itself captured under write authority, so it
    /// works outside a worktree too.
    pub(crate) fn for_checkpoint(
        session: &SessionInfo,
        perms: &Permissions,
    ) -> Result<Self, String> {
        let root = workspace::repository_root(&session.directory).unwrap_or_else(|_| {
            std::fs::canonicalize(&session.directory).unwrap_or_else(|_| session.directory.clone())
        });
        Self::bind(
            &session.directory,
            private_workspace_paths(session),
            perms,
            root,
        )
    }

    /// Send-safe form for the model-callable `workspace_diff` tool: the same
    /// worktree and protected-state guards, bound to snapshot paths instead of
    /// a `SessionInfo` (whose interior cells are not `Send`). `private` must be
    /// a directory-level superset of `private_workspace_paths` for the whole
    /// process lifetime, so per-boundary offload rotation cannot open a gap.
    pub(crate) fn for_workspace_tool(
        directory: &std::path::Path,
        private: Vec<std::path::PathBuf>,
        perms: &Permissions,
    ) -> Result<Self, String> {
        let root = workspace::repository_root(directory)?;
        Self::bind(directory, private, perms, root)
    }

    pub(crate) fn bind(
        directory: &std::path::Path,
        private: Vec<std::path::PathBuf>,
        perms: &Permissions,
        root: std::path::PathBuf,
    ) -> Result<Self, String> {
        // Fail closed when the workspace itself lives inside Kitsu-private
        // state: host-owned or not, Kitsu never discloses its own runtime state
        // as workspace evidence.
        if perms.is_within_protected_read_root(&root) {
            return Err(format!(
                "refusing to gather workspace evidence from inside Kitsu-private state ({}); \
                 run Kitsu from a workspace outside its own state directory",
                root.display()
            ));
        }
        Ok(Self {
            root,
            directory: directory.to_path_buf(),
            private,
        })
    }

    /// Resolve a user-typed path against the bound root. Rejects `..` escapes
    /// and anything outside the repository before any I/O (workspace.rs).
    pub(crate) fn pathspec(&self, path: Option<&str>) -> Result<Option<String>, String> {
        workspace::pathspec_from_directory(&self.root, &self.directory, path.unwrap_or(""))
    }

    /// One concise line for `/permissions` when there is no evidence root.
    /// The raw `git` discovery error belongs in a `/diff` failure, not in a
    /// standing disclosure of what Kitsu can and cannot see.
    pub(crate) fn unavailable_reason(session: &SessionInfo, perms: &Permissions) -> String {
        match workspace::repository_root(&session.directory) {
            Err(_) => "not inside a Git worktree".to_string(),
            Ok(root) if perms.is_within_protected_read_root(&root) => {
                "the workspace is inside Kitsu-private state".to_string()
            }
            Ok(_) => "unavailable".to_string(),
        }
    }

    pub(crate) fn notice_private_exclusions(&self) {
        if self
            .private
            .iter()
            .any(|path| path_is_within(path, &self.root))
        {
            eprintln!("~ active Kitsu config/private state omitted from workspace evidence");
        }
    }
}

pub(crate) fn authorized_workspace_review(
    session: &SessionInfo,
    perms: &Permissions,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    request: ReviewRequest,
) -> Result<workspace::ReviewEvidence, String> {
    if let ReviewScope::Checkpoint(id) = request.scope {
        let authority = WorkspaceEvidenceAuthority::for_checkpoint(session, perms)?;
        let report = journal.checkpoint_preview(id).ok_or_else(|| {
            format!("checkpoint #{id} is not available; run /checkpoints to list recoverable checkpoints")
        })?;
        let pathspec = authority.pathspec(request.path.as_deref())?;
        return workspace::review_evidence_excluding(
            &authority.root,
            workspace::ReviewTarget::Checkpoint(report),
            pathspec.as_deref(),
            &authority.private,
        );
    }
    let authority = WorkspaceEvidenceAuthority::for_session(session, perms)?;
    let target = match request.scope {
        ReviewScope::Working => workspace::ReviewTarget::WorkingTree,
        ReviewScope::Staged => workspace::ReviewTarget::Staged,
        ReviewScope::Unstaged => workspace::ReviewTarget::Unstaged,
        ReviewScope::Base(revision) => workspace::ReviewTarget::Base(revision),
        ReviewScope::Commit(revision) => workspace::ReviewTarget::Commit(revision),
        ReviewScope::Checkpoint(_) => unreachable!("checkpoint scope handled above"),
    };
    let pathspec = authority.pathspec(request.path.as_deref())?;
    authority.notice_private_exclusions();
    workspace::review_evidence_excluding(
        &authority.root,
        target,
        pathspec.as_deref(),
        &authority.private,
    )
}
