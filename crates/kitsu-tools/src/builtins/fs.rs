//! Read-only filesystem tools: `fs.read_file`, `fs.list_dir`, `repo.grep`.
//! Each validates its path through the [`Permissions`] before touching disk and runs
//! the blocking IO on a blocking thread. Errors are returned as [`ToolError`]
//! and serialized back to the model as a tool result by the orchestrator.

use std::collections::VecDeque;
#[cfg(unix)]
use std::ffi::{CStr, CString, OsString};
use std::future::Future;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::ToolError;
use crate::permissions::Permissions;
use crate::HandlerFn;
use kitsu_providers::ToolDescriptor;

static PRIVATE_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
fn open_absolute_no_follow(
    path: &Path,
    final_flags: libc::c_int,
) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Component;

    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "authorized path is not absolute",
        ));
    }
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::RootDir | Component::CurDir => None,
            Component::Normal(component) => Some(Ok(component)),
            Component::ParentDir | Component::Prefix(_) => Some(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "authorized path was not canonical",
            ))),
        })
        .collect::<std::io::Result<Vec<_>>>()?;

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut current = options.open(Path::new("/"))?;
    for (index, component) in components.iter().enumerate() {
        let component = CString::new(component.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "authorized path contains an interior NUL byte",
            )
        })?;
        let flags = if index + 1 == components.len() {
            final_flags | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        let fd = unsafe { libc::openat(current.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        current = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(current)
}

/// Create a private text artifact without following directory symlinks or
/// replacing any existing entry. Candidate collisions (including symlinks)
/// are skipped, so restarting the process with a reset sequence cannot clobber
/// an artifact referenced by an older session transcript.
pub fn write_unique_private_text_file(
    directory: &Path,
    stem: &str,
    content: &str,
) -> std::io::Result<PathBuf> {
    write_unique_private_text_file_with_sequence(directory, stem, content, &PRIVATE_FILE_SEQUENCE)
}

fn write_unique_private_text_file_with_sequence(
    directory: &Path,
    stem: &str,
    content: &str,
    sequence: &AtomicU64,
) -> std::io::Result<PathBuf> {
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private artifact stem must contain only ASCII letters, digits, '-' or '_'",
        ));
    }

    #[cfg(unix)]
    {
        let directory_file = open_absolute_no_follow(
            directory,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NONBLOCK,
        )?;
        for _ in 0..256 {
            let number = sequence.fetch_add(1, Ordering::Relaxed);
            let name = format!("{stem}-{number}.txt");
            let c_name = CString::new(name.as_bytes()).expect("validated artifact name has no NUL");
            let fd = unsafe {
                libc::openat(
                    directory_file.as_raw_fd(),
                    c_name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error);
            }
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            if let Err(error) = file
                .write_all(content.as_bytes())
                .and_then(|_| file.sync_all())
            {
                unsafe {
                    libc::unlinkat(directory_file.as_raw_fd(), c_name.as_ptr(), 0);
                }
                return Err(error);
            }
            let _ = directory_file.sync_all();
            return Ok(directory.join(name));
        }
    }

    #[cfg(not(unix))]
    {
        let metadata = std::fs::symlink_metadata(directory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(std::io::Error::other(
                "private artifact directory is not a regular directory",
            ));
        }
        for _ in 0..256 {
            let number = sequence.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!("{stem}-{number}.txt"));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            match options.open(&path) {
                Ok(mut file) => {
                    if let Err(error) = file
                        .write_all(content.as_bytes())
                        .and_then(|_| file.sync_all())
                    {
                        let _ = std::fs::remove_file(&path);
                        return Err(error);
                    }
                    return Ok(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique private artifact file",
    ))
}

struct CappedText {
    text: String,
    truncated: bool,
}

fn read_capped_text(mut file: std::fs::File, max_bytes: usize) -> std::io::Result<CappedText> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("read target is not a regular file"));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(max_bytes)
            .min(max_bytes.saturating_add(1)),
    );
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = metadata.len() > max_bytes as u64 || bytes.len() > max_bytes;
    bytes.truncate(bytes.len().min(max_bytes));
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text.to_string(),
        Err(error) if truncated && error.error_len().is_none() => {
            String::from_utf8(bytes[..error.valid_up_to()].to_vec())
                .expect("validated UTF-8 prefix")
        }
        Err(error) => {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
    };
    Ok(CappedText { text, truncated })
}

fn open_read_file(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        open_absolute_no_follow(path, libc::O_RDONLY | libc::O_NONBLOCK)
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new().read(true).open(path)
    }
}

fn read_authorized_file(path: &Path, max_bytes: usize) -> std::io::Result<CappedText> {
    read_authorized_file_with_hook(path, max_bytes, || {})
}

fn read_authorized_file_with_hook<F>(
    path: &Path,
    max_bytes: usize,
    after_pin: F,
) -> std::io::Result<CappedText>
where
    F: FnOnce(),
{
    let file = open_read_file(path)?;
    after_pin();
    read_capped_text(file, max_bytes)
}

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
    let max_bytes = policy.max_bytes();
    let content = tokio::task::spawn_blocking(move || read_authorized_file(&canon, max_bytes))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;

    let sliced = match (a.start_line, a.end_line) {
        (None, None) => content.text,
        (start, end) => {
            let s = start.unwrap_or(1).max(1);
            let e = end.unwrap_or(usize::MAX);
            content
                .text
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
    let mut sliced = sliced;
    if content.truncated {
        sliced.push_str("\n...[truncated: output exceeded byte cap]");
    }
    Ok(policy.cap_output(sliced))
}

// --- fs.list_dir ------------------------------------------------------------

#[derive(Deserialize)]
struct ListDirArgs {
    path: String,
}

#[cfg(unix)]
struct DirectoryStream(*mut libc::DIR);

#[cfg(unix)]
impl Drop for DirectoryStream {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}

struct AuthorizedDirectoryEntry {
    /// Namespace path beneath the path that was authorized. Keeping this
    /// candidate is important if an ancestor is renamed after the directory
    /// descriptor is pinned: a later canonicalize must not make a known
    /// protected child appear unrelated.
    literal_path: PathBuf,
    /// Best-effort resolution hides symlink aliases that point into a protected
    /// subtree during enumeration.
    resolved_path: PathBuf,
    display: String,
}

fn list_authorized_directory(path: &Path) -> std::io::Result<Vec<AuthorizedDirectoryEntry>> {
    list_authorized_directory_with_hook(path, || {})
}

fn list_authorized_directory_with_hook<F>(
    path: &Path,
    after_pin: F,
) -> std::io::Result<Vec<AuthorizedDirectoryEntry>>
where
    F: FnOnce(),
{
    #[cfg(unix)]
    {
        let directory =
            open_absolute_no_follow(path, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NONBLOCK)?;
        after_pin();
        let raw_fd = directory.into_raw_fd();
        let stream = unsafe { libc::fdopendir(raw_fd) };
        if stream.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(raw_fd);
            }
            return Err(error);
        }
        let stream = DirectoryStream(stream);
        let mut entries = Vec::new();
        loop {
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            let name = OsString::from_vec(name.to_vec());
            let c_name = CString::new(name.as_bytes()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "directory entry contains an interior NUL byte",
                )
            })?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            let status = unsafe {
                libc::fstatat(
                    libc::dirfd(stream.0),
                    c_name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            let is_dir = status == 0
                && (unsafe { stat.assume_init().st_mode } & libc::S_IFMT) == libc::S_IFDIR;
            let entry_path = path.join(&name);
            let resolved =
                std::fs::canonicalize(&entry_path).unwrap_or_else(|_| entry_path.clone());
            let suffix = if is_dir { "/" } else { "" };
            entries.push(AuthorizedDirectoryEntry {
                literal_path: entry_path,
                resolved_path: resolved,
                display: format!("{}{suffix}", name.to_string_lossy()),
            });
        }
        Ok(entries)
    }

    #[cfg(not(unix))]
    {
        after_pin();
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let literal_path = entry.path();
            let resolved =
                std::fs::canonicalize(&literal_path).unwrap_or_else(|_| literal_path.clone());
            let name = entry.file_name().to_string_lossy().to_string();
            let suffix = if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                "/"
            } else {
                ""
            };
            entries.push(AuthorizedDirectoryEntry {
                literal_path,
                resolved_path: resolved,
                display: format!("{name}{suffix}"),
            });
        }
        Ok(entries)
    }
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
    let entries = tokio::task::spawn_blocking(move || list_authorized_directory(&canon))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("list failed: {e}")))?;
    let mut visible = entries
        .into_iter()
        .filter_map(|entry| {
            (!policy.is_read_protected(&entry.literal_path)
                && !policy.is_read_protected(&entry.resolved_path))
            .then_some(entry.display)
        })
        .collect::<Vec<_>>();
    visible.sort();
    let listing = visible.join("\n");
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

fn escape_rg_glob_literal(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for character in path.chars() {
        if matches!(
            character,
            '\\' | '*' | '?' | '[' | ']' | '{' | '}' | ',' | '!'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn relative_path_to_rg_glob_literal(path: &Path) -> Option<String> {
    use std::path::Component;

    let mut separated = String::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            if matches!(component, Component::CurDir) {
                continue;
            }
            return None;
        };
        if !separated.is_empty() {
            separated.push('/');
        }
        separated.push_str(component.to_str()?);
    }
    Some(escape_rg_glob_literal(&separated))
}

struct PinnedGrepRoot {
    command_path: PathBuf,
    #[cfg(unix)]
    _descriptor: std::fs::File,
}

impl PinnedGrepRoot {
    fn open(path: &Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            let descriptor = open_absolute_no_follow(path, libc::O_RDONLY | libc::O_NONBLOCK)?;
            let metadata = descriptor.metadata()?;
            if !metadata.is_file() && !metadata.is_dir() {
                return Err(std::io::Error::other(
                    "grep target is not a regular file or directory",
                ));
            }

            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                // `F_DUPFD` intentionally produces a descriptor without
                // FD_CLOEXEC. The child opens this stable capability through
                // procfs after exec; the parent keeps it alive until rg exits.
                let inherited = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_DUPFD, 3) };
                if inherited < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let inherited = unsafe { std::fs::File::from_raw_fd(inherited) };
                Ok(Self {
                    command_path: PathBuf::from(format!("/proc/self/fd/{}", inherited.as_raw_fd())),
                    _descriptor: inherited,
                })
            }

            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            Ok(Self {
                command_path: path.to_path_buf(),
                _descriptor: descriptor,
            })
        }

        #[cfg(not(unix))]
        Ok(Self {
            command_path: path.to_path_buf(),
        })
    }
}

#[derive(Debug)]
enum ProcessTermination {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

#[derive(Debug)]
struct CapturedProcess {
    termination: ProcessTermination,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Notify the detached process supervisor when the caller's future is dropped.
/// The supervisor owns the child, so it can both kill and reap it asynchronously.
struct CancelChildOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

impl CancelChildOnDrop {
    fn disarm(&mut self) {
        self.0.take();
    }
}

impl Drop for CancelChildOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            let _ = cancel.send(());
        }
    }
}

async fn capture_capped<R>(mut stream: R, max_bytes: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    // Keep one byte beyond the cap so Permissions::cap_output can add its
    // explicit truncation marker, while continuing to drain the pipe so the
    // child cannot deadlock on a full stdout/stderr buffer.
    let retain = max_bytes.saturating_add(1);
    let mut captured = Vec::with_capacity(retain.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = retain.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    Ok(captured)
}

async fn terminate_and_reap(child: &mut tokio::process::Child) -> std::io::Result<()> {
    // `start_kill` can lose a race with natural process exit. Waiting is the
    // authoritative cleanup step and reaps the child on Unix either way.
    let kill_error = child.start_kill().err();
    match child.wait().await {
        Ok(_) => Ok(()),
        Err(wait_error) => Err(std::io::Error::other(match kill_error {
            Some(kill_error) => {
                format!("failed to kill child ({kill_error}) and reap it ({wait_error})")
            }
            None => format!("failed to reap killed child: {wait_error}"),
        })),
    }
}

async fn supervise_child(
    mut child: tokio::process::Child,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
    timeout: Duration,
    max_bytes: usize,
) -> std::io::Result<CapturedProcess> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("child stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("child stderr was not captured"))?;
    let stdout_task = tokio::spawn(capture_capped(stdout, max_bytes));
    let stderr_task = tokio::spawn(capture_capped(stderr, max_bytes));

    let termination = tokio::select! {
        status = child.wait() => status.map(ProcessTermination::Exited),
        _ = tokio::time::sleep(timeout) => {
            terminate_and_reap(&mut child).await.map(|()| ProcessTermination::TimedOut)
        }
        _ = &mut cancel => {
            terminate_and_reap(&mut child).await.map(|()| ProcessTermination::Cancelled)
        }
    };

    // A wait error might leave the child running. Fail closed and make a
    // best-effort synchronous cleanup before collecting its pipe readers.
    if termination.is_err() {
        let _ = terminate_and_reap(&mut child).await;
    }

    let (stdout, stderr) = tokio::join!(stdout_task, stderr_task);
    let stdout = stdout
        .map_err(|error| std::io::Error::other(format!("stdout reader join error: {error}")))??;
    let stderr = stderr
        .map_err(|error| std::io::Error::other(format!("stderr reader join error: {error}")))??;

    Ok(CapturedProcess {
        termination: termination?,
        stdout,
        stderr,
    })
}

async fn run_captured_command(
    command: &mut tokio::process::Command,
    timeout: Duration,
    max_bytes: usize,
) -> std::io::Result<CapturedProcess> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let mut cancel_on_drop = CancelChildOnDrop(Some(cancel_tx));
    let supervisor = tokio::spawn(supervise_child(child, cancel_rx, timeout, max_bytes));
    let captured = supervisor.await.map_err(|error| {
        std::io::Error::other(format!("process supervisor join error: {error}"))
    })?;
    cancel_on_drop.disarm();
    captured
}

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
    grep_run_with_hook(policy, args, || {}).await
}

async fn grep_run_with_hook<F>(
    policy: &Permissions,
    args: &str,
    after_pin: F,
) -> Result<String, ToolError>
where
    F: FnOnce(),
{
    let a: GrepArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let canon = policy.check_read(Path::new(&a.path)).await?;
    let pin_path = canon.clone();
    let grep_root = tokio::task::spawn_blocking(move || PinnedGrepRoot::open(&pin_path))
        .await
        .map_err(|error| ToolError::ExecError(format!("grep pin join error: {error}")))?
        .map_err(|error| ToolError::ExecError(format!("grep target changed: {error}")))?;
    after_pin();
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
            let relative = relative_path_to_rg_glob_literal(relative).ok_or_else(|| {
                ToolError::PolicyDenied(format!(
                    "cannot safely exclude protected path '{}' from repository search",
                    private.display()
                ))
            })?;
            command.arg("--glob").arg(format!("!{relative}"));
            command.arg("--glob").arg(format!("!{relative}/**"));
        }
    }
    command.arg("--").arg(&pattern).arg(&grep_root.command_path);

    let output = run_captured_command(&mut command, GREP_TIMEOUT, policy.max_bytes())
        .await
        .map_err(|e| ToolError::ExecError(format!("rg failed: {e}")))?;
    drop(grep_root);
    let stdout = policy.cap_output(String::from_utf8_lossy(&output.stdout).into_owned());
    let stderr = policy.cap_output(String::from_utf8_lossy(&output.stderr).into_owned());
    let diagnostics = stderr.trim();

    match output.termination {
        ProcessTermination::Exited(status) => match status.code() {
            Some(0) => Ok(stdout),
            // rg documents exit 1 as a successful search with no matches.
            Some(1) => Ok("(no matches)".to_string()),
            Some(code) => {
                let detail = if diagnostics.is_empty() {
                    "no diagnostics".to_string()
                } else {
                    diagnostics.to_string()
                };
                Err(ToolError::ExecError(format!(
                    "rg exited with status {code}: {detail}"
                )))
            }
            None => {
                let detail = if diagnostics.is_empty() {
                    "no diagnostics".to_string()
                } else {
                    diagnostics.to_string()
                };
                Err(ToolError::ExecError(format!(
                    "rg terminated without an exit code: {detail}"
                )))
            }
        },
        ProcessTermination::TimedOut => {
            let detail = if diagnostics.is_empty() {
                String::new()
            } else {
                format!(": {diagnostics}")
            };
            Err(ToolError::ExecError(format!(
                "rg timed out after {} seconds{detail}",
                GREP_TIMEOUT.as_secs()
            )))
        }
        ProcessTermination::Cancelled => Err(ToolError::ExecError("rg cancelled".to_string())),
    }
}

// --- fs.write_file ----------------------------------------------------------

#[derive(Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static JOURNAL_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static JOURNAL_TURN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

const JOURNAL_TURN_LIMIT: usize = 32;

#[derive(Clone, Debug)]
struct FileSnapshot {
    content: Vec<u8>,
    permissions: std::fs::Permissions,
    identity: FileIdentity,
    modified: Option<std::time::SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FileIdentity {
    #[cfg(unix)]
    Unix {
        device: u64,
        inode: u64,
        owner: u32,
        group: u32,
        links: u64,
    },
    #[cfg(windows)]
    Windows { created: u64 },
    #[cfg(not(any(unix, windows)))]
    Portable {
        created: Option<std::time::SystemTime>,
    },
}

#[derive(Clone)]
struct JournalChange {
    path: PathBuf,
    before: Option<FileSnapshot>,
    after: FileSnapshot,
}

#[derive(Clone)]
struct JournalTurn {
    id: u64,
    label: String,
    changes: Vec<JournalChange>,
}

struct PendingTurn {
    token_id: u64,
    label: String,
    changes: Vec<JournalChange>,
}

#[derive(Default)]
struct JournalState {
    next_id: u64,
    active: Option<PendingTurn>,
    history: VecDeque<JournalTurn>,
    /// Checkpoints created asynchronously (background, late, or an attributed
    /// segment split by either one) that have not yet been surfaced to the UI.
    unreported: VecDeque<JournalTurnReport>,
}

#[derive(Debug)]
struct MutationTracker {
    in_flight: AtomicUsize,
    idle: tokio::sync::Notify,
    /// Commit and journal-record form one ordered operation. Without this gate,
    /// two blocking workers could commit A then B but acquire the journal lock
    /// in the opposite order, producing an unusable undo stack.
    commit_order: Mutex<()>,
}

impl Default for MutationTracker {
    fn default() -> Self {
        Self {
            in_flight: AtomicUsize::new(0),
            idle: tokio::sync::Notify::new(),
            commit_order: Mutex::new(()),
        }
    }
}

struct MutationLease {
    tracker: Arc<MutationTracker>,
}

impl Drop for MutationLease {
    fn drop(&mut self) {
        if self.tracker.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.tracker.idle.notify_waiters();
        }
    }
}

#[derive(Clone, Debug)]
struct JournalAttribution {
    journal_id: u64,
    turn_id: u64,
    label: String,
}

tokio::task_local! {
    static JOURNAL_ATTRIBUTION: JournalAttribution;
}

/// Opaque identity for one foreground turn. Callers scope the agent future
/// with [`WriteJournal::scope_turn`] so writes performed by that future retain
/// this identity even if their blocking commit completes after cancellation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalTurnToken {
    journal_id: u64,
    turn_id: u64,
    label: String,
}

/// Result of opening a new explicitly-attributed foreground turn.
#[derive(Debug)]
pub struct JournalTurnStart {
    pub token: JournalTurnToken,
    pub previous: Option<JournalTurnReport>,
}

/// In-memory, session-scoped record of model-originated file writes. The
/// journal stores exact pre/post images so undo can fail closed if either the
/// user or another process changed a file after the tool returned.
#[derive(Clone)]
pub struct WriteJournal {
    id: u64,
    inner: Arc<Mutex<JournalState>>,
    mutations: Arc<MutationTracker>,
}

impl Default for WriteJournal {
    fn default() -> Self {
        Self {
            id: JOURNAL_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            inner: Arc::new(Mutex::new(JournalState::default())),
            mutations: Arc::new(MutationTracker::default()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalChangeReport {
    pub path: PathBuf,
    pub change: &'static str,
    pub before_bytes: Option<usize>,
    pub after_bytes: usize,
    pub first_changed_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalTurnReport {
    pub id: u64,
    pub label: String,
    pub changes: Vec<JournalChangeReport>,
}

impl WriteJournal {
    pub fn new() -> Self {
        Self::default()
    }

    fn allocate_checkpoint_id(state: &mut JournalState) -> u64 {
        state.next_id = state.next_id.saturating_add(1);
        state.next_id
    }

    /// Start a foreground turn with an explicit identity. The returned token
    /// must scope the agent future through [`Self::scope_turn`]. Tokio task-local
    /// values are deliberately not inherited by spawned tasks, so detached
    /// background agents create standalone checkpoints instead of being folded
    /// into whichever foreground prompt happens to be active later.
    pub fn begin_attributed_turn(&self, label: impl Into<String>) -> JournalTurnStart {
        let mut state = self.inner.lock().expect("write journal");
        let previous = finalize_active(&mut state).map(turn_report);
        let label = sanitize_turn_label(label.into());
        let turn_id = JOURNAL_TURN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        state.active = Some(PendingTurn {
            token_id: turn_id,
            label: label.clone(),
            changes: Vec::new(),
        });
        JournalTurnStart {
            token: JournalTurnToken {
                journal_id: self.id,
                turn_id,
                label,
            },
            previous,
        }
    }

    /// Run `future` with file mutations explicitly attributed to `token`.
    /// Spawned Tokio tasks do not inherit this scope; their writes are recorded
    /// as independent background checkpoints.
    pub async fn scope_turn<F: Future>(&self, token: &JournalTurnToken, future: F) -> F::Output {
        debug_assert_eq!(token.journal_id, self.id);
        JOURNAL_ATTRIBUTION
            .scope(
                JournalAttribution {
                    journal_id: token.journal_id,
                    turn_id: token.turn_id,
                    label: token.label.clone(),
                },
                future,
            )
            .await
    }

    /// Wait until every filesystem mutation that has already entered its
    /// blocking commit section has completed and recorded its outcome. Call
    /// this after cancelling/dropping a foreground turn and before finalizing
    /// its checkpoint or changing session permissions.
    pub async fn drain_mutations(&self) {
        loop {
            // Register the waiter first, then inspect the count, to avoid a
            // completion notification landing between those two operations.
            let idle = self.mutations.idle.notified();
            if self.mutations.in_flight.load(Ordering::Acquire) == 0 {
                return;
            }
            idle.await;
        }
    }

    /// Wait for mutations already in their blocking commit section, then take
    /// reports that could not be returned synchronously by `begin`/`finish`.
    /// Frontends should call this after each agent completion and may also call
    /// it at the top of the next input loop to surface detached-agent writes.
    pub async fn drain_and_take_reports(&self) -> Vec<JournalTurnReport> {
        self.drain_mutations().await;
        self.take_unreported_reports()
    }

    /// Take finalized background/late reports without waiting. This is useful
    /// at an otherwise-idle prompt, where no foreground mutation is in flight.
    pub fn take_unreported_reports(&self) -> Vec<JournalTurnReport> {
        self.inner
            .lock()
            .expect("write journal")
            .unreported
            .drain(..)
            .collect()
    }

    /// Finalize exactly `token`. A mismatched token never finalizes a newer
    /// prompt; the explicit identity is what closes late-write attribution.
    pub fn finish_attributed_turn(&self, token: &JournalTurnToken) -> Option<JournalTurnReport> {
        if token.journal_id != self.id {
            return None;
        }
        let mut state = self.inner.lock().expect("write journal");
        if !state
            .active
            .as_ref()
            .is_some_and(|active| active.token_id == token.turn_id)
        {
            return None;
        }
        finalize_active(&mut state).map(turn_report)
    }

    /// Start attributing subsequent writes to one foreground model turn.
    /// Any forgotten prior turn is finalized first so evidence is never lost.
    pub fn begin_turn(&self, label: impl Into<String>) -> Option<JournalTurnReport> {
        self.begin_attributed_turn(label).previous
    }

    /// Finalize the current turn. An empty turn creates no undo checkpoint.
    pub fn finish_turn(&self) -> Option<JournalTurnReport> {
        let mut state = self.inner.lock().expect("write journal");
        finalize_active(&mut state).map(turn_report)
    }

    /// Revoke all undo authority at a session boundary.
    pub fn reset(&self) {
        *self.inner.lock().expect("write journal") = JournalState::default();
    }

    pub fn undo_depth(&self) -> usize {
        self.inner.lock().expect("write journal").history.len()
    }

    fn begin_mutation(&self) -> (MutationLease, Option<JournalAttribution>) {
        self.mutations.in_flight.fetch_add(1, Ordering::AcqRel);
        let attribution = JOURNAL_ATTRIBUTION
            .try_with(Clone::clone)
            .ok()
            .filter(|attribution| attribution.journal_id == self.id);
        (
            MutationLease {
                tracker: self.mutations.clone(),
            },
            attribution,
        )
    }

    fn record(
        &self,
        attribution: Option<JournalAttribution>,
        path: PathBuf,
        before: Option<FileSnapshot>,
        after: FileSnapshot,
    ) {
        let mut state = self.inner.lock().expect("write journal");
        if let Some(attribution) = attribution {
            if let Some(active) = state
                .active
                .as_mut()
                .filter(|active| active.token_id == attribution.turn_id)
            {
                record_change(active, path, before, after);
                return;
            }

            // Defensive fallback for a caller that finalized without draining:
            // preserve the mutation as its own retrievable checkpoint, never in
            // the newer active prompt. Flush an older segment of that newer
            // prompt first: otherwise a same-file interleave would make /undo
            // try to restore the checkpoints in the wrong mutation order.
            if let Some(turn) = flush_active_segment(&mut state) {
                state.unreported.push_back(turn_report(turn));
            }
            let id = Self::allocate_checkpoint_id(&mut state);
            let turn = JournalTurn {
                id,
                label: format!("{} (late file mutation)", attribution.label),
                changes: vec![JournalChange {
                    path,
                    before,
                    after,
                }],
            };
            push_history(&mut state, turn.clone());
            state.unreported.push_back(turn_report(turn));
            return;
        }

        if let Some(turn) = flush_active_segment(&mut state) {
            state.unreported.push_back(turn_report(turn));
        }
        let id = Self::allocate_checkpoint_id(&mut state);
        let turn = JournalTurn {
            id,
            label: "background tool write".to_string(),
            changes: vec![JournalChange {
                path,
                before,
                after,
            }],
        };
        push_history(&mut state, turn.clone());
        state.unreported.push_back(turn_report(turn));
    }

    /// Restore the latest checkpoint only if every target is still the exact
    /// file object and metadata written by that checkpoint. Permission checks
    /// and all conflict checks complete before the first restore is attempted.
    pub async fn undo_last(&self, policy: &Permissions) -> Result<JournalTurnReport, ToolError> {
        let turn = {
            let state = self.inner.lock().expect("write journal");
            if state.active.is_some() {
                return Err(ToolError::ExecError(
                    "cannot undo while a model turn is still active".to_string(),
                ));
            }
            state.history.back().cloned().ok_or_else(|| {
                ToolError::InvalidArgs("nothing to undo in this session".to_string())
            })?
        };

        let mut authorized_paths = Vec::with_capacity(turn.changes.len());
        for change in &turn.changes {
            let authorized = policy.check_write(&change.path).await?;
            if authorized != change.path {
                return Err(ToolError::PolicyDenied(format!(
                    "undo target '{}' resolved to unexpected path '{}'",
                    change.path.display(),
                    authorized.display()
                )));
            }
            authorized_paths.push(authorized);
        }

        let targets = tokio::task::spawn_blocking(move || {
            authorized_paths
                .iter()
                .map(|path| MutationTarget::pin(path))
                .collect::<std::io::Result<Vec<_>>>()
        })
        .await
        .map_err(|error| ToolError::ExecError(format!("undo pin join error: {error}")))?
        .map_err(|error| ToolError::ExecError(format!("undo target pin failed: {error}")))?;

        let mut checked = turn.clone();
        let mutations = self.mutations.clone();
        let (checked, applied) = tokio::task::spawn_blocking(move || {
            let _commit_order = mutations.commit_order.lock().expect("mutation order");
            verify_undo_changes(&checked.changes, &targets)?;
            let applied = apply_undo_transaction(&mut checked.changes, &targets);
            Ok::<_, ToolError>((checked, applied))
        })
        .await
        .map_err(|error| ToolError::ExecError(format!("undo join error: {error}")))??;

        let mut state = self.inner.lock().expect("write journal");
        let checkpoint_position = state
            .history
            .iter()
            .position(|checkpoint| checkpoint.id == turn.id);
        match applied {
            Ok(restored) => {
                if let Some(position) = checkpoint_position {
                    state.history.remove(position);
                    state.unreported.retain(|report| report.id != turn.id);
                    rebind_previous_checkpoints(&mut state, position, &turn, restored);
                }
                Ok(turn_report(turn))
            }
            Err(error) if error.rollback_complete => {
                if let Some(position) = checkpoint_position {
                    state.history[position] = checked;
                }
                Err(ToolError::ExecError(error.message))
            }
            Err(error) => {
                if let Some(position) = checkpoint_position {
                    state.history.remove(position);
                    state.unreported.retain(|report| report.id != turn.id);
                }
                Err(ToolError::ExecError(format!(
                    "{}; undo checkpoint {} was discarded because a safe retry cannot be guaranteed",
                    error.message, turn.id
                )))
            }
        }
    }
}

fn rebind_previous_checkpoints(
    state: &mut JournalState,
    removed_position: usize,
    undone: &JournalTurn,
    restored: Vec<AppliedUndo>,
) {
    for applied in restored {
        let Some(restored) = applied.restored else {
            continue;
        };
        let change = &undone.changes[applied.index];
        let Some(before) = change.before.as_ref() else {
            continue;
        };
        if let Some(previous) = state
            .history
            .iter_mut()
            .take(removed_position)
            .rev()
            .flat_map(|turn| turn.changes.iter_mut().rev())
            .find(|candidate| candidate.path == change.path)
        {
            // Only bridge a genuinely adjacent journal state. If a user edit
            // existed between the checkpoints, preserving the stale identity
            // makes the older /undo correctly fail closed.
            if previous.after.matches(before) {
                previous.after = restored;
            }
        }
    }
}

fn sanitize_turn_label(label: String) -> String {
    label
        .replace(['\n', '\r', '\t'], " ")
        .chars()
        .take(120)
        .collect()
}

fn record_change(
    active: &mut PendingTurn,
    path: PathBuf,
    before: Option<FileSnapshot>,
    after: FileSnapshot,
) {
    if let Some(existing) = active.changes.iter_mut().find(|change| change.path == path) {
        existing.after = after;
    } else {
        active.changes.push(JournalChange {
            path,
            before,
            after,
        });
    }
}

fn push_history(state: &mut JournalState, turn: JournalTurn) {
    if state.history.len() == JOURNAL_TURN_LIMIT {
        state.history.pop_front();
    }
    state.history.push_back(turn);
}

/// Finalize only the mutations accumulated so far while keeping the same turn
/// token active. This is used when a standalone mutation interleaves with a
/// foreground turn: each contiguous segment becomes its own checkpoint, so
/// same-file undo preconditions and history order remain truthful.
fn flush_active_segment(state: &mut JournalState) -> Option<JournalTurn> {
    let active = state.active.as_mut()?;
    if active.changes.is_empty() {
        return None;
    }
    let label = active.label.clone();
    let changes = std::mem::take(&mut active.changes);
    let id = WriteJournal::allocate_checkpoint_id(state);
    let turn = JournalTurn { id, label, changes };
    push_history(state, turn.clone());
    Some(turn)
}

fn finalize_active(state: &mut JournalState) -> Option<JournalTurn> {
    let pending = state.active.take()?;
    if pending.changes.is_empty() {
        return None;
    }
    state.next_id = state.next_id.saturating_add(1);
    let turn = JournalTurn {
        id: state.next_id,
        label: pending.label,
        changes: pending.changes,
    };
    push_history(state, turn.clone());
    Some(turn)
}

fn first_changed_line(before: Option<&[u8]>, after: &[u8]) -> usize {
    let before = before.unwrap_or_default();
    let common = before
        .iter()
        .zip(after)
        .take_while(|(left, right)| left == right)
        .count();
    before[..common]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

fn turn_report(turn: JournalTurn) -> JournalTurnReport {
    JournalTurnReport {
        id: turn.id,
        label: turn.label,
        changes: turn
            .changes
            .into_iter()
            .map(|change| JournalChangeReport {
                first_changed_line: first_changed_line(
                    change
                        .before
                        .as_ref()
                        .map(|snapshot| snapshot.content.as_slice()),
                    &change.after.content,
                ),
                path: change.path,
                change: if change.before.is_some() {
                    "updated"
                } else {
                    "created"
                },
                before_bytes: change
                    .before
                    .as_ref()
                    .map(|snapshot| snapshot.content.len()),
                after_bytes: change.after.content.len(),
            })
            .collect(),
    }
}

/// A write target whose parent directory has been opened without following
/// symlinks. On Unix every namespace mutation is relative to this descriptor,
/// so renaming an ancestor and replacing it with an outside-pointing symlink
/// cannot redirect an already-authorized operation.
struct MutationTarget {
    #[cfg(not(unix))]
    path: PathBuf,
    #[cfg(unix)]
    parent: std::fs::File,
    #[cfg(unix)]
    name: CString,
}

impl MutationTarget {
    fn pin(path: &Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            use std::path::Component;

            if !path.is_absolute() {
                return Err(std::io::Error::other(
                    "authorized write target is not absolute",
                ));
            }
            let file_name = path
                .file_name()
                .ok_or_else(|| std::io::Error::other("write target has no file name"))?;
            let name = CString::new(file_name.as_bytes()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "write target contains an interior NUL byte",
                )
            })?;
            let parent_path = path
                .parent()
                .ok_or_else(|| std::io::Error::other("write target has no parent"))?;
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
            let mut parent = options.open(Path::new("/"))?;
            for component in parent_path.components() {
                match component {
                    Component::RootDir | Component::CurDir => {}
                    Component::Normal(component) => {
                        let component = CString::new(component.as_bytes()).map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "write target contains an interior NUL byte",
                            )
                        })?;
                        let fd = unsafe {
                            libc::openat(
                                parent.as_raw_fd(),
                                component.as_ptr(),
                                libc::O_RDONLY
                                    | libc::O_DIRECTORY
                                    | libc::O_NOFOLLOW
                                    | libc::O_CLOEXEC,
                            )
                        };
                        if fd < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        parent = unsafe { std::fs::File::from_raw_fd(fd) };
                    }
                    Component::ParentDir | Component::Prefix(_) => {
                        return Err(std::io::Error::other(
                            "authorized write target was not canonical",
                        ));
                    }
                }
            }
            Ok(Self { parent, name })
        }

        #[cfg(not(unix))]
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    fn read_snapshot_limited(&self, max_bytes: usize) -> std::io::Result<Option<FileSnapshot>> {
        #[cfg(unix)]
        {
            self.read_named_snapshot_limited(&self.name, max_bytes)
        }
        #[cfg(not(unix))]
        {
            read_snapshot_limited_portable(&self.path, max_bytes)
        }
    }

    #[cfg(unix)]
    fn read_named_snapshot_limited(
        &self,
        name: &CStr,
        max_bytes: usize,
    ) -> std::io::Result<Option<FileSnapshot>> {
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            };
        }
        read_snapshot_from_file(unsafe { std::fs::File::from_raw_fd(fd) }, max_bytes).map(Some)
    }

    #[cfg(unix)]
    fn create_temporary(&self) -> std::io::Result<(CString, std::fs::File)> {
        for _ in 0..32 {
            let sequence = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = CString::new(format!(
                ".kitsu-write-{}-{sequence}.tmp",
                std::process::id()
            ))
            .expect("generated temporary name has no NUL");
            let fd = unsafe {
                libc::openat(
                    self.parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd >= 0 {
                return Ok((name, unsafe { std::fs::File::from_raw_fd(fd) }));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a temporary write file",
        ))
    }

    #[cfg(unix)]
    fn unlink_name(&self, name: &CStr) -> std::io::Result<()> {
        let result = unsafe { libc::unlinkat(self.parent.as_raw_fd(), name.as_ptr(), 0) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(unix)]
    fn link_names(&self, old: &CStr, new: &CStr) -> std::io::Result<()> {
        let result = unsafe {
            libc::linkat(
                self.parent.as_raw_fd(),
                old.as_ptr(),
                self.parent.as_raw_fd(),
                new.as_ptr(),
                0,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(unix)]
    fn rename_name(&self, old: &CStr, new: &CStr) -> std::io::Result<()> {
        let result = unsafe {
            libc::renameat(
                self.parent.as_raw_fd(),
                old.as_ptr(),
                self.parent.as_raw_fd(),
                new.as_ptr(),
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(unix)]
    fn rename_exchange(&self, left: &CStr, right: &CStr) -> std::io::Result<()> {
        unix_renameat_with_flags(
            self.parent.as_raw_fd(),
            left,
            self.parent.as_raw_fd(),
            right,
            RenameMode::Exchange,
        )
    }

    #[cfg(unix)]
    fn rename_noreplace(&self, old: &CStr, new: &CStr) -> std::io::Result<()> {
        unix_renameat_with_flags(
            self.parent.as_raw_fd(),
            old,
            self.parent.as_raw_fd(),
            new,
            RenameMode::NoReplace,
        )
    }

    #[cfg(unix)]
    fn move_to_unique(&self, old: &CStr, purpose: &str) -> std::io::Result<CString> {
        for _ in 0..32 {
            let sequence = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = CString::new(format!(
                ".kitsu-{purpose}-{}-{sequence}.tmp",
                std::process::id()
            ))
            .expect("generated quarantine name has no NUL");
            match self.rename_noreplace(old, &candidate) {
                Ok(()) => return Ok(candidate),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a quarantine name",
        ))
    }

    fn sync_parent_best_effort(&self) {
        #[cfg(unix)]
        let _ = self.parent.sync_all();
        #[cfg(not(unix))]
        if let Some(parent) = self.path.parent() {
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum RenameMode {
    Exchange,
    NoReplace,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn unix_renameat_with_flags(
    old_dir: libc::c_int,
    old: &CStr,
    new_dir: libc::c_int,
    new: &CStr,
    mode: RenameMode,
) -> std::io::Result<()> {
    let flags = match mode {
        RenameMode::Exchange => libc::RENAME_EXCHANGE,
        RenameMode::NoReplace => libc::RENAME_NOREPLACE,
    };
    let result =
        unsafe { libc::renameat2(old_dir, old.as_ptr(), new_dir, new.as_ptr(), flags as _) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_vendor = "apple")]
fn unix_renameat_with_flags(
    old_dir: libc::c_int,
    old: &CStr,
    new_dir: libc::c_int,
    new: &CStr,
    mode: RenameMode,
) -> std::io::Result<()> {
    let flags = match mode {
        RenameMode::Exchange => libc::RENAME_SWAP,
        RenameMode::NoReplace => libc::RENAME_EXCL,
    };
    let result = unsafe { libc::renameatx_np(old_dir, old.as_ptr(), new_dir, new.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn unix_renameat_with_flags(
    _old_dir: libc::c_int,
    _old: &CStr,
    _new_dir: libc::c_int,
    _new: &CStr,
    _mode: RenameMode,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this Unix platform lacks a race-free rename-exchange primitive",
    ))
}

fn read_snapshot_from_file(
    mut file: std::fs::File,
    max_bytes: usize,
) -> std::io::Result<FileSnapshot> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("write target is not a regular file"));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(std::io::Error::other(format!(
            "existing file exceeds the {max_bytes}-byte journal limit"
        )));
    }
    let mut content = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(max_bytes));
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut content)?;
    if content.len() > max_bytes {
        return Err(std::io::Error::other(format!(
            "existing file exceeds the {max_bytes}-byte journal limit"
        )));
    }
    Ok(FileSnapshot::from_metadata(content, &metadata))
}

#[cfg(not(unix))]
fn read_snapshot_limited_portable(
    path: &Path,
    max_bytes: usize,
) -> std::io::Result<Option<FileSnapshot>> {
    let file = match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    read_snapshot_from_file(file, max_bytes).map(Some)
}

#[cfg(test)]
fn read_snapshot_limited(path: &Path, max_bytes: usize) -> std::io::Result<Option<FileSnapshot>> {
    MutationTarget::pin(path)?.read_snapshot_limited(max_bytes)
}

impl FileSnapshot {
    fn from_metadata(content: Vec<u8>, metadata: &std::fs::Metadata) -> Self {
        Self {
            content,
            permissions: metadata.permissions(),
            identity: file_identity(metadata),
            modified: metadata.modified().ok(),
        }
    }

    fn matches(&self, other: &Self) -> bool {
        self.content == other.content
            && self.permissions == other.permissions
            && self.identity == other.identity
            && self.modified == other.modified
    }

    #[cfg(not(unix))]
    fn matches_metadata(&self, metadata: &std::fs::Metadata) -> bool {
        metadata.is_file()
            && metadata.len() == self.content.len() as u64
            && metadata.permissions() == self.permissions
            && file_identity(metadata) == self.identity
            && metadata.modified().ok() == self.modified
    }
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        group: metadata.gid(),
        links: metadata.nlink(),
    }
}

#[cfg(windows)]
fn file_identity(metadata: &std::fs::Metadata) -> FileIdentity {
    use std::os::windows::fs::MetadataExt;
    FileIdentity::Windows {
        created: metadata.creation_time(),
    }
}

#[cfg(not(any(unix, windows)))]
fn file_identity(metadata: &std::fs::Metadata) -> FileIdentity {
    FileIdentity::Portable {
        created: metadata.created().ok(),
    }
}

fn verify_undo_changes(
    changes: &[JournalChange],
    targets: &[MutationTarget],
) -> Result<(), ToolError> {
    for (change, target) in changes.iter().zip(targets) {
        let current = target
            .read_snapshot_limited(change.after.content.len())
            .map_err(|error| {
                ToolError::ExecError(format!(
                    "undo conflict at '{}': cannot verify current file: {error}",
                    change.path.display()
                ))
            })?
            .ok_or_else(|| {
                ToolError::ExecError(format!(
                    "undo conflict at '{}': file no longer exists",
                    change.path.display()
                ))
            })?;
        if !change.after.matches(&current) {
            return Err(ToolError::ExecError(format!(
                "undo conflict at '{}': file identity, metadata, or contents changed after the model write; nothing was restored",
                change.path.display()
            )));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct AppliedUndo {
    index: usize,
    restored: Option<FileSnapshot>,
}

struct UndoApplyError {
    message: String,
    rollback_complete: bool,
}

struct UndoStepError {
    error: std::io::Error,
    state_known: bool,
}

fn apply_undo_transaction(
    changes: &mut [JournalChange],
    targets: &[MutationTarget],
) -> Result<Vec<AppliedUndo>, UndoApplyError> {
    let mut applied = Vec::with_capacity(changes.len());
    for index in (0..changes.len()).rev() {
        let change = &changes[index];
        let target = &targets[index];
        let result: Result<Option<FileSnapshot>, UndoStepError> = match &change.before {
            Some(before) => match atomic_replace_checked(
                target,
                &before.content,
                Some(before.permissions.clone()),
                Some(&change.after),
            ) {
                Ok(Some(restored)) => Ok(Some(restored)),
                Ok(None) => Err(UndoStepError {
                    error: std::io::Error::other("file changed during restore"),
                    state_known: true,
                }),
                Err(error) => Err(UndoStepError {
                    error,
                    state_known: false,
                }),
            },
            None => match remove_created_file_checked(target, &change.after) {
                Ok(true) => Ok(None),
                Ok(false) => Err(UndoStepError {
                    error: std::io::Error::other("file changed during removal"),
                    state_known: true,
                }),
                Err(error) => Err(UndoStepError {
                    error,
                    state_known: false,
                }),
            },
        };

        match result {
            Ok(restored) => applied.push(AppliedUndo { index, restored }),
            Err(failure) => {
                let original = format!(
                    "undo {} failed at '{}': {}",
                    if change.before.is_some() {
                        "restore"
                    } else {
                        "removal"
                    },
                    change.path.display(),
                    failure.error
                );
                return match rollback_applied_undos(changes, targets, applied) {
                    Ok(()) if failure.state_known => Err(UndoApplyError {
                        message: format!("{original}; prior restores were rolled back"),
                        rollback_complete: true,
                    }),
                    Ok(()) => Err(UndoApplyError {
                        message: format!(
                            "{original}; prior restores were rolled back, but the failing target's state is uncertain"
                        ),
                        rollback_complete: false,
                    }),
                    Err(rollback) => Err(UndoApplyError {
                        message: format!("{original}; rollback also failed: {rollback}"),
                        rollback_complete: false,
                    }),
                };
            }
        }
    }
    Ok(applied)
}

fn rollback_applied_undos(
    changes: &mut [JournalChange],
    targets: &[MutationTarget],
    applied: Vec<AppliedUndo>,
) -> std::io::Result<()> {
    for action in applied.into_iter().rev() {
        let change = &changes[action.index];
        let after_content = change.after.content.clone();
        let after_permissions = change.after.permissions.clone();
        let path = change.path.clone();
        match action.restored {
            Some(restored) => {
                let rolled_back = atomic_replace_checked(
                    &targets[action.index],
                    &after_content,
                    Some(after_permissions),
                    Some(&restored),
                )?
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "'{}' changed before it could be rolled back",
                        path.display()
                    ))
                })?;
                changes[action.index].after = rolled_back;
            }
            None => {
                let rolled_back = create_file_exclusive(
                    &targets[action.index],
                    &after_content,
                    Some(after_permissions),
                )?;
                changes[action.index].after = rolled_back;
            }
        }
    }
    Ok(())
}

fn remove_created_file_checked(
    target: &MutationTarget,
    expected: &FileSnapshot,
) -> std::io::Result<bool> {
    remove_created_file_checked_with_hook(target, expected, || {})
}

fn remove_created_file_checked_with_hook<F>(
    target: &MutationTarget,
    expected: &FileSnapshot,
    before_commit: F,
) -> std::io::Result<bool>
where
    F: FnOnce(),
{
    #[cfg(unix)]
    {
        before_commit();
        let quarantine = target.move_to_unique(&target.name, "remove")?;
        let current = target.read_named_snapshot_limited(&quarantine, expected.content.len());
        match current {
            Ok(Some(current)) if expected.matches(&current) => {
                // The directory entry is already gone. Quarantine cleanup is
                // post-commit and cannot make the undo appear uncommitted.
                let _ = target.unlink_name(&quarantine);
                target.sync_parent_best_effort();
                Ok(true)
            }
            current => {
                let restore = target.rename_noreplace(&quarantine, &target.name);
                if let Err(restore) = restore {
                    return Err(std::io::Error::other(format!(
                        "file changed during removal; the displaced entry was retained as '{}' because restoration failed: {restore}",
                        quarantine.to_string_lossy()
                    )));
                }
                let _ = current;
                Ok(false)
            }
        }
    }

    #[cfg(not(unix))]
    {
        let current = target
            .read_snapshot_limited(expected.content.len())?
            .ok_or_else(|| std::io::Error::other("file no longer exists"))?;
        if !expected.matches(&current) {
            return Ok(false);
        }
        before_commit();
        let metadata = std::fs::symlink_metadata(&target.path)?;
        if !expected.matches_metadata(&metadata) {
            return Ok(false);
        }
        std::fs::remove_file(&target.path)?;
        target.sync_parent_best_effort();
        Ok(true)
    }
}

/// Replace one file through a same-directory temporary file. On Unix a checked
/// replace first exchanges the temporary and target entries, then verifies the
/// displaced inode. A replacement that lands after the snapshot is therefore
/// detected and restored instead of being overwritten by a check/rename race.
fn atomic_replace_checked(
    target: &MutationTarget,
    content: &[u8],
    permissions: Option<std::fs::Permissions>,
    expected: Option<&FileSnapshot>,
) -> std::io::Result<Option<FileSnapshot>> {
    atomic_replace_checked_with_hook(target, content, permissions, expected, || {})
}

fn atomic_replace_checked_with_hook<F>(
    target: &MutationTarget,
    content: &[u8],
    permissions: Option<std::fs::Permissions>,
    expected: Option<&FileSnapshot>,
    before_commit: F,
) -> std::io::Result<Option<FileSnapshot>>
where
    F: FnOnce(),
{
    #[cfg(unix)]
    {
        let (temporary, mut temporary_file) = target.create_temporary()?;
        let mut temporary_is_prepared = true;
        let result = (|| {
            temporary_file.write_all(content)?;
            let permissions =
                permissions.or_else(|| expected.map(|existing| existing.permissions.clone()));
            if let Some(permissions) = permissions {
                temporary_file.set_permissions(permissions)?;
            }
            temporary_file.sync_all()?;
            let committed =
                FileSnapshot::from_metadata(content.to_vec(), &temporary_file.metadata()?);
            before_commit();

            if let Some(expected) = expected {
                match target.rename_exchange(&temporary, &target.name) {
                    Ok(()) => temporary_is_prepared = false,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error),
                }
                let displaced = target
                    .read_named_snapshot_limited(&temporary, expected.content.len())
                    .ok()
                    .flatten();
                let installed = target.read_snapshot_limited(content.len()).ok().flatten();
                if displaced
                    .as_ref()
                    .is_some_and(|snapshot| expected.matches(snapshot))
                    && installed
                        .as_ref()
                        .is_some_and(|snapshot| committed.matches(snapshot))
                {
                    // The displaced entry is exactly the checked old inode.
                    // Cleanup is post-commit and must not turn a committed
                    // write into a reported failure.
                    let _ = target.unlink_name(&temporary);
                    target.sync_parent_best_effort();
                    return Ok(Some(committed));
                }

                match rollback_exchanged_replace(target, &temporary, &committed)? {
                    true => Ok(None),
                    false => Err(std::io::Error::other(format!(
                        "write conflict: the target changed again during CAS rollback; the displaced entry was retained as '{}'",
                        temporary.to_string_lossy()
                    ))),
                }
            } else {
                target.rename_name(&temporary, &target.name)?;
                temporary_is_prepared = false;
                target.sync_parent_best_effort();
                Ok(Some(committed))
            }
        })();
        drop(temporary_file);
        if temporary_is_prepared {
            let _ = target.unlink_name(&temporary);
        }
        result
    }

    #[cfg(not(unix))]
    atomic_replace_checked_portable(target, content, permissions, expected, before_commit)
}

#[cfg(unix)]
fn rollback_exchanged_replace(
    target: &MutationTarget,
    displaced_name: &CStr,
    committed: &FileSnapshot,
) -> std::io::Result<bool> {
    // Move whatever currently occupies the target out of the way without
    // overwriting another entry. If it is still our prepared inode, the
    // displaced file can be restored with another no-replace rename. If a
    // third party replaced it after the exchange, put that later entry back
    // and retain the displaced file under its recovery name.
    let installed_name = target.move_to_unique(&target.name, "rollback")?;
    let installed = target
        .read_named_snapshot_limited(&installed_name, committed.content.len())
        .ok()
        .flatten();
    if installed
        .as_ref()
        .is_some_and(|snapshot| committed.matches(snapshot))
    {
        if let Err(error) = target.rename_noreplace(displaced_name, &target.name) {
            let _ = target.rename_noreplace(&installed_name, &target.name);
            return Err(std::io::Error::other(format!(
                "could not restore the raced target after CAS failure: {error}"
            )));
        }
        let _ = target.unlink_name(&installed_name);
        target.sync_parent_best_effort();
        Ok(true)
    } else {
        target.rename_noreplace(&installed_name, &target.name)?;
        Ok(false)
    }
}

#[cfg(not(unix))]
fn atomic_replace_checked_portable<F>(
    target: &MutationTarget,
    content: &[u8],
    permissions: Option<std::fs::Permissions>,
    expected: Option<&FileSnapshot>,
    before_commit: F,
) -> std::io::Result<Option<FileSnapshot>>
where
    F: FnOnce(),
{
    let path = &target.path;
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
    let mut file = Some(file.expect("temporary path and file are assigned together"));
    let result = (|| {
        let temporary_file = file
            .as_mut()
            .expect("temporary file remains open until rename");
        temporary_file.write_all(content)?;
        let current = match expected {
            Some(expected) => {
                let current = read_snapshot_limited(path, expected.content.len())?;
                match current {
                    Some(current) if expected.matches(&current) => Some(current),
                    _ => return Ok(None),
                }
            }
            None => None,
        };
        let permissions = permissions.or_else(|| {
            current
                .as_ref()
                .map(|existing| existing.permissions.clone())
        });
        if let Some(permissions) = permissions {
            temporary_file.set_permissions(permissions)?;
        }
        temporary_file.sync_all()?;
        let committed = FileSnapshot::from_metadata(content.to_vec(), &temporary_file.metadata()?);

        before_commit();
        if let Some(expected) = expected {
            let metadata = std::fs::symlink_metadata(path)?;
            if !expected.matches_metadata(&metadata) {
                return Ok(None);
            }
        }
        drop(file.take());
        std::fs::rename(&temporary, path)?;
        if let Ok(directory) = std::fs::File::open(parent) {
            // Rename has committed at this point. Treat directory sync as
            // best-effort so callers never mistake a durability warning for a
            // pre-commit failure and incorrectly roll back another file.
            let _ = directory.sync_all();
        }
        Ok(Some(committed))
    })();
    // Close the temporary before cleanup on platforms that do not allow an
    // open file to be removed. The successful path already took this option.
    drop(file.take());
    if !matches!(result, Ok(Some(_))) {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
fn atomic_replace(
    path: &Path,
    content: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> std::io::Result<FileSnapshot> {
    let target = MutationTarget::pin(path)?;
    atomic_replace_checked(&target, content, permissions, None)
        .map(|replaced| replaced.expect("an unconditional atomic replace must commit"))
}

fn commit_whole_file(
    target: &MutationTarget,
    content: &[u8],
    previous: Option<&FileSnapshot>,
) -> std::io::Result<FileSnapshot> {
    match previous {
        Some(expected) => atomic_replace_checked(
            target,
            content,
            Some(expected.permissions.clone()),
            Some(expected),
        )?
        .ok_or_else(|| {
            std::io::Error::other("write conflict: target changed while the write was prepared")
        }),
        None => create_file_exclusive(target, content, None).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                std::io::Error::other(
                    "write conflict: target was created while the write was prepared",
                )
            } else {
                error
            }
        }),
    }
}

fn create_file_exclusive(
    target: &MutationTarget,
    content: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> std::io::Result<FileSnapshot> {
    #[cfg(unix)]
    {
        let (temporary, mut file) = target.create_temporary()?;
        let result = (|| {
            file.write_all(content)?;
            if let Some(permissions) = permissions {
                file.set_permissions(permissions)?;
            }
            file.sync_all()?;
            target.link_names(&temporary, &target.name)?;
            // Link count returns to one after temporary-name cleanup, so take
            // the journal snapshot only after that cleanup.
            let _ = target.unlink_name(&temporary);
            let created = FileSnapshot::from_metadata(content.to_vec(), &file.metadata()?);
            target.sync_parent_best_effort();
            Ok(created)
        })();
        if result.is_err() {
            let _ = target.unlink_name(&temporary);
        }
        result
    }

    #[cfg(not(unix))]
    {
        let path = &target.path;
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
            let created = FileSnapshot::from_metadata(content.to_vec(), &file.metadata()?);
            drop(file);
            // hard_link is the portable no-overwrite primitive: unlike rename it
            // fails atomically if another actor recreated the path.
            std::fs::hard_link(&temporary, path)?;
            // The target now exists and shares this inode. Temporary-name cleanup
            // is post-commit and therefore best-effort, just like directory sync.
            let _ = std::fs::remove_file(&temporary);
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
            Ok(created)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

pub fn write_file_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    write_file_tool_with_journal(policy, None)
}

pub fn write_file_tool_with_journal(
    policy: Arc<Permissions>,
    journal: Option<WriteJournal>,
) -> (ToolDescriptor, HandlerFn) {
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
        let journal = journal.clone();
        Box::pin(async move { write_file_run_with_journal(&policy, journal.as_ref(), &args).await })
    });
    (descriptor, handler)
}

#[cfg(test)]
async fn write_file_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    write_file_run_with_journal(policy, None, args).await
}

/// Known chat-template control tokens across common local-model formats
/// (ChatML, Llama 3, Mistral). Their presence in file content means the
/// model's raw completion leaked its own protocol markers instead of the
/// text it was supposed to produce.
const CHAT_TEMPLATE_TOKENS: &[&str] = &[
    "<|im_start|>",
    "<|im_end|>",
    "<|endoftext|>",
    "<|eot_id|>",
    "<|start_header_id|>",
    "<|end_header_id|>",
    "[INST]",
    "[/INST]",
];

/// Best-effort detection of obviously-broken generated content: a model's raw
/// protocol tokens leaking into the file, or a would-be multi-line file whose
/// newlines never got un-escaped (literal `\n` two-char sequences instead of
/// real line breaks). Advisory only — never blocks the write — a heuristic
/// false positive on legitimately unusual content is far cheaper than a
/// silently corrupted file (a real dogfooding finding: a Python file written
/// with literal `\n` was a guaranteed SyntaxError, reported as a clean write).
fn content_quality_warning(content: &str) -> Option<&'static str> {
    if CHAT_TEMPLATE_TOKENS.iter().any(|tok| content.contains(tok)) {
        return Some(
            "content contains a raw chat-template token (e.g. <|im_start|>) — \
             the model likely emitted its own control tokens instead of real content",
        );
    }
    let plausible_source_file = content.len() > 200;
    let has_real_newline = content.contains('\n');
    let literal_escapes = content.matches("\\n").count();
    if plausible_source_file && !has_real_newline && literal_escapes >= 2 {
        return Some(
            "content has no real line breaks but contains literal \\n escape sequences — \
             likely unescaped text rather than a real multi-line file",
        );
    }
    None
}

async fn write_file_run_with_journal(
    policy: &Permissions,
    journal: Option<&WriteJournal>,
    args: &str,
) -> Result<String, ToolError> {
    let a: WriteFileArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    policy.check_write_size(a.content.len())?;
    let target = policy.check_write(Path::new(&a.path)).await?;
    let bytes = a.content.len();
    let quality_warning = content_quality_warning(&a.content);
    let display = target.display().to_string();
    let content = a.content.into_bytes();
    let written = target.clone();
    let blocking_journal = journal.cloned();
    let tracked_mutation = journal.map(WriteJournal::begin_mutation);
    let max_bytes = policy.max_bytes();
    let outcome =
        tokio::task::spawn_blocking(move || -> std::io::Result<(&'static str, Option<usize>)> {
            let (_mutation_lease, attribution) = match tracked_mutation {
                Some((lease, attribution)) => (Some(lease), attribution),
                None => (None, None),
            };
            let _commit_order = blocking_journal.as_ref().map(|journal| {
                journal
                    .mutations
                    .commit_order
                    .lock()
                    .expect("mutation order")
            });
            let mutation_target = MutationTarget::pin(&written)?;
            let previous = mutation_target.read_snapshot_limited(max_bytes)?;
            let previous_bytes = previous.as_ref().map(|existing| existing.content.len());
            if previous
                .as_ref()
                .is_some_and(|existing| existing.content == content)
            {
                return Ok(("unchanged", previous_bytes));
            }
            let change = if previous.is_none() {
                "created"
            } else {
                "updated"
            };
            let committed = commit_whole_file(&mutation_target, &content, previous.as_ref())?;
            if let Some(journal) = blocking_journal.as_ref() {
                journal.record(attribution, written, previous, committed);
            }
            Ok((change, previous_bytes))
        })
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("write failed: {e}")))?;
    Ok(serde_json::json!({
        "written": display,
        "bytes": bytes,
        "change": outcome.0,
        "previous_bytes": outcome.1,
        "warning": quality_warning,
    })
    .to_string())
}

// --- fs.edit_file -----------------------------------------------------------

#[derive(Deserialize)]
struct EditFileArgs {
    path: String,
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}

fn read_existing_text(
    target: MutationTarget,
    max_bytes: usize,
) -> std::io::Result<(MutationTarget, String, FileSnapshot)> {
    let snapshot = target
        .read_snapshot_limited(max_bytes)?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"))?;
    let text = String::from_utf8(snapshot.content.clone())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok((target, text, snapshot))
}

pub fn edit_file_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    edit_file_tool_with_journal(policy, None)
}

pub fn edit_file_tool_with_journal(
    policy: Arc<Permissions>,
    journal: Option<WriteJournal>,
) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_edit_file".to_string(),
        description: "Replace exact UTF-8 text in an existing file at a write-allowed path. \
                      old_text is a precondition and must occur exactly once unless replace_all \
                      is true. The edit is refused without changing the file if the precondition \
                      does not match or the file changes before commit."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Existing UTF-8 file to edit"},
                "old_text": {"type": "string", "description": "Exact non-empty text expected in the file"},
                "new_text": {"type": "string", "description": "Replacement text"},
                "replace_all": {"type": "boolean", "default": false, "description": "Replace every non-overlapping occurrence (default false)"}
            },
            "required": ["path", "old_text", "new_text"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        let journal = journal.clone();
        Box::pin(async move { edit_file_run_with_journal(&policy, journal.as_ref(), &args).await })
    });
    (descriptor, handler)
}

#[cfg(test)]
async fn edit_file_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    edit_file_run_with_journal(policy, None, args).await
}

async fn edit_file_run_with_journal(
    policy: &Permissions,
    journal: Option<&WriteJournal>,
    args: &str,
) -> Result<String, ToolError> {
    let a: EditFileArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    if a.old_text.is_empty() {
        return Err(ToolError::InvalidArgs(
            "edit conflict: old_text must not be empty; file was not modified".to_string(),
        ));
    }
    if a.old_text == a.new_text {
        return Err(ToolError::InvalidArgs(
            "old_text and new_text are identical; file was not modified".to_string(),
        ));
    }
    // Reject an obviously oversized replacement before doing any file IO. The
    // complete post-edit content is checked again below.
    policy.check_write_size(a.new_text.len())?;
    let target = policy.check_write(Path::new(&a.path)).await?;
    let display = target.display().to_string();
    let read_target = target.clone();
    let max_bytes = policy.max_bytes();
    let (mutation_target, original, original_snapshot) = tokio::task::spawn_blocking(move || {
        let mutation_target = MutationTarget::pin(&read_target)?;
        read_existing_text(mutation_target, max_bytes)
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("edit read failed: {e}")))?;

    let matches = original.match_indices(&a.old_text).count();
    if matches == 0 {
        return Err(ToolError::InvalidArgs(
            "edit conflict: old_text was not found; file was not modified".to_string(),
        ));
    }
    if !a.replace_all && matches != 1 {
        return Err(ToolError::InvalidArgs(format!(
            "edit conflict: old_text matched {matches} times; add context or set replace_all; file was not modified"
        )));
    }

    let replacements = if a.replace_all { matches } else { 1 };
    let bytes = a
        .old_text
        .len()
        .checked_mul(replacements)
        .and_then(|removed| original.len().checked_sub(removed))
        .and_then(|remaining| {
            a.new_text
                .len()
                .checked_mul(replacements)
                .and_then(|added| remaining.checked_add(added))
        })
        .unwrap_or(usize::MAX);
    // Check the calculated size before allocating the replacement. This keeps
    // replace_all from amplifying a bounded input file into a huge allocation.
    policy.check_write_size(bytes)?;
    let updated = if a.replace_all {
        original.replace(&a.old_text, &a.new_text)
    } else {
        original.replacen(&a.old_text, &a.new_text, 1)
    };
    debug_assert_eq!(updated.len(), bytes);
    let quality_warning = content_quality_warning(&updated);
    let previous_bytes = original.len();
    let updated = updated.into_bytes();
    let written = target.clone();
    let blocking_journal = journal.cloned();
    let tracked_mutation = journal.map(WriteJournal::begin_mutation);
    let committed = tokio::task::spawn_blocking(move || {
        let (_mutation_lease, attribution) = match tracked_mutation {
            Some((lease, attribution)) => (Some(lease), attribution),
            None => (None, None),
        };
        let _commit_order = blocking_journal.as_ref().map(|journal| {
            journal
                .mutations
                .commit_order
                .lock()
                .expect("mutation order")
        });
        let committed =
            atomic_replace_checked(&mutation_target, &updated, None, Some(&original_snapshot))?;
        if let (Some(journal), Some(after)) = (blocking_journal.as_ref(), committed.as_ref()) {
            journal.record(attribution, written, Some(original_snapshot), after.clone());
        }
        Ok::<bool, std::io::Error>(committed.is_some())
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("edit write failed: {e}")))?;
    if !committed {
        return Err(ToolError::ExecError(
            "edit conflict: file changed while the edit was prepared; file was not modified"
                .to_string(),
        ));
    }

    Ok(serde_json::json!({
        "edited": display,
        "bytes": bytes,
        "change": "updated",
        "previous_bytes": previous_bytes,
        "replacements": replacements,
        "warning": quality_warning,
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

    #[test]
    fn private_artifact_allocation_survives_a_sequence_restart() {
        let dir = tempfile::tempdir().unwrap();
        let first_sequence = AtomicU64::new(0);
        let first = write_unique_private_text_file_with_sequence(
            dir.path(),
            "call",
            "first",
            &first_sequence,
        )
        .unwrap();

        // Simulate a fresh process, whose in-memory sequence starts at zero.
        let restarted_sequence = AtomicU64::new(0);
        let second = write_unique_private_text_file_with_sequence(
            dir.path(),
            "call",
            "second",
            &restarted_sequence,
        )
        .unwrap();

        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(first).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(second).unwrap(), "second");
    }

    #[cfg(unix)]
    #[test]
    fn private_artifact_allocation_skips_symlink_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "outside").unwrap();
        std::fs::write(dir.path().join("call-0.txt"), "existing").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("call-1.txt")).unwrap();
        let sequence = AtomicU64::new(0);

        let allocated =
            write_unique_private_text_file_with_sequence(dir.path(), "call", "new", &sequence)
                .unwrap();

        assert_eq!(allocated.file_name().unwrap(), "call-2.txt");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("call-0.txt")).unwrap(),
            "existing"
        );
        assert_eq!(std::fs::read_to_string(outside.path()).unwrap(), "outside");
        assert_eq!(std::fs::read_to_string(allocated).unwrap(), "new");
    }

    async fn run_journaled_write(
        journal: &WriteJournal,
        policy: &Permissions,
        label: &str,
        args: &str,
    ) -> JournalTurnReport {
        let start = journal.begin_attributed_turn(label);
        assert!(start.previous.is_none());
        journal
            .scope_turn(&start.token, async {
                write_file_run_with_journal(policy, Some(journal), args)
                    .await
                    .unwrap();
            })
            .await;
        journal.drain_mutations().await;
        journal.finish_attributed_turn(&start.token).unwrap()
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
    async fn read_file_caps_input_before_allocating_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        std::fs::write(&path, "x".repeat(crate::permissions::DEFAULT_MAX_BYTES * 4)).unwrap();
        let policy =
            Permissions::for_roots(&[dir.path().to_string_lossy().into_owned()], &[]).unwrap();
        let args = serde_json::json!({"path": path}).to_string();

        let output = read_file_run(&policy, &args).await.unwrap();

        assert!(output.ends_with("...[truncated: output exceeded byte cap]"));
        assert!(output.len() <= crate::permissions::DEFAULT_MAX_BYTES + 64);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pinned_read_survives_an_ancestor_symlink_swap_without_redirecting() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let nested = allowed.join("nested");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let target = nested.join("target.txt");
        std::fs::write(&target, "AUTHORIZED").unwrap();
        std::fs::write(outside.join("target.txt"), "OUTSIDE").unwrap();
        let policy =
            Permissions::for_roots(&[allowed.to_string_lossy().into_owned()], &[]).unwrap();
        let authorized = policy.check_read(&target).await.unwrap();
        let moved = allowed.join("moved-nested");

        let output = read_authorized_file_with_hook(
            &authorized,
            crate::permissions::DEFAULT_MAX_BYTES,
            || {
                std::fs::rename(&nested, &moved).unwrap();
                std::os::unix::fs::symlink(&outside, &nested).unwrap();
            },
        )
        .unwrap();

        assert_eq!(output.text, "AUTHORIZED");
        assert_eq!(
            std::fs::read_to_string(outside.join("target.txt")).unwrap(),
            "OUTSIDE"
        );
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

    #[cfg(unix)]
    #[tokio::test]
    async fn pinned_listing_survives_an_ancestor_symlink_swap_without_redirecting() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let nested = allowed.join("nested");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(nested.join("authorized.txt"), "ok").unwrap();
        std::fs::write(outside.join("outside.txt"), "secret").unwrap();
        let policy =
            Permissions::for_roots(&[allowed.to_string_lossy().into_owned()], &[]).unwrap();
        let authorized = policy.check_read(&nested).await.unwrap();
        let moved = allowed.join("moved-nested");

        let entries = list_authorized_directory_with_hook(&authorized, || {
            std::fs::rename(&nested, &moved).unwrap();
            std::os::unix::fs::symlink(&outside, &nested).unwrap();
        })
        .unwrap();
        let listing = entries
            .into_iter()
            .map(|entry| entry.display)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(listing.contains("authorized.txt"));
        assert!(!listing.contains("outside.txt"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protected_listing_stays_hidden_during_an_ancestor_swap() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let private = allowed.join("sessions");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(allowed.join("visible.txt"), "ok").unwrap();
        let policy =
            Permissions::for_roots(&[allowed.to_string_lossy().into_owned()], &[]).unwrap();
        policy
            .protect_read_path(&private.to_string_lossy())
            .unwrap();
        let authorized = policy.check_read(&allowed).await.unwrap();
        let moved = dir.path().join("moved-allowed");

        let entries = list_authorized_directory_with_hook(&authorized, || {
            std::fs::rename(&allowed, &moved).unwrap();
            std::os::unix::fs::symlink(&outside, &allowed).unwrap();
        })
        .unwrap();
        let visible = entries
            .into_iter()
            .filter(|entry| {
                !policy.is_read_protected(&entry.literal_path)
                    && !policy.is_read_protected(&entry.resolved_path)
            })
            .map(|entry| entry.display)
            .collect::<Vec<_>>();

        assert_eq!(visible, vec!["visible.txt"]);
    }

    #[tokio::test]
    async fn private_kitsu_state_is_hidden_inside_an_allowed_root() {
        let (dir, root) = temp_with_file("visible.txt", "ok");
        let private = dir.path().join("sessions");
        std::fs::create_dir(&private).unwrap();
        let secret = private.join("saved.json");
        std::fs::write(&secret, "TRANSCRIPT_SECRET").unwrap();
        let artifacts = private.join("artifacts");
        std::fs::create_dir(&artifacts).unwrap();
        let artifact = artifacts.join("result.txt");
        std::fs::write(&artifact, "TRUSTED_RESULT").unwrap();
        let policy = Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap();
        policy
            .protect_read_path(&private.to_string_lossy())
            .unwrap();
        policy
            .allow_internal_read_root(&artifacts.to_string_lossy())
            .unwrap();

        let list = list_dir_run(&policy, &serde_json::json!({"path": root}).to_string())
            .await
            .unwrap();
        assert!(list.contains("visible.txt"));
        assert!(!list.contains("sessions"));
        let read = read_file_run(&policy, &serde_json::json!({"path": secret}).to_string()).await;
        assert!(matches!(read, Err(ToolError::PolicyDenied(_))));
        let artifact_list =
            list_dir_run(&policy, &serde_json::json!({"path": artifacts}).to_string())
                .await
                .unwrap();
        assert_eq!(artifact_list, "result.txt");
        let artifact_read =
            read_file_run(&policy, &serde_json::json!({"path": artifact}).to_string())
                .await
                .unwrap();
        assert_eq!(artifact_read, "TRUSTED_RESULT");
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
            let artifact_grep = grep_run(
                &policy,
                &serde_json::json!({"path": artifacts, "pattern": "TRUSTED_RESULT"}).to_string(),
            )
            .await
            .unwrap();
            assert!(artifact_grep.contains("result.txt:1:TRUSTED_RESULT"));
        }
    }

    #[tokio::test]
    async fn grep_escapes_protected_paths_with_glob_metacharacters() {
        if std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("[private].toml");
        std::fs::write(&secret, "GLOB_META_SECRET").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "visible").unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap();
        policy.protect_read_path(&secret.to_string_lossy()).unwrap();

        let output = grep_run(
            &policy,
            &serde_json::json!({"path": root, "pattern": "GLOB_META_SECRET"}).to_string(),
        )
        .await
        .unwrap();

        assert_eq!(output, "(no matches)");
    }

    #[test]
    fn rg_glob_literal_escaping_covers_every_metacharacter() {
        assert_eq!(
            escape_rg_glob_literal(r"a*b?c[d]{e},!f\g"),
            r"a\*b\?c\[d\]\{e\}\,\!f\\g"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn grep_excludes_multiple_literal_metacharacter_names() {
        if std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap();
        for name in [
            "*private*.txt",
            "private?.txt",
            "{private,secret}.txt",
            "!private.txt",
            r"private\name.txt",
        ] {
            let secret = dir.path().join(name);
            std::fs::write(&secret, "ALL_GLOB_METACHARS_SECRET").unwrap();
            policy.protect_read_path(&secret.to_string_lossy()).unwrap();
        }

        let output = grep_run(
            &policy,
            &serde_json::json!({
                "path": root,
                "pattern": "ALL_GLOB_METACHARS_SECRET"
            })
            .to_string(),
        )
        .await
        .unwrap();

        assert_eq!(output, "(no matches)");
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[tokio::test]
    async fn grep_uses_the_pinned_root_after_an_ancestor_swap() {
        if std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let nested = allowed.join("nested");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(nested.join("source.txt"), "PINNED_AUTHORIZED").unwrap();
        std::fs::write(outside.join("source.txt"), "PINNED_OUTSIDE").unwrap();
        let policy =
            Permissions::for_roots(&[allowed.to_string_lossy().into_owned()], &[]).unwrap();
        let moved = allowed.join("moved-nested");
        let args = serde_json::json!({"path": nested, "pattern": "PINNED_"}).to_string();

        let output = grep_run_with_hook(&policy, &args, || {
            std::fs::rename(&nested, &moved).unwrap();
            std::os::unix::fs::symlink(&outside, &nested).unwrap();
        })
        .await
        .unwrap();

        assert!(output.contains("PINNED_AUTHORIZED"), "{output}");
        assert!(!output.contains("PINNED_OUTSIDE"), "{output}");
    }

    #[tokio::test]
    async fn grep_distinguishes_no_matches_from_process_errors() {
        if std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let (dir, root) = temp_with_file("a.txt", "present\n");
        let policy = Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap();

        let no_match = grep_run(
            &policy,
            &serde_json::json!({"path": root, "pattern": "absent"}).to_string(),
        )
        .await
        .unwrap();
        assert_eq!(no_match, "(no matches)");

        let invalid = grep_run(
            &policy,
            &serde_json::json!({"path": dir.path(), "pattern": "["}).to_string(),
        )
        .await
        .expect_err("invalid regex must be a process error, not no matches");
        match invalid {
            ToolError::ExecError(message) => {
                assert!(message.contains("status 2"), "{message}");
                assert!(message.to_ascii_lowercase().contains("regex"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[cfg(unix)]
    async fn assert_process_is_reaped(pid: i32) {
        for _ in 0..200 {
            let result = unsafe { libc::kill(pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("process {pid} was not killed and reaped");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captured_command_timeout_kills_reaps_and_caps_stderr() {
        let script = format!("echo $$; printf '{}' >&2; exec sleep 30", "x".repeat(256));
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(script);
        let output = run_captured_command(&mut command, Duration::from_millis(250), 32)
            .await
            .unwrap();

        assert!(matches!(output.termination, ProcessTermination::TimedOut));
        assert_eq!(output.stderr.len(), 33, "capture keeps cap plus sentinel");
        let pid = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert_process_is_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_captured_command_kills_and_reaps_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let child_pid_file = pid_file.clone();
        let runner = tokio::spawn(async move {
            let mut command = tokio::process::Command::new("sh");
            command
                .arg("-c")
                .arg("echo $$ > \"$1\"; exec sleep 30")
                .arg("sh")
                .arg(child_pid_file);
            run_captured_command(&mut command, Duration::from_secs(30), 32).await
        });

        let pid = {
            let mut pid = None;
            for _ in 0..200 {
                if let Ok(value) = std::fs::read_to_string(&pid_file) {
                    pid = value.trim().parse::<i32>().ok();
                    if pid.is_some() {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            pid.expect("child did not publish its pid")
        };

        runner.abort();
        assert!(runner.await.unwrap_err().is_cancelled());
        assert_process_is_reaped(pid).await;
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

    #[test]
    fn content_quality_warning_flags_chat_template_tokens() {
        assert!(content_quality_warning("<|im_start|>system\nhi").is_some());
        assert!(content_quality_warning("normal code, no tokens here").is_none());
    }

    #[test]
    fn content_quality_warning_flags_unescaped_literal_newlines() {
        let broken = "def f():\\n    return 1\\n\\ndef g():\\n    return 2\\n".repeat(6);
        assert!(content_quality_warning(&broken).is_some());

        // A real multi-line file (actual newlines) is never flagged, even if
        // it also happens to contain the literal two-char sequence "\n" as text
        // (e.g. documenting an escape sequence).
        let real_file = "fn main() {\n    println!(\"a literal \\\\n in a string\");\n}\n"
            .repeat(6);
        assert!(content_quality_warning(&real_file).is_none());

        // Short content is never flagged regardless of shape (avoids false
        // positives on tiny legitimate one-liners).
        assert!(content_quality_warning("a\\nb").is_none());
    }

    #[tokio::test]
    async fn write_file_flags_literal_newline_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[], &[root]).unwrap();
        let target = dir.path().join("broken.py");
        let corrupted = "def f():\\n    return 1\\n\\ndef g():\\n    return 2\\n".repeat(6);
        let args = serde_json::json!({
            "path": target.to_string_lossy(),
            "content": corrupted,
        })
        .to_string();

        let out = write_file_run(&policy, &args).await.expect("write ok");
        assert!(out.contains("\"warning\":\"content has no real line breaks"));
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

    #[tokio::test]
    async fn edit_file_replaces_one_exact_match_and_reports_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[], &[root]).unwrap();
        let target = dir.path().join("existing.txt");
        std::fs::write(&target, "alpha\nbeta\ngamma\n").unwrap();
        let args = serde_json::json!({
            "path": target,
            "old_text": "beta",
            "new_text": "BETA"
        })
        .to_string();

        let result: serde_json::Value =
            serde_json::from_str(&edit_file_run(&policy, &args).await.unwrap()).unwrap();
        assert_eq!(result["change"], "updated");
        assert_eq!(result["bytes"], 17);
        assert_eq!(result["previous_bytes"], 17);
        assert_eq!(result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
    }

    #[tokio::test]
    async fn edit_file_refuses_missing_or_ambiguous_preconditions_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[], &[root]).unwrap();
        let target = dir.path().join("existing.txt");
        let original = "same\nmiddle\nsame\n";
        std::fs::write(&target, original).unwrap();

        for old_text in ["missing", "same", ""] {
            let args = serde_json::json!({
                "path": target,
                "old_text": old_text,
                "new_text": "replacement"
            })
            .to_string();
            let error = edit_file_run(&policy, &args).await.unwrap_err();
            assert!(matches!(error, ToolError::InvalidArgs(_)));
            assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
        }
    }

    #[tokio::test]
    async fn edit_file_replace_all_is_explicit_and_size_checked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[], &[root]).unwrap();
        let target = dir.path().join("existing.txt");
        std::fs::write(&target, "same middle same").unwrap();
        let args = serde_json::json!({
            "path": target,
            "old_text": "same",
            "new_text": "different",
            "replace_all": true
        })
        .to_string();

        let result: serde_json::Value =
            serde_json::from_str(&edit_file_run(&policy, &args).await.unwrap()).unwrap();
        assert_eq!(result["replacements"], 2);
        assert_eq!(result["previous_bytes"], 16);
        assert_eq!(result["bytes"], 26);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "different middle different"
        );

        let amplified = serde_json::json!({
            "path": target,
            "old_text": "different",
            "new_text": "x".repeat(crate::permissions::DEFAULT_MAX_WRITE_BYTES / 2 + 1),
            "replace_all": true
        })
        .to_string();
        assert!(matches!(
            edit_file_run(&policy, &amplified).await,
            Err(ToolError::PolicyDenied(_))
        ));

        let too_large = "x".repeat(crate::permissions::DEFAULT_MAX_WRITE_BYTES + 1);
        let oversized = serde_json::json!({
            "path": target,
            "old_text": "middle",
            "new_text": too_large
        })
        .to_string();
        assert!(matches!(
            edit_file_run(&policy, &oversized).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "different middle different"
        );
    }

    #[test]
    fn atomic_checked_replace_refuses_a_changed_file_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "changed").unwrap();
        let mut expected = read_snapshot_limited(&target, 7).unwrap().unwrap();
        expected.content = b"expected".to_vec();
        let mutation_target = MutationTarget::pin(&target).unwrap();

        let committed =
            atomic_replace_checked(&mutation_target, b"replacement", None, Some(&expected))
                .unwrap();

        assert!(committed.is_none());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "changed");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn whole_file_commit_refuses_existing_target_race() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "snapshotted").unwrap();
        let previous = read_snapshot_limited(&target, 32).unwrap().unwrap();

        let _user_version =
            atomic_replace(&target, b"user edit", Some(previous.permissions.clone())).unwrap();
        let mutation_target = MutationTarget::pin(&target).unwrap();
        let error =
            commit_whole_file(&mutation_target, b"model edit", Some(&previous)).unwrap_err();

        assert!(error.to_string().contains("write conflict"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "user edit");
    }

    #[test]
    fn whole_file_commit_refuses_absent_target_race() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        assert!(read_snapshot_limited(&target, 32).unwrap().is_none());

        std::fs::write(&target, "user creation").unwrap();
        let mutation_target = MutationTarget::pin(&target).unwrap();
        let error = commit_whole_file(&mutation_target, b"model creation", None).unwrap_err();

        assert!(error.to_string().contains("write conflict"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "user creation");
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

    #[cfg(unix)]
    #[test]
    fn pinned_parent_cannot_be_redirected_by_ancestor_symlink_swap() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let nested = allowed.join("nested");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let target = nested.join("target.txt");
        let outside_target = outside.join("target.txt");
        std::fs::write(&target, "authorized").unwrap();
        std::fs::write(&outside_target, "protected").unwrap();
        let expected = read_snapshot_limited(&target, 32).unwrap().unwrap();
        let pinned = MutationTarget::pin(&target).unwrap();

        let moved = allowed.join("moved-nested");
        std::fs::rename(&nested, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, &nested).unwrap();

        commit_whole_file(&pinned, b"safe update", Some(&expected)).unwrap();
        assert_eq!(
            std::fs::read_to_string(moved.join("target.txt")).unwrap(),
            "safe update"
        );
        assert_eq!(
            std::fs::read_to_string(outside_target).unwrap(),
            "protected"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pin_fails_closed_if_ancestor_is_swapped_after_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let nested = allowed.join("nested");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let requested = nested.join("new.txt");
        let outside_target = outside.join("new.txt");
        let policy = Permissions::for_roots(&[], &[allowed.to_string_lossy().to_string()]).unwrap();
        let authorized = policy.check_write(&requested).await.unwrap();

        std::fs::rename(&nested, allowed.join("moved-nested")).unwrap();
        std::os::unix::fs::symlink(&outside, &nested).unwrap();

        assert!(MutationTarget::pin(&authorized).is_err());
        assert!(!outside_target.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[test]
    fn checked_replace_restores_final_component_raced_after_prepare() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let replacement = dir.path().join("user-replacement.txt");
        std::fs::write(&target, "expected").unwrap();
        let expected = read_snapshot_limited(&target, 32).unwrap().unwrap();
        let pinned = MutationTarget::pin(&target).unwrap();

        let committed =
            atomic_replace_checked_with_hook(&pinned, b"model", None, Some(&expected), || {
                std::fs::write(&replacement, "user replacement").unwrap();
                std::fs::rename(&replacement, &target).unwrap();
            })
            .unwrap();

        assert!(committed.is_none());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "user replacement"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[test]
    fn checked_remove_does_not_unlink_final_component_raced_after_prepare() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let replacement = dir.path().join("user-replacement.txt");
        std::fs::write(&target, "expected").unwrap();
        let expected = read_snapshot_limited(&target, 32).unwrap().unwrap();
        let pinned = MutationTarget::pin(&target).unwrap();

        let removed = remove_created_file_checked_with_hook(&pinned, &expected, || {
            std::fs::write(&replacement, "user replacement").unwrap();
            std::fs::rename(&replacement, &target).unwrap();
        })
        .unwrap();

        assert!(!removed);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "user replacement"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
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

    #[tokio::test]
    async fn journal_undo_restores_updated_file_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("tracked.txt");
        std::fs::write(&target, "before\nline two\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let args = serde_json::json!({
            "path": target,
            "content": "after\nline two\n"
        })
        .to_string();
        let report = run_journaled_write(&journal, &policy, "edit tracked", &args).await;
        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].change, "updated");
        assert_eq!(report.changes[0].first_changed_line, 1);

        let undone = journal.undo_last(&policy).await.unwrap();
        assert_eq!(undone.id, report.id);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "before\nline two\n"
        );
        assert_eq!(journal.undo_depth(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
    }

    #[tokio::test]
    async fn journal_undo_removes_created_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("created.txt");
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let args = serde_json::json!({"path": target, "content": "new"}).to_string();
        run_journaled_write(&journal, &policy, "create", &args).await;

        journal.undo_last(&policy).await.unwrap();
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn journal_undo_refuses_later_user_edits_without_popping_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("conflict.txt");
        std::fs::write(&target, "before").unwrap();
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let args = serde_json::json!({"path": target, "content": "model"}).to_string();
        run_journaled_write(&journal, &policy, "model edit", &args).await;
        std::fs::write(&target, "user edit").unwrap();

        let error = journal.undo_last(&policy).await.unwrap_err();
        assert!(error.to_string().contains("undo conflict"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "user edit");
        assert_eq!(journal.undo_depth(), 1);
    }

    #[tokio::test]
    async fn journal_undo_refuses_metadata_only_changes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("metadata-conflict.txt");
        std::fs::write(&target, "before").unwrap();
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let args = serde_json::json!({"path": target, "content": "model"}).to_string();
        run_journaled_write(&journal, &policy, "model edit", &args).await;

        let mut permissions = std::fs::metadata(&target).unwrap().permissions();
        assert!(!permissions.readonly());
        permissions.set_readonly(true);
        std::fs::set_permissions(&target, permissions).unwrap();

        let error = journal.undo_last(&policy).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("identity, metadata, or contents"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "model");
        assert_eq!(journal.undo_depth(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn journal_undo_refuses_recreated_inode_with_identical_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("inode-conflict.txt");
        std::fs::write(&target, "before").unwrap();
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let args = serde_json::json!({"path": target, "content": "model"}).to_string();
        run_journaled_write(&journal, &policy, "model edit", &args).await;

        let old_identity = file_identity(&std::fs::metadata(&target).unwrap());
        std::fs::remove_file(&target).unwrap();
        std::fs::write(&target, "model").unwrap();
        assert_ne!(
            old_identity,
            file_identity(&std::fs::metadata(&target).unwrap())
        );

        let error = journal.undo_last(&policy).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("identity, metadata, or contents"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "model");
        assert_eq!(journal.undo_depth(), 1);
    }

    #[test]
    fn multi_file_undo_rolls_back_prior_restore_when_later_restore_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        std::fs::write(&first, "first-before").unwrap();
        std::fs::write(&second, "second-before").unwrap();
        let first_before = read_snapshot_limited(&first, 32).unwrap().unwrap();
        let second_before = read_snapshot_limited(&second, 32).unwrap().unwrap();
        let first_after = atomic_replace(
            &first,
            b"first-after",
            Some(first_before.permissions.clone()),
        )
        .unwrap();
        let second_after = atomic_replace(
            &second,
            b"second-after",
            Some(second_before.permissions.clone()),
        )
        .unwrap();

        // Recreate the first path with identical bytes. Applying in reverse
        // restores second first, then detects first's stale identity.
        let _replacement = atomic_replace(
            &first,
            b"first-after",
            Some(first_after.permissions.clone()),
        )
        .unwrap();
        let stale_second_after = second_after.clone();
        let mut changes = vec![
            JournalChange {
                path: first.clone(),
                before: Some(first_before),
                after: first_after,
            },
            JournalChange {
                path: second.clone(),
                before: Some(second_before),
                after: second_after,
            },
        ];

        let targets = vec![
            MutationTarget::pin(&first).unwrap(),
            MutationTarget::pin(&second).unwrap(),
        ];
        let error = apply_undo_transaction(&mut changes, &targets).unwrap_err();
        assert!(error.message.contains("prior restores were rolled back"));
        assert!(error.rollback_complete);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "first-after");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "second-after");
        let current_second = read_snapshot_limited(&second, 32).unwrap().unwrap();
        assert!(changes[1].after.matches(&current_second));
        assert!(!stale_second_after.matches(&changes[1].after));
    }

    #[tokio::test]
    async fn late_cancelled_write_cannot_join_a_newer_foreground_turn() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cancelled.txt");
        std::fs::write(&target, "before").unwrap();
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let cancelled = journal.begin_attributed_turn("cancelled write");
        let (lease, attribution) = journal
            .scope_turn(&cancelled.token, async { journal.begin_mutation() })
            .await;
        let mutation_target = MutationTarget::pin(&target).unwrap();
        let before = mutation_target.read_snapshot_limited(32).unwrap().unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let thread_journal = journal.clone();
        let written = target.clone();
        let worker = std::thread::spawn(move || {
            release_rx.recv().unwrap();
            let after = commit_whole_file(&mutation_target, b"after", Some(&before)).unwrap();
            thread_journal.record(attribution, written, Some(before), after);
            drop(lease);
        });

        assert!(journal.finish_attributed_turn(&cancelled.token).is_none());
        let newer = journal.begin_attributed_turn("newer foreground");
        release_tx.send(()).unwrap();
        journal.drain_mutations().await;
        worker.join().unwrap();
        assert!(journal.finish_attributed_turn(&newer.token).is_none());
        assert_eq!(journal.undo_depth(), 1);
        let latest = journal
            .inner
            .lock()
            .unwrap()
            .history
            .back()
            .cloned()
            .unwrap();
        assert!(latest.label.contains("cancelled write"));
        assert!(latest.label.contains("late file mutation"));
        journal.undo_last(&policy).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "before");
    }

    #[tokio::test]
    async fn spawned_background_write_gets_a_standalone_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("background.txt");
        let policy = Arc::new(
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap(),
        );
        let journal = WriteJournal::new();
        let foreground = journal.begin_attributed_turn("foreground");
        let task_policy = policy.clone();
        let task_journal = journal.clone();
        let args = serde_json::json!({"path": target, "content": "background"}).to_string();
        journal
            .scope_turn(&foreground.token, async move {
                tokio::spawn(async move {
                    write_file_run_with_journal(&task_policy, Some(&task_journal), &args)
                        .await
                        .unwrap();
                })
                .await
                .unwrap();
            })
            .await;
        journal.drain_mutations().await;

        assert!(journal.finish_attributed_turn(&foreground.token).is_none());
        assert_eq!(journal.undo_depth(), 1);
        let latest = journal
            .inner
            .lock()
            .unwrap()
            .history
            .back()
            .cloned()
            .unwrap();
        assert_eq!(latest.label, "background tool write");
        journal.undo_last(&policy).await.unwrap();
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn interleaved_background_write_preserves_same_file_undo_order() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("interleaved.txt");
        std::fs::write(&target, "original").unwrap();
        let policy = Arc::new(
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap(),
        );
        let journal = WriteJournal::new();
        let foreground = journal.begin_attributed_turn("foreground");
        journal
            .scope_turn(&foreground.token, async {
                let first =
                    serde_json::json!({"path": target, "content": "foreground-1"}).to_string();
                write_file_run_with_journal(&policy, Some(&journal), &first)
                    .await
                    .unwrap();

                let background_policy = policy.clone();
                let background_journal = journal.clone();
                let background =
                    serde_json::json!({"path": target, "content": "background"}).to_string();
                tokio::spawn(async move {
                    write_file_run_with_journal(
                        &background_policy,
                        Some(&background_journal),
                        &background,
                    )
                    .await
                    .unwrap();
                })
                .await
                .unwrap();

                let last =
                    serde_json::json!({"path": target, "content": "foreground-2"}).to_string();
                write_file_run_with_journal(&policy, Some(&journal), &last)
                    .await
                    .unwrap();
            })
            .await;

        let asynchronous = journal.drain_and_take_reports().await;
        assert_eq!(asynchronous.len(), 2);
        assert_eq!(asynchronous[0].label, "foreground");
        assert_eq!(asynchronous[1].label, "background tool write");
        let final_segment = journal.finish_attributed_turn(&foreground.token).unwrap();
        assert!(asynchronous[0].id < asynchronous[1].id);
        assert!(asynchronous[1].id < final_segment.id);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "foreground-2");

        journal.undo_last(&policy).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "background");
        journal.undo_last(&policy).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "foreground-1");
        journal.undo_last(&policy).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
    }

    #[tokio::test]
    async fn journal_coalesces_multiple_writes_to_one_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("multi.txt");
        std::fs::write(&target, "original").unwrap();
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let start = journal.begin_attributed_turn("multiple writes");
        journal
            .scope_turn(&start.token, async {
                for content in ["first", "second"] {
                    let args = serde_json::json!({"path": target, "content": content}).to_string();
                    write_file_run_with_journal(&policy, Some(&journal), &args)
                        .await
                        .unwrap();
                }
            })
            .await;
        journal.drain_mutations().await;
        let report = journal.finish_attributed_turn(&start.token).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");

        journal.undo_last(&policy).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
    }
}
