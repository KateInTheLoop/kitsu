//! Durable local chat-session snapshots.
//!
//! Kitsu does not serialize auth configuration or permission grants. Session
//! transcripts do contain user/model/tool text, however, and that text may
//! itself include sensitive material. Permission grants are intentionally not
//! restored: resuming context must not silently restore filesystem authority.

#[cfg(unix)]
use std::ffi::{CStr, CString};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{SecondsFormat, Utc};
use kitsu_providers::ChatMessage;
use serde::{Deserialize, Serialize};

const SESSION_VERSION: u32 = 1;
const MAX_SESSION_BYTES: u64 = 16 * 1024 * 1024;
const ARCHIVED_DIR_NAME: &str = "archived";
const LOCKS_DIR_NAME: &str = ".locks";
const ARTIFACTS_SUFFIX: &str = ".artifacts";
static SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    /// Deterministic lease failure for reset atomicity tests. Thread-local
    /// state keeps parallel session tests isolated from one another.
    static FAIL_NEXT_SESSION_LEASE: std::cell::Cell<bool> = const {
        std::cell::Cell::new(false)
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub version: u32,
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub directory: String,
    pub gateway: String,
    pub model: String,
    /// Optional human-memorable name (cycle-13 session audit). Resolvable in
    /// `--resume <name>`; absent for unnamed sessions and in old snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_context: Option<String>,
    pub messages: Vec<ChatMessage>,
}

/// Exact lineage returned by a successful durable-session fork. Selectors such
/// as `last` are resolved before cloning so callers can report the immutable
/// source identity alongside the new snapshot.
#[derive(Debug, Clone)]
pub struct SessionFork {
    pub source_session_id: String,
    pub session: SavedSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub updated_at: String,
    pub model: String,
    pub gateway: String,
    pub directory: String,
    pub preview: String,
    pub turns: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionExportFormat {
    Markdown,
    Json,
}

#[derive(Debug)]
pub struct SessionStore {
    dir: PathBuf,
    directory: PrivateDirectory,
    id: String,
    created_at: String,
    name: Option<String>,
    lease: SessionLease,
}

/// Evidence for one atomically installed session snapshot. On Unix the file
/// descriptor is the exact inode created by the commit and remains valid even
/// if another same-user process immediately renames, hard-links, or unlinks the
/// visible snapshot path.
#[derive(Debug)]
pub struct SnapshotCommit {
    path: PathBuf,
    #[cfg(unix)]
    file: std::fs::File,
}

impl SnapshotCommit {
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(unix)]
    pub fn file(&self) -> &std::fs::File {
        &self.file
    }
}

/// A session snapshot save failed. If installation completed before a cleanup
/// or durability error, Unix callers still receive the exact installed inode so
/// private-identity bookkeeping cannot lose the file merely because the save is
/// reported as unsuccessful.
#[derive(Debug)]
pub struct SnapshotSaveError {
    message: String,
    installed_commit: Option<SnapshotCommit>,
}

impl SnapshotSaveError {
    pub fn installed_commit(&self) -> Option<&SnapshotCommit> {
        self.installed_commit.as_ref()
    }
}

impl std::fmt::Display for SnapshotSaveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SnapshotSaveError {}

/// Failure while installing a user-requested, host-owned output file. The
/// destination normally remains unchanged on failure; `installed` is true only
/// when the atomic rename completed and a later durability sync failed.
#[derive(Debug)]
pub(crate) struct OutputFileWriteError {
    message: String,
    pub(crate) installed: bool,
}

impl std::fmt::Display for OutputFileWriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OutputFileWriteError {}

/// A process-wide/session-wide exclusion lease. Lock files remain in place so
/// dropping one lease can never unlink the inode while another process is
/// waiting on or has acquired it.
#[derive(Debug)]
struct SessionLease {
    #[cfg(any(unix, windows))]
    _file: std::fs::File,
    #[cfg(not(any(unix, windows)))]
    directory: PathBuf,
}

/// A session namespace pinned for the lifetime of an operation (and, for an
/// active store, for the lifetime of the store). On Unix every child lookup is
/// relative to this descriptor, so renaming or replacing a pathname ancestor
/// cannot redirect later reads, writes, locks, or removals.
#[derive(Debug)]
struct PrivateDirectory {
    path: PathBuf,
    #[cfg(unix)]
    file: std::fs::File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivateEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn fresh_id() -> String {
    let timestamp = Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| Utc::now().timestamp_millis().saturating_mul(1_000_000));
    let sequence = SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("s-{timestamp}-{}-{sequence}", std::process::id())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// A user-chosen session name: short, filesystem-safe, and unambiguous with
/// the selector vocabulary (`last`, `auto`) and generated ids (`s-…`).
pub fn valid_session_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > 40 {
        return Err("session name must be 1-40 characters".to_string());
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("session name may contain only letters, digits, '-' and '_'".to_string());
    }
    if name == "last" || name == "auto" {
        return Err(format!("'{name}' is a reserved resume selector"));
    }
    if name.starts_with("s-") {
        return Err("session names starting with 's-' would shadow session ids".to_string());
    }
    Ok(())
}

/// Resolve a resume selector to a session id: `last`/empty → newest with a
/// real turn; `auto` → newest whose recorded working directory is the current
/// one; a valid existing id → itself; otherwise an exact match on session
/// NAME (ambiguity is an error listing the candidate ids).
fn resolve_selector(
    summaries: &[SessionSummary],
    selector: &str,
    excluded_id: Option<&str>,
) -> Result<String, String> {
    if selector == "last" || selector.is_empty() {
        return preferred_last_id(summaries, excluded_id).ok_or_else(|| {
            "no saved sessions — start one with kitsu chat --session (add --name <name> for a memorable handle) or /new".to_string()
        });
    }
    if selector == "auto" {
        let cwd = std::env::current_dir()
            .map_err(|error| format!("cannot resolve the current directory: {error}"))?
            .display()
            .to_string();
        return summaries
            .iter()
            .filter(|summary| excluded_id != Some(summary.id.as_str()))
            .find(|summary| summary.directory == cwd)
            .map(|summary| summary.id.clone())
            .ok_or_else(|| {
                format!("no saved session recorded in this directory ({cwd}); try --resume last or /sessions")
            });
    }
    if valid_id(selector) && summaries.iter().any(|summary| summary.id == selector) {
        return Ok(selector.to_string());
    }
    let named: Vec<&SessionSummary> = summaries
        .iter()
        .filter(|summary| summary.name.as_deref() == Some(selector))
        .collect();
    match named.as_slice() {
        [only] => Ok(only.id.clone()),
        // Only id-SHAPED selectors may fall through to a direct read (they can
        // address snapshots outside the live listing). A mistyped NAME must
        // hit the friendly error below, never a raw filesystem path.
        [] if selector.starts_with("s-") && valid_id(selector) => Ok(selector.to_string()),
        [] => Err(format!(
            "no session named '{selector}' — see /sessions or `kitsu sessions`"
        )),
        many => Err(format!(
            "session name '{selector}' is ambiguous; use an id: {}",
            many.iter()
                .map(|summary| summary.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn exact_id<'a>(selector: &'a str, operation: &str) -> Result<&'a str, String> {
    if selector.is_empty() || selector == "last" {
        return Err(format!(
            "{operation} requires an exact session id; 'last' is not accepted"
        ));
    }
    if !selector.starts_with("s-") {
        return Err(format!(
            "{operation} requires an exact session id — if '{selector}' is a session name, find its id with `kitsu sessions {selector}`"
        ));
    }
    if !valid_id(selector) {
        return Err(format!("invalid session id '{selector}'"));
    }
    Ok(selector)
}

#[cfg(not(unix))]
fn prepare_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create session directory {}: {error}", dir.display()))?;
    let metadata = std::fs::symlink_metadata(dir).map_err(|error| {
        format!(
            "cannot inspect session directory {}: {error}",
            dir.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(format!(
            "session directory '{}' is not a regular directory",
            dir.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!("cannot secure session directory {}: {error}", dir.display())
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn c_name(name: &OsStr) -> std::io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "filesystem name contains a NUL byte",
        )
    })
}

#[cfg(unix)]
fn openat_file(
    directory: &std::fs::File,
    name: &OsStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> std::io::Result<std::fs::File> {
    let name = c_name(name)?;
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, mode) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
}

#[cfg(unix)]
fn open_directory_at(directory: &std::fs::File, name: &OsStr) -> std::io::Result<std::fs::File> {
    openat_file(
        directory,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )
}

#[cfg(unix)]
fn mkdirat(directory: &std::fs::File, name: &OsStr) -> std::io::Result<()> {
    let name = c_name(name)?;
    let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn unlinkat(directory: &std::fs::File, name: &OsStr) -> std::io::Result<()> {
    let name = c_name(name)?;
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn remove_dir_at(directory: &std::fs::File, name: &OsStr) -> std::io::Result<()> {
    let name = c_name(name)?;
    let result =
        unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn renameat(
    source_dir: &std::fs::File,
    source: &OsStr,
    target_dir: &std::fs::File,
    target: &OsStr,
) -> std::io::Result<()> {
    let source = c_name(source)?;
    let target = c_name(target)?;
    let result = unsafe {
        libc::renameat(
            source_dir.as_raw_fd(),
            source.as_ptr(),
            target_dir.as_raw_fd(),
            target.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn linkat(
    source_dir: &std::fs::File,
    source: &OsStr,
    target_dir: &std::fs::File,
    target: &OsStr,
) -> std::io::Result<()> {
    let source = c_name(source)?;
    let target = c_name(target)?;
    let result = unsafe {
        libc::linkat(
            source_dir.as_raw_fd(),
            source.as_ptr(),
            target_dir.as_raw_fd(),
            target.as_ptr(),
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
fn directory_open_error(path: &Path, context: &str, error: std::io::Error) -> String {
    if matches!(
        error.raw_os_error(),
        Some(libc::ELOOP) | Some(libc::ENOTDIR)
    ) {
        format!(
            "{context} directory '{}' is not a regular directory",
            path.display()
        )
    } else {
        format!(
            "cannot inspect {context} directory {}: {error}",
            path.display()
        )
    }
}

#[cfg(unix)]
fn open_directory_path(path: &Path, create: bool) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Component;

    // Resolve relative paths against the process's physical cwd and collapse
    // `..` lexically before opening anything. Otherwise a rename of a directory
    // already opened by the walk could change what a later openat("..") means.
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut components = Vec::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                let _ = components.pop();
            }
            Component::Normal(name) => components.push(name),
        }
    }

    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open("/")?;

    for name in components {
        match open_directory_at(&directory, name) {
            Ok(child) => directory = child,
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                match mkdirat(&directory, name) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                directory = open_directory_at(&directory, name)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(directory)
}

impl PrivateDirectory {
    fn prepare(path: &Path, context: &str) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let file = open_directory_path(path, true)
                .map_err(|error| directory_open_error(path, context, error))?;
            file.set_permissions(std::fs::Permissions::from_mode(0o700))
                .map_err(|error| {
                    format!(
                        "cannot secure {context} directory {}: {error}",
                        path.display()
                    )
                })?;
            Ok(Self {
                path: path.to_path_buf(),
                file,
            })
        }

        #[cfg(not(unix))]
        {
            let _ = context;
            prepare_dir(path)?;
            Ok(Self {
                path: path.to_path_buf(),
            })
        }
    }

    fn open(path: &Path, context: &str) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let file = open_directory_path(path, false)
                .map_err(|error| directory_open_error(path, context, error))?;
            Ok(Self {
                path: path.to_path_buf(),
                file,
            })
        }

        #[cfg(not(unix))]
        {
            existing_regular_dir(path, context)?;
            Ok(Self {
                path: path.to_path_buf(),
            })
        }
    }

    fn open_optional(path: &Path, context: &str) -> Result<Option<Self>, String> {
        #[cfg(unix)]
        {
            match open_directory_path(path, false) {
                Ok(file) => Ok(Some(Self {
                    path: path.to_path_buf(),
                    file,
                })),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(directory_open_error(path, context, error)),
            }
        }

        #[cfg(not(unix))]
        {
            match std::fs::symlink_metadata(path) {
                Ok(metadata)
                    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() =>
                {
                    Err(format!(
                        "{context} directory '{}' is not a regular directory",
                        path.display()
                    ))
                }
                Ok(_) => Ok(Some(Self {
                    path: path.to_path_buf(),
                })),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(format!(
                    "cannot inspect {context} directory {}: {error}",
                    path.display()
                )),
            }
        }
    }

    fn prepare_child(&self, name: &OsStr, context: &str) -> Result<Self, String> {
        let path = self.path.join(name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let file = match open_directory_at(&self.file, name) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match mkdirat(&self.file, name) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(directory_open_error(&path, context, error)),
                    }
                    open_directory_at(&self.file, name)
                        .map_err(|error| directory_open_error(&path, context, error))?
                }
                Err(error) => return Err(directory_open_error(&path, context, error)),
            };
            file.set_permissions(std::fs::Permissions::from_mode(0o700))
                .map_err(|error| {
                    format!(
                        "cannot secure {context} directory {}: {error}",
                        path.display()
                    )
                })?;
            Ok(Self { path, file })
        }

        #[cfg(not(unix))]
        {
            let _ = context;
            prepare_dir(&path)?;
            Ok(Self { path })
        }
    }

    fn open_child(&self, name: &OsStr, context: &str) -> Result<Self, String> {
        let path = self.path.join(name);
        #[cfg(unix)]
        {
            let file = open_directory_at(&self.file, name)
                .map_err(|error| directory_open_error(&path, context, error))?;
            Ok(Self { path, file })
        }

        #[cfg(not(unix))]
        {
            existing_regular_dir(&path, context)?;
            Ok(Self { path })
        }
    }

    fn open_optional_child(&self, name: &OsStr, context: &str) -> Result<Option<Self>, String> {
        match self.entry_kind(name) {
            Ok(PrivateEntryKind::Directory) => self.open_child(name, context).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Ok(_) => Err(format!(
                "{context} directory '{}' is not a regular directory",
                self.path.join(name).display()
            )),
            Err(error) => Err(format!(
                "cannot inspect {context} directory {}: {error}",
                self.path.join(name).display()
            )),
        }
    }

    fn create_child_exclusive(&self, name: &OsStr, context: &str) -> Result<Self, String> {
        let path = self.path.join(name);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            mkdirat(&self.file, name).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    format!(
                        "{context} directory '{}' already exists; refusing to overwrite it",
                        path.display()
                    )
                } else {
                    format!(
                        "cannot create {context} directory {}: {error}",
                        path.display()
                    )
                }
            })?;
            let file = match open_directory_at(&self.file, name) {
                Ok(file) => file,
                Err(error) => {
                    let _ = remove_dir_at(&self.file, name);
                    return Err(format!(
                        "cannot open new {context} directory {}: {error}",
                        path.display()
                    ));
                }
            };
            if let Err(error) = file.set_permissions(std::fs::Permissions::from_mode(0o700)) {
                drop(file);
                let _ = remove_dir_at(&self.file, name);
                return Err(format!(
                    "cannot secure {context} directory {}: {error}",
                    path.display()
                ));
            }
            Ok(Self { path, file })
        }

        #[cfg(not(unix))]
        {
            std::fs::create_dir(&path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    format!(
                        "{context} directory '{}' already exists; refusing to overwrite it",
                        path.display()
                    )
                } else {
                    format!(
                        "cannot create {context} directory {}: {error}",
                        path.display()
                    )
                }
            })?;
            Ok(Self { path })
        }
    }

    fn entry_kind(&self, name: &OsStr) -> std::io::Result<PrivateEntryKind> {
        #[cfg(unix)]
        {
            let name = c_name(name)?;
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            let result = unsafe {
                libc::fstatat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mode = unsafe { metadata.assume_init() }.st_mode & libc::S_IFMT;
            Ok(if mode == libc::S_IFDIR {
                PrivateEntryKind::Directory
            } else if mode == libc::S_IFREG {
                PrivateEntryKind::File
            } else if mode == libc::S_IFLNK {
                PrivateEntryKind::Symlink
            } else {
                PrivateEntryKind::Other
            })
        }

        #[cfg(not(unix))]
        {
            let metadata = std::fs::symlink_metadata(self.path.join(name))?;
            let file_type = metadata.file_type();
            Ok(if file_type.is_dir() {
                PrivateEntryKind::Directory
            } else if file_type.is_file() {
                PrivateEntryKind::File
            } else if file_type.is_symlink() {
                PrivateEntryKind::Symlink
            } else {
                PrivateEntryKind::Other
            })
        }
    }

    fn open_regular_file(&self, name: &OsStr, context: &str) -> Result<std::fs::File, String> {
        let path = self.path.join(name);

        #[cfg(unix)]
        let file = openat_file(
            &self.file,
            name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        )
        .map_err(|error| format!("cannot open {context} file '{}': {error}", path.display()))?;

        #[cfg(not(unix))]
        let file = {
            if self.entry_kind(name).map_err(|error| {
                format!(
                    "cannot inspect {context} file '{}': {error}",
                    path.display()
                )
            })? != PrivateEntryKind::File
            {
                return Err(format!(
                    "{context} file '{}' is not a regular file",
                    path.display()
                ));
            }
            std::fs::File::open(&path).map_err(|error| {
                format!("cannot open {context} file '{}': {error}", path.display())
            })?
        };

        let metadata = file.metadata().map_err(|error| {
            format!(
                "cannot inspect opened {context} file '{}': {error}",
                path.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "{context} file '{}' is not a regular file",
                path.display()
            ));
        }
        Ok(file)
    }

    fn create_regular_file(&self, name: &OsStr, context: &str) -> Result<std::fs::File, String> {
        let path = self.path.join(name);

        #[cfg(unix)]
        {
            openat_file(
                &self.file,
                name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    format!(
                        "{context} file '{}' already exists; refusing to overwrite it",
                        path.display()
                    )
                } else {
                    format!("cannot create {context} file '{}': {error}", path.display())
                }
            })
        }

        #[cfg(not(unix))]
        {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        format!(
                            "{context} file '{}' already exists; refusing to overwrite it",
                            path.display()
                        )
                    } else {
                        format!("cannot create {context} file '{}': {error}", path.display())
                    }
                })
        }
    }

    fn remove_empty_child(&self, name: &OsStr) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            remove_dir_at(&self.file, name)
        }

        #[cfg(not(unix))]
        {
            std::fs::remove_dir(self.path.join(name))
        }
    }

    fn sync(&self, context: &str) -> Result<(), String> {
        #[cfg(unix)]
        {
            #[cfg(test)]
            if fail_next_private_directory_sync() {
                return Err(format!(
                    "cannot sync {context} directory: injected post-install failure"
                ));
            }
            self.file
                .sync_all()
                .map_err(|error| format!("cannot sync {context} directory: {error}"))
        }

        #[cfg(not(unix))]
        {
            sync_directory(&self.path, context)
        }
    }

    fn entry_names(&self, context: &str) -> Result<Vec<OsString>, String> {
        #[cfg(unix)]
        {
            // Opening `.` gives this traversal an independent directory offset;
            // dup(2) would share an offset and make repeated enumeration incomplete.
            let fd = open_directory_at(&self.file, OsStr::new("."))
                .map_err(|error| {
                    format!(
                        "cannot list {context} directory {}: {error}",
                        self.path.display()
                    )
                })?
                .into_raw_fd();
            let stream = unsafe { libc::fdopendir(fd) };
            if stream.is_null() {
                let error = std::io::Error::last_os_error();
                unsafe {
                    libc::close(fd);
                }
                return Err(format!(
                    "cannot list {context} directory {}: {error}",
                    self.path.display()
                ));
            }

            let mut names = Vec::new();
            loop {
                let entry = unsafe { libc::readdir(stream) };
                if entry.is_null() {
                    break;
                }
                let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
                names.push(OsString::from_vec(bytes.to_vec()));
            }
            unsafe {
                libc::closedir(stream);
            }
            Ok(names)
        }

        #[cfg(not(unix))]
        {
            std::fs::read_dir(&self.path)
                .map_err(|error| {
                    format!(
                        "cannot list {context} directory {}: {error}",
                        self.path.display()
                    )
                })
                .map(|entries| entries.flatten().map(|entry| entry.file_name()).collect())
        }
    }
}

fn artifacts_name(id: &str) -> String {
    format!("{id}{ARTIFACTS_SUFFIX}")
}

fn is_dot_entry(name: &OsStr) -> bool {
    name == OsStr::new(".") || name == OsStr::new("..")
}

fn unsafe_artifact_entry(directory: &PrivateDirectory, name: &OsStr, context: &str) -> String {
    format!(
        "{context} entry '{}' is not a regular file or directory",
        directory.path.join(name).display()
    )
}

fn validate_artifact_tree(directory: &PrivateDirectory, context: &str) -> Result<(), String> {
    fn validate(directory: &PrivateDirectory, context: &str, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err(format!(
                "{context} directory '{}' exceeds the nesting limit",
                directory.path.display()
            ));
        }
        for name in directory.entry_names(context)? {
            if is_dot_entry(&name) {
                continue;
            }
            match directory.entry_kind(&name).map_err(|error| {
                format!(
                    "cannot inspect {context} entry '{}': {error}",
                    directory.path.join(&name).display()
                )
            })? {
                PrivateEntryKind::Directory => {
                    let child = directory.open_child(&name, context)?;
                    validate(&child, context, depth + 1)?;
                }
                PrivateEntryKind::File => {
                    drop(directory.open_regular_file(&name, context)?);
                }
                PrivateEntryKind::Symlink | PrivateEntryKind::Other => {
                    return Err(unsafe_artifact_entry(directory, &name, context));
                }
            }
        }
        Ok(())
    }

    validate(directory, context, 0)
}

fn remove_artifact_tree(
    parent: &PrivateDirectory,
    name: &OsStr,
    directory: &PrivateDirectory,
    context: &str,
) -> Result<(), String> {
    fn remove_contents(
        directory: &PrivateDirectory,
        context: &str,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err(format!(
                "{context} directory '{}' exceeds the nesting limit",
                directory.path.display()
            ));
        }
        for name in directory.entry_names(context)? {
            if is_dot_entry(&name) {
                continue;
            }
            match directory.entry_kind(&name).map_err(|error| {
                format!(
                    "cannot inspect {context} entry '{}': {error}",
                    directory.path.join(&name).display()
                )
            })? {
                PrivateEntryKind::Directory => {
                    let child = directory.open_child(&name, context)?;
                    remove_contents(&child, context, depth + 1)?;
                    directory.remove_empty_child(&name).map_err(|error| {
                        format!(
                            "cannot remove {context} directory '{}': {error}",
                            directory.path.join(&name).display()
                        )
                    })?;
                }
                // unlinkat/remove_file removes the entry itself and never
                // follows a symlink. Validation rejects these in source trees,
                // but accepting them here keeps rollback safe under a race.
                PrivateEntryKind::File | PrivateEntryKind::Symlink | PrivateEntryKind::Other => {
                    directory.remove_file(&name).map_err(|error| {
                        format!(
                            "cannot remove {context} entry '{}': {error}",
                            directory.path.join(&name).display()
                        )
                    })?;
                }
            }
        }
        directory.sync(context)
    }

    remove_contents(directory, context, 0)?;
    parent.remove_empty_child(name).map_err(|error| {
        format!(
            "cannot remove {context} directory '{}': {error}",
            parent.path.join(name).display()
        )
    })?;
    parent.sync(context)
}

fn copy_artifact_tree_exclusive(
    source: &PrivateDirectory,
    target_parent: &PrivateDirectory,
    target_name: &OsStr,
    context: &str,
) -> Result<PrivateDirectory, String> {
    fn copy_contents(
        source: &PrivateDirectory,
        target: &PrivateDirectory,
        context: &str,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err(format!(
                "{context} directory '{}' exceeds the nesting limit",
                source.path.display()
            ));
        }
        for name in source.entry_names(context)? {
            if is_dot_entry(&name) {
                continue;
            }
            match source.entry_kind(&name).map_err(|error| {
                format!(
                    "cannot inspect {context} entry '{}': {error}",
                    source.path.join(&name).display()
                )
            })? {
                PrivateEntryKind::Directory => {
                    if depth >= 64 {
                        return Err(format!(
                            "{context} directory '{}' exceeds the nesting limit",
                            source.path.join(&name).display()
                        ));
                    }
                    let source_child = source.open_child(&name, context)?;
                    let target_child = target.create_child_exclusive(&name, context)?;
                    copy_contents(&source_child, &target_child, context, depth + 1)?;
                    target_child.sync(context)?;
                }
                PrivateEntryKind::File => {
                    let mut input = source.open_regular_file(&name, context)?;
                    let mut output = target.create_regular_file(&name, context)?;
                    std::io::copy(&mut input, &mut output).map_err(|error| {
                        format!(
                            "cannot copy {context} file '{}': {error}",
                            source.path.join(&name).display()
                        )
                    })?;
                    output.sync_all().map_err(|error| {
                        format!(
                            "cannot sync {context} file '{}': {error}",
                            target.path.join(&name).display()
                        )
                    })?;
                }
                PrivateEntryKind::Symlink | PrivateEntryKind::Other => {
                    return Err(unsafe_artifact_entry(source, &name, context));
                }
            }
        }
        target.sync(context)
    }

    let target = target_parent.create_child_exclusive(target_name, context)?;
    if let Err(error) = copy_contents(source, &target, context, 0) {
        return match remove_artifact_tree(target_parent, target_name, &target, context) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!(
                "{error}; incomplete {context} copy remains at '{}': {cleanup_error}",
                target.path.display()
            )),
        };
    }
    Ok(target)
}

fn ensure_artifact_target_absent(
    parent: &PrivateDirectory,
    name: &OsStr,
    context: &str,
) -> Result<(), String> {
    match parent.entry_kind(name) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(format!(
            "{context} directory '{}' already exists; refusing to overwrite it",
            parent.path.join(name).display()
        )),
        Err(error) => Err(format!(
            "cannot inspect {context} directory '{}': {error}",
            parent.path.join(name).display()
        )),
    }
}

fn paired_write_failure(
    failure: AtomicWriteFailure,
    artifact_parent: &PrivateDirectory,
    artifact_name: &OsStr,
    copied_artifacts: Option<&PrivateDirectory>,
    artifact_context: &str,
    primary_path: &Path,
) -> String {
    if failure.installed {
        return format!(
            "{}; complete recovery copy remains at '{}'",
            failure.message,
            primary_path.display()
        );
    }
    let Some(copied_artifacts) = copied_artifacts else {
        return failure.message;
    };
    match remove_artifact_tree(
        artifact_parent,
        artifact_name,
        copied_artifacts,
        artifact_context,
    ) {
        Ok(()) => failure.message,
        Err(cleanup_error) => format!(
            "{}; copied artifacts remain at '{}': {cleanup_error}",
            failure.message,
            copied_artifacts.path.display()
        ),
    }
}

fn exact_path_boundary_before(value: &str, index: usize) -> bool {
    index == 0
        || value[..index]
            .chars()
            .next_back()
            .map_or(true, |character| {
                !character.is_alphanumeric() && !matches!(character, '_' | '-' | '.' | '/' | '\\')
            })
}

fn exact_path_boundary_after(value: &str, index: usize) -> bool {
    index == value.len()
        || value[index..].chars().next().map_or(true, |character| {
            character.is_whitespace()
                || matches!(
                    character,
                    '/' | '\\' | '.' | ',' | ';' | ':' | ')' | ']' | '}' | '\'' | '"'
                )
        })
}

fn rewrite_exact_path(value: &mut String, old: &str, new: &str) -> usize {
    if old.is_empty() {
        return 0;
    }
    let mut output = String::with_capacity(value.len().saturating_add(new.len()));
    let mut cursor = 0;
    let mut replacements = 0;
    while let Some(relative) = value[cursor..].find(old) {
        let start = cursor + relative;
        let end = start + old.len();
        if exact_path_boundary_before(value, start) && exact_path_boundary_after(value, end) {
            output.push_str(&value[cursor..start]);
            output.push_str(new);
            cursor = end;
            replacements += 1;
        } else {
            output.push_str(&value[cursor..end]);
            cursor = end;
        }
    }
    output.push_str(&value[cursor..]);
    *value = output;
    replacements
}

fn rewrite_artifact_references(saved: &mut SavedSession, old: &Path, new: &Path) -> usize {
    let old = old.display().to_string();
    let new = new.display().to_string();
    let escaped_old = serde_json::to_string(&old).ok().and_then(|value| {
        value
            .get(1..value.len().saturating_sub(1))
            .map(str::to_string)
    });
    let escaped_new = serde_json::to_string(&new).ok().and_then(|value| {
        value
            .get(1..value.len().saturating_sub(1))
            .map(str::to_string)
    });
    let mut replacements = 0;
    for message in &mut saved.messages {
        replacements += rewrite_exact_path(&mut message.content, &old, &new);
        for call in &mut message.tool_calls {
            replacements += rewrite_exact_path(&mut call.args, &old, &new);
            if let (Some(escaped_old), Some(escaped_new)) =
                (escaped_old.as_deref(), escaped_new.as_deref())
            {
                if escaped_old != old {
                    replacements += rewrite_exact_path(&mut call.args, escaped_old, escaped_new);
                }
            }
        }
    }
    replacements
}

fn artifact_reference_count(saved: &SavedSession, path: &Path) -> usize {
    let mut copy = saved.clone();
    rewrite_artifact_references(&mut copy, path, path)
}

impl SessionLease {
    fn acquire(dir: &PrivateDirectory, id: &str) -> Result<Self, String> {
        if !valid_id(id) {
            return Err("cannot lease a session with an invalid id".to_string());
        }
        #[cfg(test)]
        if FAIL_NEXT_SESSION_LEASE.with(|failure| failure.replace(false)) {
            return Err("injected session lease failure".to_string());
        }
        let locks_dir = dir.prepare_child(OsStr::new(LOCKS_DIR_NAME), "session lock")?;
        let lock_name = format!("{id}.lock");
        let lock_path = locks_dir.path.join(&lock_name);

        #[cfg(windows)]
        match std::fs::symlink_metadata(&lock_path) {
            Ok(metadata)
                if metadata.file_type().is_symlink() || !metadata.file_type().is_file() =>
            {
                return Err(format!(
                    "session lock '{}' is not a regular file",
                    lock_path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot inspect session lock '{}': {error}",
                    lock_path.display()
                ));
            }
            _ => {}
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let file = openat_file(
                &locks_dir.file,
                OsStr::new(&lock_name),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o600,
            )
            .map_err(|error| {
                if error.raw_os_error() == Some(libc::ELOOP) {
                    format!(
                        "session lock '{}' is not a regular file",
                        lock_path.display()
                    )
                } else {
                    format!(
                        "cannot open session lock '{}': {error}",
                        lock_path.display()
                    )
                }
            })?;
            let metadata = file.metadata().map_err(|error| {
                format!(
                    "cannot inspect session lock '{}': {error}",
                    lock_path.display()
                )
            })?;
            if !metadata.file_type().is_file() {
                return Err(format!(
                    "session lock '{}' is not a regular file",
                    lock_path.display()
                ));
            }

            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || error.raw_os_error() == Some(libc::EAGAIN)
                {
                    return Err(format!(
                        "session '{id}' is already active in this or another process"
                    ));
                }
                return Err(format!(
                    "cannot lock session '{}' for exclusive use: {error}",
                    id
                ));
            }
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    format!(
                        "cannot secure session lock '{}': {error}",
                        lock_path.display()
                    )
                })?;
            Ok(Self { _file: file })
        }

        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;

            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0);
            let file = match options.open(&lock_path) {
                Ok(file) => file,
                Err(error) if matches!(error.raw_os_error(), Some(32) | Some(33)) => {
                    return Err(format!(
                        "session '{id}' is already active in this or another process"
                    ));
                }
                Err(error) => {
                    return Err(format!(
                        "cannot open session lock '{}': {error}",
                        lock_path.display()
                    ));
                }
            };
            let metadata = file.metadata().map_err(|error| {
                format!(
                    "cannot inspect session lock '{}': {error}",
                    lock_path.display()
                )
            })?;
            if !metadata.file_type().is_file() {
                return Err(format!(
                    "session lock '{}' is not a regular file",
                    lock_path.display()
                ));
            }
            Ok(Self { _file: file })
        }

        #[cfg(not(any(unix, windows)))]
        {
            match std::fs::create_dir(&lock_path) {
                Ok(()) => Ok(Self {
                    directory: lock_path,
                }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
                    "session '{id}' is already active in this or another process"
                )),
                Err(error) => Err(format!(
                    "cannot create session lock '{}': {error}",
                    lock_path.display()
                )),
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
impl Drop for SessionLease {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.directory);
    }
}

fn decode_saved_file(file: std::fs::File, path: &Path, id: &str) -> Result<SavedSession, String> {
    let opened = file
        .metadata()
        .map_err(|error| format!("cannot inspect session '{}': {error}", path.display()))?;
    if !opened.is_file() || opened.len() > MAX_SESSION_BYTES {
        return Err(format!(
            "session '{}' is not a safe regular file",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.take(MAX_SESSION_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read session '{}': {error}", path.display()))?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        return Err(format!(
            "session '{}' exceeds the 16 MiB safety limit",
            path.display()
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| format!("session '{}' is not UTF-8: {error}", path.display()))?;
    let saved: SavedSession = serde_json::from_str(&text)
        .map_err(|error| format!("cannot parse session '{}': {error}", path.display()))?;
    if saved.version != SESSION_VERSION {
        return Err(format!(
            "session '{}' uses unsupported format version {}",
            saved.id, saved.version
        ));
    }
    if saved.id != id || !valid_id(&saved.id) {
        return Err("session id does not match its filename".to_string());
    }
    Ok(saved)
}

#[cfg(not(unix))]
fn read_saved_file(path: &Path, id: &str) -> Result<SavedSession, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect session '{}': {error}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "session '{}' is not a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_SESSION_BYTES {
        return Err(format!(
            "session '{}' exceeds the 16 MiB safety limit",
            path.display()
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    let file = options
        .open(path)
        .map_err(|error| format!("cannot read session '{}': {error}", path.display()))?;
    decode_saved_file(file, path, id)
}

impl PrivateDirectory {
    fn read_saved(&self, id: &str) -> Result<SavedSession, String> {
        let name = format!("{id}.json");
        let path = self.path.join(&name);
        #[cfg(unix)]
        {
            let file = openat_file(
                &self.file,
                OsStr::new(&name),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0,
            )
            .map_err(|error| {
                if error.raw_os_error() == Some(libc::ELOOP) {
                    format!("session '{}' is not a regular file", path.display())
                } else {
                    format!("cannot read session '{}': {error}", path.display())
                }
            })?;
            decode_saved_file(file, &path, id)
        }

        #[cfg(not(unix))]
        {
            read_saved_file(&path, id)
        }
    }
}

#[cfg(not(unix))]
fn sync_directory(dir: &Path, context: &str) -> Result<(), String> {
    // Opening a directory is not supported on every platform. Where it is,
    // sync it so the atomic installation/removal is durable too.
    if let Ok(directory) = std::fs::File::open(dir) {
        directory
            .sync_all()
            .map_err(|error| format!("cannot sync {context} directory: {error}"))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn existing_regular_dir(dir: &Path, context: &str) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(dir).map_err(|error| {
        format!(
            "cannot inspect {context} directory {}: {error}",
            dir.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(format!(
            "{context} directory '{}' is not a regular directory",
            dir.display()
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct AtomicWriteFailure {
    message: String,
    installed: bool,
    #[cfg(unix)]
    file: Option<std::fs::File>,
}

#[derive(Debug)]
struct AtomicWriteCommit {
    #[cfg(unix)]
    file: std::fs::File,
}

impl AtomicWriteFailure {
    fn before_install(message: String) -> Self {
        Self {
            message,
            installed: false,
            #[cfg(unix)]
            file: None,
        }
    }

    #[cfg(any(not(unix), test))]
    fn after_install(message: String) -> Self {
        Self {
            message,
            installed: true,
            #[cfg(unix)]
            file: None,
        }
    }

    #[cfg(unix)]
    fn after_install_with_file(message: String, file: std::fs::File) -> Self {
        Self {
            message,
            installed: true,
            file: Some(file),
        }
    }

    fn into_snapshot_save_error(self, path: PathBuf) -> SnapshotSaveError {
        #[cfg(unix)]
        {
            let Self { message, file, .. } = self;
            SnapshotSaveError {
                message,
                installed_commit: file.map(|file| SnapshotCommit { path, file }),
            }
        }

        #[cfg(not(unix))]
        {
            let _ = path;
            SnapshotSaveError {
                message: self.message,
                installed_commit: None,
            }
        }
    }
}

/// Portable fallback for a complete same-directory write. `installed` records
/// whether an error happened after the destination became visible, which lets
/// multi-file session operations preserve a complete recovery copy.
#[cfg(not(unix))]
fn atomic_write_private_detailed(
    target: &Path,
    bytes: &[u8],
    replace: bool,
    context: &str,
) -> Result<AtomicWriteCommit, AtomicWriteFailure> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    existing_regular_dir(parent, context).map_err(AtomicWriteFailure::before_install)?;
    if target.file_name().is_none() {
        return Err(AtomicWriteFailure::before_install(format!(
            "cannot write {context} '{}': destination has no filename",
            target.display()
        )));
    }

    let temporary = parent.join(format!(".kitsu-{}.tmp", fresh_id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| {
            AtomicWriteFailure::before_install(format!(
                "cannot create temporary {context} for '{}': {error}",
                target.display()
            ))
        })?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(AtomicWriteFailure::before_install(format!(
            "cannot write {context} '{}': {error}",
            target.display()
        )));
    }
    drop(file);

    if replace {
        if let Err(error) = std::fs::rename(&temporary, target) {
            let _ = std::fs::remove_file(&temporary);
            return Err(AtomicWriteFailure::before_install(format!(
                "cannot finalize {context} '{}': {error}",
                target.display()
            )));
        }
    } else {
        if let Err(error) = std::fs::hard_link(&temporary, target) {
            let _ = std::fs::remove_file(&temporary);
            let message = if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "{context} '{}' already exists; refusing to overwrite it",
                    target.display()
                )
            } else {
                format!("cannot finalize {context} '{}': {error}", target.display())
            };
            return Err(AtomicWriteFailure::before_install(message));
        }
        if let Err(error) = std::fs::remove_file(&temporary) {
            return Err(AtomicWriteFailure::after_install(format!(
                "{context} '{}' was written, but its temporary link could not be removed: {error}",
                target.display()
            )));
        }
    }

    sync_directory(parent, context)
        .map_err(AtomicWriteFailure::after_install)
        .map(|()| AtomicWriteCommit {})
}

/// Descriptor-relative variant used for private session namespaces. The
/// temporary and destination names are interpreted only within `directory`.
fn atomic_write_private_in_detailed(
    directory: &PrivateDirectory,
    target_name: &OsStr,
    bytes: &[u8],
    replace: bool,
    context: &str,
) -> Result<AtomicWriteCommit, AtomicWriteFailure> {
    let target = directory.path.join(target_name);

    #[cfg(unix)]
    {
        let temporary_name = format!(".kitsu-{}.tmp", fresh_id());
        let mut file = openat_file(
            &directory.file,
            OsStr::new(&temporary_name),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
        .map_err(|error| {
            AtomicWriteFailure::before_install(format!(
                "cannot create temporary {context} for '{}': {error}",
                target.display()
            ))
        })?;
        if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            let _ = unlinkat(&directory.file, OsStr::new(&temporary_name));
            return Err(AtomicWriteFailure::before_install(format!(
                "cannot write {context} '{}': {error}",
                target.display()
            )));
        }
        if replace {
            if let Err(error) = renameat(
                &directory.file,
                OsStr::new(&temporary_name),
                &directory.file,
                target_name,
            ) {
                let _ = unlinkat(&directory.file, OsStr::new(&temporary_name));
                return Err(AtomicWriteFailure::before_install(format!(
                    "cannot finalize {context} '{}': {error}",
                    target.display()
                )));
            }
        } else {
            if let Err(error) = linkat(
                &directory.file,
                OsStr::new(&temporary_name),
                &directory.file,
                target_name,
            ) {
                let _ = unlinkat(&directory.file, OsStr::new(&temporary_name));
                let message = if error.kind() == std::io::ErrorKind::AlreadyExists {
                    format!(
                        "{context} '{}' already exists; refusing to overwrite it",
                        target.display()
                    )
                } else {
                    format!("cannot finalize {context} '{}': {error}", target.display())
                };
                return Err(AtomicWriteFailure::before_install(message));
            }
            if let Err(error) = unlinkat(&directory.file, OsStr::new(&temporary_name)) {
                return Err(AtomicWriteFailure::after_install_with_file(
                    format!(
                        "{context} '{}' was written, but its temporary link could not be removed: {error}",
                        target.display()
                    ),
                    file,
                ));
            }
        }
        if let Err(error) = directory.sync(context) {
            return Err(AtomicWriteFailure::after_install_with_file(error, file));
        }
        Ok(AtomicWriteCommit { file })
    }

    #[cfg(not(unix))]
    {
        atomic_write_private_detailed(&target, bytes, replace, context)
    }
}

/// Validate a future last-message destination without creating or replacing
/// anything. Parent components are opened without following symlinks on Unix;
/// an existing destination must be a regular file.
pub(crate) fn validate_output_file_target(target: &Path) -> Result<(), String> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = target.file_name().ok_or_else(|| {
        format!(
            "--output-last-message destination '{}' has no filename",
            target.display()
        )
    })?;
    let directory = PrivateDirectory::open(parent, "output-last-message")?;
    match directory.entry_kind(name) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(PrivateEntryKind::File) => Ok(()),
        Ok(_) => Err(format!(
            "--output-last-message destination '{}' is not a regular file",
            target.display()
        )),
        Err(error) => Err(format!(
            "cannot inspect --output-last-message destination '{}': {error}",
            target.display()
        )),
    }
}

/// Replace one user-selected output file atomically with the exact assistant
/// bytes. The same-directory temporary file is mode 0600 on Unix, synced before
/// a descriptor-relative rename, and the pinned parent directory is synced
/// afterward. No model tool participates in this write.
pub(crate) fn atomic_write_output_file(
    target: &Path,
    bytes: &[u8],
) -> Result<(), OutputFileWriteError> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = target.file_name().ok_or_else(|| OutputFileWriteError {
        message: format!(
            "--output-last-message destination '{}' has no filename",
            target.display()
        ),
        installed: false,
    })?;
    let directory = PrivateDirectory::open(parent, "output-last-message").map_err(|message| {
        OutputFileWriteError {
            message,
            installed: false,
        }
    })?;
    atomic_write_private_in_detailed(&directory, name, bytes, true, "output-last-message")
        .map(|_| ())
        .map_err(|failure| OutputFileWriteError {
            message: failure.message,
            installed: failure.installed,
        })
}

/// Read one small host-selected input through the same descriptor-pinned,
/// no-symlink path discipline used by private state. This does not grant the
/// model any filesystem authority over the source file.
pub(crate) fn read_bounded_host_file(
    target: &Path,
    max_bytes: u64,
    context: &str,
) -> Result<Vec<u8>, String> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .ok_or_else(|| format!("{context} path '{}' has no filename", target.display()))?;
    let directory = PrivateDirectory::open(parent, context)?;
    let file = directory.open_regular_file(name, context)?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "cannot inspect {context} file '{}': {error}",
            target.display()
        )
    })?;
    if metadata.len() > max_bytes {
        return Err(format!(
            "{context} file '{}' is {} bytes; limit is {max_bytes}",
            target.display(),
            metadata.len()
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {context} file '{}': {error}", target.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "{context} file '{}' grew beyond the {max_bytes} byte limit while it was read",
            target.display()
        ));
    }
    Ok(bytes)
}

impl PrivateDirectory {
    fn remove_file(&self, name: &OsStr) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            unlinkat(&self.file, name)
        }

        #[cfg(not(unix))]
        {
            std::fs::remove_file(self.path.join(name))
        }
    }
}

/// Read one bounded owner-private value from a protected namespace below the
/// config-scoped state directory. Credential code uses this narrow wrapper so
/// it inherits the session store's descriptor-relative no-follow traversal
/// without exposing path-based secret I/O.
pub(crate) fn read_private_value(
    state_dir: &Path,
    namespace: &OsStr,
    name: &OsStr,
    max_bytes: u64,
    context: &str,
) -> Result<Option<Vec<u8>>, String> {
    let Some(state) = PrivateDirectory::open_optional(state_dir, context)? else {
        return Ok(None);
    };
    let Some(directory) = state.open_optional_child(namespace, context)? else {
        return Ok(None);
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = directory.file.metadata().map_err(|error| {
            format!(
                "cannot inspect {context} directory '{}': {error}",
                directory.path.display()
            )
        })?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(format!(
                "{context} directory '{}' is not owner-only",
                directory.path.display()
            ));
        }
    }
    let path = directory.path.join(name);
    let kind = match directory.entry_kind(name) {
        Ok(kind) => kind,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot inspect {context} '{}': {error}",
                path.display()
            ))
        }
    };
    if kind != PrivateEntryKind::File {
        return Err(format!(
            "{context} '{}' is not a private regular file",
            path.display()
        ));
    }

    #[cfg(unix)]
    let file = openat_file(
        &directory.file,
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    )
    .map_err(|error| format!("cannot open {context} '{}': {error}", path.display()))?;
    #[cfg(not(unix))]
    let file = std::fs::OpenOptions::new()
        .read(true)
        .open(&path)
        .map_err(|error| format!("cannot open {context} '{}': {error}", path.display()))?;

    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {context} '{}': {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(format!(
            "{context} '{}' is not a bounded regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(format!(
                "{context} '{}' is not an owner-only single-link file",
                path.display()
            ));
        }
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(
        usize::try_from(metadata.len()).unwrap_or(0),
    ));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {context} '{}': {error}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!("{context} exceeds its safety limit"));
    }
    Ok(Some(std::mem::take(&mut *bytes)))
}

/// Atomically replace one bounded owner-private value beneath the config state
/// directory. The final file is `0600` and every traversed/created directory is
/// opened without following symlinks.
pub(crate) fn write_private_value(
    state_dir: &Path,
    namespace: &OsStr,
    name: &OsStr,
    bytes: &[u8],
    max_bytes: u64,
    context: &str,
) -> Result<(), String> {
    if bytes.len() as u64 > max_bytes {
        return Err(format!("{context} exceeds its safety limit"));
    }
    let state = PrivateDirectory::prepare(state_dir, context)?;
    let directory = state.prepare_child(namespace, context)?;
    atomic_write_private_in_detailed(&directory, name, bytes, true, context)
        .map(|_| ())
        .map_err(|failure| failure.message)
}

/// Remove one validated private value. Missing state/value is an idempotent
/// success; an unsafe leaf is refused rather than followed or silently hidden.
pub(crate) fn remove_private_value(
    state_dir: &Path,
    namespace: &OsStr,
    name: &OsStr,
    max_bytes: u64,
    context: &str,
) -> Result<bool, String> {
    if read_private_value(state_dir, namespace, name, max_bytes, context)?.is_none() {
        return Ok(false);
    }
    let state = PrivateDirectory::open(state_dir, context)?;
    let directory = state.open_child(namespace, context)?;
    directory
        .remove_file(name)
        .map_err(|error| format!("cannot remove {context}: {error}"))?;
    directory.sync(context)?;
    Ok(true)
}

fn encode_saved(saved: &SavedSession) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec_pretty(saved)
        .map_err(|error| format!("cannot encode session: {error}"))?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        return Err("session snapshot exceeds the 16 MiB safety limit".to_string());
    }
    Ok(bytes)
}

fn write_saved_file(
    dir: &PrivateDirectory,
    saved: &SavedSession,
    replace: bool,
) -> Result<(PathBuf, AtomicWriteCommit), SnapshotSaveError> {
    if !valid_id(&saved.id) {
        return Err(SnapshotSaveError {
            message: "saved session has an invalid id".to_string(),
            installed_commit: None,
        });
    }
    let name = format!("{}.json", saved.id);
    let target = dir.path.join(&name);
    let bytes = encode_saved(saved).map_err(|message| SnapshotSaveError {
        message,
        installed_commit: None,
    })?;
    let commit =
        atomic_write_private_in_detailed(dir, OsStr::new(&name), &bytes, replace, "session")
            .map_err(|error| error.into_snapshot_save_error(target.clone()))?;
    Ok((target, commit))
}

fn markdown_export(saved: &SavedSession) -> String {
    fn quoted(value: &str) -> String {
        serde_json::to_string(value).unwrap_or_else(|_| "\"<unavailable>\"".to_string())
    }

    fn indented(output: &mut String, value: &str) {
        if value.is_empty() {
            output.push_str("    (empty)\n");
            return;
        }
        for line in value.split('\n') {
            output.push_str("    ");
            output.push_str(line);
            output.push('\n');
        }
    }

    let mut output = String::from("# Kitsu session export\n\n");
    output.push_str(&format!("- Session ID: {}\n", quoted(&saved.id)));
    output.push_str(&format!("- Created: {}\n", quoted(&saved.created_at)));
    output.push_str(&format!("- Updated: {}\n", quoted(&saved.updated_at)));
    output.push_str(&format!("- Gateway: {}\n", quoted(&saved.gateway)));
    output.push_str(&format!("- Model: {}\n", quoted(&saved.model)));
    output.push_str(&format!("- Directory: {}\n", quoted(&saved.directory)));
    if let Some(project_context) = &saved.project_context {
        output.push_str(&format!("- Project context: {}\n", quoted(project_context)));
    }
    output.push_str("\n## Conversation\n");

    for (index, message) in saved.messages.iter().enumerate() {
        let role = match message.role {
            kitsu_providers::Role::System => "System",
            kitsu_providers::Role::User => "User",
            kitsu_providers::Role::Assistant => "Assistant",
            kitsu_providers::Role::Tool => "Tool",
        };
        output.push_str(&format!("\n### {}. {role}\n\n", index + 1));
        if let Some(call_id) = &message.tool_call_id {
            output.push_str(&format!("Tool call ID: {}\n\n", quoted(call_id)));
        }
        indented(&mut output, &message.content);
        if !message.tool_calls.is_empty() {
            output.push_str("\nTool calls:\n\n");
            let calls = serde_json::to_string_pretty(&message.tool_calls)
                .unwrap_or_else(|_| "[\"<unavailable>\"]".to_string());
            indented(&mut output, &calls);
        }
    }
    output
}

#[cfg(all(test, unix))]
thread_local! {
    static AFTER_SESSION_DIRECTORY_PINNED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
    static FAIL_NEXT_PRIVATE_DIRECTORY_SYNC: std::cell::Cell<bool> = const {
        std::cell::Cell::new(false)
    };
}

#[cfg(all(test, unix))]
fn after_session_directory_pinned() {
    AFTER_SESSION_DIRECTORY_PINNED.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(all(test, unix))]
fn fail_next_private_directory_sync() -> bool {
    FAIL_NEXT_PRIVATE_DIRECTORY_SYNC.with(|fail| fail.replace(false))
}

#[cfg(not(all(test, unix)))]
fn after_session_directory_pinned() {}

fn prepare_session_directory(path: &Path) -> Result<PrivateDirectory, String> {
    let directory = PrivateDirectory::prepare(path, "session")?;
    after_session_directory_pinned();
    Ok(directory)
}

fn open_session_directory(path: &Path) -> Result<PrivateDirectory, String> {
    let directory = PrivateDirectory::open(path, "session")?;
    after_session_directory_pinned();
    Ok(directory)
}

fn open_optional_session_directory(path: &Path) -> Result<Option<PrivateDirectory>, String> {
    let directory = PrivateDirectory::open_optional(path, "session")?;
    if directory.is_some() {
        after_session_directory_pinned();
    }
    Ok(directory)
}

fn list_in(directory: &PrivateDirectory) -> Result<Vec<SessionSummary>, String> {
    let names = directory.entry_names("session")?;

    let mut summaries = Vec::new();
    for name in names {
        let path = Path::new(&name);
        if path.extension().and_then(|part| part.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|part| part.to_str()) else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        let Ok(saved) = directory.read_saved(id) else {
            continue;
        };
        let turns = saved
            .messages
            .iter()
            .filter(|message| matches!(message.role, kitsu_providers::Role::User))
            .count();
        let preview = saved
            .messages
            .iter()
            .find(|message| matches!(message.role, kitsu_providers::Role::User))
            .map(|message| {
                let first = message.content.lines().next().unwrap_or("").trim();
                let mut preview: String = first.chars().take(48).collect();
                if first.chars().count() > 48 {
                    preview.push('…');
                }
                preview
            })
            .unwrap_or_else(|| "(no prompt yet)".to_string());
        summaries.push(SessionSummary {
            id: saved.id,
            name: saved.name,
            updated_at: saved.updated_at,
            model: saved.model,
            gateway: saved.gateway,
            directory: saved.directory,
            preview,
            turns,
        });
    }
    summaries.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    Ok(summaries)
}

/// Prefer the newest session that has at least one user turn. A freshly
/// created durable snapshot is useful for crash recovery, but it should not
/// displace the user's most recent real conversation when resolving `last`.
/// If every eligible snapshot is empty, retain the historical newest fallback.
fn preferred_last_id(summaries: &[SessionSummary], excluded_id: Option<&str>) -> Option<String> {
    let eligible = || {
        summaries
            .iter()
            .filter(|summary| excluded_id != Some(summary.id.as_str()))
    };
    eligible()
        .find(|summary| summary.turns > 0)
        .or_else(|| eligible().next())
        .map(|summary| summary.id.clone())
}

impl SessionStore {
    pub fn create(dir: PathBuf) -> Result<Self, String> {
        let directory = prepare_session_directory(&dir)?;
        let created_at = now();
        let id = fresh_id();
        let lease = SessionLease::acquire(&directory, &id)?;
        Ok(Self {
            dir,
            directory,
            id,
            created_at,
            name: None,
            lease,
        })
    }

    pub fn from_saved(dir: PathBuf, saved: &SavedSession) -> Result<Self, String> {
        if !valid_id(&saved.id) {
            return Err("saved session has an invalid id".to_string());
        }
        let directory = prepare_session_directory(&dir)?;
        let lease = SessionLease::acquire(&directory, &saved.id)?;
        // `load` intentionally does not hold a lease. Re-read after acquiring
        // one so a resume cannot resurrect or overwrite a snapshot changed by
        // another process between load and activation.
        let current = directory.read_saved(&saved.id)?;
        if encode_saved(&current)? != encode_saved(saved)? {
            return Err(format!(
                "session '{}' changed while it was being resumed; retry the resume",
                saved.id
            ));
        }
        Ok(Self {
            dir,
            directory,
            id: saved.id.clone(),
            created_at: saved.created_at.clone(),
            name: saved.name.clone(),
            lease,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Set or clear the session's human-memorable name. Validation is the
    /// caller's job via [`valid_session_name`]; persistence happens on the
    /// next save.
    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.dir.join(format!("{}.artifacts", self.id))
    }

    pub fn reset(&mut self) -> Result<(), String> {
        let id = fresh_id();
        // Acquire first. If this fails, the old identity and its lease remain
        // intact, allowing the caller to keep using the current session.
        let lease = SessionLease::acquire(&self.directory, &id)?;
        self.id = id;
        self.created_at = now();
        // Names are unique resume selectors for one saved conversation. A
        // fresh boundary must never make two sessions claim the same name.
        self.name = None;
        self.lease = lease;
        Ok(())
    }

    pub fn save(
        &self,
        directory: &Path,
        gateway: &str,
        model: &str,
        project_context: Option<&Path>,
        messages: &[ChatMessage],
    ) -> Result<SnapshotCommit, SnapshotSaveError> {
        let saved = SavedSession {
            version: SESSION_VERSION,
            id: self.id.clone(),
            created_at: self.created_at.clone(),
            updated_at: now(),
            directory: directory.display().to_string(),
            gateway: gateway.to_string(),
            model: model.to_string(),
            name: self.name.clone(),
            project_context: project_context.map(|path| path.display().to_string()),
            messages: messages.to_vec(),
        };
        let (path, commit) = write_saved_file(&self.directory, &saved, true)?;
        Ok(SnapshotCommit {
            path,
            #[cfg(unix)]
            file: commit.file,
        })
    }

    pub fn load(dir: &Path, selector: &str) -> Result<SavedSession, String> {
        let directory = open_session_directory(dir)?;
        let id = resolve_selector(&list_in(&directory)?, selector, None)?;
        directory.read_saved(&id)
    }

    /// Resolve an in-process session switch. `last` means the most recently
    /// updated *other* session; otherwise saving the active session immediately
    /// before resolution makes it select itself every time.
    pub fn load_other(
        dir: &Path,
        selector: &str,
        active_id: Option<&str>,
    ) -> Result<SavedSession, String> {
        let directory = open_session_directory(dir)?;
        let id = resolve_selector(&list_in(&directory)?, selector, active_id)?;
        if active_id == Some(id.as_str()) {
            return Err(format!("session '{selector}' is already active"));
        }
        directory.read_saved(&id)
    }

    /// Set or clear a saved session's name without touching its transcript or
    /// its `updated_at` (a rename must not hijack `last` resolution). The
    /// active session's next autosave wins over a concurrent rename — rename
    /// idle sessions, or use `--name` on the live one.
    pub fn rename(
        dir: &Path,
        selector: &str,
        new_name: Option<&str>,
    ) -> Result<SavedSession, String> {
        if let Some(name) = new_name {
            valid_session_name(name)?;
        }
        let directory = open_session_directory(dir)?;
        let summaries = list_in(&directory)?;
        let id = resolve_selector(&summaries, selector, None)?;
        if let Some(name) = new_name {
            if summaries
                .iter()
                .any(|summary| summary.id != id && summary.name.as_deref() == Some(name))
            {
                return Err(format!("another session is already named '{name}'"));
            }
        }
        let mut saved = directory.read_saved(&id)?;
        saved.name = new_name.map(str::to_string);
        write_saved_file(&directory, &saved, true).map_err(|error| error.to_string())?;
        Ok(saved)
    }

    /// Clone a saved conversation into a new, durable session. Only snapshot
    /// data is copied; permission grants live outside `SavedSession` and are
    /// therefore deliberately absent from the fork. Offloaded tool evidence is
    /// copied into the new session's sidecar and message pointers are rewritten
    /// to that independent copy. The exact source identity resolved from
    /// `selector` is retained for user-facing and machine-readable lineage
    /// receipts.
    pub fn fork_with_source(dir: &Path, selector: &str) -> Result<SessionFork, String> {
        let directory = open_session_directory(dir)?;
        let mut forked = if selector == "last" || selector.is_empty() {
            let id = preferred_last_id(&list_in(&directory)?, None).ok_or_else(|| {
                "no saved sessions — start one with kitsu chat --session (add --name <name> for a memorable handle) or /new".to_string()
            })?;
            directory.read_saved(&id)?
        } else if valid_id(selector) {
            directory.read_saved(selector)?
        } else {
            return Err(format!("invalid session id '{selector}'"));
        };
        let source_session_id = forked.id.clone();
        let source_artifacts_name = artifacts_name(&forked.id);
        let source_artifacts_path = directory.path.join(&source_artifacts_name);
        let source_artifacts = directory
            .open_optional_child(OsStr::new(&source_artifacts_name), "session artifacts")?;
        let referenced_artifacts = artifact_reference_count(&forked, &source_artifacts_path);
        if referenced_artifacts > 0 && source_artifacts.is_none() {
            return Err(format!(
                "session '{}' references missing artifact directory '{}'",
                forked.id,
                source_artifacts_path.display()
            ));
        }

        let timestamp = now();
        forked.id = fresh_id();
        // Names are unique resume selectors, not transcript metadata. Keeping
        // the source name on a second snapshot makes `--resume <name>`
        // ambiguous immediately after a successful fork.
        forked.name = None;
        forked.created_at = timestamp.clone();
        forked.updated_at = timestamp;
        let forked_artifacts_name = artifacts_name(&forked.id);
        let forked_artifacts_path = directory.path.join(&forked_artifacts_name);
        let forked_snapshot_name = format!("{}.json", forked.id);
        let forked_snapshot_path = directory.path.join(&forked_snapshot_name);
        rewrite_artifact_references(&mut forked, &source_artifacts_path, &forked_artifacts_path);
        let forked_bytes = encode_saved(&forked)?;
        let copied_artifacts = if let Some(source_artifacts) = source_artifacts.as_ref() {
            Some(copy_artifact_tree_exclusive(
                source_artifacts,
                &directory,
                OsStr::new(&forked_artifacts_name),
                "forked session artifacts",
            )?)
        } else {
            ensure_artifact_target_absent(
                &directory,
                OsStr::new(&forked_artifacts_name),
                "forked session artifacts",
            )?;
            None
        };
        atomic_write_private_in_detailed(
            &directory,
            OsStr::new(&forked_snapshot_name),
            &forked_bytes,
            false,
            "session",
        )
        .map_err(|failure| {
            paired_write_failure(
                failure,
                &directory,
                OsStr::new(&forked_artifacts_name),
                copied_artifacts.as_ref(),
                "forked session artifacts",
                &forked_snapshot_path,
            )
        })?;
        Ok(SessionFork {
            source_session_id,
            session: forked,
        })
    }

    /// Export a saved session without replacing an existing destination. When
    /// the snapshot has an artifact sidecar, a no-clobber sibling named
    /// `<destination-filename>.artifacts` is copied too and exported message
    /// pointers are rewritten to it. The primary path is installed only after
    /// the sidecar succeeds, with rollback if the primary install fails.
    pub fn export(
        dir: &Path,
        selector: &str,
        destination: &Path,
        format: SessionExportFormat,
    ) -> Result<PathBuf, String> {
        let directory = open_session_directory(dir)?;
        let mut saved = if selector == "last" || selector.is_empty() {
            let id = preferred_last_id(&list_in(&directory)?, None).ok_or_else(|| {
                "no saved sessions — start one with kitsu chat --session (add --name <name> for a memorable handle) or /new".to_string()
            })?;
            directory.read_saved(&id)?
        } else if valid_id(selector) {
            directory.read_saved(selector)?
        } else {
            return Err(format!("invalid session id '{selector}'"));
        };
        let source_artifacts_name = artifacts_name(&saved.id);
        let source_artifacts_path = directory.path.join(&source_artifacts_name);
        let source_artifacts = directory
            .open_optional_child(OsStr::new(&source_artifacts_name), "session artifacts")?;
        let referenced_artifacts = artifact_reference_count(&saved, &source_artifacts_path);
        if referenced_artifacts > 0 && source_artifacts.is_none() {
            return Err(format!(
                "session '{}' references missing artifact directory '{}'",
                saved.id,
                source_artifacts_path.display()
            ));
        }

        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let Some(name) = destination.file_name() else {
            return Err(format!(
                "cannot write session export '{}': destination has no filename",
                destination.display()
            ));
        };
        let destination_dir = PrivateDirectory::open(parent, "session export")?;
        let mut destination_artifacts_name = name.to_os_string();
        destination_artifacts_name.push(ARTIFACTS_SUFFIX);
        let destination_artifacts_path = destination.with_file_name(&destination_artifacts_name);
        rewrite_artifact_references(
            &mut saved,
            &source_artifacts_path,
            &destination_artifacts_path,
        );
        let bytes = match format {
            SessionExportFormat::Markdown => markdown_export(&saved).into_bytes(),
            SessionExportFormat::Json => encode_saved(&saved)?,
        };
        let copied_artifacts = if let Some(source_artifacts) = source_artifacts.as_ref() {
            Some(copy_artifact_tree_exclusive(
                source_artifacts,
                &destination_dir,
                &destination_artifacts_name,
                "session export artifacts",
            )?)
        } else {
            ensure_artifact_target_absent(
                &destination_dir,
                &destination_artifacts_name,
                "session export artifacts",
            )?;
            None
        };
        atomic_write_private_in_detailed(&destination_dir, name, &bytes, false, "session export")
            .map_err(|failure| {
            paired_write_failure(
                failure,
                &destination_dir,
                &destination_artifacts_name,
                copied_artifacts.as_ref(),
                "session export artifacts",
                destination,
            )
        })?;
        Ok(destination.to_path_buf())
    }

    /// Move an inactive snapshot under the store's private archive directory.
    /// The lease freezes autosaves and competing lifecycle operations. The
    /// copied archive is then installed atomically without clobbering before
    /// the live entry is removed, so an interruption can leave a recoverable
    /// duplicate but cannot lose the snapshot.
    pub fn archive(dir: &Path, selector: &str, active_id: Option<&str>) -> Result<PathBuf, String> {
        let id = exact_id(selector, "archive")?;
        if active_id == Some(id) {
            return Err(format!("cannot archive active session '{id}'"));
        }

        let directory = prepare_session_directory(dir)?;
        let _lease = SessionLease::acquire(&directory, id)?;
        let name = format!("{id}.json");
        let saved = directory.read_saved(id)?;
        let artifacts_name = artifacts_name(id);
        let artifacts_path = directory.path.join(&artifacts_name);
        let source_artifacts =
            directory.open_optional_child(OsStr::new(&artifacts_name), "session artifacts")?;
        if artifact_reference_count(&saved, &artifacts_path) > 0 && source_artifacts.is_none() {
            return Err(format!(
                "session '{id}' references missing artifact directory '{}'",
                artifacts_path.display()
            ));
        }
        let archived_dir =
            directory.prepare_child(OsStr::new(ARCHIVED_DIR_NAME), "session archive")?;
        let target = archived_dir.path.join(&name);
        let archived_artifacts_path = archived_dir.path.join(&artifacts_name);
        let mut archived_saved = saved.clone();
        rewrite_artifact_references(
            &mut archived_saved,
            &artifacts_path,
            &archived_artifacts_path,
        );
        let saved_bytes = encode_saved(&archived_saved)?;
        let archived_artifacts = if let Some(source_artifacts) = source_artifacts.as_ref() {
            Some(copy_artifact_tree_exclusive(
                source_artifacts,
                &archived_dir,
                OsStr::new(&artifacts_name),
                "session archive artifacts",
            )?)
        } else {
            ensure_artifact_target_absent(
                &archived_dir,
                OsStr::new(&artifacts_name),
                "session archive artifacts",
            )?;
            None
        };
        if let Err(failure) = atomic_write_private_in_detailed(
            &archived_dir,
            OsStr::new(&name),
            &saved_bytes,
            false,
            "session archive",
        ) {
            return Err(paired_write_failure(
                failure,
                &archived_dir,
                OsStr::new(&artifacts_name),
                archived_artifacts.as_ref(),
                "session archive artifacts",
                &target,
            ));
        }

        if let Err(error) = directory.remove_file(OsStr::new(&name)) {
            let snapshot_cleanup = archived_dir.remove_file(OsStr::new(&name));
            let artifact_cleanup = if let Some(archived_artifacts) = archived_artifacts.as_ref() {
                remove_artifact_tree(
                    &archived_dir,
                    OsStr::new(&artifacts_name),
                    archived_artifacts,
                    "session archive artifacts",
                )
            } else {
                Ok(())
            };
            if snapshot_cleanup.is_ok() && artifact_cleanup.is_ok() {
                return Err(format!("cannot archive session '{id}': {error}"));
            }
            return Err(format!(
                "cannot archive session '{id}': {error}; archived recovery copy may remain at '{}': snapshot cleanup: {}; artifact cleanup: {}",
                target.display(),
                snapshot_cleanup
                    .err()
                    .map_or_else(|| "ok".to_string(), |error| error.to_string()),
                artifact_cleanup.err().unwrap_or_else(|| "ok".to_string())
            ));
        }
        if let Some(source_artifacts) = source_artifacts.as_ref() {
            if let Err(error) = remove_artifact_tree(
                &directory,
                OsStr::new(&artifacts_name),
                source_artifacts,
                "session artifacts",
            ) {
                directory.sync("session")?;
                return Err(format!(
                    "session '{id}' was archived at '{}', but its live artifact duplicate could not be removed: {error}",
                    target.display()
                ));
            }
        }
        directory.sync("session")?;
        Ok(target)
    }

    /// Restore an archived snapshot without replacing a live session with the
    /// same id. This is the supported inverse of `archive`.
    pub fn restore_archived(dir: &Path, selector: &str) -> Result<SavedSession, String> {
        let id = exact_id(selector, "restore")?;
        let directory = prepare_session_directory(dir)?;
        let _lease = SessionLease::acquire(&directory, id)?;
        let archived_dir =
            directory.open_child(OsStr::new(ARCHIVED_DIR_NAME), "session archive")?;
        let name = format!("{id}.json");
        let mut saved = archived_dir.read_saved(id)?;
        let artifacts_name = artifacts_name(id);
        let live_artifacts_path = directory.path.join(&artifacts_name);
        let archived_artifacts_path = archived_dir.path.join(&artifacts_name);
        let archived_artifacts = archived_dir
            .open_optional_child(OsStr::new(&artifacts_name), "session archive artifacts")?;
        if (artifact_reference_count(&saved, &live_artifacts_path) > 0
            || artifact_reference_count(&saved, &archived_artifacts_path) > 0)
            && archived_artifacts.is_none()
        {
            return Err(format!(
                "archived session '{id}' references missing artifact directory '{}'",
                archived_dir.path.join(&artifacts_name).display()
            ));
        }
        rewrite_artifact_references(&mut saved, &archived_artifacts_path, &live_artifacts_path);
        let saved_bytes = encode_saved(&saved)?;
        let target = directory.path.join(&name);
        let restored_artifacts = if let Some(archived_artifacts) = archived_artifacts.as_ref() {
            Some(copy_artifact_tree_exclusive(
                archived_artifacts,
                &directory,
                OsStr::new(&artifacts_name),
                "restored session artifacts",
            )?)
        } else {
            ensure_artifact_target_absent(
                &directory,
                OsStr::new(&artifacts_name),
                "restored session artifacts",
            )?;
            None
        };
        if let Err(failure) = atomic_write_private_in_detailed(
            &directory,
            OsStr::new(&name),
            &saved_bytes,
            false,
            "session",
        ) {
            return Err(paired_write_failure(
                failure,
                &directory,
                OsStr::new(&artifacts_name),
                restored_artifacts.as_ref(),
                "restored session artifacts",
                &target,
            ));
        }

        if let Err(error) = archived_dir.remove_file(OsStr::new(&name)) {
            let snapshot_cleanup = directory.remove_file(OsStr::new(&name));
            let artifact_cleanup = if let Some(restored_artifacts) = restored_artifacts.as_ref() {
                remove_artifact_tree(
                    &directory,
                    OsStr::new(&artifacts_name),
                    restored_artifacts,
                    "restored session artifacts",
                )
            } else {
                Ok(())
            };
            if snapshot_cleanup.is_ok() && artifact_cleanup.is_ok() {
                return Err(format!("cannot restore archived session '{id}': {error}"));
            }
            return Err(format!(
                "cannot restore archived session '{id}': {error}; restored recovery copy may remain at '{}': snapshot cleanup: {}; artifact cleanup: {}",
                target.display(),
                snapshot_cleanup
                    .err()
                    .map_or_else(|| "ok".to_string(), |error| error.to_string()),
                artifact_cleanup.err().unwrap_or_else(|| "ok".to_string())
            ));
        }
        if let Some(archived_artifacts) = archived_artifacts.as_ref() {
            if let Err(error) = remove_artifact_tree(
                &archived_dir,
                OsStr::new(&artifacts_name),
                archived_artifacts,
                "session archive artifacts",
            ) {
                archived_dir.sync("session archive")?;
                directory.sync("session")?;
                return Err(format!(
                    "session '{id}' was restored at '{}', but its archived artifact duplicate could not be removed: {error}",
                    target.display()
                ));
            }
        }
        archived_dir.sync("session archive")?;
        directory.sync("session")?;
        Ok(saved)
    }

    /// Permanently delete one inactive session. Resolution is intentionally
    /// exact: `last`, an empty selector, and id prefixes are never expanded.
    pub fn delete(dir: &Path, selector: &str, active_id: Option<&str>) -> Result<(), String> {
        let id = exact_id(selector, "delete")?;
        if active_id == Some(id) {
            return Err(format!("cannot delete active session '{id}'"));
        }
        let directory = open_session_directory(dir)?;
        let _lease = SessionLease::acquire(&directory, id)?;
        let name = format!("{id}.json");
        // Validate both the snapshot identity and file type before unlinking.
        let _saved = directory.read_saved(id)?;
        let artifacts_name = artifacts_name(id);
        let artifacts =
            directory.open_optional_child(OsStr::new(&artifacts_name), "session artifacts")?;
        if let Some(artifacts) = artifacts.as_ref() {
            validate_artifact_tree(artifacts, "session artifacts")?;
        }
        directory
            .remove_file(OsStr::new(&name))
            .map_err(|error| format!("cannot delete session '{id}': {error}"))?;
        if let Some(artifacts) = artifacts.as_ref() {
            if let Err(error) = remove_artifact_tree(
                &directory,
                OsStr::new(&artifacts_name),
                artifacts,
                "session artifacts",
            ) {
                directory.sync("session")?;
                return Err(format!(
                    "session '{id}' snapshot was deleted, but its artifact directory could not be removed: {error}"
                ));
            }
        }
        directory.sync("session")
    }

    pub fn list(dir: &Path) -> Result<Vec<SessionSummary>, String> {
        let Some(directory) = open_optional_session_directory(dir)? else {
            return Ok(Vec::new());
        };
        list_in(&directory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn after_next_session_directory_pin(hook: impl FnOnce() + 'static) {
        AFTER_SESSION_DIRECTORY_PINNED.with(|slot| {
            assert!(
                slot.borrow().is_none(),
                "a namespace-swap hook is already set"
            );
            *slot.borrow_mut() = Some(Box::new(hook));
        });
    }

    #[cfg(unix)]
    fn inject_next_private_directory_sync_failure() {
        FAIL_NEXT_PRIVATE_DIRECTORY_SYNC.with(|fail| {
            assert!(
                !fail.replace(true),
                "a private-directory sync failure is already queued"
            );
        });
    }

    #[test]
    fn output_file_write_is_exact_atomic_and_replaces_only_a_regular_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("answer.txt");
        std::fs::write(&target, "stale").unwrap();

        validate_output_file_target(&target).unwrap();
        atomic_write_output_file(&target, b"exact assistant bytes").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"exact assistant bytes");
        assert!(root.path().read_dir().unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kitsu-")
        }));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn output_file_validation_rejects_symlinks_and_reports_post_install_sync_failure() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside.txt");
        std::fs::write(&outside, "outside").unwrap();
        let linked = root.path().join("linked.txt");
        symlink(&outside, &linked).unwrap();
        assert!(validate_output_file_target(&linked)
            .unwrap_err()
            .contains("not a regular file"));
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside");

        let target = root.path().join("installed.txt");
        inject_next_private_directory_sync_failure();
        let error = atomic_write_output_file(&target, b"installed exactly").unwrap_err();
        assert!(error.installed);
        assert_eq!(std::fs::read(&target).unwrap(), b"installed exactly");
    }

    #[test]
    fn session_names_validate_resolve_and_stay_unique() {
        assert!(valid_session_name("friday-refactor").is_ok());
        for bad in ["last", "auto", "s-123", "has space", ""] {
            assert!(valid_session_name(bad).is_err(), "{bad} must be rejected");
        }

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let messages = vec![ChatMessage::system("system"), ChatMessage::user("hello")];

        let store = SessionStore::create(dir.clone()).unwrap();
        store
            .save(root.path(), "local", "model-a", None, &messages)
            .unwrap();
        let first = store.id().to_string();
        drop(store);

        let renamed = SessionStore::rename(&dir, &first, Some("fox-den")).unwrap();
        assert_eq!(renamed.name.as_deref(), Some("fox-den"));
        assert_eq!(
            SessionStore::load(&dir, "fox-den").unwrap().id,
            first,
            "a name resolves in resume selectors"
        );
        assert_eq!(
            SessionStore::list(&dir).unwrap()[0].name.as_deref(),
            Some("fox-den"),
            "listings carry the name"
        );

        let second = SessionStore::create(dir.clone()).unwrap();
        second
            .save(root.path(), "local", "model-a", None, &messages)
            .unwrap();
        let second_id = second.id().to_string();
        drop(second);
        assert!(
            SessionStore::rename(&dir, &second_id, Some("fox-den")).is_err(),
            "duplicate names would make resume ambiguous"
        );
        // Clearing a name works and frees it up.
        SessionStore::rename(&dir, &first, None).unwrap();
        assert!(SessionStore::rename(&dir, &second_id, Some("fox-den")).is_ok());
    }

    #[test]
    fn saves_loads_and_lists_without_credentials() {
        let root = tempfile::tempdir().unwrap();
        let mut store = SessionStore::create(root.path().join("sessions")).unwrap();
        let original_id = store.id().to_string();
        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("world", vec![]),
        ];
        let commit = store
            .save(root.path(), "local", "model-a", None, &messages)
            .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let session_dir_mode = std::fs::metadata(root.path().join("sessions"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let file_mode = std::fs::metadata(
                root.path()
                    .join("sessions")
                    .join(format!("{original_id}.json")),
            )
            .unwrap()
            .permissions()
            .mode()
                & 0o777;
            let locks_dir = root.path().join("sessions").join(LOCKS_DIR_NAME);
            let locks_dir_mode =
                std::fs::metadata(&locks_dir).unwrap().permissions().mode() & 0o777;
            let lock_mode = std::fs::metadata(locks_dir.join(format!("{original_id}.lock")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(session_dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
            assert_eq!(locks_dir_mode, 0o700);
            assert_eq!(lock_mode, 0o600);
        }

        let loaded = SessionStore::load(&root.path().join("sessions"), "last").unwrap();
        assert_eq!(loaded.id, original_id);
        assert_eq!(loaded.messages, messages);
        assert_eq!(loaded.gateway, "local");

        let list = SessionStore::list(&root.path().join("sessions")).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].turns, 1);
        assert_eq!(list[0].gateway, "local");
        assert_eq!(list[0].preview, "hello");
        assert_eq!(
            commit.path(),
            root.path()
                .join("sessions")
                .join(format!("{original_id}.json"))
        );

        store.reset().unwrap();
        assert_ne!(store.id(), original_id);
    }

    #[test]
    fn last_prefers_a_real_conversation_over_a_newer_zero_turn_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let productive = SessionStore::create(dir.clone()).unwrap();
        productive
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[
                    ChatMessage::system("system"),
                    ChatMessage::user("real turn"),
                ],
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let empty = SessionStore::create(dir.clone()).unwrap();
        empty
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::system("system")],
            )
            .unwrap();

        let listed = SessionStore::list(&dir).unwrap();
        assert_eq!(listed[0].id, empty.id(), "fixture must be newest");
        assert_eq!(listed[0].turns, 0);
        assert_eq!(
            SessionStore::load(&dir, "last").unwrap().id,
            productive.id()
        );
        assert_eq!(SessionStore::load(&dir, empty.id()).unwrap().id, empty.id());
        assert_eq!(
            SessionStore::load_other(&dir, "last", Some(empty.id()))
                .unwrap()
                .id,
            productive.id()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_returns_the_exact_inode_for_private_identity_observation() {
        use kitsu_tools::{Mode, Permissions, ToolError};

        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let store = SessionStore::create(sessions.clone()).unwrap();
        let broad = root.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&broad),
            std::slice::from_ref(&broad),
            Mode::AutoAllow,
        )
        .unwrap();
        perms
            .protect_read_path(&sessions.to_string_lossy())
            .unwrap();
        perms
            .protect_write_path(&sessions.to_string_lossy())
            .unwrap();

        let commit = store
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::system("private transcript")],
            )
            .unwrap();
        let alias = root.path().join("surviving-session-alias.json");
        std::fs::hard_link(commit.path(), &alias).unwrap();
        std::fs::remove_file(commit.path()).unwrap();

        perms
            .observe_protected_file(commit.path(), commit.file())
            .unwrap();
        assert!(matches!(
            perms.check_read(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_install_save_failure_preserves_the_exact_inode_for_observation() {
        use kitsu_tools::{Mode, Permissions, ToolError};

        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let store = SessionStore::create(sessions.clone()).unwrap();
        let broad = root.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&broad),
            std::slice::from_ref(&broad),
            Mode::AutoAllow,
        )
        .unwrap();
        perms
            .protect_read_path(&sessions.to_string_lossy())
            .unwrap();
        perms
            .protect_write_path(&sessions.to_string_lossy())
            .unwrap();

        inject_next_private_directory_sync_failure();
        let error = store
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::system("private transcript")],
            )
            .expect_err("the injected directory-sync failure must surface");
        assert!(error.to_string().contains("injected post-install failure"));
        let commit = error
            .installed_commit()
            .expect("an after-install failure must retain the installed descriptor");
        let alias = root.path().join("surviving-failed-session-alias.json");
        std::fs::hard_link(commit.path(), &alias).unwrap();
        std::fs::remove_file(commit.path()).unwrap();

        perms
            .observe_protected_file(commit.path(), commit.file())
            .unwrap();
        assert!(matches!(
            perms.check_read(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[test]
    fn last_still_resolves_when_only_zero_turn_snapshots_exist() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let empty = SessionStore::create(dir.clone()).unwrap();
        empty
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::system("system")],
            )
            .unwrap();

        assert_eq!(SessionStore::load(&dir, "last").unwrap().id, empty.id());
    }

    #[test]
    fn rejects_path_like_session_ids() {
        let root = tempfile::tempdir().unwrap();
        assert!(SessionStore::load(root.path(), "../secret").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_session_files() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside.json");
        std::fs::write(&outside, "{}").unwrap();
        symlink(&outside, root.path().join("s-safe-id.json")).unwrap();
        let error = SessionStore::load(root.path(), "s-safe-id").unwrap_err();
        assert!(error.contains("not a regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_session_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let linked = root.path().join("sessions");
        symlink(&real, &linked).unwrap();
        assert!(SessionStore::create(linked).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn active_store_keeps_snapshot_and_lease_under_the_pinned_ancestor() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let original_parent = root.path().join("original-parent");
        let session_dir = original_parent.join("sessions");
        let outside_parent = root.path().join("outside-parent");
        let outside_sessions = outside_parent.join("sessions");
        std::fs::create_dir_all(&outside_sessions).unwrap();

        let mut store = SessionStore::create(session_dir.clone()).unwrap();
        let original_id = store.id().to_string();
        store
            .save(
                root.path(),
                "gateway",
                "model-before",
                None,
                &[ChatMessage::user("before")],
            )
            .unwrap();

        let pinned_parent = root.path().join("pinned-parent");
        std::fs::rename(&original_parent, &pinned_parent).unwrap();
        symlink(&outside_parent, &original_parent).unwrap();

        store
            .save(
                root.path(),
                "gateway",
                "model-after",
                None,
                &[ChatMessage::user("after")],
            )
            .unwrap();
        store.reset().unwrap();
        let reset_id = store.id().to_string();
        store
            .save(
                root.path(),
                "gateway",
                "model-reset",
                None,
                &[ChatMessage::user("reset")],
            )
            .unwrap();

        let pinned_sessions = pinned_parent.join("sessions");
        assert_eq!(
            SessionStore::load(&pinned_sessions, &original_id)
                .unwrap()
                .model,
            "model-after"
        );
        assert_eq!(
            SessionStore::load(&pinned_sessions, &reset_id)
                .unwrap()
                .model,
            "model-reset"
        );
        assert!(pinned_sessions
            .join(LOCKS_DIR_NAME)
            .join(format!("{reset_id}.lock"))
            .is_file());
        assert!(SessionStore::list(&outside_sessions).unwrap().is_empty());
        assert!(!outside_sessions.join(LOCKS_DIR_NAME).exists());
    }

    #[test]
    fn reset_is_atomic_on_lease_failure_and_starts_unnamed() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let mut store = SessionStore::create(dir.clone()).unwrap();
        store.set_name(Some("named-source".to_string()));
        let original_id = store.id().to_string();
        store
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::user("durable before reset")],
            )
            .unwrap();

        FAIL_NEXT_SESSION_LEASE.with(|failure| failure.set(true));
        let error = store.reset().unwrap_err();
        assert!(error.contains("injected session lease failure"));
        assert_eq!(store.id(), original_id);
        assert_eq!(store.name(), Some("named-source"));
        assert_eq!(
            SessionStore::load(&dir, &original_id)
                .unwrap()
                .name
                .as_deref(),
            Some("named-source")
        );

        store.reset().unwrap();
        assert_ne!(store.id(), original_id);
        assert_eq!(store.name(), None);
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_operations_ignore_ancestor_swaps_after_pinning() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let original_parent = root.path().join("archive-parent");
        let session_dir = original_parent.join("sessions");
        let store = SessionStore::create(session_dir.clone()).unwrap();
        let id = store.id().to_string();
        let source_artifacts = store.artifacts_dir();
        std::fs::create_dir_all(&source_artifacts).unwrap();
        let source_artifact = source_artifacts.join("evidence.txt");
        std::fs::write(&source_artifact, "pinned evidence").unwrap();
        store
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::tool(
                    "call",
                    format!("full result saved to {}", source_artifact.display()),
                )],
            )
            .unwrap();
        drop(store);

        let outside_parent = root.path().join("outside-parent");
        let outside_sessions = outside_parent.join("sessions");
        std::fs::create_dir_all(&outside_sessions).unwrap();
        let outside_snapshot = outside_sessions.join(format!("{id}.json"));
        std::fs::copy(session_dir.join(format!("{id}.json")), &outside_snapshot).unwrap();
        let outside_before = std::fs::read(&outside_snapshot).unwrap();
        let outside_artifacts = outside_sessions.join(artifacts_name(&id));
        std::fs::create_dir(&outside_artifacts).unwrap();
        let outside_artifact = outside_artifacts.join("evidence.txt");
        std::fs::write(&outside_artifact, "outside evidence").unwrap();

        let archive_parent = root.path().join("archive-parent-pinned");
        let swap_from = original_parent.clone();
        let swap_to = archive_parent.clone();
        let swap_target = outside_parent.clone();
        after_next_session_directory_pin(move || {
            std::fs::rename(swap_from, swap_to).unwrap();
            symlink(swap_target, original_parent).unwrap();
        });
        SessionStore::archive(&session_dir, &id, None).unwrap();

        let archived_sessions = archive_parent.join("sessions");
        assert!(!archived_sessions.join(format!("{id}.json")).exists());
        assert!(archived_sessions
            .join(ARCHIVED_DIR_NAME)
            .join(format!("{id}.json"))
            .is_file());
        assert_eq!(
            std::fs::read_to_string(
                archived_sessions
                    .join(ARCHIVED_DIR_NAME)
                    .join(artifacts_name(&id))
                    .join("evidence.txt")
            )
            .unwrap(),
            "pinned evidence"
        );
        assert_eq!(std::fs::read(&outside_snapshot).unwrap(), outside_before);
        assert_eq!(
            std::fs::read_to_string(&outside_artifact).unwrap(),
            "outside evidence"
        );
        assert!(!outside_sessions.join(ARCHIVED_DIR_NAME).exists());

        let restore_parent = root.path().join("restore-parent-pinned");
        let restore_path = archive_parent.join("sessions");
        let swap_from = archive_parent.clone();
        let swap_to = restore_parent.clone();
        let swap_target = outside_parent.clone();
        after_next_session_directory_pin(move || {
            std::fs::rename(swap_from, swap_to).unwrap();
            symlink(swap_target, archive_parent).unwrap();
        });
        SessionStore::restore_archived(&restore_path, &id).unwrap();

        let restored_sessions = restore_parent.join("sessions");
        assert!(restored_sessions.join(format!("{id}.json")).is_file());
        assert!(!restored_sessions
            .join(ARCHIVED_DIR_NAME)
            .join(format!("{id}.json"))
            .exists());
        assert_eq!(
            std::fs::read_to_string(
                restored_sessions
                    .join(artifacts_name(&id))
                    .join("evidence.txt")
            )
            .unwrap(),
            "pinned evidence"
        );
        assert_eq!(std::fs::read(&outside_snapshot).unwrap(), outside_before);
        assert_eq!(
            std::fs::read_to_string(&outside_artifact).unwrap(),
            "outside evidence"
        );

        let delete_parent = root.path().join("delete-parent-pinned");
        let delete_path = restore_parent.join("sessions");
        let swap_from = restore_parent.clone();
        let swap_to = delete_parent.clone();
        let swap_target = outside_parent.clone();
        after_next_session_directory_pin(move || {
            std::fs::rename(swap_from, swap_to).unwrap();
            symlink(swap_target, restore_parent).unwrap();
        });
        SessionStore::delete(&delete_path, &id, None).unwrap();

        assert!(!delete_parent
            .join("sessions")
            .join(format!("{id}.json"))
            .exists());
        assert!(!delete_parent
            .join("sessions")
            .join(artifacts_name(&id))
            .exists());
        assert_eq!(std::fs::read(&outside_snapshot).unwrap(), outside_before);
        assert_eq!(
            std::fs::read_to_string(&outside_artifact).unwrap(),
            "outside evidence"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_session_lock_files() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let store = SessionStore::create(dir.clone()).unwrap();
        store
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::user("private")],
            )
            .unwrap();
        let saved = SessionStore::load(&dir, store.id()).unwrap();
        let lock_path = dir
            .join(LOCKS_DIR_NAME)
            .join(format!("{}.lock", store.id()));
        drop(store);
        std::fs::remove_file(&lock_path).unwrap();

        let outside = root.path().join("outside.lock");
        std::fs::write(&outside, b"unchanged").unwrap();
        symlink(&outside, &lock_path).unwrap();

        assert!(SessionStore::from_saved(dir.clone(), &saved)
            .unwrap_err()
            .contains("not a regular file"));
        assert!(SessionStore::archive(&dir, &saved.id, None)
            .unwrap_err()
            .contains("not a regular file"));
        assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
    }

    #[test]
    fn load_other_excludes_the_active_session_from_last() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let first = SessionStore::create(dir.clone()).unwrap();
        first
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::system("system"), ChatMessage::user("first")],
            )
            .unwrap();
        let second = SessionStore::create(dir.clone()).unwrap();
        second
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::system("system"), ChatMessage::user("second")],
            )
            .unwrap();

        let loaded = SessionStore::load_other(&dir, "last", Some(second.id())).unwrap();

        assert_eq!(loaded.id, first.id());
        assert!(
            SessionStore::load_other(&dir, second.id(), Some(second.id()))
                .unwrap_err()
                .contains("already active")
        );
    }

    #[test]
    fn resume_refuses_a_session_leased_by_another_handle() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let store = SessionStore::create(dir.clone()).unwrap();
        store
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::user("leased")],
            )
            .unwrap();
        let saved = SessionStore::load(&dir, store.id()).unwrap();

        let error = SessionStore::from_saved(dir.clone(), &saved).unwrap_err();
        assert!(error.contains("already active"));

        drop(store);
        let resumed = SessionStore::from_saved(dir, &saved).unwrap();
        assert_eq!(resumed.id(), saved.id);
    }

    #[test]
    fn resume_rejects_a_snapshot_that_changed_before_lease_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let original = SessionStore::create(dir.clone()).unwrap();
        original
            .save(
                root.path(),
                "gateway",
                "model-old",
                None,
                &[ChatMessage::user("old")],
            )
            .unwrap();
        let stale = SessionStore::load(&dir, original.id()).unwrap();
        drop(original);

        let updater = SessionStore::from_saved(dir.clone(), &stale).unwrap();
        updater
            .save(
                root.path(),
                "gateway",
                "model-new",
                None,
                &[ChatMessage::user("new")],
            )
            .unwrap();
        drop(updater);

        let error = SessionStore::from_saved(dir.clone(), &stale).unwrap_err();
        assert!(error.contains("changed while it was being resumed"));
        assert_eq!(
            SessionStore::load(&dir, &stale.id).unwrap().model,
            "model-new"
        );
    }

    #[test]
    fn fork_uses_a_fresh_identity_and_preserves_snapshot_provenance() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let mut source = SessionStore::create(dir.clone()).unwrap();
        source.set_name(Some("named-source".to_string()));
        let project_context = root.path().join("KITSU.md");
        let messages = vec![
            ChatMessage::system("system provenance"),
            ChatMessage::user("fork this conversation"),
            ChatMessage::assistant("done", vec![]),
        ];
        source
            .save(
                root.path(),
                "gateway-a",
                "model-a",
                Some(&project_context),
                &messages,
            )
            .unwrap();

        // Forking `last` resolves the current source exactly; permissions are
        // not an input to or a field of the durable snapshot API.
        let source_id = source.id().to_string();
        let receipt = SessionStore::fork_with_source(&dir, "last").unwrap();
        assert_eq!(receipt.source_session_id, source_id);
        let forked = receipt.session;

        assert_ne!(forked.id, source_id);
        assert_eq!(forked.name, None);
        assert_eq!(
            SessionStore::load(&dir, "named-source").unwrap().id,
            source.id()
        );
        assert_eq!(forked.gateway, "gateway-a");
        assert_eq!(forked.model, "model-a");
        assert_eq!(forked.directory, root.path().display().to_string());
        assert_eq!(
            forked.project_context,
            Some(project_context.display().to_string())
        );
        assert_eq!(forked.messages, messages);
        assert_eq!(SessionStore::list(&dir).unwrap().len(), 2);

        let encoded = std::fs::read_to_string(dir.join(format!("{}.json", forked.id))).unwrap();
        assert!(!encoded.contains("permission"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(format!("{}.json", forked.id)))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn fork_copies_artifacts_and_rewrites_exact_message_paths() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let source = SessionStore::create(dir.clone()).unwrap();
        let source_artifacts = source.artifacts_dir();
        let nested = source_artifacts.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let source_file = nested.join("call-1.txt");
        std::fs::write(&source_file, "durable evidence").unwrap();
        let lookalike = format!("{}-backup/ignored.txt", source_artifacts.display());
        let messages = vec![
            ChatMessage::tool(
                "call-1",
                format!(
                    "[tool output 1.0 KB — full result saved to {}.]\nlookalike: {lookalike}",
                    source_file.display()
                ),
            ),
            ChatMessage::assistant(
                "re-read",
                vec![kitsu_providers::ToolCall {
                    tool_name: "fs_read_file".to_string(),
                    args: format!(
                        "{{\"path\":{}}}",
                        serde_json::to_string(&source_file.display().to_string()).unwrap()
                    ),
                    call_id: "read-1".to_string(),
                }],
            ),
        ];
        source
            .save(root.path(), "gateway", "model", None, &messages)
            .unwrap();

        let forked = SessionStore::fork_with_source(&dir, source.id())
            .unwrap()
            .session;
        let forked_artifacts = dir.join(artifacts_name(&forked.id));
        let forked_file = forked_artifacts.join("nested").join("call-1.txt");

        assert_eq!(
            std::fs::read_to_string(&forked_file).unwrap(),
            "durable evidence"
        );
        assert_eq!(
            std::fs::read_to_string(&source_file).unwrap(),
            "durable evidence"
        );
        assert!(forked.messages[0]
            .content
            .contains(&forked_file.display().to_string()));
        assert!(!forked.messages[0]
            .content
            .contains(&source_file.display().to_string()));
        assert!(forked.messages[0].content.contains(&lookalike));
        assert!(forked.messages[1].tool_calls[0]
            .args
            .contains(&forked_file.display().to_string()));
        let persisted = SessionStore::load(&dir, &forked.id).unwrap();
        assert_eq!(persisted.messages, forked.messages);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&forked_artifacts)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&forked_file)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn export_bundles_artifacts_rewrites_pointers_and_rolls_back_on_clobber() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let source = SessionStore::create(dir.clone()).unwrap();
        let source_artifacts = source.artifacts_dir();
        std::fs::create_dir_all(&source_artifacts).unwrap();
        let source_file = source_artifacts.join("call.txt");
        std::fs::write(&source_file, "exported evidence").unwrap();
        source
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::tool(
                    "call",
                    format!("full result saved to {}", source_file.display()),
                )],
            )
            .unwrap();

        let destination = root.path().join("conversation.json");
        SessionStore::export(&dir, source.id(), &destination, SessionExportFormat::Json).unwrap();
        let destination_artifacts = root.path().join("conversation.json.artifacts");
        let destination_file = destination_artifacts.join("call.txt");
        let exported: SavedSession =
            serde_json::from_slice(&std::fs::read(&destination).unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&destination_file).unwrap(),
            "exported evidence"
        );
        assert!(exported.messages[0]
            .content
            .contains(&destination_file.display().to_string()));
        assert!(!exported.messages[0]
            .content
            .contains(&source_file.display().to_string()));

        let markdown_destination = root.path().join("conversation.md");
        SessionStore::export(
            &dir,
            source.id(),
            &markdown_destination,
            SessionExportFormat::Markdown,
        )
        .unwrap();
        let markdown_artifacts = root.path().join("conversation.md.artifacts");
        let markdown_file = markdown_artifacts.join("call.txt");
        assert_eq!(
            std::fs::read_to_string(&markdown_file).unwrap(),
            "exported evidence"
        );
        let markdown = std::fs::read_to_string(&markdown_destination).unwrap();
        assert!(markdown.contains(&markdown_file.display().to_string()));
        assert!(!markdown.contains(&source_file.display().to_string()));

        let blocked = root.path().join("blocked.json");
        std::fs::write(&blocked, "do not replace").unwrap();
        let error = SessionStore::export(&dir, source.id(), &blocked, SessionExportFormat::Json)
            .unwrap_err();
        assert!(error.contains("refusing to overwrite"));
        assert_eq!(std::fs::read_to_string(&blocked).unwrap(), "do not replace");
        assert!(!root.path().join("blocked.json.artifacts").exists());

        let collision = root.path().join("collision.json");
        let collision_artifacts = root.path().join("collision.json.artifacts");
        std::fs::create_dir(&collision_artifacts).unwrap();
        std::fs::write(collision_artifacts.join("sentinel"), "unchanged").unwrap();
        assert!(
            SessionStore::export(&dir, source.id(), &collision, SessionExportFormat::Json,)
                .unwrap_err()
                .contains("refusing to overwrite")
        );
        assert!(!collision.exists());
        assert_eq!(
            std::fs::read_to_string(collision_artifacts.join("sentinel")).unwrap(),
            "unchanged"
        );
    }

    #[test]
    fn paired_write_rollback_keeps_evidence_once_the_primary_is_installed() {
        let root = tempfile::tempdir().unwrap();
        let directory = PrivateDirectory::prepare(root.path(), "test").unwrap();
        let installed_name = OsStr::new("installed.artifacts");
        let installed = directory
            .create_child_exclusive(installed_name, "test artifacts")
            .unwrap();
        std::fs::write(installed.path.join("evidence.txt"), "recoverable").unwrap();

        let error = paired_write_failure(
            AtomicWriteFailure::after_install("directory sync failed".to_string()),
            &directory,
            installed_name,
            Some(&installed),
            "test artifacts",
            &root.path().join("installed.json"),
        );
        assert!(error.contains("complete recovery copy remains"));
        assert_eq!(
            std::fs::read_to_string(installed.path.join("evidence.txt")).unwrap(),
            "recoverable"
        );

        let rollback_name = OsStr::new("rollback.artifacts");
        let rollback = directory
            .create_child_exclusive(rollback_name, "test artifacts")
            .unwrap();
        std::fs::write(rollback.path.join("evidence.txt"), "temporary").unwrap();
        let error = paired_write_failure(
            AtomicWriteFailure::before_install("primary failed".to_string()),
            &directory,
            rollback_name,
            Some(&rollback),
            "test artifacts",
            &root.path().join("rollback.json"),
        );
        assert_eq!(error, "primary failed");
        assert!(!root.path().join("rollback.artifacts").exists());
    }

    #[test]
    fn export_is_human_readable_atomic_and_no_clobber() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let source = SessionStore::create(dir.clone()).unwrap();
        source
            .save(
                root.path(),
                "gateway-a",
                "model-a",
                None,
                &[
                    ChatMessage::system("system"),
                    ChatMessage::user("hello\nsecond line"),
                    ChatMessage::assistant("world", vec![]),
                ],
            )
            .unwrap();

        let markdown_path = root.path().join("conversation.md");
        let exported = SessionStore::export(
            &dir,
            source.id(),
            &markdown_path,
            SessionExportFormat::Markdown,
        )
        .unwrap();
        assert_eq!(exported, markdown_path);
        let markdown = std::fs::read_to_string(&markdown_path).unwrap();
        assert!(markdown.contains("# Kitsu session export"));
        assert!(markdown.contains("- Model: \"model-a\""));
        assert!(markdown.contains("### 2. User"));
        assert!(markdown.contains("    hello\n    second line"));

        let before = std::fs::read(&markdown_path).unwrap();
        let error =
            SessionStore::export(&dir, source.id(), &markdown_path, SessionExportFormat::Json)
                .unwrap_err();
        assert!(error.contains("refusing to overwrite"));
        assert_eq!(std::fs::read(&markdown_path).unwrap(), before);

        let json_path = root.path().join("conversation.json");
        SessionStore::export(&dir, source.id(), &json_path, SessionExportFormat::Json).unwrap();
        let exported: SavedSession =
            serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
        assert_eq!(exported.id, source.id());
        assert_eq!(exported.model, "model-a");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&markdown_path, &json_path] {
                let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn export_refuses_a_symlinked_destination_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let source = SessionStore::create(dir.clone()).unwrap();
        source
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::user("private")],
            )
            .unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let linked = root.path().join("linked");
        symlink(&real, &linked).unwrap();

        let error = SessionStore::export(
            &dir,
            source.id(),
            &linked.join("export.md"),
            SessionExportFormat::Markdown,
        )
        .unwrap_err();

        assert!(error.contains("not a regular directory"));
        assert!(!real.join("export.md").exists());
    }

    #[test]
    fn archive_hides_an_inactive_session_and_restore_recovers_it() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let first = SessionStore::create(dir.clone()).unwrap();
        let first_id = first.id().to_string();
        first
            .save(
                root.path(),
                "gateway",
                "model-one",
                None,
                &[ChatMessage::user("first")],
            )
            .unwrap();
        let second = SessionStore::create(dir.clone()).unwrap();
        second
            .save(
                root.path(),
                "gateway",
                "model-two",
                None,
                &[ChatMessage::user("second")],
            )
            .unwrap();

        assert!(SessionStore::archive(&dir, "last", Some(second.id()))
            .unwrap_err()
            .contains("exact session id"));
        assert!(SessionStore::archive(&dir, second.id(), Some(second.id()))
            .unwrap_err()
            .contains("active session"));

        // `active_id` protects this process's UI state; the lease independently
        // protects sessions active through any handle or process.
        assert!(SessionStore::archive(&dir, &first_id, Some(second.id()))
            .unwrap_err()
            .contains("already active"));
        let first_artifacts = first.artifacts_dir();
        std::fs::create_dir_all(&first_artifacts).unwrap();
        let first_artifact = first_artifacts.join("evidence.txt");
        std::fs::write(&first_artifact, "archived evidence").unwrap();
        first
            .save(
                root.path(),
                "gateway",
                "model-latest",
                None,
                &[
                    ChatMessage::user("latest"),
                    ChatMessage::tool(
                        "call",
                        format!("full result saved to {}", first_artifact.display()),
                    ),
                ],
            )
            .unwrap();
        drop(first);
        let archived = SessionStore::archive(&dir, &first_id, Some(second.id())).unwrap();
        assert_eq!(
            archived,
            dir.join(ARCHIVED_DIR_NAME).join(format!("{first_id}.json"))
        );
        assert!(!dir.join(format!("{first_id}.json")).exists());
        assert!(!first_artifacts.exists());
        assert!(archived.exists());
        let archived_artifacts = dir.join(ARCHIVED_DIR_NAME).join(artifacts_name(&first_id));
        let archived_artifact = archived_artifacts.join("evidence.txt");
        assert_eq!(
            std::fs::read_to_string(&archived_artifact).unwrap(),
            "archived evidence"
        );
        let archived_saved: SavedSession =
            serde_json::from_slice(&std::fs::read(&archived).unwrap()).unwrap();
        assert!(archived_saved.messages[1]
            .content
            .contains(&archived_artifact.display().to_string()));
        assert!(!archived_saved.messages[1]
            .content
            .contains(&first_artifact.display().to_string()));
        let listed = SessionStore::list(&dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, second.id());
        assert_eq!(SessionStore::load(&dir, "last").unwrap().id, second.id());

        // Recovery never clobbers a live path, even when an archived copy with
        // the same exact id exists.
        let live_path = dir.join(format!("{first_id}.json"));
        std::fs::write(&live_path, b"do not replace").unwrap();
        assert!(SessionStore::restore_archived(&dir, &first_id)
            .unwrap_err()
            .contains("refusing to overwrite"));
        assert_eq!(std::fs::read(&live_path).unwrap(), b"do not replace");
        assert!(archived.exists());
        assert!(archived_artifact.exists());
        assert!(!first_artifacts.exists());
        std::fs::remove_file(&live_path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let archive_mode = std::fs::metadata(dir.join(ARCHIVED_DIR_NAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let file_mode = std::fs::metadata(&archived).unwrap().permissions().mode() & 0o777;
            let artifacts_mode = std::fs::metadata(&archived_artifacts)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let artifact_mode = std::fs::metadata(&archived_artifact)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(archive_mode, 0o700);
            assert_eq!(file_mode, 0o600);
            assert_eq!(artifacts_mode, 0o700);
            assert_eq!(artifact_mode, 0o600);
        }

        let restored = SessionStore::restore_archived(&dir, &first_id).unwrap();
        assert_eq!(restored.id, first_id);
        assert_eq!(restored.model, "model-latest");
        assert!(!archived.exists());
        assert!(!archived_artifacts.exists());
        assert!(dir.join(format!("{first_id}.json")).exists());
        assert_eq!(
            std::fs::read_to_string(&first_artifact).unwrap(),
            "archived evidence"
        );
        assert!(restored.messages[1]
            .content
            .contains(&first_artifact.display().to_string()));
        assert_eq!(SessionStore::list(&dir).unwrap().len(), 2);
    }

    #[test]
    fn delete_requires_an_exact_inactive_session_id() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let first = SessionStore::create(dir.clone()).unwrap();
        let first_id = first.id().to_string();
        first
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::user("first")],
            )
            .unwrap();
        let first_artifacts = first.artifacts_dir();
        std::fs::create_dir_all(first_artifacts.join("nested")).unwrap();
        std::fs::write(first_artifacts.join("nested/evidence.txt"), "delete me").unwrap();
        let active = SessionStore::create(dir.clone()).unwrap();
        active
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::user("active")],
            )
            .unwrap();

        for selector in ["", "last"] {
            assert!(SessionStore::delete(&dir, selector, Some(active.id()))
                .unwrap_err()
                .contains("exact session id"));
        }
        assert!(SessionStore::delete(&dir, "../outside", Some(active.id())).is_err());
        let prefix = &first_id[..first_id.len() - 1];
        assert!(SessionStore::delete(&dir, prefix, Some(active.id())).is_err());
        assert!(dir.join(format!("{first_id}.json")).exists());

        assert!(SessionStore::delete(&dir, active.id(), Some(active.id()))
            .unwrap_err()
            .contains("active session"));
        assert!(dir.join(format!("{}.json", active.id())).exists());

        assert!(SessionStore::delete(&dir, &first_id, Some(active.id()))
            .unwrap_err()
            .contains("already active"));
        drop(first);
        SessionStore::delete(&dir, &first_id, Some(active.id())).unwrap();
        assert!(!dir.join(format!("{first_id}.json")).exists());
        assert!(!first_artifacts.exists());
        assert_eq!(SessionStore::list(&dir).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn artifact_lifecycle_refuses_symlinked_sidecars_without_touching_targets() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        let source = SessionStore::create(dir.clone()).unwrap();
        source
            .save(
                root.path(),
                "gateway",
                "model",
                None,
                &[ChatMessage::user("preserve")],
            )
            .unwrap();
        let id = source.id().to_string();
        drop(source);

        let outside = root.path().join("outside-artifacts");
        std::fs::create_dir(&outside).unwrap();
        let sentinel = outside.join("sentinel.txt");
        std::fs::write(&sentinel, "unchanged").unwrap();
        let sidecar = dir.join(artifacts_name(&id));
        symlink(&outside, &sidecar).unwrap();

        assert!(SessionStore::fork_with_source(&dir, &id).is_err());
        assert!(SessionStore::export(
            &dir,
            &id,
            &root.path().join("export.json"),
            SessionExportFormat::Json,
        )
        .is_err());
        assert!(SessionStore::archive(&dir, &id, None).is_err());
        assert!(SessionStore::delete(&dir, &id, None).is_err());
        assert!(dir.join(format!("{id}.json")).is_file());
        assert!(sidecar.is_symlink());
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "unchanged");
        assert!(!root.path().join("export.json").exists());

        std::fs::remove_file(&sidecar).unwrap();
        std::fs::create_dir(&sidecar).unwrap();
        symlink(&sentinel, sidecar.join("linked.txt")).unwrap();
        assert!(SessionStore::fork_with_source(&dir, &id).is_err());
        assert!(SessionStore::export(
            &dir,
            &id,
            &root.path().join("nested-export.json"),
            SessionExportFormat::Json,
        )
        .is_err());
        assert!(SessionStore::archive(&dir, &id, None).is_err());
        assert!(SessionStore::delete(&dir, &id, None).is_err());
        assert!(dir.join(format!("{id}.json")).is_file());
        assert!(sidecar.join("linked.txt").is_symlink());
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "unchanged");
        assert!(!root.path().join("nested-export.json").exists());
        assert!(!root.path().join("nested-export.json.artifacts").exists());
        assert!(!dir
            .join(ARCHIVED_DIR_NAME)
            .join(artifacts_name(&id))
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn destructive_lifecycle_operations_refuse_symlinked_snapshots() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        std::fs::create_dir(&dir).unwrap();
        let outside = root.path().join("outside.json");
        std::fs::write(&outside, "outside").unwrap();
        let linked = dir.join("linked.json");
        symlink(&outside, &linked).unwrap();

        assert!(SessionStore::archive(&dir, "linked", None).is_err());
        assert!(SessionStore::delete(&dir, "linked", None).is_err());
        assert!(linked.exists());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside");
    }
}
