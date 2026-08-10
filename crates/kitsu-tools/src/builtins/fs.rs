//! Read-only filesystem tools: `fs.read_file`, `fs.list_dir`, `repo.grep`.
//! Each validates its path through the [`Permissions`] before touching disk and runs
//! the blocking IO on a blocking thread. Errors are returned as [`ToolError`]
//! and serialized back to the model as a tool result by the orchestrator.

use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::error::ToolError;
use crate::permissions::Permissions;
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

pub fn read_file_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
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

async fn read_file_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadFileArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let canon = policy.check_read(Path::new(&a.path)).await?;
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

pub fn list_dir_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
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

async fn list_dir_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ListDirArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let canon = policy.check_read(Path::new(&a.path)).await?;
    let protected = policy.protected_read_roots();
    let listing = tokio::task::spawn_blocking(move || -> Result<String, std::io::Error> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&canon)? {
            let entry = entry?;
            let resolved = std::fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path());
            if protected.iter().any(|root| resolved.starts_with(root)) {
                continue;
            }
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

pub fn grep_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
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

async fn grep_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: GrepArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let canon = policy.check_read(Path::new(&a.path)).await?;
    let max = a.max_count.unwrap_or(GREP_DEFAULT_MAX);
    let pattern = a.pattern.clone();

    // Fixed argv, never a shell. Pattern and path are separate argv elements.
    let mut command = tokio::process::Command::new("rg");
    command
        .arg("--line-number")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--max-count")
        .arg(max.to_string());
    for private in policy.protected_read_roots() {
        if let Ok(relative) = private.strip_prefix(&canon) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            command.arg("--glob").arg(format!("!{relative}"));
            command.arg("--glob").arg(format!("!{relative}/**"));
        }
    }
    let fut = command
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

// --- fs.write_file ----------------------------------------------------------

#[derive(Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct ExistingFile {
    len: usize,
    same: bool,
    permissions: std::fs::Permissions,
}

fn inspect_existing(path: &Path, content: &[u8]) -> std::io::Result<Option<ExistingFile>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("write target is not a regular file"));
    }
    let len = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    let same = if len == content.len() {
        let mut previous = Vec::with_capacity(len);
        Read::by_ref(&mut file)
            .take(len.saturating_add(1) as u64)
            .read_to_end(&mut previous)?;
        previous == content
    } else {
        false
    };
    Ok(Some(ExistingFile {
        len,
        same,
        permissions: metadata.permissions(),
    }))
}

/// Replace one file through a same-directory temporary file. `rename` replaces
/// a raced final-component symlink instead of following it, closing the gap
/// between permission approval and the actual write.
fn atomic_replace(
    path: &Path,
    content: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("write target has no parent"))?;
    let mut temporary = None;
    let mut file = None;
    for _ in 0..16 {
        let sequence = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".kitsu-write-{}-{sequence}.tmp",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        match options.open(&candidate) {
            Ok(opened) => {
                temporary = Some(candidate);
                file = Some(opened);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let temporary = temporary.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a temporary write file",
        )
    })?;
    let mut file = file.expect("temporary path and file are assigned together");
    let result = (|| {
        file.write_all(content)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        if let Ok(directory) = std::fs::File::open(parent) {
            directory.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub fn write_file_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_write_file".to_string(),
        description:
            "Create or overwrite a UTF-8 text file at a write-allowed path with the given \
                      content. Use this to generate code or other files. The parent directory must \
                      already exist. Returns the path written and byte count."
                .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to write (created or overwritten)"},
                "content": {"type": "string", "description": "Full UTF-8 file contents"}
            },
            "required": ["path", "content"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { write_file_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn write_file_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: WriteFileArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    policy.check_write_size(a.content.len())?;
    let target = policy.check_write(Path::new(&a.path)).await?;
    let bytes = a.content.len();
    let display = target.display().to_string();
    let content = a.content.into_bytes();
    let written = target.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let previous = inspect_existing(&written, &content)?;
        if previous.as_ref().is_some_and(|existing| existing.same) {
            return Ok(("unchanged", previous.as_ref().map(|existing| existing.len)));
        }
        let change = if previous.is_none() {
            "created"
        } else {
            "updated"
        };
        let previous_bytes = previous.as_ref().map(|existing| existing.len);
        let permissions = previous.map(|existing| existing.permissions);
        atomic_replace(&written, &content, permissions).map(|_| (change, previous_bytes))
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("write failed: {e}")))?;
    Ok(serde_json::json!({
        "written": display,
        "bytes": bytes,
        "change": outcome.0,
        "previous_bytes": outcome.1,
    })
    .to_string())
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
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
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
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("a.txt");
        let args =
            serde_json::json!({"path": path.to_string_lossy(), "start_line": 2, "end_line": 3})
                .to_string();
        let out = read_file_run(&policy, &args).await.expect("read ok");
        assert_eq!(out, "l2\nl3");
    }

    #[tokio::test]
    async fn read_file_bad_args_is_invalid() {
        let policy = Permissions::for_roots(&[], &[]).expect("perms");
        assert!(matches!(
            read_file_run(&policy, "not json").await,
            Err(ToolError::InvalidArgs(_))
        ));
    }

    #[tokio::test]
    async fn list_dir_lists_entries() {
        let (dir, root) = temp_with_file("a.txt", "x");
        std::fs::create_dir(dir.path().join("sub")).expect("mk sub");
        let policy = Permissions::for_roots(std::slice::from_ref(&root), &[]).expect("perms");
        let args = serde_json::json!({"path": root}).to_string();
        let out = list_dir_run(&policy, &args).await.expect("list ok");
        assert!(out.contains("a.txt"));
        assert!(out.contains("sub/"));
    }

    #[tokio::test]
    async fn private_kitsu_state_is_hidden_inside_an_allowed_root() {
        let (dir, root) = temp_with_file("visible.txt", "ok");
        let private = dir.path().join("sessions");
        std::fs::create_dir(&private).unwrap();
        let secret = private.join("saved.json");
        std::fs::write(&secret, "TRANSCRIPT_SECRET").unwrap();
        let policy = Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap();
        policy
            .protect_read_path(&private.to_string_lossy())
            .unwrap();

        let list = list_dir_run(&policy, &serde_json::json!({"path": root}).to_string())
            .await
            .unwrap();
        assert!(list.contains("visible.txt"));
        assert!(!list.contains("sessions"));
        let read = read_file_run(&policy, &serde_json::json!({"path": secret}).to_string()).await;
        assert!(matches!(read, Err(ToolError::PolicyDenied(_))));
        if std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_ok()
        {
            let grep = grep_run(
                &policy,
                &serde_json::json!({"path": root, "pattern": "TRANSCRIPT_SECRET"}).to_string(),
            )
            .await
            .unwrap();
            assert_eq!(grep, "(no matches)");
        }
    }

    #[tokio::test]
    async fn write_file_creates_in_write_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[], &[root]).expect("perms");
        let target = dir.path().join("out.rs");
        let args = serde_json::json!({
            "path": target.to_string_lossy(),
            "content": "fn main() {}\n"
        })
        .to_string();
        let out = write_file_run(&policy, &args).await.expect("write ok");
        assert!(out.contains("\"bytes\""));
        assert!(out.contains("\"change\":\"created\""));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn main() {}\n");
    }

    #[tokio::test]
    async fn write_file_reports_updated_and_unchanged_truthfully() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[], &[root]).unwrap();
        let target = dir.path().join("existing.txt");
        std::fs::write(&target, "old").unwrap();
        let args = serde_json::json!({
            "path": target.to_string_lossy(),
            "content": "new"
        })
        .to_string();

        let changed = write_file_run(&policy, &args).await.unwrap();
        assert!(changed.contains("\"change\":\"updated\""));
        assert!(changed.contains("\"previous_bytes\":3"));

        let unchanged = write_file_run(&policy, &args).await.unwrap();
        assert!(unchanged.contains("\"change\":\"unchanged\""));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_replaces_a_raced_symlink_instead_of_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "ORIGINAL").unwrap();
        let target = dir.path().join("target.txt");
        std::os::unix::fs::symlink(&outside, &target).unwrap();

        atomic_replace(&target, b"SAFE", None).unwrap();

        assert!(!std::fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "SAFE");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "ORIGINAL");
    }

    #[tokio::test]
    async fn write_file_rejects_outside_write_root() {
        let allowed = tempfile::tempdir().expect("allowed");
        let other = tempfile::tempdir().expect("other");
        let policy = Permissions::for_roots(&[], &[allowed.path().to_string_lossy().to_string()])
            .expect("perms");
        let bad = other.path().join("evil.rs");
        let args = serde_json::json!({"path": bad.to_string_lossy(), "content": "x"}).to_string();
        assert!(matches!(
            write_file_run(&policy, &args).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(
            !bad.exists(),
            "must not have written outside the write root"
        );
    }

    #[tokio::test]
    async fn write_file_denied_when_no_write_roots() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Read allowed, but no write roots configured → writes denied.
        let policy = Permissions::for_roots(&[dir.path().to_string_lossy().to_string()], &[])
            .expect("perms");
        let target = dir.path().join("x.rs");
        let args =
            serde_json::json!({"path": target.to_string_lossy(), "content": "x"}).to_string();
        assert!(matches!(
            write_file_run(&policy, &args).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn write_file_dotdot_escape_is_denied() {
        let base = tempfile::tempdir().expect("base");
        let allowed = base.path().join("allowed");
        let sibling = base.path().join("sibling");
        std::fs::create_dir_all(&allowed).expect("mk allowed");
        std::fs::create_dir_all(&sibling).expect("mk sibling");
        let policy =
            Permissions::for_roots(&[], &[allowed.to_string_lossy().to_string()]).expect("perms");
        // Escape via the allowed root's parent into the sibling dir.
        let escape = allowed.join("..").join("sibling").join("pwned.rs");
        let args =
            serde_json::json!({"path": escape.to_string_lossy(), "content": "x"}).to_string();
        assert!(matches!(
            write_file_run(&policy, &args).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(!sibling.join("pwned.rs").exists());
    }
}
