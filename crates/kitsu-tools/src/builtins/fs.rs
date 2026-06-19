//! Read-only filesystem tools: `fs.read_file`, `fs.list_dir`, `repo.grep`.
//! Each validates its path through the [`Policy`] before touching disk and runs
//! the blocking IO on a blocking thread. Errors are returned as [`ToolError`]
//! and serialized back to the model as a tool result by the orchestrator.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::error::ToolError;
use crate::policy::Policy;
use crate::HandlerFn;
use kitsu_providers::ToolDescriptor;

// --- fs.read_file -----------------------------------------------------------

#[derive(Deserialize)]
struct ReadFileArgs {
    path: String,
    /// Optional 1-based inclusive line range [start, end].
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

pub fn read_file_tool(policy: Arc<Policy>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_file".to_string(),
        description: "Read a UTF-8 text file from an allowed path. Optionally restrict to a \
                      1-based inclusive line range with start_line/end_line."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to read"},
                "start_line": {"type": "integer", "description": "1-based first line (optional)"},
                "end_line": {"type": "integer", "description": "1-based last line, inclusive (optional)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_file_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_file_run(policy: &Policy, args: &str) -> Result<String, ToolError> {
    let a: ReadFileArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let canon = policy.check_read(Path::new(&a.path))?;
    let content = tokio::task::spawn_blocking(move || std::fs::read_to_string(&canon))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;

    let sliced = match (a.start_line, a.end_line) {
        (None, None) => content,
        (start, end) => {
            let s = start.unwrap_or(1).max(1);
            let e = end.unwrap_or(usize::MAX);
            content
                .lines()
                .enumerate()
                .filter(|(i, _)| {
                    let ln = i + 1;
                    ln >= s && ln <= e
                })
                .map(|(_, l)| l)
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    Ok(policy.cap_output(sliced))
}

// --- fs.list_dir ------------------------------------------------------------

#[derive(Deserialize)]
struct ListDirArgs {
    path: String,
}

pub fn list_dir_tool(policy: Arc<Policy>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_list_dir".to_string(),
        description: "List the entries (depth 1) of a directory at an allowed path. Directories \
                      are suffixed with '/'."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory path to list"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { list_dir_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn list_dir_run(policy: &Policy, args: &str) -> Result<String, ToolError> {
    let a: ListDirArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let canon = policy.check_read(Path::new(&a.path))?;
    let listing = tokio::task::spawn_blocking(move || -> Result<String, std::io::Error> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&canon)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let suffix = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                "/"
            } else {
                ""
            };
            entries.push(format!("{name}{suffix}"));
        }
        entries.sort();
        Ok(entries.join("\n"))
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("list failed: {e}")))?;
    Ok(policy.cap_output(listing))
}

// --- repo.grep --------------------------------------------------------------

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: String,
    #[serde(default)]
    max_count: Option<usize>,
}

const GREP_TIMEOUT: Duration = Duration::from_secs(10);
const GREP_DEFAULT_MAX: usize = 100;

pub fn grep_tool(policy: Arc<Policy>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "repo_grep".to_string(),
        description: "Search file contents under an allowed path using ripgrep. Returns matching \
                      lines with file:line prefixes."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex/text to search for"},
                "path": {"type": "string", "description": "Directory or file to search under"},
                "max_count": {"type": "integer", "description": "Max matching lines (default 100)"}
            },
            "required": ["pattern", "path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { grep_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn grep_run(policy: &Policy, args: &str) -> Result<String, ToolError> {
    let a: GrepArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let canon = policy.check_read(Path::new(&a.path))?;
    let max = a.max_count.unwrap_or(GREP_DEFAULT_MAX);
    let pattern = a.pattern.clone();

    // Fixed argv, never a shell. Pattern and path are separate argv elements.
    let fut = tokio::process::Command::new("rg")
        .arg("--line-number")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--max-count")
        .arg(max.to_string())
        .arg("--")
        .arg(&pattern)
        .arg(&canon)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let output = tokio::time::timeout(GREP_TIMEOUT, fut)
        .await
        .map_err(|_| ToolError::ExecError("rg timed out".to_string()))?
        .map_err(|e| ToolError::ExecError(format!("rg failed to start: {e}")))?;

    // rg exits 1 when there are no matches — that's not an error for us.
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.is_empty() {
        return Ok("(no matches)".to_string());
    }
    Ok(policy.cap_output(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_with_file(name: &str, body: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).expect("create");
        f.write_all(body.as_bytes()).expect("write");
        let root = dir.path().to_string_lossy().to_string();
        (dir, root)
    }

    #[tokio::test]
    async fn read_file_reads_allowed_and_rejects_outside() {
        let (dir, root) = temp_with_file("a.txt", "line1\nline2\nline3\n");
        let policy = Policy::from_roots(&[root]).expect("policy");
        let path = dir.path().join("a.txt");
        let args = serde_json::json!({"path": path.to_string_lossy()}).to_string();
        let out = read_file_run(&policy, &args).await.expect("read ok");
        assert!(out.contains("line2"));

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().expect("other");
        let bad = other.path().join("x.txt");
        std::fs::write(&bad, "secret").expect("write");
        let bad_args = serde_json::json!({"path": bad.to_string_lossy()}).to_string();
        assert!(matches!(
            read_file_run(&policy, &bad_args).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn read_file_line_range() {
        let (dir, root) = temp_with_file("a.txt", "l1\nl2\nl3\nl4\n");
        let policy = Policy::from_roots(&[root]).expect("policy");
        let path = dir.path().join("a.txt");
        let args =
            serde_json::json!({"path": path.to_string_lossy(), "start_line": 2, "end_line": 3})
                .to_string();
        let out = read_file_run(&policy, &args).await.expect("read ok");
        assert_eq!(out, "l2\nl3");
    }

    #[tokio::test]
    async fn read_file_bad_args_is_invalid() {
        let policy = Policy::from_roots(&[]).expect("policy");
        assert!(matches!(
            read_file_run(&policy, "not json").await,
            Err(ToolError::InvalidArgs(_))
        ));
    }

    #[tokio::test]
    async fn list_dir_lists_entries() {
        let (dir, root) = temp_with_file("a.txt", "x");
        std::fs::create_dir(dir.path().join("sub")).expect("mk sub");
        let policy = Policy::from_roots(&[root.clone()]).expect("policy");
        let args = serde_json::json!({"path": root}).to_string();
        let out = list_dir_run(&policy, &args).await.expect("list ok");
        assert!(out.contains("a.txt"));
        assert!(out.contains("sub/"));
    }
}
