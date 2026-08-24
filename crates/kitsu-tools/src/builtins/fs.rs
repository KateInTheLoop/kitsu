//! Read-only filesystem tools: `fs.read_file`, `fs.list_dir`, `repo.grep`.
//! Each validates its path through the [`Permissions`] before touching disk and runs
//! the blocking IO on a blocking thread. Errors are returned as [`ToolError`]
//! and serialized back to the model as a tool result by the orchestrator.

use std::collections::VecDeque;
#[cfg(unix)]
use std::ffi::{CStr, CString, OsStr, OsString};
use std::future::Future;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
#[cfg(any(not(unix), test))]
use std::process::{ExitStatus, Stdio};
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use regex::Regex;
use serde::Deserialize;
#[cfg(any(not(unix), test))]
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

/// A host-owned streaming artifact which is retained only after [`Self::finish`].
///
/// The file is allocated with an exclusive create and, on Unix, a pinned
/// no-follow directory handle and mode `0600`. Dropping or explicitly aborting
/// an unfinished writer removes the partial artifact. This type is deliberately
/// a Rust host API rather than a model-callable tool.
#[derive(Debug)]
pub struct PrivateArtifactWriter {
    path: PathBuf,
    file: Option<std::fs::File>,
    #[cfg(unix)]
    directory: std::fs::File,
    #[cfg(unix)]
    name: CString,
    active: bool,
}

impl PrivateArtifactWriter {
    /// The final protected path. The path may be referenced externally only
    /// after [`Self::finish`] succeeds.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Flush and durably retain the artifact, returning its protected path.
    /// Any failure leaves the writer armed for cleanup during `Drop`.
    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        self.finish_in_place()
    }

    /// In-place form of [`Self::finish`] for owners that must retain explicit
    /// cleanup authority after a flush or durability failure.
    pub fn finish_in_place(&mut self) -> std::io::Result<PathBuf> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("private artifact writer is no longer active"))?;
        file.flush()?;
        file.sync_all()?;
        #[cfg(unix)]
        self.directory.sync_all()?;
        self.file.take();
        self.active = false;
        Ok(self.path.clone())
    }

    /// Remove a partial artifact immediately. A failed cleanup is retried once
    /// when the writer is dropped and is also returned to the caller.
    pub fn abort(&mut self) -> std::io::Result<()> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.file.take();

        #[cfg(unix)]
        let result = {
            let status =
                unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
            if status == 0 {
                let _ = self.directory.sync_all();
                Ok(())
            } else {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        };

        #[cfg(not(unix))]
        let result = match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };

        if result.is_ok() {
            self.active = false;
        }
        result
    }
}

impl Write for PrivateArtifactWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("private artifact writer is no longer active"))?
            .write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("private artifact writer is no longer active"))?
            .flush()
    }
}

impl Drop for PrivateArtifactWriter {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// Allocate a unique private artifact for incremental host-side writes.
pub fn create_unique_private_artifact(
    directory: &Path,
    stem: &str,
) -> std::io::Result<PrivateArtifactWriter> {
    create_unique_private_artifact_with_sequence(directory, stem, &PRIVATE_FILE_SEQUENCE)
}

fn write_unique_private_text_file_with_sequence(
    directory: &Path,
    stem: &str,
    content: &str,
    sequence: &AtomicU64,
) -> std::io::Result<PathBuf> {
    let mut writer = create_unique_private_artifact_with_sequence(directory, stem, sequence)?;
    writer.write_all(content.as_bytes())?;
    writer.finish()
}

fn create_unique_private_artifact_with_sequence(
    directory: &Path,
    stem: &str,
    sequence: &AtomicU64,
) -> std::io::Result<PrivateArtifactWriter> {
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
            if unsafe { libc::fchmod(fd, 0o600) } < 0 {
                let error = std::io::Error::last_os_error();
                unsafe {
                    libc::close(fd);
                }
                unsafe {
                    libc::unlinkat(directory_file.as_raw_fd(), c_name.as_ptr(), 0);
                }
                return Err(error);
            }
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            return Ok(PrivateArtifactWriter {
                path: directory.join(name),
                file: Some(file),
                directory: directory_file,
                name: c_name,
                active: true,
            });
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
                Ok(file) => {
                    return Ok(PrivateArtifactWriter {
                        path,
                        file: Some(file),
                        active: true,
                    })
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
    /// Total on-disk size in bytes (cheap via metadata) so a capped read can
    /// disclose how much was NOT served (cycle-19/29 audits: a model read
    /// 1731 of 15682 lines and said DONE with nothing marking the gap).
    total_bytes: u64,
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
    Ok(CappedText {
        text,
        truncated,
        total_bytes: metadata.len(),
    })
}

/// Read lines `[start, end]` (1-based, inclusive) with the byte cap applied
/// to the *served range*, not the absolute file prefix. The pre-fix behavior
/// read only the first `max_bytes` from byte 0 and then sliced the range out
/// of that prefix, so any range starting past the cap returned almost
/// nothing — on a ~128 KiB source file this burned 6-7 thrashing tool rounds
/// per turn before the model gave up (kimi-k3 cycle-1 supervisor finding).
/// Skipped prefix lines cost I/O only, never memory.
fn read_capped_line_range(
    file: std::fs::File,
    max_bytes: usize,
    start: usize,
    end: usize,
) -> std::io::Result<CappedText> {
    use std::io::BufRead;

    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("read target is not a regular file"));
    }
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    let mut line_number = 0usize;
    let mut text = String::new();
    let mut truncated = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        line_number += 1;
        if line_number < start {
            continue;
        }
        if line_number > end {
            break;
        }
        // Match the historical `.lines()` slicing: line terminators are
        // stripped and served lines are joined with '\n'.
        let stripped = line
            .strip_suffix('\n')
            .map(|rest| rest.strip_suffix('\r').unwrap_or(rest))
            .unwrap_or(&line);
        // Prefix each served line with its ABSOLUTE line number + a tab, so a
        // model citing a ranged read counts from the file's real numbering
        // instead of self-counting from `start` (which drifts ~1 line,
        // especially on a blank first line — the R1.7 fix). The prefix is
        // display-only: fs_edit_file matches raw file text, so `old_text`
        // must NOT include it (documented in the tool description).
        let prefix = format!("{line_number}\t");
        let separator = usize::from(!text.is_empty());
        if text.len() + separator + prefix.len() + stripped.len() > max_bytes {
            truncated = true;
            if text.is_empty() {
                // A single line larger than the whole budget: serve the line
                // number prefix and the content prefix on a char boundary.
                text.push_str(&prefix);
                let budget = max_bytes.saturating_sub(prefix.len());
                let mut cut = budget.min(stripped.len());
                while cut > 0 && !stripped.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.push_str(&stripped[..cut]);
            }
            break;
        }
        if separator == 1 {
            text.push('\n');
        }
        text.push_str(&prefix);
        text.push_str(stripped);
    }
    Ok(CappedText {
        text,
        truncated,
        total_bytes: metadata.len(),
    })
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

#[cfg(test)]
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
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    start_line: Option<usize>,
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    end_line: Option<usize>,
}

pub fn read_file_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_file".to_string(),
        description: "Read a UTF-8 text file from an allowed path. Optionally restrict to a \
                      1-based inclusive line range with start_line/end_line. A ranged read \
                      prefixes each line with its absolute line number and a tab (e.g. \
                      \"42\\t<code>\") so you can cite exact line numbers; this prefix is \
                      display-only — do NOT include it in fs_edit_file old_text. A full read \
                      (no range) is returned verbatim with no prefixes."
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
    read_file_run_with_hook(policy, args, || {}).await
}

async fn read_file_run_with_hook<F>(
    policy: &Permissions,
    args: &str,
    before_open: F,
) -> Result<String, ToolError>
where
    F: FnOnce(),
{
    let a: ReadFileArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let max_bytes = policy.max_bytes();
    before_open();
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    // Ranged reads stream to the requested lines and apply the byte cap to
    // the range itself; only a full-file read caps from the file start.
    let content = match (a.start_line, a.end_line) {
        (None, None) => tokio::task::spawn_blocking(move || read_capped_text(file, max_bytes)),
        (start, end) => {
            let start = start.unwrap_or(1).max(1);
            let end = end.unwrap_or(usize::MAX);
            tokio::task::spawn_blocking(move || read_capped_line_range(file, max_bytes, start, end))
        }
    }
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;

    // Disclose what the caller actually received (post-slice), not the
    // pre-slice absolute read — the earlier figure misdescribed narrow
    // ranges on large files (kimi-k3 cycle-1 supervisor finding).
    let served_bytes = content.text.len();
    // The size disclosure is appended AFTER policy capping: cap_output trims
    // the tail, so an earlier append would be eaten and replaced by its
    // generic marker (the pre-fix behavior). The ~100-byte overflow past the
    // cap is deliberate — the disclosure is the point.
    let mut capped = policy.cap_output(content.text);
    if content.truncated {
        capped.push_str(&format!(
            "\n...[truncated: output exceeded byte cap — served {} KiB of {} KiB total; re-read a line range for the rest]",
            served_bytes / 1024,
            content.total_bytes / 1024,
        ));
    }
    Ok(capped)
}

// --- fs.read_csv ------------------------------------------------------------

/// Default number of data rows returned in one `fs_read_csv` window when the
/// caller does not specify `max_rows`. Small enough to stay well under the
/// output cap on wide tables; the caller pages with `start_row`/`max_rows`.
const CSV_DEFAULT_MAX_ROWS: usize = 50;
/// Hard ceiling on a single window regardless of the requested `max_rows`, so
/// one call can never blow the output budget on a huge table.
const CSV_MAX_ROWS_CEILING: usize = 500;
/// Per-column values sampled for type inference (bounds work on wide files).
const CSV_TYPE_SAMPLE: usize = 200;

#[derive(Deserialize)]
struct ReadCsvArgs {
    path: String,
    /// 1-based first DATA row to show (the header is never a data row).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    start_row: Option<usize>,
    /// Max data rows in this window (clamped to `CSV_MAX_ROWS_CEILING`).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_rows: Option<usize>,
    /// Single-character field delimiter; defaults to ',' (accepts e.g. "\t").
    #[serde(default)]
    delimiter: Option<String>,
    /// Treat the first row as data, not a header (default: header present).
    #[serde(default, deserialize_with = "lenient_bool")]
    no_header: bool,
}

/// Accept `true`/`false`, `"true"`/`"false"` (any case), and `0`/`1` for
/// boolean flags: small models routinely emit booleans as quoted strings, and
/// a hard serde rejection burns a whole tool round with no repair hint — the
/// same footgun `lenient_opt_usize` fixes for integers (found via a live
/// llama3.2 fs_read_csv run passing `no_header: "false"`).
fn lenient_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Lenient {
        Bool(bool),
        Int(i64),
        Text(String),
    }
    match Lenient::deserialize(deserializer)? {
        Lenient::Bool(b) => Ok(b),
        Lenient::Int(0) => Ok(false),
        Lenient::Int(1) => Ok(true),
        Lenient::Int(other) => Err(serde::de::Error::custom(format!(
            "expected a boolean, got {other} — pass true or false"
        ))),
        Lenient::Text(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Ok(true),
            "false" | "no" | "0" | "" => Ok(false),
            other => Err(serde::de::Error::custom(format!(
                "expected a boolean, got '{other}' — pass true or false (quoted \"true\" is also accepted)"
            ))),
        },
    }
}

/// A column's inferred value type across the sampled rows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CsvType {
    Empty,
    Integer,
    Float,
    Bool,
    String,
}

impl CsvType {
    fn label(self) -> &'static str {
        match self {
            CsvType::Empty => "empty",
            CsvType::Integer => "int",
            CsvType::Float => "float",
            CsvType::Bool => "bool",
            CsvType::String => "string",
        }
    }

    /// The type of a single cell value in isolation.
    fn of_cell(value: &str) -> CsvType {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return CsvType::Empty;
        }
        if trimmed.parse::<i64>().is_ok() {
            return CsvType::Integer;
        }
        if trimmed.parse::<f64>().is_ok() {
            return CsvType::Float;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower == "true" || lower == "false" {
            return CsvType::Bool;
        }
        CsvType::String
    }

    /// Fold a new cell type into the running column type. Integers widen to
    /// floats; anything mixed or non-numeric collapses to string; `Empty`
    /// cells never override a known type (nullable columns keep their type).
    fn merge(self, cell: CsvType) -> CsvType {
        match (self, cell) {
            (a, CsvType::Empty) => a,
            (CsvType::Empty, b) => b,
            (a, b) if a == b => a,
            (CsvType::Integer, CsvType::Float) | (CsvType::Float, CsvType::Integer) => {
                CsvType::Float
            }
            _ => CsvType::String,
        }
    }
}

pub fn read_csv_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_csv".to_string(),
        description: "Read a CSV/TSV file as structured data from an allowed path: reports column \
                      headers, total data-row count, and an inferred type per column, then returns \
                      a bounded window of rows. Page with start_row (1-based data row) and \
                      max_rows. Set delimiter for TSV (\"\\t\") and no_header for headerless files. \
                      Malformed rows (wrong field count) are disclosed, never silently dropped."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "CSV/TSV file path to read"},
                "start_row": {"type": "integer", "description": "1-based first data row to show (optional, default 1)"},
                "max_rows": {"type": "integer", "description": "Max data rows in this window (optional, default 50, max 500)"},
                "delimiter": {"type": "string", "description": "Field delimiter, one character (optional, default ',', use \"\\t\" for TSV)"},
                "no_header": {"type": "boolean", "description": "Treat the first row as data rather than a header (optional, default false)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_csv_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_csv_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadCsvArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let delimiter = match a.delimiter.as_deref() {
        // An omitted or empty delimiter defaults to comma: small models pass
        // `""` for optional string args constantly, and erroring on it burns a
        // tool round (found via a live llama3.2 fs_read_csv run).
        None | Some("") => b',',
        Some("\\t") | Some("\t") => b'\t',
        Some(text) => {
            let mut bytes = text.bytes();
            match (bytes.next(), bytes.next()) {
                (Some(b), None) => b,
                _ => {
                    return Err(ToolError::InvalidArgs(format!(
                        "delimiter must be a single character, got {text:?} (use \"\\t\" for TSV)"
                    )))
                }
            }
        }
    };
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let max_bytes = policy.max_bytes();
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let content = tokio::task::spawn_blocking(move || read_capped_text(file, max_bytes))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;

    let start_row = a.start_row.unwrap_or(1).max(1);
    let max_rows = a
        .max_rows
        .unwrap_or(CSV_DEFAULT_MAX_ROWS)
        .min(CSV_MAX_ROWS_CEILING);
    let rendered = render_csv(
        &content.text,
        delimiter,
        a.no_header,
        start_row,
        max_rows,
        content.truncated,
    );
    Ok(policy.cap_output(rendered))
}

/// Pure CSV formatter: parses `text` and produces the disclosure block. Kept
/// free of IO so it is unit-testable in isolation.
fn render_csv(
    text: &str,
    delimiter: u8,
    no_header: bool,
    start_row: usize,
    max_rows: usize,
    byte_truncated: bool,
) -> String {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false) // we manage the header row ourselves
        .flexible(true) // accept ragged rows so we can DISCLOSE them, not error
        .from_reader(text.as_bytes());

    let mut records = reader.records();
    let headers: Vec<String> = if no_header {
        Vec::new()
    } else {
        match records.next() {
            Some(Ok(rec)) => rec.iter().map(|f| f.to_string()).collect(),
            Some(Err(e)) => return format!("csv> parse error on header row: {e}"),
            None => return "csv> empty file (no rows)".to_string(),
        }
    };

    let expected_cols = headers.len();
    let mut col_types: Vec<CsvType> = Vec::new();
    let mut total_rows = 0usize;
    let mut malformed: Vec<(usize, usize)> = Vec::new();
    let mut window: Vec<Vec<String>> = Vec::new();
    let end_row = start_row.saturating_add(max_rows);

    for rec in records {
        let rec = match rec {
            Ok(rec) => rec,
            Err(_) => continue,
        };
        // A trailing empty line parses as a zero-field record; ignore it.
        if rec.is_empty() || (rec.len() == 1 && rec.get(0).map(str::is_empty).unwrap_or(false)) {
            continue;
        }
        total_rows += 1;
        let fields: Vec<String> = rec.iter().map(|f| f.to_string()).collect();

        // Grow inferred columns if a header was absent or a row is wider.
        if fields.len() > col_types.len() {
            col_types.resize(fields.len(), CsvType::Empty);
        }
        if total_rows <= CSV_TYPE_SAMPLE {
            for (idx, value) in fields.iter().enumerate() {
                col_types[idx] = col_types[idx].merge(CsvType::of_cell(value));
            }
        }
        if expected_cols > 0 && fields.len() != expected_cols {
            malformed.push((total_rows, fields.len()));
        }
        if total_rows >= start_row && total_rows < end_row {
            window.push(fields);
        }
    }

    // With a header, its width is authoritative; a wider malformed row is an
    // anomaly we disclose, not an extra column. Headerless files report the
    // widest row seen.
    let col_count = if headers.is_empty() {
        col_types.len()
    } else {
        expected_cols
    };
    let mut out = String::new();
    let kind = if delimiter == b'\t' { "TSV" } else { "CSV" };
    out.push_str(&format!(
        "csv> {kind}: {col_count} columns, {total_rows} data rows"
    ));
    if byte_truncated {
        out.push_str(" (file exceeded the read byte cap — row count and window cover only the portion read; re-read with a smaller window is not needed, but totals are a lower bound)");
    }
    out.push('\n');

    if !headers.is_empty() {
        let cols: Vec<String> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let ty = col_types.get(i).copied().unwrap_or(CsvType::Empty);
                format!("{h}:{}", ty.label())
            })
            .collect();
        out.push_str(&format!("columns: {}\n", cols.join(", ")));
    } else {
        let cols: Vec<String> = col_types
            .iter()
            .enumerate()
            .map(|(i, ty)| format!("col{}:{}", i + 1, ty.label()))
            .collect();
        out.push_str(&format!("columns (no header): {}\n", cols.join(", ")));
    }

    if total_rows == 0 {
        out.push_str("(no data rows)\n");
    } else if window.is_empty() {
        out.push_str(&format!(
            "(no rows in window: start_row {start_row} is past the {total_rows} data rows)\n"
        ));
    } else {
        let last = start_row + window.len() - 1;
        out.push_str(&format!("rows {start_row}-{last} of {total_rows}:\n"));
        for (offset, fields) in window.iter().enumerate() {
            let row_no = start_row + offset;
            let cells: Vec<String> = fields
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let name = headers
                        .get(i)
                        .map(String::as_str)
                        .unwrap_or("col")
                        .to_string();
                    if headers.is_empty() {
                        format!("{name}{}={v}", i + 1)
                    } else {
                        format!("{name}={v}")
                    }
                })
                .collect();
            out.push_str(&format!("{row_no} | {}\n", cells.join(" | ")));
        }
        if last < total_rows {
            out.push_str(&format!(
                "[showing {} of {total_rows} rows; pass start_row/max_rows for more]\n",
                window.len()
            ));
        }
    }

    if !malformed.is_empty() {
        let shown: Vec<String> = malformed
            .iter()
            .take(10)
            .map(|(row, got)| format!("row {row} ({got} fields, expected {expected_cols})"))
            .collect();
        let more = if malformed.len() > 10 {
            format!(" (+{} more)", malformed.len() - 10)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "[disclosure: {} row(s) with an unexpected field count: {}{}]\n",
            malformed.len(),
            shown.join(", "),
            more
        ));
    }

    out
}

/// Accept both `1` and `"1"` for optional line numbers: small models emit
/// numeric strings constantly, and rejecting them burned a round per call
/// with no repair hint (cycle-27 audit).
fn lenient_opt_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Lenient {
        Number(usize),
        Text(String),
    }
    match Option::<Lenient>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Lenient::Number(value)) => Ok(Some(value)),
        Some(Lenient::Text(text)) => text.trim().parse::<usize>().map(Some).map_err(|_| {
            serde::de::Error::custom(format!(
                "expected a line number, got '{text}' — pass an integer like 12 (quoted \"12\" is also accepted)"
            ))
        }),
    }
}

// --- fs.read_pdf ------------------------------------------------------------

/// Default number of pages returned in one `fs_read_pdf` window.
const PDF_DEFAULT_MAX_PAGES: usize = 10;
/// Hard ceiling on pages per call so one call cannot blow the output budget.
const PDF_MAX_PAGES_CEILING: usize = 50;
/// Refuse to parse a PDF larger than this (lopdf loads the whole file; this
/// bounds memory). Generous for text documents, well below OOM territory.
const PDF_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Deserialize)]
struct ReadPdfArgs {
    path: String,
    /// 1-based first page to extract (optional, default 1).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    start_page: Option<usize>,
    /// Max pages in this window (clamped to `PDF_MAX_PAGES_CEILING`).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_pages: Option<usize>,
}

pub fn read_pdf_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_pdf".to_string(),
        description: "Extract the text layer of a PDF from an allowed path, page by page. Reports \
                      total page count and returns a bounded page window (start_page/max_pages). \
                      Text-layer only — no OCR; a scanned/image-only page is disclosed as having \
                      no extractable text rather than returning garbage."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "PDF file path to read"},
                "start_page": {"type": "integer", "description": "1-based first page (optional, default 1)"},
                "max_pages": {"type": "integer", "description": "Max pages in this window (optional, default 10, max 50)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_pdf_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_pdf_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadPdfArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let metadata = file
        .metadata()
        .map_err(|e| ToolError::ExecError(format!("stat failed: {e}")))?;
    if metadata.len() > PDF_MAX_FILE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "PDF is {} MiB, over the {} MiB read limit — extract the pages you need with an external tool first",
            metadata.len() / (1024 * 1024),
            PDF_MAX_FILE_BYTES / (1024 * 1024),
        )));
    }
    let path = authorization.path().to_path_buf();
    let start_page = a.start_page.unwrap_or(1).max(1);
    let max_pages = a
        .max_pages
        .unwrap_or(PDF_DEFAULT_MAX_PAGES)
        .min(PDF_MAX_PAGES_CEILING);

    let rendered = tokio::task::spawn_blocking(move || extract_pdf(&path, start_page, max_pages))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("pdf parse failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

/// Load `path` with lopdf and render the disclosure block for the requested
/// page window. Blocking (CPU-bound parse); callers run it off the reactor.
fn extract_pdf(path: &Path, start_page: usize, max_pages: usize) -> Result<String, String> {
    let doc = lopdf::Document::load(path).map_err(|e| e.to_string())?;
    let pages = doc.get_pages(); // BTreeMap<u32, ObjectId>, 1-based page numbers
    let total_pages = pages.len();
    let mut extracted: Vec<(u32, Result<String, String>)> = Vec::new();
    let end_page = start_page.saturating_add(max_pages);
    for &page_no in pages.keys() {
        let idx = page_no as usize;
        if idx < start_page || idx >= end_page {
            continue;
        }
        let text = doc
            .extract_text(&[page_no])
            .map_err(|e| e.to_string())
            .map(|t| t.trim().to_string());
        extracted.push((page_no, text));
    }
    Ok(render_pdf(&extracted, total_pages, start_page))
}

/// Pure formatter: turn per-page extraction results into the disclosure block.
/// Kept IO-free so it is unit-testable without a real PDF.
fn render_pdf(
    extracted: &[(u32, Result<String, String>)],
    total_pages: usize,
    start_page: usize,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "pdf> {total_pages} page(s), text-layer only (no OCR)\n"
    ));
    if total_pages == 0 {
        out.push_str("(no pages found — the file may not be a valid PDF)\n");
        return out;
    }
    if extracted.is_empty() {
        out.push_str(&format!(
            "(no pages in window: start_page {start_page} is past the {total_pages} page(s))\n"
        ));
        return out;
    }
    let first = extracted.first().map(|(n, _)| *n).unwrap_or(0);
    let last = extracted.last().map(|(n, _)| *n).unwrap_or(0);
    out.push_str(&format!("pages {first}-{last} of {total_pages}:\n"));
    let mut image_only = 0usize;
    for (page_no, result) in extracted {
        out.push_str(&format!("\n--- page {page_no} ---\n"));
        match result {
            Ok(text) if !text.is_empty() => {
                out.push_str(text);
                out.push('\n');
            }
            Ok(_) => {
                image_only += 1;
                out.push_str("(no extractable text on this page — likely a scanned/image-only page; OCR is not applied)\n");
            }
            Err(e) => {
                out.push_str(&format!("(text extraction failed for this page: {e})\n"));
            }
        }
    }
    if (last as usize) < total_pages {
        out.push_str(&format!(
            "\n[showing pages {first}-{last} of {total_pages}; pass start_page/max_pages for more]\n"
        ));
    }
    if image_only > 0 {
        out.push_str(&format!(
            "[disclosure: {image_only} page(s) in this window had no text layer]\n"
        ));
    }
    out
}

// --- fs.read_xlsx -----------------------------------------------------------

const XLSX_DEFAULT_MAX_ROWS: usize = 50;
const XLSX_MAX_ROWS_CEILING: usize = 500;
/// Refuse to parse a spreadsheet larger than this (calamine decompresses the
/// whole workbook; this bounds memory). Generous for real spreadsheets.
const XLSX_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Deserialize)]
struct ReadXlsxArgs {
    path: String,
    /// Sheet to read: a name, or a 0-based index as a string; default first.
    #[serde(default)]
    sheet: Option<String>,
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    start_row: Option<usize>,
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_rows: Option<usize>,
}

pub fn read_xlsx_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_xlsx".to_string(),
        description:
            "Read a spreadsheet (.xlsx/.xls/.ods) from an allowed path: lists sheet names, \
                      then returns a bounded cell window of one sheet (sheet by name or 0-based \
                      index; default the first). Page with start_row/max_rows. Empty cells render \
                      blank; cell types are preserved as text."
                .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Spreadsheet file path"},
                "sheet": {"type": "string", "description": "Sheet name, or a 0-based index as a string (optional, default first sheet)"},
                "start_row": {"type": "integer", "description": "1-based first row to show (optional, default 1)"},
                "max_rows": {"type": "integer", "description": "Max rows in this window (optional, default 50, max 500)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_xlsx_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_xlsx_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadXlsxArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let metadata = file
        .metadata()
        .map_err(|e| ToolError::ExecError(format!("stat failed: {e}")))?;
    if metadata.len() > XLSX_MAX_FILE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "spreadsheet is {} MiB, over the {} MiB read limit",
            metadata.len() / (1024 * 1024),
            XLSX_MAX_FILE_BYTES / (1024 * 1024),
        )));
    }
    let path = authorization.path().to_path_buf();
    let sheet = a.sheet.clone();
    let start_row = a.start_row.unwrap_or(1).max(1);
    let max_rows = a
        .max_rows
        .unwrap_or(XLSX_DEFAULT_MAX_ROWS)
        .min(XLSX_MAX_ROWS_CEILING);
    let rendered = tokio::task::spawn_blocking(move || {
        extract_xlsx(&path, sheet.as_deref(), start_row, max_rows)
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("spreadsheet parse failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

/// Load `path` with calamine and render the sheet list + a bounded cell window.
/// Blocking (decompresses the workbook); callers run it off the reactor.
fn extract_xlsx(
    path: &Path,
    sheet: Option<&str>,
    start_row: usize,
    max_rows: usize,
) -> Result<String, String> {
    use calamine::{open_workbook_auto, Reader};

    let mut workbook = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let names = workbook.sheet_names().to_vec();
    if names.is_empty() {
        return Ok("xlsx> no sheets found (the file may not be a valid spreadsheet)".to_string());
    }
    // Resolve the requested sheet: exact name, else a 0-based index string,
    // else the first sheet.
    let chosen = match sheet {
        None => names[0].clone(),
        Some(s) if names.iter().any(|n| n == s) => s.to_string(),
        Some(s) => match s.trim().parse::<usize>() {
            Ok(idx) if idx < names.len() => names[idx].clone(),
            _ => {
                return Ok(format!(
                    "xlsx> {} sheet(s): {}\n(sheet {:?} not found — pass an existing name or a 0-based index)",
                    names.len(),
                    names.join(", "),
                    s
                ))
            }
        },
    };
    let range = workbook
        .worksheet_range(&chosen)
        .map_err(|e| e.to_string())?;
    let (total_rows, total_cols) = range.get_size();

    let mut out = format!(
        "xlsx> {} sheet(s): {}\nsheet '{}': {} rows x {} cols\n",
        names.len(),
        names.join(", "),
        chosen,
        total_rows,
        total_cols
    );
    if total_rows == 0 {
        out.push_str("(sheet is empty)\n");
        return Ok(out);
    }
    let end_row = start_row.saturating_add(max_rows);
    let mut shown = 0usize;
    for (idx, row) in range.rows().enumerate() {
        let row_no = idx + 1; // 1-based
        if row_no < start_row || row_no >= end_row {
            continue;
        }
        if shown == 0 {
            let last = (start_row + max_rows - 1).min(total_rows);
            out.push_str(&format!("rows {start_row}-{last} of {total_rows}:\n"));
        }
        let cells: Vec<String> = row
            .iter()
            .map(|c| {
                if matches!(c, calamine::Data::Empty) {
                    String::new()
                } else {
                    c.to_string()
                }
            })
            .collect();
        out.push_str(&format!("{row_no} | {}\n", cells.join(" | ")));
        shown += 1;
    }
    if shown == 0 {
        out.push_str(&format!(
            "(no rows in window: start_row {start_row} is past the {total_rows} rows)\n"
        ));
    } else if start_row + shown - 1 < total_rows {
        out.push_str(&format!(
            "[showing {shown} of {total_rows} rows; pass start_row/max_rows for more]\n"
        ));
    }
    Ok(out)
}

// --- fs.read_image (metadata only) ------------------------------------------

/// Bytes read from an image for header/metadata parsing. Every format keeps
/// its dimensions in the leading bytes, so this bounds memory regardless of
/// the image's on-disk size.
const IMAGE_HEADER_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
struct ReadImageArgs {
    path: String,
}

pub fn read_image_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_image".to_string(),
        description: "Report an image's format and pixel dimensions (PNG/JPEG/GIF/WEBP/BMP and \
                      more) from its header — metadata only. This is a TEXT tool: it does NOT \
                      interpret the picture's content; understanding what an image depicts \
                      requires a vision-capable model/gateway. Use it to confirm a file is a \
                      valid image and learn its size without hallucinating its contents."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Image file path"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_image_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_image_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadImageArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let display = a.path.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        let mut header = Vec::new();
        Read::take(file, IMAGE_HEADER_BYTES)
            .read_to_end(&mut header)
            .map_err(|e| format!("read failed: {e}"))?;
        Ok::<_, String>(render_image_metadata(&display, &header))
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(ToolError::ExecError)?;
    Ok(policy.cap_output(rendered))
}

/// Pure formatter: derive format + dimensions from an image header. IO-free so
/// it is unit-testable from a byte slice.
fn render_image_metadata(path: &str, header: &[u8]) -> String {
    let fmt = imagesize::image_type(header)
        .map(|t| format!("{t:?}"))
        .unwrap_or_else(|_| "unrecognized".to_string());
    match imagesize::blob_size(header) {
        Ok(sz) => format!(
            "image> {path}: {fmt}, {}x{} px (metadata only — this text tool does not interpret \
             image content; a vision-capable model is required to see what it depicts)\n",
            sz.width, sz.height
        ),
        Err(_) => format!(
            "image> {path}: detected {fmt} but could not read dimensions from the header \
             (truncated or unsupported variant); not an interpretable image for a text tool\n"
        ),
    }
}

// --- fs.read_document (type-aware dispatch) ---------------------------------

#[derive(Deserialize)]
struct ReadDocumentArgs {
    path: String,
}

/// The document class `read_document` detected, and how it will be handled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DocType {
    /// Delegated to `fs_read_csv`.
    Csv,
    /// Delegated to `fs_read_pdf`.
    Pdf,
    /// Delegated to the plain UTF-8 text read (`fs_read_file`).
    Text,
    /// Recognized image; no text to extract from a text tool.
    Image(&'static str),
    /// Zip-based Office document (xlsx/docx/pptx); no extractor yet.
    Office(&'static str),
    /// Jupyter notebook (`.ipynb`) — JSON with a `cells` array.
    Notebook,
    /// HTML document (`.html`/`.htm`) — extracted to plain text.
    Html,
}

/// Detect the document class from magic bytes first (authoritative for binary
/// formats), then fall back to the file extension (needed for CSV/TSV, which
/// have no magic number). Unknown → `Text`, so the UTF-8-with-truncation
/// behavior is preserved for anything unrecognized.
fn detect_doc_type(path: &Path, magic: &[u8]) -> DocType {
    // Magic numbers for binary formats we recognize.
    if magic.starts_with(b"%PDF-") {
        return DocType::Pdf;
    }
    if magic.starts_with(&[0x89, b'P', b'N', b'G']) {
        return DocType::Image("PNG");
    }
    if magic.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return DocType::Image("JPEG");
    }
    if magic.starts_with(b"GIF87a") || magic.starts_with(b"GIF89a") {
        return DocType::Image("GIF");
    }
    // RIFF....WEBP
    if magic.len() >= 12 && magic.starts_with(b"RIFF") && &magic[8..12] == b"WEBP" {
        return DocType::Image("WEBP");
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    // Zip container: distinguish Office formats by extension.
    if magic.starts_with(&[b'P', b'K', 0x03, 0x04]) {
        return match ext.as_deref() {
            Some("xlsx") => DocType::Office("XLSX"),
            Some("docx") => DocType::Office("DOCX"),
            Some("pptx") => DocType::Office("PPTX"),
            _ => DocType::Office("ZIP-based"),
        };
    }
    match ext.as_deref() {
        Some("csv") | Some("tsv") => DocType::Csv,
        Some("ipynb") => DocType::Notebook,
        Some("html") | Some("htm") => DocType::Html,
        Some("pdf") => DocType::Pdf, // extensionless-magic already handled above
        // Extension-only image hints (e.g. a truncated header) still classify.
        Some("png") => DocType::Image("PNG"),
        Some("jpg") | Some("jpeg") => DocType::Image("JPEG"),
        Some("gif") => DocType::Image("GIF"),
        Some("webp") => DocType::Image("WEBP"),
        Some("xlsx") => DocType::Office("XLSX"),
        Some("docx") => DocType::Office("DOCX"),
        Some("pptx") => DocType::Office("PPTX"),
        _ => DocType::Text,
    }
}

pub fn read_document_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "read_document".to_string(),
        description: "Read a file by auto-detecting its type (extension + magic bytes) and routing \
                      to the right reader: CSV/TSV → structured table, PDF → text layer, plain text \
                      → text, XLSX → spreadsheet cells, DOCX → paragraph text, PPTX → slide text, \
                      .ipynb → notebook cells, .html/.htm → readable text. Always discloses the \
                      detected type and which reader ran. Images report metadata only; unknown \
                      zip-based Office types are detected and disclosed but not text-extracted. \
                      Unknown types fall back to a UTF-8 text read. For paging a large CSV/PDF, \
                      call fs_read_csv/fs_read_pdf directly."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to read (any supported type)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_document_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_document_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadDocumentArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    // Authorize + sniff the first bytes for magic-number detection. This uses
    // the same guard the delegated readers use, so no path escapes it.
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let mut magic = [0u8; 16];
    let n = {
        use std::io::Read as _;
        let mut f = &file;
        f.read(&mut magic).unwrap_or(0)
    };
    let doc_type = detect_doc_type(authorization.path(), &magic[..n]);

    // Re-serialize just the path for the delegate readers (they re-authorize
    // the same path — cheap and keeps a single guard implementation).
    let path_args = serde_json::json!({ "path": a.path }).to_string();
    let (label, body) = match doc_type {
        DocType::Csv => (
            "CSV/TSV → fs_read_csv",
            read_csv_run(policy, &path_args).await?,
        ),
        DocType::Pdf => ("PDF → fs_read_pdf", read_pdf_run(policy, &path_args).await?),
        DocType::Text => (
            "text → fs_read_file",
            read_file_run(policy, &path_args).await?,
        ),
        DocType::Image(_) => (
            "image → fs_read_image (metadata)",
            read_image_run(policy, &path_args).await?,
        ),
        DocType::Office("XLSX") => (
            "XLSX → fs_read_xlsx",
            read_xlsx_run(policy, &path_args).await?,
        ),
        DocType::Office("DOCX") => (
            "DOCX → fs_read_docx",
            read_docx_run(policy, &path_args).await?,
        ),
        DocType::Office("PPTX") => (
            "PPTX → fs_read_pptx",
            read_pptx_run(policy, &path_args).await?,
        ),
        DocType::Notebook => (
            "notebook → fs_read_notebook",
            read_notebook_run(policy, &path_args).await?,
        ),
        DocType::Html => (
            "HTML → fs_read_html",
            read_html_run(policy, &path_args).await?,
        ),
        DocType::Office(fmt) => (
            "office (no extractor yet)",
            format!(
                "detected {fmt} (zip-based Office document). kitsu has no structured extractor \
                 for this type yet — convert it to CSV/text first, or use a dedicated tool. Not \
                 read as raw bytes to avoid emitting binary garbage."
            ),
        ),
    };
    let disclosure = format!("read_document> {}: {label}\n", a.path);
    // The delegate readers already cap their own output; the disclosure line
    // is a small fixed prefix, so no additional cap is needed here.
    Ok(format!("{disclosure}{body}"))
}

// --- fs.read_docx (Word paragraph-text extraction) --------------------------

const DOCX_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Decompressed `word/document.xml` cap — bounds a zip-bomb regardless of the
/// (compressed) file-size guard above.
const DOCX_MAX_XML_BYTES: u64 = 64 * 1024 * 1024;
const DOCX_DEFAULT_MAX_PARAGRAPHS: usize = 500;
const DOCX_MAX_PARAGRAPHS_CEILING: usize = 5000;
/// Hard ceiling on paragraphs collected while parsing (bounds memory even
/// before the display cap applies).
const DOCX_MAX_COLLECT: usize = 50_000;

#[derive(Deserialize)]
struct ReadDocxArgs {
    path: String,
    /// Paragraphs shown (default 500, max 5000).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_paragraphs: Option<usize>,
}

/// Parse `word/document.xml` text: concatenate `<w:t>` runs into paragraphs
/// (`<w:p>`), turning `<w:tab/>` into a tab and `<w:br/>`/`<w:cr/>` into a
/// newline. Returns the collected paragraphs and whether the collect ceiling
/// was hit. Malformed XML stops parsing and returns what was gathered.
/// Shared OOXML text extraction: concatenate `text_tag` runs into paragraphs
/// delimited by `para_tag`, mapping `tab_tag` → tab and any `break_tags` →
/// newline. Used by both docx (`w:t`/`w:p`) and pptx (`a:t`/`a:p`). Malformed
/// XML stops parsing and returns what was gathered (honest partial).
fn parse_ooxml_paragraphs(
    xml: &str,
    text_tag: &[u8],
    para_tag: &[u8],
    tab_tag: Option<&[u8]>,
    break_tags: &[&[u8]],
) -> (Vec<String>, bool) {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    let mut collect_capped = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == text_tag => in_text = true,
            Ok(Event::End(e)) => {
                let name = e.name();
                if name.as_ref() == text_tag {
                    in_text = false;
                } else if name.as_ref() == para_tag {
                    paragraphs.push(std::mem::take(&mut current));
                    if paragraphs.len() >= DOCX_MAX_COLLECT {
                        collect_capped = true;
                        break;
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.name();
                if tab_tag == Some(name.as_ref()) {
                    current.push('\t');
                } else if break_tags.contains(&name.as_ref()) {
                    current.push('\n');
                }
            }
            Ok(Event::Text(e)) if in_text => {
                if let Ok(decoded) = e.decode() {
                    match quick_xml::escape::unescape(&decoded) {
                        Ok(text) => current.push_str(&text),
                        Err(_) => current.push_str(&decoded),
                    }
                }
            }
            // 0.41 surfaces `&ref;` / `&#n;` as their own event, not inside Text.
            Ok(Event::GeneralRef(e)) if in_text => {
                if let Ok(Some(c)) = e.resolve_char_ref() {
                    current.push(c);
                } else if let Ok(name) = e.decode() {
                    match name.as_ref() {
                        "amp" => current.push('&'),
                        "lt" => current.push('<'),
                        "gt" => current.push('>'),
                        "quot" => current.push('"'),
                        "apos" => current.push('\''),
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    // A trailing run not closed by the paragraph tag.
    if !current.is_empty() {
        paragraphs.push(current);
    }
    (paragraphs, collect_capped)
}

fn parse_docx_paragraphs(xml: &str) -> (Vec<String>, bool) {
    parse_ooxml_paragraphs(
        xml,
        b"w:t",
        b"w:p",
        Some(b"w:tab".as_slice()),
        &[b"w:br".as_slice(), b"w:cr".as_slice()],
    )
}

fn render_docx(display: &str, xml: &str, max_paragraphs: usize, xml_truncated: bool) -> String {
    let (paragraphs, collect_capped) = parse_docx_paragraphs(xml);
    // Empty <w:p> are spacing; report and display only paragraphs with text.
    let with_text: Vec<&String> = paragraphs.iter().filter(|p| !p.trim().is_empty()).collect();
    let total = with_text.len();

    let mut notes = String::new();
    if xml_truncated {
        notes.push_str(&format!(
            ", document.xml truncated at {} MiB",
            DOCX_MAX_XML_BYTES / (1024 * 1024)
        ));
    }
    if collect_capped {
        notes.push_str(&format!(", stopped at {DOCX_MAX_COLLECT} paragraphs"));
    }

    let mut out = format!(
        "docx> {display}: {total} paragraph(s) with text \
         [text only — no formatting, tables, or images]{notes}\n"
    );
    if total == 0 {
        out.push_str(
            "(no extractable text — the document may be empty, image-only, or use an \
             unsupported structure)\n",
        );
        return out;
    }
    let shown = total.min(max_paragraphs);
    for para in &with_text[..shown] {
        out.push_str(para);
        out.push('\n');
    }
    if total > shown {
        out.push_str(&format!(
            "[showing {shown} of {total} paragraphs; raise max_paragraphs for more]\n"
        ));
    }
    out
}

fn extract_docx(path: &Path, display: &str, max_paragraphs: usize) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| std::io::Error::other(format!("not a valid .docx (zip) file: {e}")))?;
    let entry = archive.by_name("word/document.xml").map_err(|_| {
        std::io::Error::other("not a Word .docx document (no word/document.xml inside)")
    })?;
    let mut buf = Vec::new();
    Read::take(entry, DOCX_MAX_XML_BYTES).read_to_end(&mut buf)?;
    let xml_truncated = buf.len() as u64 >= DOCX_MAX_XML_BYTES;
    let xml = String::from_utf8_lossy(&buf);
    Ok(render_docx(display, &xml, max_paragraphs, xml_truncated))
}

pub fn read_docx_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_docx".to_string(),
        description: "Extract the paragraph text of a Word .docx document (text only — no \
                      formatting, tables, or images), bounded and paginated by paragraph. \
                      Pure-Rust zip + XML; same allowlist as fs_read_file; a non-.docx or \
                      corrupt file is disclosed honestly, never emitted as binary garbage."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to a .docx file"},
                "max_paragraphs": {"type": "integer", "description": "Paragraphs shown (optional, default 500, max 5000)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_docx_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_docx_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadDocxArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let metadata = file
        .metadata()
        .map_err(|e| ToolError::ExecError(format!("stat failed: {e}")))?;
    if metadata.len() > DOCX_MAX_FILE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "document is {} MiB, over the {} MiB read limit",
            metadata.len() / (1024 * 1024),
            DOCX_MAX_FILE_BYTES / (1024 * 1024),
        )));
    }
    let path = authorization.path().to_path_buf();
    let display = a.path.clone();
    let max = a
        .max_paragraphs
        .unwrap_or(DOCX_DEFAULT_MAX_PARAGRAPHS)
        .clamp(1, DOCX_MAX_PARAGRAPHS_CEILING);
    let rendered = tokio::task::spawn_blocking(move || extract_docx(&path, &display, max))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("docx parse failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

// --- fs.read_html (best-effort HTML → text) ---------------------------------

const HTML_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Deserialize)]
struct ReadHtmlArgs {
    path: String,
}

/// Common HTML named entities beyond the XML five (which quick_xml decodes).
fn html_named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "nbsp" => " ",
        "copy" => "(c)",
        "reg" => "(r)",
        "trade" => "(tm)",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "lsquo" | "rsquo" | "sbquo" => "'",
        "ldquo" | "rdquo" | "bdquo" => "\"",
        "laquo" => "«",
        "raquo" => "»",
        "bull" => "•",
        "middot" => "·",
        "deg" => "°",
        _ => return None,
    })
}

/// Decode one entity body (the text between `&` and `;`). Falls back to the
/// literal `&body;` when unknown.
fn decode_html_entity(body: &str) -> String {
    if let Some(mapped) = html_named_entity(body) {
        return mapped.to_string();
    }
    if let Some(rest) = body.strip_prefix('#') {
        let code = if let Some(hex) = rest.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()
        } else {
            rest.parse::<u32>().ok()
        };
        if let Some(c) = code.and_then(char::from_u32) {
            return c.to_string();
        }
    }
    match quick_xml::escape::unescape(&format!("&{body};")) {
        Ok(s) => s.into_owned(),
        Err(_) => format!("&{body};"),
    }
}

/// A block-level tag whose presence should become a line break in the text.
fn is_html_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "br"
            | "hr"
            | "li"
            | "tr"
            | "table"
            | "blockquote"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "main"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "pre"
            | "figure"
            | "figcaption"
            | "dl"
            | "dt"
            | "dd"
            | "form"
            | "title"
    )
}

/// Collapse each line's internal whitespace to single spaces and cap runs of
/// blank lines at one.
fn collapse_html_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_blank = false;
    for line in text.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            pending_blank = true;
            continue;
        }
        if !out.is_empty() && pending_blank {
            out.push('\n');
        }
        pending_blank = false;
        out.push_str(&collapsed);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Best-effort HTML → text: drop `<script>`/`<style>`/comments wholesale (so
/// `a<b` inside a script never derails tag parsing), strip remaining tags,
/// emit a newline at block-level tags, and decode entities.
fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let hb = html.as_bytes();
    let n = hb.len();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    while i < n {
        match hb[i] {
            b'<' => {
                if lower[i..].starts_with("<!--") {
                    i = lower[i..].find("-->").map_or(n, |e| i + e + 3);
                    continue;
                }
                // Skip <script>…</script> and <style>…</style> as raw blocks.
                let mut skipped = false;
                for tag in ["script", "style"] {
                    let open = format!("<{tag}");
                    if lower[i..].starts_with(&open)
                        && matches!(
                            hb.get(i + open.len()),
                            Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
                        )
                    {
                        let close = format!("</{tag}>");
                        i = lower[i..].find(&close).map_or(n, |e| i + e + close.len());
                        skipped = true;
                        break;
                    }
                }
                if skipped {
                    continue;
                }
                let Some(rel_end) = lower[i..].find('>') else {
                    break; // unterminated tag — drop the rest
                };
                let inner = &lower[i + 1..i + rel_end];
                let name_part = inner.strip_prefix('/').unwrap_or(inner);
                let name: String = name_part
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                if is_html_block_tag(&name) && !out.ends_with('\n') {
                    out.push('\n');
                }
                i += rel_end + 1;
            }
            b'&' => {
                // Bounded lookahead for the entity terminator.
                if let Some(rel) = lower[i + 1..].find(';') {
                    if rel <= 32 {
                        out.push_str(&decode_html_entity(&html[i + 1..i + 1 + rel]));
                        i += 1 + rel + 1;
                        continue;
                    }
                }
                out.push('&');
                i += 1;
            }
            _ => {
                // Append the whole run of text up to the next '<' or '&'.
                let rest = &html[i..];
                let stop = rest.find(['<', '&']).unwrap_or(rest.len());
                out.push_str(&rest[..stop]);
                i += stop;
            }
        }
    }
    collapse_html_whitespace(&out)
}

fn render_html(display: &str, html: &str) -> String {
    let text = html_to_text(html);
    let mut out = format!(
        "html> {display}: text extracted (tags stripped, scripts/styles removed, \
         whitespace collapsed — layout and formatting are not preserved)\n"
    );
    if text.is_empty() {
        out.push_str("(no extractable text)\n");
    } else {
        out.push_str(&text);
        out.push('\n');
    }
    out
}

pub fn read_html_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_html".to_string(),
        description: "Extract readable text from an HTML file (tags stripped, <script>/<style>/\
                      comments removed, entities decoded, block tags become line breaks). \
                      Best-effort — layout, tables, and formatting are NOT preserved. Dep-free; \
                      same allowlist as fs_read_file."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to an .html/.htm file"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_html_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_html_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadHtmlArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let metadata = file
        .metadata()
        .map_err(|e| ToolError::ExecError(format!("stat failed: {e}")))?;
    if metadata.len() > HTML_MAX_FILE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "document is {} MiB, over the {} MiB read limit",
            metadata.len() / (1024 * 1024),
            HTML_MAX_FILE_BYTES / (1024 * 1024),
        )));
    }
    let display = a.path.clone();
    let rendered = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut buf = Vec::new();
        Read::take(file, HTML_MAX_FILE_BYTES).read_to_end(&mut buf)?;
        let html = String::from_utf8_lossy(&buf);
        Ok(render_html(&display, &html))
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

// --- fs.read_notebook (Jupyter .ipynb cell extraction) ----------------------

const NOTEBOOK_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const NOTEBOOK_DEFAULT_MAX_CELLS: usize = 100;
const NOTEBOOK_MAX_CELLS_CEILING: usize = 1000;
/// Source lines shown per cell before eliding.
const NOTEBOOK_MAX_SOURCE_LINES: usize = 60;
/// Output summaries shown per code cell.
const NOTEBOOK_MAX_OUTPUTS: usize = 12;

#[derive(Deserialize)]
struct ReadNotebookArgs {
    path: String,
    /// Cells shown (default 100, max 1000).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_cells: Option<usize>,
}

/// A notebook `source`/`text` field: a JSON string, or an array of line strings.
fn notebook_value_text(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(a)) => {
            a.iter().filter_map(|v| v.as_str()).collect::<String>()
        }
        _ => String::new(),
    }
}

/// One-line summary of a code-cell output (never the full payload).
fn notebook_output_summary(output: &serde_json::Value) -> Option<String> {
    let kind = output.get("output_type").and_then(|v| v.as_str())?;
    match kind {
        "stream" => {
            let name = output
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("stream");
            let text = notebook_value_text(output.get("text"));
            let lines = text.lines().count().max(usize::from(!text.is_empty()));
            Some(format!(
                "{name} ({lines} line{})",
                if lines == 1 { "" } else { "s" }
            ))
        }
        "execute_result" | "display_data" => {
            let data = output.get("data");
            if let Some(plain) = data.and_then(|d| d.get("text/plain")) {
                let text = notebook_value_text(Some(plain));
                let first = text.lines().next().unwrap_or("").trim();
                let shown: String = first.chars().take(120).collect();
                Some(format!("result: {shown}"))
            } else {
                let mut mimes: Vec<&str> = data
                    .and_then(|d| d.as_object())
                    .map(|o| o.keys().map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                mimes.sort();
                Some(format!("result: {}", mimes.join(", ")))
            }
        }
        "error" => {
            let ename = output
                .get("ename")
                .and_then(|v| v.as_str())
                .unwrap_or("Error");
            let evalue = output.get("evalue").and_then(|v| v.as_str()).unwrap_or("");
            let evalue: String = evalue.chars().take(120).collect();
            Some(format!("error: {ename}: {evalue}"))
        }
        _ => None,
    }
}

fn render_notebook(display: &str, json_text: &str, max_cells: usize) -> String {
    let value: serde_json::Value = match serde_json::from_str(json_text) {
        Ok(v) => v,
        Err(e) => return format!("notebook> {display}: could not parse notebook JSON — {e}\n"),
    };
    let Some(cells) = value.get("cells").and_then(|c| c.as_array()) else {
        return format!(
            "notebook> {display}: not a Jupyter notebook (no top-level `cells` array; \
             nbformat 4 expected)\n"
        );
    };

    let language = value
        .get("metadata")
        .and_then(|m| m.get("kernelspec"))
        .and_then(|k| k.get("language"))
        .and_then(|l| l.as_str());
    let code_count = cells
        .iter()
        .filter(|c| c.get("cell_type").and_then(|t| t.as_str()) == Some("code"))
        .count();
    let md_count = cells
        .iter()
        .filter(|c| c.get("cell_type").and_then(|t| t.as_str()) == Some("markdown"))
        .count();

    let lang_note = match language {
        Some(l) => format!(" [language: {l}]"),
        None => String::new(),
    };
    let mut out = format!(
        "notebook> {display}: {} cell(s) ({code_count} code, {md_count} markdown){lang_note}\n",
        cells.len()
    );

    let shown = cells.len().min(max_cells);
    for (idx, cell) in cells.iter().take(shown).enumerate() {
        let cell_type = cell
            .get("cell_type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");
        let exec = cell
            .get("execution_count")
            .and_then(|v| v.as_u64())
            .map(|n| format!(" (exec {n})"))
            .unwrap_or_default();
        out.push_str(&format!("[{}] {cell_type}{exec}:\n", idx + 1));

        let source = notebook_value_text(cell.get("source"));
        let src_lines: Vec<&str> = source.lines().collect();
        for line in src_lines.iter().take(NOTEBOOK_MAX_SOURCE_LINES) {
            out.push_str(line);
            out.push('\n');
        }
        if src_lines.len() > NOTEBOOK_MAX_SOURCE_LINES {
            out.push_str(&format!(
                "    …[{} more source line(s)]\n",
                src_lines.len() - NOTEBOOK_MAX_SOURCE_LINES
            ));
        }

        if let Some(outputs) = cell.get("outputs").and_then(|o| o.as_array()) {
            let summaries: Vec<String> =
                outputs.iter().filter_map(notebook_output_summary).collect();
            for summary in summaries.iter().take(NOTEBOOK_MAX_OUTPUTS) {
                out.push_str(&format!("  → {summary}\n"));
            }
            if summaries.len() > NOTEBOOK_MAX_OUTPUTS {
                out.push_str(&format!(
                    "  → …[{} more output(s)]\n",
                    summaries.len() - NOTEBOOK_MAX_OUTPUTS
                ));
            }
        }
    }
    if cells.len() > shown {
        out.push_str(&format!(
            "[showing {shown} of {} cells; raise max_cells for more]\n",
            cells.len()
        ));
    }
    out
}

pub fn read_notebook_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_notebook".to_string(),
        description: "Extract the cells of a Jupyter notebook (.ipynb): each cell's type \
                      (code/markdown/raw), its source, and a one-line summary of each code cell's \
                      outputs (stream line counts, result mime/text, errors) — never the full \
                      output payload. Reads the notebook as structure instead of dumping raw JSON. \
                      Dep-free (serde_json); same allowlist as fs_read_file; a non-notebook or \
                      malformed file is disclosed honestly."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to a .ipynb notebook"},
                "max_cells": {"type": "integer", "description": "Cells shown (optional, default 100, max 1000)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_notebook_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_notebook_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadNotebookArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let metadata = file
        .metadata()
        .map_err(|e| ToolError::ExecError(format!("stat failed: {e}")))?;
    if metadata.len() > NOTEBOOK_MAX_FILE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "notebook is {} MiB, over the {} MiB read limit",
            metadata.len() / (1024 * 1024),
            NOTEBOOK_MAX_FILE_BYTES / (1024 * 1024),
        )));
    }
    let display = a.path.clone();
    let max_cells = a
        .max_cells
        .unwrap_or(NOTEBOOK_DEFAULT_MAX_CELLS)
        .clamp(1, NOTEBOOK_MAX_CELLS_CEILING);
    let rendered = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut buf = Vec::new();
        Read::take(file, NOTEBOOK_MAX_FILE_BYTES).read_to_end(&mut buf)?;
        let text = String::from_utf8_lossy(&buf);
        Ok(render_notebook(&display, &text, max_cells))
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

// --- fs.read_pptx (PowerPoint slide-text extraction) ------------------------

const PPTX_MAX_TOTAL_XML_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Deserialize)]
struct ReadPptxArgs {
    path: String,
    /// Paragraphs shown across all slides (default 500, max 5000).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_paragraphs: Option<usize>,
}

/// pptx text runs are `<a:t>`, paragraphs `<a:p>`, line breaks `<a:br/>`.
fn parse_pptx_paragraphs(xml: &str) -> Vec<String> {
    let (paras, _capped) = parse_ooxml_paragraphs(xml, b"a:t", b"a:p", None, &[b"a:br".as_slice()]);
    paras
}

/// `ppt/slides/slideN.xml` → N. Excludes layouts, masters, notes, and rels.
fn pptx_slide_number(name: &str) -> Option<u32> {
    name.strip_prefix("ppt/slides/slide")?
        .strip_suffix(".xml")?
        .parse::<u32>()
        .ok()
}

fn extract_pptx(path: &Path, display: &str, max_paragraphs: usize) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| std::io::Error::other(format!("not a valid .pptx (zip) file: {e}")))?;
    // Collect slide entry names (owned) so the archive can be re-borrowed mutably.
    let mut slides: Vec<(u32, String)> = archive
        .file_names()
        .filter_map(|n| pptx_slide_number(n).map(|num| (num, n.to_string())))
        .collect();
    if slides.is_empty() {
        return Err(std::io::Error::other(
            "not a PowerPoint .pptx document (no ppt/slides/slideN.xml inside)",
        ));
    }
    // Numeric order — lexical would put slide10 before slide2.
    slides.sort_by_key(|(num, _)| *num);

    let mut body = String::new();
    let mut total_paras = 0usize;
    let mut shown = 0usize;
    let mut budget = 0u64;
    let mut byte_capped = false;
    for (num, name) in &slides {
        if budget >= PPTX_MAX_TOTAL_XML_BYTES {
            byte_capped = true;
            break;
        }
        let Ok(entry) = archive.by_name(name) else {
            continue;
        };
        let mut buf = Vec::new();
        if Read::take(entry, DOCX_MAX_XML_BYTES)
            .read_to_end(&mut buf)
            .is_err()
        {
            continue;
        }
        budget += buf.len() as u64;
        let xml = String::from_utf8_lossy(&buf);
        let paras = parse_pptx_paragraphs(&xml);
        let with_text: Vec<&String> = paras.iter().filter(|p| !p.trim().is_empty()).collect();
        total_paras += with_text.len();
        if with_text.is_empty() || shown >= max_paragraphs {
            continue;
        }
        body.push_str(&format!("--- slide {num} ---\n"));
        for para in with_text {
            if shown >= max_paragraphs {
                break;
            }
            body.push_str(para);
            body.push('\n');
            shown += 1;
        }
    }

    let note = if byte_capped {
        format!(
            ", stopped at {} MiB of slide XML",
            PPTX_MAX_TOTAL_XML_BYTES / (1024 * 1024)
        )
    } else {
        String::new()
    };
    let mut out = format!(
        "pptx> {display}: {} slide(s), {total_paras} paragraph(s) with text \
         [text only — no speaker notes, formatting, or images]{note}\n",
        slides.len()
    );
    if total_paras == 0 {
        out.push_str(
            "(no extractable text — the slides may be image-only or use an unsupported structure)\n",
        );
        return Ok(out);
    }
    out.push_str(&body);
    if shown < total_paras {
        out.push_str(&format!(
            "[showing {shown} of {total_paras} paragraphs; raise max_paragraphs for more]\n"
        ));
    }
    Ok(out)
}

pub fn read_pptx_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_read_pptx".to_string(),
        description: "Extract the slide text of a PowerPoint .pptx (text only — no speaker notes, \
                      formatting, or images), grouped per slide and bounded by paragraph. \
                      Pure-Rust zip + XML; same allowlist as fs_read_file; a non-.pptx or corrupt \
                      file is disclosed honestly, never emitted as binary garbage."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to a .pptx file"},
                "max_paragraphs": {"type": "integer", "description": "Paragraphs shown across all slides (optional, default 500, max 5000)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_pptx_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_pptx_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadPptxArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let metadata = file
        .metadata()
        .map_err(|e| ToolError::ExecError(format!("stat failed: {e}")))?;
    if metadata.len() > DOCX_MAX_FILE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "document is {} MiB, over the {} MiB read limit",
            metadata.len() / (1024 * 1024),
            DOCX_MAX_FILE_BYTES / (1024 * 1024),
        )));
    }
    let path = authorization.path().to_path_buf();
    let display = a.path.clone();
    let max = a
        .max_paragraphs
        .unwrap_or(DOCX_DEFAULT_MAX_PARAGRAPHS)
        .clamp(1, DOCX_MAX_PARAGRAPHS_CEILING);
    let rendered = tokio::task::spawn_blocking(move || extract_pptx(&path, &display, max))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("pptx parse failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

// --- fs.scan_secrets (apparent-credential guard for diffs/files) ------------
//
// Detection mirrors the kitsu binary's credential filter
// (subagent::thread::sanitize_and_redact_text). Kept as a self-contained copy
// because kitsu-tools cannot depend on the binary crate; a future refactor
// could hoist these into a shared module the binary also adopts. A line is
// "flagged" iff redacting it changes it, and only the REDACTED line is ever
// emitted — the tool never echoes the raw secret it found.

const SECRET_DEFAULT_MAX_FINDINGS: usize = 200;
const SECRET_MAX_FINDINGS_CEILING: usize = 2000;
const SECRET_PREFIXES: &[&str] = &[
    "github_pat_",
    "hf_",
    "sk-",
    "ghp_",
    "gho_",
    "glpat-",
    "npm_",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "AIza",
];
const SECRET_ASSIGN_KEYWORDS: &[&str] = &[
    "api_key",
    "apikey",
    "access_token",
    "auth_token",
    "authorization",
    "password",
    "secret",
    "token",
];

#[derive(Deserialize)]
struct ScanSecretsArgs {
    path: String,
    /// Flagged lines shown (default 200, max 2000).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_findings: Option<usize>,
}

fn redact_known_prefixes(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        let tail = &value[index..];
        let Some(prefix) = SECRET_PREFIXES
            .iter()
            .find(|prefix| tail.starts_with(**prefix))
        else {
            let character = tail.chars().next().expect("non-empty tail");
            output.push(character);
            index += character.len_utf8();
            continue;
        };
        let mut end = index + prefix.len();
        while end < value.len() {
            let character = value[end..].chars().next().expect("non-empty tail");
            if !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')) {
                break;
            }
            end += character.len_utf8();
        }
        // Short words such as "hf_model" are ordinary identifiers; a credential
        // prefix is redacted only when followed by a plausible secret body.
        if end.saturating_sub(index + prefix.len()) >= 8 {
            output.push_str("<redacted>");
            index = end;
        } else {
            let character = tail.chars().next().expect("non-empty tail");
            output.push(character);
            index += character.len_utf8();
        }
    }
    output
}

fn redact_url_userinfo(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(relative) = value[search..].find("://") {
        let authority_start = search + relative + 3;
        let authority_end = value[authority_start..]
            .char_indices()
            .find_map(|(offset, character)| {
                matches!(character, '/' | '?' | '#' | ' ' | '\n' | '\t')
                    .then_some(authority_start + offset)
            })
            .unwrap_or(value.len());
        if let Some(relative_at) = value[authority_start..authority_end].rfind('@') {
            let at = authority_start + relative_at;
            output.push_str(&value[copied..authority_start]);
            output.push_str("<redacted>");
            output.push_str(&value[at..authority_end]);
            copied = authority_end;
        }
        search = authority_end;
        if search == value.len() {
            break;
        }
    }
    output.push_str(&value[copied..]);
    output
}

fn redact_assignment(value: &str, keyword: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(relative) = lowercase[search..].find(keyword) {
        let start = search + relative;
        let before_ok = start == 0 || !lowercase.as_bytes()[start - 1].is_ascii_alphanumeric();
        let after_keyword = start + keyword.len();
        let after_ok = after_keyword == lowercase.len()
            || !lowercase.as_bytes()[after_keyword].is_ascii_alphanumeric();
        if !before_ok || !after_ok {
            search = after_keyword;
            continue;
        }
        let mut delimiter = after_keyword;
        while delimiter < value.len() && value.as_bytes()[delimiter].is_ascii_whitespace() {
            delimiter += 1;
        }
        if delimiter == value.len() || !matches!(value.as_bytes()[delimiter], b'=' | b':') {
            search = after_keyword;
            continue;
        }
        let mut secret_start = delimiter + 1;
        while secret_start < value.len()
            && (value.as_bytes()[secret_start].is_ascii_whitespace()
                || matches!(value.as_bytes()[secret_start], b'\'' | b'"'))
        {
            secret_start += 1;
        }
        let mut end = secret_start;
        while end < value.len()
            && !value.as_bytes()[end].is_ascii_whitespace()
            && !matches!(
                value.as_bytes()[end],
                b',' | b';' | b'&' | b'\'' | b'"' | b'}' | b']'
            )
        {
            end += 1;
        }
        output.push_str(&value[copied..secret_start]);
        output.push_str("<redacted>");
        copied = end;
        search = end;
    }
    output.push_str(&value[copied..]);
    output
}

fn redact_bearer(value: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(relative) = lowercase[search..].find("bearer") {
        let start = search + relative;
        let before_ok = start == 0 || !lowercase.as_bytes()[start - 1].is_ascii_alphanumeric();
        let mut token_start = start + "bearer".len();
        if !before_ok
            || token_start == value.len()
            || !value.as_bytes()[token_start].is_ascii_whitespace()
        {
            search = token_start;
            continue;
        }
        while token_start < value.len() && value.as_bytes()[token_start].is_ascii_whitespace() {
            token_start += 1;
        }
        let mut end = token_start;
        while end < value.len()
            && !value.as_bytes()[end].is_ascii_whitespace()
            && !matches!(value.as_bytes()[end], b',' | b';' | b'\'' | b'"')
        {
            end += 1;
        }
        output.push_str(&value[copied..token_start]);
        output.push_str("<redacted>");
        copied = end;
        search = end;
    }
    output.push_str(&value[copied..]);
    output
}

/// The full redaction pipeline (no terminal sanitize — the scanner works on
/// raw file bytes already). A line is a "finding" iff this changes it.
///
/// Bearer runs FIRST: on `Authorization: Bearer <token>` the `authorization:`
/// assignment rule would otherwise redact the literal word "Bearer" as its
/// value and leave the real token exposed — unacceptable for a tool whose
/// output must never echo a secret. Redacting the bearer token up front closes
/// that leak; assignments then run last over the already-scrubbed line.
fn redact_secrets_line(value: &str) -> String {
    let mut output = redact_url_userinfo(&redact_known_prefixes(&redact_bearer(value)));
    for keyword in SECRET_ASSIGN_KEYWORDS {
        output = redact_assignment(&output, keyword);
    }
    output
}

/// Which detector(s) fired on this line — for the human-facing label.
fn detect_secret_kinds(value: &str) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    if redact_known_prefixes(value) != value {
        kinds.push("token-prefix");
    }
    if redact_url_userinfo(value) != value {
        kinds.push("url-credentials");
    }
    if SECRET_ASSIGN_KEYWORDS
        .iter()
        .any(|kw| redact_assignment(value, kw) != value)
    {
        kinds.push("secret-assignment");
    }
    if redact_bearer(value) != value {
        kinds.push("bearer-token");
    }
    kinds
}

pub fn scan_secrets_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "scan_secrets".to_string(),
        description: "Flag lines that appear to contain a credential — known API-key/token \
                      prefixes (hf_/sk-/ghp_/AKIA/AIza/…), URL user:pass@host, secret \
                      assignments (api_key=/password:/token=/…), and Bearer tokens. Point it at a \
                      file — e.g. a `git diff` written to disk before commit — and it reports each \
                      flagged line number with the matched kind and the line REDACTED (it never \
                      echoes the raw secret). Same allowlist as fs_read_file. Advisory: it can miss \
                      novel formats and may flag placeholders, so it never blocks."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to scan (e.g. a saved git diff)"},
                "max_findings": {"type": "integer", "description": "Flagged lines shown (optional, default 200, max 2000)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { scan_secrets_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn scan_secrets_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ScanSecretsArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let display = a.path.clone();
    let cap = a
        .max_findings
        .unwrap_or(SECRET_DEFAULT_MAX_FINDINGS)
        .min(SECRET_MAX_FINDINGS_CEILING);
    let rendered = tokio::task::spawn_blocking(move || scan_secrets_blocking(file, &display, cap))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("scan failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

fn scan_secrets_blocking(
    file: std::fs::File,
    display: &str,
    cap: usize,
) -> std::io::Result<String> {
    use std::io::BufRead;

    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("scan target is not a regular file"));
    }
    let mut reader = std::io::BufReader::new(Read::take(file, SCAN_MAX_SCAN_BYTES + 1));
    let mut line = String::new();
    let mut line_no = 0usize;
    let mut total = 0usize;
    let mut bytes_read = 0u64;
    let mut scan_truncated = false;
    let mut shown: Vec<(usize, Vec<&'static str>, String)> = Vec::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        bytes_read += n as u64;
        if bytes_read > SCAN_MAX_SCAN_BYTES {
            scan_truncated = true;
            break;
        }
        line_no += 1;
        let redacted = redact_secrets_line(&line);
        if redacted != line {
            total += 1;
            if shown.len() < cap {
                let kinds = detect_secret_kinds(&line);
                shown.push((line_no, kinds, truncate_line(&redacted)));
            }
        }
    }

    let scanned_note = if scan_truncated {
        format!(
            " — stopped after scanning {} MiB (refine or split the file)",
            SCAN_MAX_SCAN_BYTES / (1024 * 1024)
        )
    } else {
        String::new()
    };
    let mut out = format!(
        "scan_secrets> {display}: {total} line(s) with apparent credentials in {line_no} lines \
         scanned{scanned_note}\n"
    );
    if total == 0 {
        out.push_str(
            "no apparent credentials found (checked known token prefixes, url user:pass, secret \
             assignments, and bearer tokens)\n",
        );
        return Ok(out);
    }
    for (no, kinds, redacted) in &shown {
        let label = if kinds.is_empty() {
            String::new()
        } else {
            format!("[{}] ", kinds.join(", "))
        };
        out.push_str(&format!("{no}: {label}{redacted}\n"));
    }
    if total > shown.len() {
        out.push_str(&format!(
            "[showing {} of {} flagged lines; raise max_findings for more]\n",
            shown.len(),
            total
        ));
    }
    Ok(out)
}

// --- fs.audit_summary (cargo-audit advisory summary) ------------------------

const AUDIT_DEFAULT_MAX_ITEMS: usize = 100;
const AUDIT_MAX_ITEMS_CEILING: usize = 1000;

#[derive(Deserialize)]
struct AuditSummaryArgs {
    path: String,
    /// Items shown per section (default 100, max 1000).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_items: Option<usize>,
}

fn json_field_str<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

/// `"name version"` for a cargo-audit package object.
fn audit_package_label(pkg: &serde_json::Value) -> String {
    let name = json_field_str(pkg, "name").unwrap_or("<unknown>");
    match json_field_str(pkg, "version") {
        Some(ver) => format!("{name} {ver}"),
        None => name.to_string(),
    }
}

/// A cargo-audit array-of-strings field (`versions.patched`, `advisory.aliases`).
fn json_str_array(v: Option<&serde_json::Value>) -> Vec<&str> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default()
}

fn render_audit_summary(display: &str, json_text: &str, max_items: usize) -> String {
    let value: serde_json::Value = match serde_json::from_str(json_text) {
        Ok(v) => v,
        Err(e) => return format!("audit> {display}: could not parse cargo-audit JSON — {e}\n"),
    };

    let vulns = value.get("vulnerabilities");
    let vuln_list: Vec<&serde_json::Value> = vulns
        .and_then(|v| v.get("list"))
        .and_then(|l| l.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let vuln_count = vulns
        .and_then(|v| v.get("count"))
        .and_then(|c| c.as_u64())
        .map(|c| c as usize)
        .unwrap_or(vuln_list.len());

    // warnings: { "<kind>": [ { package, advisory, kind }, … ], … }
    let warnings_obj = value.get("warnings").and_then(|w| w.as_object());
    let warning_total: usize = warnings_obj
        .map(|o| {
            o.values()
                .filter_map(|v| v.as_array())
                .map(|a| a.len())
                .sum()
        })
        .unwrap_or(0);

    let vuln_word = if vuln_count == 1 {
        "vulnerability"
    } else {
        "vulnerabilities"
    };
    let mut out =
        format!("audit> {display}: {vuln_count} {vuln_word}, {warning_total} warning(s)\n");
    if vuln_count == 0 && warning_total == 0 {
        out.push_str("no known vulnerabilities or advisory warnings\n");
        return out;
    }

    // Vulnerabilities first (the real risk), each with its fix.
    if !vuln_list.is_empty() {
        out.push_str("vulnerabilities:\n");
        let mut shown = 0usize;
        for v in &vuln_list {
            if shown >= max_items {
                break;
            }
            let adv = v.get("advisory");
            let id = adv
                .and_then(|a| json_field_str(a, "id"))
                .unwrap_or("<no id>");
            let title = adv.and_then(|a| json_field_str(a, "title")).unwrap_or("");
            let pkg = v
                .get("package")
                .map(audit_package_label)
                .unwrap_or_else(|| "<unknown>".to_string());
            out.push_str(&format!("  [{id}] {pkg} — {title}\n"));
            let patched = json_str_array(v.get("versions").and_then(|ver| ver.get("patched")));
            if patched.is_empty() {
                out.push_str("    fix: no patched release available\n");
            } else {
                out.push_str(&format!("    fix: upgrade to {}\n", patched.join(" or ")));
            }
            let aliases = json_str_array(adv.and_then(|a| a.get("aliases")));
            if !aliases.is_empty() {
                out.push_str(&format!("    aka: {}\n", aliases.join(", ")));
            }
            shown += 1;
        }
        if vuln_list.len() > shown {
            out.push_str(&format!(
                "  [showing {shown} of {} vulnerabilities]\n",
                vuln_list.len()
            ));
        }
    }

    // Advisory warnings (unmaintained / yanked / unsound …) grouped by kind.
    if let Some(warnings) = warnings_obj {
        if warning_total > 0 {
            out.push_str("warnings:\n");
            let mut kinds: Vec<&String> = warnings.keys().collect();
            kinds.sort();
            let mut wshown = 0usize;
            'kinds: for kind in kinds {
                let Some(items) = warnings.get(kind).and_then(|x| x.as_array()) else {
                    continue;
                };
                for w in items {
                    if wshown >= max_items {
                        break 'kinds;
                    }
                    let pkg = w
                        .get("package")
                        .map(audit_package_label)
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let title = w
                        .get("advisory")
                        .and_then(|a| json_field_str(a, "title"))
                        .unwrap_or("");
                    if title.is_empty() {
                        out.push_str(&format!("  [{kind}] {pkg}\n"));
                    } else {
                        out.push_str(&format!("  [{kind}] {pkg} — {title}\n"));
                    }
                    wshown += 1;
                }
            }
            if warning_total > wshown {
                out.push_str(&format!(
                    "  [showing {wshown} of {warning_total} warnings]\n"
                ));
            }
        }
    }
    out
}

pub fn audit_summary_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "audit_summary".to_string(),
        description: "Summarize a `cargo audit --json` report into a ranked risk view: each \
                      vulnerability with its advisory id, affected package + version, title, CVE \
                      aliases, and the patched version to upgrade to; then advisory warnings \
                      (unmaintained/yanked/unsound) grouped by kind. Give it a JSON file you \
                      produced with `cargo audit --json`. Read-only; same allowlist as \
                      fs_read_file; malformed JSON is disclosed, never guessed."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to a `cargo audit --json` output file"},
                "max_items": {"type": "integer", "description": "Items shown per section (optional, default 100, max 1000)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { audit_summary_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn audit_summary_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: AuditSummaryArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let max = a
        .max_items
        .unwrap_or(AUDIT_DEFAULT_MAX_ITEMS)
        .clamp(1, AUDIT_MAX_ITEMS_CEILING);
    let content = read_config_text(policy, &a.path).await?;
    let rendered = render_audit_summary(&a.path, &content, max);
    Ok(policy.cap_output(rendered))
}

// --- fs.read_manifest (dependency / build manifest summary) -----------------

/// Dependency names shown per section by default / at most before eliding.
const MANIFEST_DEFAULT_MAX_DEPS: usize = 50;
const MANIFEST_MAX_DEPS_CEILING: usize = 500;

#[derive(Deserialize)]
struct ReadManifestArgs {
    path: String,
    /// Dependency names listed per section (default 50, max 500).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_deps: Option<usize>,
}

enum ManifestKind {
    Cargo,
    PackageJson,
    Pyproject,
    Unknown,
}

fn detect_manifest_kind(path: &str, content: &str) -> ManifestKind {
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match name.as_str() {
        "cargo.toml" => return ManifestKind::Cargo,
        "package.json" => return ManifestKind::PackageJson,
        "pyproject.toml" => return ManifestKind::Pyproject,
        _ => {}
    }
    // Non-standard filename: fall back to extension + a content-shape hint.
    if name.ends_with(".json") {
        return ManifestKind::PackageJson;
    }
    if name.ends_with(".toml") {
        // pyproject uses [project] / [tool.poetry]; Cargo uses [package].
        if content.contains("[project]") || content.contains("[tool.poetry") {
            return ManifestKind::Pyproject;
        }
        return ManifestKind::Cargo;
    }
    ManifestKind::Unknown
}

/// One bounded, sorted, de-duplicated dependency section line.
fn format_dep_section(label: &str, mut names: Vec<String>, max: usize) -> String {
    names.sort();
    names.dedup();
    let total = names.len();
    if total == 0 {
        return format!("  {label}: 0\n");
    }
    let shown = total.min(max);
    let listed = names[..shown].join(", ");
    let more = if total > shown {
        format!(", +{} more", total - shown)
    } else {
        String::new()
    };
    format!("  {label} ({total}): {listed}{more}\n")
}

fn toml_table_keys(v: &toml::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

/// `section[key]` as a string slice, borrowing from `section`.
fn toml_str<'a>(section: Option<&'a toml::Value>, key: &str) -> Option<&'a str> {
    section.and_then(|s| s.get(key)).and_then(|x| x.as_str())
}

/// Extract the distribution name from a PEP 508 requirement string:
/// `requests>=2.0` → `requests`, `ruff (==0.1); python_version>'3.7'` → `ruff`.
fn requirement_name(req: &str) -> String {
    let req = req.trim();
    let end = req
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
        .unwrap_or(req.len());
    req[..end].to_string()
}

fn render_cargo_manifest(display: &str, content: &str, max: usize) -> Result<String, String> {
    let v: toml::Value = toml::from_str(content).map_err(|e| e.to_string())?;
    let pkg = v.get("package");
    let pkg_str = |k: &str| {
        pkg.and_then(|p| p.get(k))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
    };
    let name = pkg_str("name");
    let version = pkg_str("version");
    let edition = pkg_str("edition");
    let is_workspace = v.get("workspace").is_some();

    let mut out = String::from("manifest> ");
    let header = match (&name, &version) {
        (Some(n), Some(ver)) => format!("{display}: Cargo (Rust) — {n} v{ver}"),
        (Some(n), None) => format!("{display}: Cargo (Rust) — {n}"),
        _ if is_workspace => format!("{display}: Cargo (Rust) — virtual workspace manifest"),
        _ => format!("{display}: Cargo (Rust)"),
    };
    out.push_str(&header);
    if let Some(e) = edition {
        out.push_str(&format!(" (edition {e})"));
    }
    out.push('\n');
    out.push_str(&format_dep_section(
        "dependencies",
        toml_table_keys(&v, "dependencies"),
        max,
    ));
    out.push_str(&format_dep_section(
        "dev-dependencies",
        toml_table_keys(&v, "dev-dependencies"),
        max,
    ));
    let build = toml_table_keys(&v, "build-dependencies");
    if !build.is_empty() {
        out.push_str(&format_dep_section("build-dependencies", build, max));
    }
    let features = v
        .get("features")
        .and_then(|x| x.as_table())
        .map(|t| t.len())
        .unwrap_or(0);
    if features > 0 {
        out.push_str(&format!("  features: {features}\n"));
    }
    Ok(out)
}

fn render_package_json(display: &str, content: &str, max: usize) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(content).map_err(|e| e.to_string())?;
    let obj_keys = |k: &str| -> Vec<String> {
        v.get(k)
            .and_then(|x| x.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    let name = v.get("name").and_then(|x| x.as_str());
    let version = v.get("version").and_then(|x| x.as_str());

    let mut out = String::from("manifest> ");
    let header = match (name, version) {
        (Some(n), Some(ver)) => format!("{display}: package.json (Node) — {n} v{ver}"),
        (Some(n), None) => format!("{display}: package.json (Node) — {n}"),
        _ => format!("{display}: package.json (Node)"),
    };
    out.push_str(&header);
    out.push('\n');
    out.push_str(&format_dep_section(
        "dependencies",
        obj_keys("dependencies"),
        max,
    ));
    out.push_str(&format_dep_section(
        "devDependencies",
        obj_keys("devDependencies"),
        max,
    ));
    let peer = obj_keys("peerDependencies");
    if !peer.is_empty() {
        out.push_str(&format_dep_section("peerDependencies", peer, max));
    }
    let scripts = obj_keys("scripts");
    if !scripts.is_empty() {
        out.push_str(&format_dep_section("scripts", scripts, max));
    }
    Ok(out)
}

fn render_pyproject(display: &str, content: &str, max: usize) -> Result<String, String> {
    let v: toml::Value = toml::from_str(content).map_err(|e| e.to_string())?;
    let project = v.get("project");
    let poetry = v.get("tool").and_then(|t| t.get("poetry"));
    // PEP 621 [project] is canonical; fall back to [tool.poetry] for name/version.
    let name = toml_str(project, "name").or_else(|| toml_str(poetry, "name"));
    let version = toml_str(project, "version").or_else(|| toml_str(poetry, "version"));

    // PEP 621: [project].dependencies is an array of requirement strings.
    let mut deps: Vec<String> = project
        .and_then(|p| p.get("dependencies"))
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|d| d.as_str().map(requirement_name))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    // Poetry: [tool.poetry.dependencies] is a table (skip the `python` pin).
    if deps.is_empty() {
        if let Some(table) = poetry
            .and_then(|p| p.get("dependencies"))
            .and_then(|x| x.as_table())
        {
            deps = table
                .keys()
                .filter(|k| k.as_str() != "python")
                .cloned()
                .collect();
        }
    }
    let backend = v
        .get("build-system")
        .and_then(|b| b.get("build-backend"))
        .and_then(|x| x.as_str());

    let mut out = String::from("manifest> ");
    let header = match (name, version) {
        (Some(n), Some(ver)) => format!("{display}: pyproject.toml (Python) — {n} v{ver}"),
        (Some(n), None) => format!("{display}: pyproject.toml (Python) — {n}"),
        _ => format!("{display}: pyproject.toml (Python)"),
    };
    out.push_str(&header);
    out.push('\n');
    out.push_str(&format_dep_section("dependencies", deps, max));
    if let Some(b) = backend {
        out.push_str(&format!("  build-backend: {b}\n"));
    }
    Ok(out)
}

fn render_manifest(display: &str, path: &str, content: &str, max: usize) -> String {
    let result = match detect_manifest_kind(path, content) {
        ManifestKind::Cargo => render_cargo_manifest(display, content, max),
        ManifestKind::PackageJson => render_package_json(display, content, max),
        ManifestKind::Pyproject => render_pyproject(display, content, max),
        ManifestKind::Unknown => Err(
            "unrecognized manifest (expected Cargo.toml, package.json, or pyproject.toml)"
                .to_string(),
        ),
    };
    match result {
        Ok(s) => s,
        Err(e) => format!("manifest> {display}: could not parse — {e}\n"),
    }
}

pub fn read_manifest_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "read_manifest".to_string(),
        description: "Summarize a project manifest (Cargo.toml, package.json, or pyproject.toml): \
                      package name/version, edition/build-backend, and dependency names by section \
                      (bounded + de-duplicated), so the model reads a project's real dependencies \
                      and build shape instead of guessing. Read-only; same allowlist as \
                      fs_read_file; a malformed manifest is disclosed honestly, not guessed."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to a Cargo.toml, package.json, or pyproject.toml"},
                "max_deps": {"type": "integer", "description": "Dependency names listed per section (optional, default 50, max 500)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { read_manifest_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn read_manifest_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ReadManifestArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let max = a
        .max_deps
        .unwrap_or(MANIFEST_DEFAULT_MAX_DEPS)
        .clamp(1, MANIFEST_MAX_DEPS_CEILING);
    let content = read_config_text(policy, &a.path).await?;
    let rendered = render_manifest(&a.path, &a.path, &content, max);
    Ok(policy.cap_output(rendered))
}

// --- fs.find_symbol (heuristic symbol navigation) ---------------------------

/// Never scan more than this many files in one call (bounds a huge tree).
const SYMBOL_MAX_FILES: usize = 5000;
/// Stop after scanning this many total bytes across all files (disclosed).
const SYMBOL_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// Read at most this many bytes from any single file (bounds memory).
const SYMBOL_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Definition sites shown — there are rarely many; references are the long list.
const SYMBOL_MAX_DEFS_SHOWN: usize = 50;
/// Reference lines shown by default and at most.
const SYMBOL_DEFAULT_MAX_REFS: usize = 100;
const SYMBOL_MAX_REFS_CEILING: usize = 1000;
/// Reject absurdly long "symbols" — they are identifiers.
const SYMBOL_MAX_LEN: usize = 128;
/// Per-line display cap so one very long line cannot blow the output budget.
const SYMBOL_LINE_DISPLAY_CAP: usize = 400;

/// Directory names never descended into during a symbol walk.
const SYMBOL_SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
];

/// A keyword that, appearing immediately before a whole-word occurrence of the
/// symbol, marks that line as a *definition* site rather than a use. Kept
/// language-agnostic: Rust/Go/JS/TS/Python/Java/Kotlin/C-family forms.
const SYMBOL_DEF_KEYWORDS: &[&str] = &[
    "fn",
    "func",
    "def",
    "class",
    "struct",
    "enum",
    "trait",
    "type",
    "interface",
    "impl",
    "mod",
    "module",
    "namespace",
    "package",
    "union",
    "const",
    "static",
    "let",
    "var",
    "val",
    "function",
];

#[derive(Deserialize)]
struct FindSymbolArgs {
    /// The identifier to locate.
    symbol: String,
    /// File or directory to search (must be allowed). A directory is walked
    /// recursively; a single file is scanned alone.
    path: String,
    /// Reference lines shown (default 100, max 1000). Definitions always shown.
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_results: Option<usize>,
}

#[derive(Default)]
struct SymbolScanBudget {
    files: usize,
    bytes: u64,
}

#[derive(Default)]
struct SymbolHits {
    defs: Vec<(String, usize, String)>,
    refs: Vec<(String, usize, String)>,
    total_defs: usize,
    total_refs: usize,
    files_scanned: usize,
    skipped_unreadable: usize,
    files_capped: bool,
}

fn is_symbol_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// True when `c` is a word boundary for identifier matching (a non-identifier
/// char, or the start/end of the line).
fn is_boundary(c: Option<char>) -> bool {
    match c {
        Some(ch) => !is_symbol_char(ch),
        None => true,
    }
}

/// Does `line` contain `symbol` as a whole word (not a fragment of a larger
/// identifier)? `symbol` is assumed non-empty.
fn line_mentions_symbol(line: &str, symbol: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = line[from..].find(symbol) {
        let start = from + rel;
        let end = start + symbol.len();
        if is_boundary(line[..start].chars().next_back()) && is_boundary(line[end..].chars().next())
        {
            return true;
        }
        from = end;
    }
    false
}

/// The maximal identifier tokens of a line, in order.
fn identifier_tokens(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    for (idx, c) in line.char_indices() {
        if is_symbol_char(c) {
            start.get_or_insert(idx);
        } else if let Some(s) = start.take() {
            tokens.push(&line[s..idx]);
        }
    }
    if let Some(s) = start {
        tokens.push(&line[s..]);
    }
    tokens
}

/// Heuristic: is this line a *definition* of `symbol`? True when a definition
/// keyword immediately precedes a whole-word occurrence (incl. `let mut <sym>`).
pub(crate) fn line_defines_symbol(line: &str, symbol: &str) -> bool {
    let tokens = identifier_tokens(line);
    for (i, tok) in tokens.iter().enumerate() {
        if *tok != symbol || i == 0 {
            continue;
        }
        let prev = tokens[i - 1];
        if SYMBOL_DEF_KEYWORDS.contains(&prev) {
            return true;
        }
        // `let mut <sym>` / `var mut <sym>` — skip the mutability qualifier.
        if prev == "mut" && i >= 2 && matches!(tokens[i - 2], "let" | "var") {
            return true;
        }
    }
    false
}

fn truncate_symbol_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() > SYMBOL_LINE_DISPLAY_CAP {
        let cut: String = trimmed.chars().take(SYMBOL_LINE_DISPLAY_CAP).collect();
        format!("{cut}…")
    } else {
        trimmed.to_string()
    }
}

// --- fs.outline_file (definitions/structure of a source file) ---------------

/// Structural definition keywords — `SYMBOL_DEF_KEYWORDS` minus local bindings
/// (`let`/`var`/`val`), which are far too noisy for a file outline.
const OUTLINE_KEYWORDS: &[&str] = &[
    "fn",
    "func",
    "def",
    "class",
    "struct",
    "enum",
    "trait",
    "type",
    "interface",
    "impl",
    "mod",
    "module",
    "namespace",
    "package",
    "union",
    "const",
    "static",
    "function",
];

const OUTLINE_DEFAULT_MAX: usize = 500;
const OUTLINE_MAX_CEILING: usize = 5000;

#[derive(Deserialize)]
struct OutlineArgs {
    path: String,
    /// Definitions shown (default 500, max 5000).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_items: Option<usize>,
}

/// Visibility/qualifier tokens that may precede a definition keyword (incl. the
/// `pub(crate)` / `pub(super)` tokens). Requiring the keyword to sit at the
/// statement START — after only these — keeps a keyword INSIDE a string or a
/// call (`describe("fn fake", …)`, `let s = "fn x"`) from being flagged.
const OUTLINE_MODIFIERS: &[&str] = &[
    "pub",
    "async",
    "unsafe",
    "extern",
    "default",
    "public",
    "private",
    "protected",
    "export",
    "final",
    "abstract",
    "crate",
    "super",
    "self",
    "in",
];

/// True when a structural keyword is at the statement start (after only
/// modifiers) AND followed by an identifier — a real definition, not a use.
/// Comment lines are filtered by the caller.
fn line_defines(line: &str) -> bool {
    let tokens = identifier_tokens(line);
    tokens.iter().enumerate().any(|(i, tok)| {
        OUTLINE_KEYWORDS.contains(tok)
            && tokens.get(i + 1).is_some()
            && tokens[..i].iter().all(|t| OUTLINE_MODIFIERS.contains(t))
    })
}

/// Skip obvious comment lines to cut false positives (`// fn foo does …`).
fn is_comment_line(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with('#')
        || trimmed.starts_with("--")
        || trimmed.starts_with("<!--")
}

fn render_outline(display: &str, text: &str, max_items: usize, truncated: bool) -> String {
    let mut hits: Vec<(usize, String)> = Vec::new();
    let mut total = 0usize;
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if is_comment_line(trimmed) || !line_defines(line) {
            continue;
        }
        total += 1;
        if hits.len() < max_items {
            hits.push((idx + 1, truncate_symbol_line(line)));
        }
    }
    let scanned = if truncated {
        format!(
            ", scanned first {} MiB",
            SYMBOL_MAX_FILE_BYTES / (1024 * 1024)
        )
    } else {
        String::new()
    };
    let mut out = format!(
        "outline> {display}: {total} definition(s) \
         [heuristic — fn/struct/class/def/type/impl/const/…{scanned}]\n"
    );
    if total == 0 {
        out.push_str("(no top-level definitions detected)\n");
        return out;
    }
    for (no, line) in &hits {
        out.push_str(&format!("  {no}: {line}\n"));
    }
    if total > hits.len() {
        out.push_str(&format!(
            "  [showing {} of {total}; raise max_items for more]\n",
            hits.len()
        ));
    }
    out
}

pub fn outline_file_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "outline_file".to_string(),
        description: "List the definitions in a source file (functions, structs/classes, enums, \
                      traits, types, impls, consts, modules) with line numbers — a quick \
                      structural map so you don't have to read the whole file. Heuristic + \
                      language-agnostic; comment lines and local let/var bindings are excluded. \
                      Same allowlist as fs_read_file. Use find_symbol to then jump to one."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to a source file"},
                "max_items": {"type": "integer", "description": "Definitions shown (optional, default 500, max 5000)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { outline_file_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn outline_file_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: OutlineArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let display = a.path.clone();
    let max = a
        .max_items
        .unwrap_or(OUTLINE_DEFAULT_MAX)
        .clamp(1, OUTLINE_MAX_CEILING);
    let rendered = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut buf = Vec::new();
        Read::take(file, SYMBOL_MAX_FILE_BYTES).read_to_end(&mut buf)?;
        let truncated = buf.len() as u64 >= SYMBOL_MAX_FILE_BYTES;
        let text = String::from_utf8_lossy(&buf);
        Ok(render_outline(&display, &text, max, truncated))
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

pub fn find_symbol_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "find_symbol".to_string(),
        description: "Locate where an identifier is DEFINED and USED without a full grep sweep. \
                      Recursively scans an allowed file or directory (skipping VCS/build dirs and \
                      binary files) and returns definition sites (fn/struct/class/def/type/const/\
                      let …) first, then references, each as path:line. A heuristic textual scan — \
                      no language server or index required, so it works anywhere and degrades to an \
                      honest 'not found' when the symbol is absent."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "Identifier to locate (letters, digits, underscore)"},
                "path": {"type": "string", "description": "Allowed file or directory to search (directories are walked recursively)"},
                "max_results": {"type": "integer", "description": "Max reference lines shown (optional, default 100, max 1000); definitions are always shown"}
            },
            "required": ["symbol", "path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { find_symbol_run(policy, &args).await })
    });
    (descriptor, handler)
}

async fn find_symbol_run(policy: Arc<Permissions>, args: &str) -> Result<String, ToolError> {
    let a: FindSymbolArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let symbol = a.symbol.trim().to_string();
    if symbol.is_empty() {
        return Err(ToolError::InvalidArgs("symbol must not be empty".into()));
    }
    if symbol.chars().count() > SYMBOL_MAX_LEN {
        return Err(ToolError::InvalidArgs(format!(
            "symbol too long (max {SYMBOL_MAX_LEN} chars)"
        )));
    }
    if !symbol.chars().all(is_symbol_char) {
        return Err(ToolError::InvalidArgs(
            "symbol must be an identifier (letters, digits, underscore)".into(),
        ));
    }
    let refs_cap = a
        .max_results
        .unwrap_or(SYMBOL_DEFAULT_MAX_REFS)
        .clamp(1, SYMBOL_MAX_REFS_CEILING);
    // Authorizes the root (file or dir), enforces protected-path denial, and
    // pins the canonical path under an allowed read root.
    let canon = policy.check_read(Path::new(&a.path)).await?;
    let policy2 = policy.clone();
    let display = a.path.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        find_symbol_blocking(&policy2, &canon, &display, &symbol, refs_cap)
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
    .map_err(|e| ToolError::ExecError(format!("find_symbol failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

fn find_symbol_blocking(
    policy: &Permissions,
    root: &Path,
    display: &str,
    symbol: &str,
    refs_cap: usize,
) -> std::io::Result<String> {
    let meta = std::fs::symlink_metadata(root)?;
    let mut hits = SymbolHits::default();
    let mut budget = SymbolScanBudget::default();
    if meta.is_file() {
        scan_one_file(
            policy,
            root,
            root,
            display,
            &mut hits,
            symbol,
            refs_cap,
            &mut budget,
        );
    } else if meta.is_dir() {
        // Iterative DFS; entries sorted so output is deterministic.
        let mut stack = vec![root.to_path_buf()];
        'walk: while let Some(dir) = stack.pop() {
            let Ok(reader) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = reader.filter_map(|e| e.ok().map(|e| e.path())).collect();
            paths.sort();
            for path in paths {
                if budget.files >= SYMBOL_MAX_FILES || budget.bytes >= SYMBOL_MAX_TOTAL_BYTES {
                    hits.files_capped = true;
                    break 'walk;
                }
                let Ok(entry_meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                // Never follow symlinks — keeps the walk inside the authorized root.
                if entry_meta.file_type().is_symlink() {
                    continue;
                }
                if entry_meta.is_dir() {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.starts_with('.') || SYMBOL_SKIP_DIRS.contains(&name) {
                        continue;
                    }
                    stack.push(path);
                } else if entry_meta.is_file() {
                    scan_one_file(
                        policy,
                        root,
                        &path,
                        display,
                        &mut hits,
                        symbol,
                        refs_cap,
                        &mut budget,
                    );
                }
            }
        }
    } else {
        return Err(std::io::Error::other(
            "find_symbol target is not a file or directory",
        ));
    }
    Ok(render_symbol_hits(display, symbol, &hits))
}

#[allow(clippy::too_many_arguments)]
fn scan_one_file(
    policy: &Permissions,
    root: &Path,
    path: &Path,
    display: &str,
    hits: &mut SymbolHits,
    symbol: &str,
    refs_cap: usize,
    budget: &mut SymbolScanBudget,
) {
    scan_one_file_with_hook(
        policy,
        root,
        path,
        display,
        hits,
        symbol,
        refs_cap,
        budget,
        || {},
    );
}

#[allow(clippy::too_many_arguments)]
fn scan_one_file_with_hook<F>(
    policy: &Permissions,
    root: &Path,
    path: &Path,
    display: &str,
    hits: &mut SymbolHits,
    symbol: &str,
    refs_cap: usize,
    budget: &mut SymbolScanBudget,
    before_open: F,
) where
    F: FnOnce(),
{
    if policy.is_read_protected(path) {
        return;
    }
    before_open();
    let Ok(file) = open_read_file(path) else {
        hits.skipped_unreadable += 1;
        return;
    };
    if policy
        .validate_opened_model_read(path, path, &file)
        .is_err()
    {
        hits.skipped_unreadable += 1;
        return;
    }
    let mut buf = Vec::new();
    if Read::take(file, SYMBOL_MAX_FILE_BYTES)
        .read_to_end(&mut buf)
        .is_err()
    {
        hits.skipped_unreadable += 1;
        return;
    }
    // Count the IO we actually performed, so the walk budget bounds real work.
    budget.files += 1;
    budget.bytes += buf.len() as u64;
    let Ok(text) = std::str::from_utf8(&buf) else {
        // Binary (or invalid UTF-8) file — not a source text; skip honestly.
        hits.skipped_unreadable += 1;
        return;
    };
    hits.files_scanned += 1;
    let rel = if path == root {
        display.to_string()
    } else {
        path.strip_prefix(root)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    };
    for (idx, line) in text.lines().enumerate() {
        if !line_mentions_symbol(line, symbol) {
            continue;
        }
        let line_no = idx + 1;
        if line_defines_symbol(line, symbol) {
            hits.total_defs += 1;
            if hits.defs.len() < SYMBOL_MAX_DEFS_SHOWN {
                hits.defs
                    .push((rel.clone(), line_no, truncate_symbol_line(line)));
            }
        } else {
            hits.total_refs += 1;
            if hits.refs.len() < refs_cap {
                hits.refs
                    .push((rel.clone(), line_no, truncate_symbol_line(line)));
            }
        }
    }
}

fn render_symbol_hits(display: &str, symbol: &str, hits: &SymbolHits) -> String {
    let capped_note = if hits.files_capped {
        format!(
            ", stopped at the {}-file / {} MiB scan cap",
            SYMBOL_MAX_FILES,
            SYMBOL_MAX_TOTAL_BYTES / (1024 * 1024)
        )
    } else {
        String::new()
    };
    let skip_note = if hits.skipped_unreadable > 0 {
        format!(", {} binary/unreadable skipped", hits.skipped_unreadable)
    } else {
        String::new()
    };
    let mut out = format!(
        "find_symbol> \"{symbol}\" in {display} — {} definition(s), {} reference(s) \
         [heuristic text scan; not a semantic index] (scanned {} file(s){capped_note}{skip_note})\n",
        hits.total_defs, hits.total_refs, hits.files_scanned
    );
    if hits.total_defs == 0 && hits.total_refs == 0 {
        out.push_str(&format!(
            "(no definitions or usages of \"{symbol}\" found)\n"
        ));
        return out;
    }
    if hits.defs.is_empty() {
        out.push_str(
            "definitions: (none found here — may be defined elsewhere, imported, or built-in)\n",
        );
    } else {
        out.push_str("definitions:\n");
        for (p, n, t) in &hits.defs {
            out.push_str(&format!("  {p}:{n}: {t}\n"));
        }
        if hits.total_defs > hits.defs.len() {
            out.push_str(&format!(
                "  [showing {} of {} definitions]\n",
                hits.defs.len(),
                hits.total_defs
            ));
        }
    }
    if !hits.refs.is_empty() {
        out.push_str("references:\n");
        for (p, n, t) in &hits.refs {
            out.push_str(&format!("  {p}:{n}: {t}\n"));
        }
        if hits.total_refs > hits.refs.len() {
            out.push_str(&format!(
                "  [showing {} of {} references; raise max_results for more]\n",
                hits.refs.len(),
                hits.total_refs
            ));
        }
    }
    out
}

// --- fs.scan_file (bounded log tail / grep) ---------------------------------

const SCAN_DEFAULT_TAIL_LINES: usize = 200;
const SCAN_MAX_TAIL_LINES: usize = 2000;
const SCAN_DEFAULT_MAX_MATCHES: usize = 200;
const SCAN_MAX_MATCHES_CEILING: usize = 2000;
/// Forward grep stops after scanning this many bytes (disclosed if hit), so a
/// pathological file can never make one call run unbounded.
const SCAN_MAX_SCAN_BYTES: u64 = 256 * 1024 * 1024;
/// Bytes read from the end for tail mode — bounds memory regardless of file
/// size (a true seek-to-end tail, not a full-file scan).
const SCAN_TAIL_WINDOW_BYTES: u64 = 4 * 1024 * 1024;
/// Per-line display cap so one very long line cannot blow the output budget.
const SCAN_LINE_DISPLAY_CAP: usize = 2000;

#[derive(Deserialize)]
struct ScanFileArgs {
    path: String,
    /// If present, grep mode: return lines containing this literal substring.
    /// Absent → tail mode (last N lines).
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default, deserialize_with = "lenient_bool")]
    ignore_case: bool,
    /// Tail-mode window (default 200, max 2000).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    tail_lines: Option<usize>,
    /// Grep-mode match cap (default 200, max 2000).
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_matches: Option<usize>,
}

pub fn scan_file_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_scan_file".to_string(),
        description: "Scan a large file (e.g. a log) without loading it whole. With a pattern: \
                      streams the file and returns matching lines (with line numbers), disclosing \
                      how many of the total matches are shown and how much was scanned. Without a \
                      pattern: returns the LAST tail_lines lines (a true seek-to-end tail). \
                      Memory stays bounded regardless of file size — use this instead of \
                      fs_read_file (which only sees the first bytes) for logs."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to scan"},
                "pattern": {"type": "string", "description": "Literal substring to grep for (optional; omit for tail mode)"},
                "ignore_case": {"type": "boolean", "description": "Case-insensitive match (optional, default false)"},
                "tail_lines": {"type": "integer", "description": "Tail mode: last N lines (optional, default 200, max 2000)"},
                "max_matches": {"type": "integer", "description": "Grep mode: max matched lines shown (optional, default 200, max 2000)"}
            },
            "required": ["path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { scan_file_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn scan_file_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: ScanFileArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let authorization = policy.authorize_read_file(Path::new(&a.path)).await?;
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let display = a.path.clone();
    let rendered = tokio::task::spawn_blocking(move || scan_file_blocking(file, &display, &a))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("scan failed: {e}")))?;
    Ok(policy.cap_output(rendered))
}

fn truncate_line(line: &str) -> String {
    let trimmed = line.trim_end_matches(['\n', '\r']);
    if trimmed.chars().count() > SCAN_LINE_DISPLAY_CAP {
        let cut: String = trimmed.chars().take(SCAN_LINE_DISPLAY_CAP).collect();
        format!("{cut}…[line truncated]")
    } else {
        trimmed.to_string()
    }
}

fn scan_file_blocking(
    file: std::fs::File,
    display: &str,
    args: &ScanFileArgs,
) -> std::io::Result<String> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("scan target is not a regular file"));
    }
    let total_bytes = metadata.len();
    match args.pattern.as_deref() {
        Some(pattern) if !pattern.is_empty() => {
            scan_grep(file, display, total_bytes, pattern, args)
        }
        _ => scan_tail(file, display, total_bytes, args),
    }
}

fn scan_grep(
    file: std::fs::File,
    display: &str,
    total_bytes: u64,
    pattern: &str,
    args: &ScanFileArgs,
) -> std::io::Result<String> {
    use std::io::BufRead;

    let cap = args
        .max_matches
        .unwrap_or(SCAN_DEFAULT_MAX_MATCHES)
        .min(SCAN_MAX_MATCHES_CEILING);
    let needle = if args.ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    // Bound total bytes scanned so a huge file cannot run unbounded.
    let mut reader = std::io::BufReader::new(Read::take(file, SCAN_MAX_SCAN_BYTES + 1));
    let mut line = String::new();
    let mut line_no = 0usize;
    let mut total_matches = 0usize;
    let mut bytes_read = 0u64;
    let mut shown: Vec<(usize, String)> = Vec::new();
    let mut scan_truncated = false;
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        bytes_read += n as u64;
        if bytes_read > SCAN_MAX_SCAN_BYTES {
            scan_truncated = true;
            break;
        }
        line_no += 1;
        let hay = if args.ignore_case {
            line.to_lowercase()
        } else {
            line.clone()
        };
        if hay.contains(&needle) {
            total_matches += 1;
            if shown.len() < cap {
                shown.push((line_no, truncate_line(&line)));
            }
        }
    }

    let case = if args.ignore_case {
        " (case-insensitive)"
    } else {
        ""
    };
    let scanned_note = if scan_truncated {
        format!(
            " — stopped after scanning {} MiB of {} MiB (refine or split the file)",
            SCAN_MAX_SCAN_BYTES / (1024 * 1024),
            total_bytes / (1024 * 1024)
        )
    } else {
        String::new()
    };
    let mut out = format!(
        "scan> {display}: grep {pattern:?}{case} — {total_matches} match(es) in {line_no} lines scanned{scanned_note}\n"
    );
    if shown.is_empty() {
        out.push_str("(no matching lines)\n");
    } else {
        for (no, text) in &shown {
            out.push_str(&format!("{no}: {text}\n"));
        }
        if total_matches > shown.len() {
            out.push_str(&format!(
                "[showing {} of {} matches; narrow the pattern or raise max_matches for more]\n",
                shown.len(),
                total_matches
            ));
        }
    }
    Ok(out)
}

fn scan_tail(
    mut file: std::fs::File,
    display: &str,
    total_bytes: u64,
    args: &ScanFileArgs,
) -> std::io::Result<String> {
    use std::io::{Seek, SeekFrom};

    let want = args
        .tail_lines
        .unwrap_or(SCAN_DEFAULT_TAIL_LINES)
        .clamp(1, SCAN_MAX_TAIL_LINES);
    // Read only the trailing window — bounds memory for any file size.
    let window = total_bytes.min(SCAN_TAIL_WINDOW_BYTES);
    let start = total_bytes - window;
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(window as usize);
    Read::by_ref(&mut file).take(window).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    // If we did not start at the file head, the first (partial) line is
    // incomplete — drop it so we only show whole lines.
    let mut lines: Vec<&str> = text.lines().collect();
    let window_clipped = start > 0;
    if window_clipped && !lines.is_empty() {
        lines.remove(0);
    }
    let total_in_window = lines.len();
    let tail: Vec<&str> = lines
        .iter()
        .skip(total_in_window.saturating_sub(want))
        .copied()
        .collect();

    let mut out = format!(
        "scan> {display}: tail — last {} line(s) of a {} byte file",
        tail.len(),
        total_bytes
    );
    if window_clipped {
        out.push_str(&format!(
            " (read the final {} KiB; earlier lines not shown)",
            window / 1024
        ));
    }
    out.push('\n');
    if tail.is_empty() {
        out.push_str("(no complete lines in the tail window)\n");
    } else {
        for line in &tail {
            out.push_str(&truncate_line(line));
            out.push('\n');
        }
    }
    Ok(out)
}

// --- fs.diff_config (config drift audit) ------------------------------------

#[derive(Deserialize)]
struct DiffConfigArgs {
    /// The current config to audit.
    path: String,
    /// The reference/baseline config to compare against.
    reference: String,
    /// Compare keys case-insensitively (e.g. sshd_config directives).
    #[serde(default, deserialize_with = "lenient_bool")]
    ignore_case: bool,
}

/// One parsed config: keys in first-seen order, a lookup keyed by the
/// (optionally case-folded) directive, and a count of duplicate keys seen.
struct ParsedConfig {
    order: Vec<String>,
    /// normalized key -> (display key, last value)
    entries: std::collections::HashMap<String, (String, String)>,
    duplicates: usize,
}

/// Parse a line-oriented config: `#`/`;` comments and blank lines skipped;
/// each entry split on the first `=`, `:`, or whitespace run. Last value wins
/// for a repeated key (with a duplicate count disclosed).
fn parse_config(text: &str, ignore_case: bool) -> ParsedConfig {
    let mut order = Vec::new();
    let mut entries: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    let mut duplicates = 0usize;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Split on the first '=', ':', or whitespace.
        let delim = line
            .char_indices()
            .find(|&(_, c)| c == '=' || c == ':' || c.is_whitespace())
            .map(|(i, _)| i);
        let (key_raw, value) = match delim {
            Some(i) => {
                let key = line[..i].trim();
                // Drop leading '='/':' delimiter chars (so `k=v`, `k==v`, and
                // `k = v` all yield the same value), then trim whitespace.
                let rest = line[i..].trim_start_matches(['=', ':']);
                (key, rest.trim())
            }
            None => (line, ""),
        };
        if key_raw.is_empty() {
            continue;
        }
        let norm = if ignore_case {
            key_raw.to_lowercase()
        } else {
            key_raw.to_string()
        };
        if entries.contains_key(&norm) {
            duplicates += 1;
        } else {
            order.push(norm.clone());
        }
        entries.insert(norm, (key_raw.to_string(), value.to_string()));
    }
    ParsedConfig {
        order,
        entries,
        duplicates,
    }
}

/// Render the drift between a `reference` (baseline) and `current` config.
fn render_config_diff(
    current: &ParsedConfig,
    reference: &ParsedConfig,
    path: &str,
    reference_path: &str,
) -> String {
    let mut added: Vec<(&str, &str)> = Vec::new(); // in current, not reference
    let mut changed: Vec<(&str, &str, &str)> = Vec::new(); // key, ref value, cur value
    let mut removed: Vec<(&str, &str)> = Vec::new(); // in reference, not current
    let mut unchanged = 0usize;

    for norm in &current.order {
        let (disp, cur_val) = &current.entries[norm];
        match reference.entries.get(norm) {
            None => added.push((disp.as_str(), cur_val.as_str())),
            Some((_, ref_val)) if ref_val != cur_val => {
                changed.push((disp.as_str(), ref_val.as_str(), cur_val.as_str()))
            }
            Some(_) => unchanged += 1,
        }
    }
    for norm in &reference.order {
        if !current.entries.contains_key(norm) {
            let (disp, ref_val) = &reference.entries[norm];
            removed.push((disp.as_str(), ref_val.as_str()));
        }
    }

    let mut out = format!(
        "config-diff> {path} vs {reference_path}: {} added, {} removed, {} changed, {} unchanged\n",
        added.len(),
        removed.len(),
        changed.len(),
        unchanged
    );
    if added.is_empty() && removed.is_empty() && changed.is_empty() {
        out.push_str("(no drift — configs are equivalent)\n");
    }
    if !changed.is_empty() {
        out.push_str("[changed — reference → current]\n");
        for (k, r, c) in &changed {
            out.push_str(&format!("~ {k}: {r:?} → {c:?}\n"));
        }
    }
    if !added.is_empty() {
        out.push_str("[added — set in current, absent from reference]\n");
        for (k, v) in &added {
            out.push_str(&format!("+ {k} = {v:?}\n"));
        }
    }
    if !removed.is_empty() {
        out.push_str("[removed — in reference, absent from current]\n");
        for (k, v) in &removed {
            out.push_str(&format!("- {k} = {v:?}\n"));
        }
    }
    let dup = current.duplicates + reference.duplicates;
    if dup > 0 {
        out.push_str(&format!(
            "[note: {dup} duplicate key line(s) seen; last value per key was used]\n"
        ));
    }
    out
}

pub fn diff_config_tool(policy: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "fs_diff_config".to_string(),
        description: "Compare a line-oriented config file against a reference/baseline and report \
                      drift: keys added, removed, or changed, plus an unchanged count. Parses \
                      `key value`, `key=value`, and `key: value` styles; skips # and ; comments. \
                      Set ignore_case for case-insensitive directives (e.g. sshd_config)."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Current config file to audit"},
                "reference": {"type": "string", "description": "Reference/baseline config to compare against"},
                "ignore_case": {"type": "boolean", "description": "Case-insensitive key comparison (optional, default false)"}
            },
            "required": ["path", "reference"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { diff_config_run(&policy, &args).await })
    });
    (descriptor, handler)
}

async fn diff_config_run(policy: &Permissions, args: &str) -> Result<String, ToolError> {
    let a: DiffConfigArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    let current_text = read_config_text(policy, &a.path).await?;
    let reference_text = read_config_text(policy, &a.reference).await?;
    let ignore_case = a.ignore_case;
    let path = a.path.clone();
    let reference = a.reference.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        let cur = parse_config(&current_text, ignore_case);
        let refc = parse_config(&reference_text, ignore_case);
        render_config_diff(&cur, &refc, &path, &reference)
    })
    .await
    .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?;
    Ok(policy.cap_output(rendered))
}

/// Authorize and read one config file (bounded by the read cap — configs are
/// small; the same guard as fs_read_file).
async fn read_config_text(policy: &Permissions, path: &str) -> Result<String, ToolError> {
    let authorization = policy.authorize_read_file(Path::new(path)).await?;
    let max_bytes = policy.max_bytes();
    let file = open_read_file(authorization.path())
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    authorization.validate_opened_file(&file, policy)?;
    let content = tokio::task::spawn_blocking(move || read_capped_text(file, max_bytes))
        .await
        .map_err(|e| ToolError::ExecError(format!("join error: {e}")))?
        .map_err(|e| ToolError::ExecError(format!("read failed: {e}")))?;
    Ok(content.text)
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
    // `repo_grep` was the one model-facing arg struct still parsing these
    // strictly. Local models quote scalars constantly (`"max_count": "100"`),
    // and a rejected call costs a whole tool round — tens of seconds of CPU
    // inference — for a value that was never ambiguous. Every other arg struct
    // in this file already coerces; this closes the gap. It matters more than
    // it used to: `repo_grep` is one of only three read tools a lean-tier
    // registry advertises, so losing it leaves a small model unable to search.
    #[serde(default, deserialize_with = "lenient_opt_usize")]
    max_count: Option<usize>,
    #[serde(default, deserialize_with = "lenient_bool")]
    literal: bool,
}

const GREP_TIMEOUT: Duration = Duration::from_secs(10);
const GREP_DEFAULT_MAX: usize = 100;
const GREP_MAX_MATCHES: usize = 1_000;
#[cfg(unix)]
const GREP_MAX_FILES: usize = 10_000;
#[cfg(unix)]
const GREP_MAX_ENTRIES: usize = 20_000;
#[cfg(unix)]
const GREP_MAX_FILE_BYTES: usize = 1024 * 1024;
#[cfg(unix)]
const GREP_MAX_SCAN_BYTES: usize = 16 * 1024 * 1024;
#[cfg(unix)]
const GREP_OUTPUT_RESERVE: usize = 256;

#[cfg(any(not(unix), test))]
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

#[cfg(not(unix))]
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
    #[cfg(not(unix))]
    command_path: PathBuf,
    #[cfg(unix)]
    descriptor: std::fs::File,
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
            Ok(Self { descriptor })
        }

        #[cfg(not(unix))]
        Ok(Self {
            command_path: path.to_path_buf(),
        })
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeSearchStop {
    MatchLimit,
    OutputLimit,
    ScanByteLimit,
    FileLimit,
    EntryLimit,
    DepthLimit,
    Deadline,
    Cancelled,
}

#[cfg(unix)]
impl NativeSearchStop {
    fn label(self) -> &'static str {
        match self {
            Self::MatchLimit => "match limit reached",
            Self::OutputLimit => "output byte limit reached",
            Self::ScanByteLimit => "scan byte limit reached",
            Self::FileLimit => "file limit reached",
            Self::EntryLimit => "directory entry limit reached",
            Self::DepthLimit => "directory depth limit reached",
            Self::Deadline => "search deadline reached",
            Self::Cancelled => "search cancelled",
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeEntryKind {
    File,
    Directory,
}

#[cfg(unix)]
#[derive(Debug)]
struct NativeDirectoryEntry {
    name: OsString,
    kind: NativeEntryKind,
}

#[cfg(unix)]
struct NativeDirectoryRead {
    entries: Vec<NativeDirectoryEntry>,
    entries_seen: usize,
    stop: Option<NativeSearchStop>,
}

#[cfg(unix)]
fn read_pinned_directory(
    directory: &std::fs::File,
    max_entries: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> std::io::Result<NativeDirectoryRead> {
    let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(duplicate);
        }
        return Err(error);
    }
    let stream = DirectoryStream(stream);
    let mut entries = Vec::new();
    let mut entries_seen = 0_usize;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(NativeDirectoryRead {
                entries,
                entries_seen,
                stop: Some(NativeSearchStop::Cancelled),
            });
        }
        if Instant::now() >= deadline {
            return Ok(NativeDirectoryRead {
                entries,
                entries_seen,
                stop: Some(NativeSearchStop::Deadline),
            });
        }
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if entries_seen >= max_entries {
            return Ok(NativeDirectoryRead {
                entries,
                entries_seen,
                stop: Some(NativeSearchStop::EntryLimit),
            });
        }
        entries_seen = entries_seen.saturating_add(1);
        let name = OsString::from_vec(name.to_vec());
        // Match ripgrep's established default: hidden entries are excluded
        // unless a future explicit schema option requests them.
        if name.as_bytes().starts_with(b".") {
            continue;
        }
        let encoded = CString::new(name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "directory entry contains an interior NUL byte",
            )
        })?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let status = unsafe {
            libc::fstatat(
                libc::dirfd(stream.0),
                encoded.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status != 0 {
            continue;
        }
        let mode = unsafe { stat.assume_init().st_mode } & libc::S_IFMT;
        let kind = if mode == libc::S_IFREG {
            NativeEntryKind::File
        } else if mode == libc::S_IFDIR {
            NativeEntryKind::Directory
        } else {
            // Symlinks, devices, sockets, and FIFOs are never search inputs.
            continue;
        };
        entries.push(NativeDirectoryEntry { name, kind });
    }
    entries.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    Ok(NativeDirectoryRead {
        entries,
        entries_seen,
        stop: None,
    })
}

#[cfg(unix)]
fn open_pinned_child(
    directory: &std::fs::File,
    name: &OsStr,
    kind: NativeEntryKind,
) -> std::io::Result<std::fs::File> {
    let name = CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "directory entry contains an interior NUL byte",
        )
    })?;
    let kind_flags = match kind {
        NativeEntryKind::File => 0,
        NativeEntryKind::Directory => libc::O_DIRECTORY,
    };
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC | kind_flags,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
struct NativeSearchCancellation(Arc<AtomicBool>);

#[cfg(unix)]
impl Drop for NativeSearchCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[cfg(unix)]
struct NativeSearch<'a> {
    permissions: &'a Permissions,
    expression: &'a Regex,
    canonical_root: &'a Path,
    root_is_file: bool,
    max_matches: usize,
    output_budget: usize,
    deadline: Instant,
    cancelled: &'a AtomicBool,
    output: String,
    matches: usize,
    files: usize,
    entries: usize,
    scanned_bytes: usize,
    skipped_files: usize,
    root_skip_reason: Option<&'static str>,
    stop: Option<NativeSearchStop>,
}

#[cfg(unix)]
impl NativeSearch<'_> {
    fn check_stop(&mut self) -> bool {
        if self.stop.is_some() {
            return true;
        }
        if self.cancelled.load(Ordering::Acquire) {
            self.stop = Some(NativeSearchStop::Cancelled);
        } else if Instant::now() >= self.deadline {
            self.stop = Some(NativeSearchStop::Deadline);
        }
        self.stop.is_some()
    }

    fn stable_path(&self, relative: &Path) -> PathBuf {
        if self.root_is_file || relative.as_os_str().is_empty() {
            self.canonical_root.to_path_buf()
        } else {
            self.canonical_root.join(relative)
        }
    }

    fn display_path(&self, relative: &Path) -> String {
        if self.root_is_file {
            self.canonical_root
                .file_name()
                .unwrap_or(self.canonical_root.as_os_str())
                .to_string_lossy()
                .into_owned()
        } else {
            relative.to_string_lossy().into_owned()
        }
    }

    fn skip_file(&mut self, reason: &'static str) {
        self.skipped_files = self.skipped_files.saturating_add(1);
        if self.root_is_file {
            self.root_skip_reason = Some(reason);
        }
    }

    fn search_file(&mut self, relative: &Path, mut file: std::fs::File) -> std::io::Result<()> {
        if self.check_stop() {
            return Ok(());
        }
        if self.files >= GREP_MAX_FILES {
            self.stop = Some(NativeSearchStop::FileLimit);
            return Ok(());
        }
        let stable_path = self.stable_path(relative);
        if self
            .permissions
            .validate_opened_model_read(&stable_path, &stable_path, &file)
            .is_err()
        {
            self.skip_file("the opened file was no longer authorized");
            return Ok(());
        }
        self.files = self.files.saturating_add(1);
        let remaining = GREP_MAX_SCAN_BYTES.saturating_sub(self.scanned_bytes);
        if remaining == 0 {
            self.stop = Some(NativeSearchStop::ScanByteLimit);
            return Ok(());
        }
        let metadata = file.metadata()?;
        if metadata.len() > GREP_MAX_FILE_BYTES as u64 {
            self.skip_file("file exceeds the 1 MiB per-file limit");
            return Ok(());
        }
        let read_budget = remaining.min(GREP_MAX_FILE_BYTES);
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(read_budget)
                .min(read_budget.saturating_add(1)),
        );
        Read::by_ref(&mut file)
            .take(read_budget.saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        let hit_read_limit = bytes.len() > read_budget;
        bytes.truncate(bytes.len().min(read_budget));
        self.scanned_bytes = self.scanned_bytes.saturating_add(bytes.len());
        if bytes.contains(&0) {
            self.skip_file("file contains binary data");
            if hit_read_limit && remaining <= GREP_MAX_FILE_BYTES {
                self.stop = Some(NativeSearchStop::ScanByteLimit);
            }
            return Ok(());
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(error) if hit_read_limit && error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("validated UTF-8 prefix")
            }
            Err(_) => {
                self.skip_file("file is not valid UTF-8 text");
                if hit_read_limit && remaining <= GREP_MAX_FILE_BYTES {
                    self.stop = Some(NativeSearchStop::ScanByteLimit);
                }
                return Ok(());
            }
        };
        if hit_read_limit && remaining > GREP_MAX_FILE_BYTES {
            // Metadata can under-report length on virtual files. Do not emit a
            // partial match set from a file that exceeded the per-file cap.
            self.skip_file("file exceeds the 1 MiB per-file limit");
            return Ok(());
        }
        let display_path = self.display_path(relative);
        for (index, line) in text.lines().enumerate() {
            if self.check_stop() {
                break;
            }
            if !self.expression.is_match(line) {
                continue;
            }
            let rendered = format!("{display_path}:{}:{line}", index + 1);
            let separator = usize::from(!self.output.is_empty());
            if self
                .output
                .len()
                .saturating_add(separator)
                .saturating_add(rendered.len())
                > self.output_budget
            {
                self.stop = Some(NativeSearchStop::OutputLimit);
                break;
            }
            if separator == 1 {
                self.output.push('\n');
            }
            self.output.push_str(&rendered);
            self.matches = self.matches.saturating_add(1);
            if self.matches >= self.max_matches {
                self.stop = Some(NativeSearchStop::MatchLimit);
                break;
            }
        }
        if hit_read_limit && self.stop.is_none() {
            self.stop = Some(NativeSearchStop::ScanByteLimit);
        }
        Ok(())
    }

    fn search_directory(
        &mut self,
        relative: &Path,
        directory: &std::fs::File,
        depth: usize,
    ) -> std::io::Result<()> {
        if self.check_stop() {
            return Ok(());
        }
        if depth > 64 {
            self.stop = Some(NativeSearchStop::DepthLimit);
            return Ok(());
        }
        let remaining_entries = GREP_MAX_ENTRIES.saturating_sub(self.entries);
        let directory_read =
            read_pinned_directory(directory, remaining_entries, self.deadline, self.cancelled)?;
        self.entries = self.entries.saturating_add(directory_read.entries_seen);
        if matches!(
            directory_read.stop,
            Some(NativeSearchStop::Cancelled | NativeSearchStop::Deadline)
        ) {
            self.stop = directory_read.stop;
            return Ok(());
        }
        for entry in directory_read.entries {
            if self.check_stop() {
                break;
            }
            let child_relative = relative.join(&entry.name);
            let stable_path = self.stable_path(&child_relative);
            if self.permissions.is_read_protected(&stable_path) {
                continue;
            }
            let child = match open_pinned_child(directory, &entry.name, entry.kind) {
                Ok(child) => child,
                Err(_) => {
                    self.skipped_files = self.skipped_files.saturating_add(1);
                    continue;
                }
            };
            match entry.kind {
                NativeEntryKind::File => self.search_file(&child_relative, child)?,
                NativeEntryKind::Directory => {
                    if self
                        .search_directory(&child_relative, &child, depth.saturating_add(1))
                        .is_err()
                    {
                        self.skipped_files = self.skipped_files.saturating_add(1);
                    }
                }
            }
        }
        if self.stop.is_none() {
            self.stop = directory_read.stop;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<String, ToolError> {
        if self.stop == Some(NativeSearchStop::Cancelled) {
            return Err(ToolError::ExecError("search cancelled".to_string()));
        }
        if self.output.is_empty() {
            if let Some(reason) = self.root_skip_reason {
                self.output.push_str("(file not searched: ");
                self.output.push_str(reason);
                self.output.push(')');
            } else {
                self.output.push_str(if self.stop.is_some() {
                    "(no matches before search stopped)"
                } else {
                    "(no matches)"
                });
            }
        }
        if let Some(stop) = self.stop {
            self.output.push_str(&format!(
                "\n...[search truncated: {}; matches={}; files={}; entries={}; scanned_bytes={}; skipped_files={}]",
                stop.label(),
                self.matches,
                self.files,
                self.entries,
                self.scanned_bytes,
                self.skipped_files,
            ));
        }
        Ok(self.permissions.cap_output(self.output))
    }
}

#[cfg(unix)]
fn native_grep(
    permissions: &Permissions,
    expression: &Regex,
    canonical_root: &Path,
    grep_root: PinnedGrepRoot,
    max_matches: usize,
    output_limit: usize,
    cancelled: &AtomicBool,
) -> Result<String, ToolError> {
    let metadata = grep_root
        .descriptor
        .metadata()
        .map_err(|error| ToolError::ExecError(format!("search root changed: {error}")))?;
    let root_is_file = metadata.is_file();
    let reserve = GREP_OUTPUT_RESERVE.min(output_limit / 2);
    let mut search = NativeSearch {
        permissions,
        expression,
        canonical_root,
        root_is_file,
        max_matches,
        output_budget: output_limit.saturating_sub(reserve),
        deadline: Instant::now() + GREP_TIMEOUT,
        cancelled,
        output: String::new(),
        matches: 0,
        files: 0,
        entries: 0,
        scanned_bytes: 0,
        skipped_files: 0,
        root_skip_reason: None,
        stop: None,
    };
    if root_is_file {
        search
            .search_file(Path::new(""), grep_root.descriptor)
            .map_err(|error| ToolError::ExecError(format!("search failed: {error}")))?;
    } else {
        search
            .search_directory(Path::new(""), &grep_root.descriptor, 0)
            .map_err(|error| ToolError::ExecError(format!("search failed: {error}")))?;
    }
    search.finish()
}

#[cfg(any(not(unix), test))]
#[derive(Debug)]
enum ProcessTermination {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

#[cfg(any(not(unix), test))]
#[derive(Debug)]
struct CapturedProcess {
    termination: ProcessTermination,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Notify the detached process supervisor when the caller's future is dropped.
/// The supervisor owns the child, so it can both kill and reap it asynchronously.
#[cfg(any(not(unix), test))]
struct CancelChildOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

#[cfg(any(not(unix), test))]
impl CancelChildOnDrop {
    fn disarm(&mut self) {
        self.0.take();
    }
}

#[cfg(any(not(unix), test))]
impl Drop for CancelChildOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            let _ = cancel.send(());
        }
    }
}

#[cfg(any(not(unix), test))]
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

#[cfg(any(not(unix), test))]
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

#[cfg(any(not(unix), test))]
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

#[cfg(any(not(unix), test))]
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
        description: "Search UTF-8 file contents recursively under an allowed file or directory. \
                      Returns deterministic path:line:text matches. Unix hosts use a native \
                      descriptor-relative walker; other hosts use a fixed-argv ripgrep fallback. \
                      Search work, matches, and output are bounded."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "maxLength": 4096, "description": "Regular expression, or exact text when literal=true"},
                "path": {"type": "string", "description": "Directory or file to search under"},
                "max_count": {"type": "integer", "minimum": 1, "maximum": GREP_MAX_MATCHES, "description": "Maximum matching lines (default 100; global on native Unix search)"},
                "literal": {"type": "boolean", "description": "Treat pattern as exact text instead of a regular expression (default false)"}
            },
            "required": ["pattern", "path"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let policy = policy.clone();
        Box::pin(async move { grep_run(policy, &args).await })
    });
    (descriptor, handler)
}

async fn grep_run(policy: Arc<Permissions>, args: &str) -> Result<String, ToolError> {
    grep_run_with_hook(policy, args, || {}).await
}

async fn grep_run_with_hook<F>(
    policy: Arc<Permissions>,
    args: &str,
    after_pin: F,
) -> Result<String, ToolError>
where
    F: FnOnce(),
{
    let a: GrepArgs =
        serde_json::from_str(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    if a.pattern.len() > 4 * 1024 {
        return Err(ToolError::InvalidArgs(
            "search pattern exceeds the 4096-byte limit".to_string(),
        ));
    }
    let max = a.max_count.unwrap_or(GREP_DEFAULT_MAX);
    if !(1..=GREP_MAX_MATCHES).contains(&max) {
        return Err(ToolError::InvalidArgs(format!(
            "max_count must be between 1 and {GREP_MAX_MATCHES}"
        )));
    }
    let compiled_pattern = if a.literal {
        regex::escape(&a.pattern)
    } else {
        a.pattern.clone()
    };
    let expression = Regex::new(&compiled_pattern)
        .map_err(|error| ToolError::InvalidArgs(format!("invalid search pattern: {error}")))?;
    let canon = policy.check_read(Path::new(&a.path)).await?;
    let pin_path = canon.clone();
    let grep_root = tokio::task::spawn_blocking(move || PinnedGrepRoot::open(&pin_path))
        .await
        .map_err(|error| ToolError::ExecError(format!("grep pin join error: {error}")))?
        .map_err(|error| ToolError::ExecError(format!("grep target changed: {error}")))?;
    after_pin();

    #[cfg(unix)]
    {
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = NativeSearchCancellation(cancelled.clone());
        let worker_policy = policy.clone();
        let output_limit = policy.max_bytes();
        tokio::task::spawn_blocking(move || {
            native_grep(
                &worker_policy,
                &expression,
                &canon,
                grep_root,
                max,
                output_limit,
                &cancelled,
            )
        })
        .await
        .map_err(|error| ToolError::ExecError(format!("search worker join error: {error}")))?
    }

    #[cfg(not(unix))]
    {
        let _ = expression;
        let pattern = a.pattern.clone();
        // Fixed argv, never a shell. Pattern and path are separate argv
        // elements. Non-Unix retains the established ripgrep backend until a
        // descriptor-relative no-reparse walker is available there.
        let mut command = tokio::process::Command::new("rg");
        command
            .arg("--line-number")
            .arg("--no-heading")
            .arg("--color=never")
            .arg("--max-count")
            .arg(max.to_string());
        if a.literal {
            command.arg("--fixed-strings");
        }
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
}

// --- fs.write_file ----------------------------------------------------------

#[derive(Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
    /// Exact content the caller previously observed. Replacing an existing
    /// file requires this precondition; creation and no-op writes omit it.
    #[serde(default)]
    expected_previous_content: Option<String>,
}

static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static JOURNAL_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static JOURNAL_TURN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

const JOURNAL_TURN_LIMIT: usize = 32;
/// Maximum retained verified diff evidence in one change/checkpoint report.
pub const MAX_JOURNAL_DIFF_BYTES: usize = 64 * 1024;

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
    /// Unified diff derived from the exact snapshots retained by the journal.
    /// Untrusted terminal controls are escaped before this string is stored.
    pub unified_diff: String,
    pub diff_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalTurnReport {
    pub id: u64,
    pub label: String,
    pub changes: Vec<JournalChangeReport>,
    /// Bounded concatenation of the per-change verified unified diffs.
    pub unified_diff: String,
    pub diff_truncated: bool,
}

/// Metadata-only checkpoint row for list UIs. Full evidence is available from
/// [`WriteJournal::checkpoint_preview`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalCheckpointSummary {
    pub id: u64,
    pub label: String,
    pub change_count: usize,
    pub paths: Vec<PathBuf>,
}

/// Host verification for a required filesystem effect. This never exposes the
/// retained snapshots: callers get only byte counts and whether the path's
/// current identity, metadata, and contents still match the latest verified
/// model-tool write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalWriteVerification {
    pub initial_bytes: Option<usize>,
    pub final_bytes: usize,
    pub net_content_changed: bool,
    pub current_matches_latest_write: bool,
}

/// Read an already-authorized write target through the same descriptor-pinned,
/// no-follow path machinery used by atomic model writes. This is host evidence
/// for completion postconditions, not a model read grant; callers must resolve
/// and authorize `path` with [`Permissions::check_write`] first.
pub fn read_verified_write_target(
    path: &Path,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, ToolError> {
    MutationTarget::pin(path)
        .and_then(|target| target.read_snapshot_limited(max_bytes))
        .map(|snapshot| snapshot.map(|snapshot| snapshot.content))
        .map_err(|error| {
            ToolError::ExecError(format!(
                "cannot inspect authorized write target '{}': {error}",
                path.display()
            ))
        })
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

    /// Snapshot all recoverable checkpoints in creation order. Reports expose
    /// verified paths and byte metadata but never the retained file contents.
    pub fn checkpoints(&self) -> Vec<JournalTurnReport> {
        self.inner
            .lock()
            .expect("write journal")
            .history
            .iter()
            .cloned()
            .map(turn_report)
            .collect()
    }

    /// Snapshot recoverable checkpoints without cloning retained diff bodies
    /// into a list UI.
    pub fn checkpoint_summaries(&self) -> Vec<JournalCheckpointSummary> {
        self.inner
            .lock()
            .expect("write journal")
            .history
            .iter()
            .map(|turn| JournalCheckpointSummary {
                id: turn.id,
                label: turn.label.clone(),
                change_count: turn.changes.len(),
                paths: turn
                    .changes
                    .iter()
                    .map(|change| change.path.clone())
                    .collect(),
            })
            .collect()
    }

    /// Verify the net model-filesystem effect recorded for `path` during this
    /// journal lifetime, including the currently active attributed turn.
    /// `None` means no retained verified write names the exact resolved path.
    /// A later external mutation (or deletion) makes
    /// `current_matches_latest_write` false, so a completion contract cannot
    /// bless state that changed after the model tool returned.
    pub fn verify_written_path(
        &self,
        path: &Path,
    ) -> Result<Option<JournalWriteVerification>, ToolError> {
        self.verify_written_path_with_current_content(path, |_| ())
            .map(|verification| verification.map(|(verification, ())| verification))
    }

    /// Verify a required write and derive one host-owned observation from the
    /// exact same descriptor-pinned current snapshot used for identity/content
    /// matching. This avoids accepting a digest read through a second open
    /// after the journal check. The snapshot bytes never escape except through
    /// the caller's bounded return value.
    pub fn verify_written_path_with_current_content<R>(
        &self,
        path: &Path,
        inspect: impl FnOnce(Option<&[u8]>) -> R,
    ) -> Result<Option<(JournalWriteVerification, R)>, ToolError> {
        let snapshots = {
            let state = self.inner.lock().expect("write journal");
            let mut first_before = None;
            let mut saw_first = false;
            let mut latest_after = None;
            for change in state
                .history
                .iter()
                .flat_map(|turn| turn.changes.iter())
                .chain(state.active.iter().flat_map(|turn| turn.changes.iter()))
                .filter(|change| change.path == path)
            {
                if !saw_first {
                    first_before = change.before.clone();
                    saw_first = true;
                }
                latest_after = Some(change.after.clone());
            }
            latest_after.map(|after| (first_before, after))
        };
        let Some((before, after)) = snapshots else {
            return Ok(None);
        };
        let current = MutationTarget::pin(path)
            .and_then(|target| target.read_snapshot_limited(after.content.len()))
            .map_err(|error| {
                ToolError::ExecError(format!(
                    "cannot verify required write '{}': {error}",
                    path.display()
                ))
            })?;
        let current_matches_latest_write = current
            .as_ref()
            .is_some_and(|current| after.matches(current));
        let net_content_changed = before
            .as_ref()
            .map_or(true, |before| before.content != after.content);
        let observation = inspect(current.as_ref().map(|snapshot| snapshot.content.as_slice()));
        Ok(Some((
            JournalWriteVerification {
                initial_bytes: before.as_ref().map(|snapshot| snapshot.content.len()),
                final_bytes: after.content.len(),
                net_content_changed,
                current_matches_latest_write,
            },
            observation,
        )))
    }

    /// Return one recoverable checkpoint with bounded, snapshot-derived diff
    /// evidence. Missing/evicted checkpoint IDs return `None`.
    pub fn checkpoint_preview(&self, id: u64) -> Option<JournalTurnReport> {
        self.inner
            .lock()
            .expect("write journal")
            .history
            .iter()
            .find(|turn| turn.id == id)
            .cloned()
            .map(turn_report)
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
        let id = {
            let state = self.inner.lock().expect("write journal");
            if state.active.is_some() {
                return Err(ToolError::ExecError(
                    "cannot undo while a model turn is still active".to_string(),
                ));
            }
            state.history.back().map(|turn| turn.id).ok_or_else(|| {
                ToolError::InvalidArgs("nothing to undo in this session".to_string())
            })?
        };
        self.undo_checkpoint(policy, id).await
    }

    /// Restore one selected checkpoint if every affected target still matches
    /// that checkpoint's exact post-write identity, metadata, and content.
    /// Selecting an older disjoint checkpoint is safe; a later write to the
    /// same path necessarily fails the identity/content precondition.
    pub async fn undo_checkpoint(
        &self,
        policy: &Permissions,
        id: u64,
    ) -> Result<JournalTurnReport, ToolError> {
        let turn = {
            let state = self.inner.lock().expect("write journal");
            if state.active.is_some() {
                return Err(ToolError::ExecError(
                    "cannot undo while a model turn is still active".to_string(),
                ));
            }
            state
                .history
                .iter()
                .find(|turn| turn.id == id)
                .cloned()
                .ok_or_else(|| {
                    ToolError::InvalidArgs(format!(
                        "undo checkpoint {id} is not available in this session"
                    ))
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

const JOURNAL_DIFF_TRUNCATION_MARKER: &str = "\n...[checkpoint diff truncated at 64 KiB]\n";

fn truncate_journal_diff(diff: &mut String, limit: usize) -> bool {
    if diff.len() <= limit {
        return false;
    }
    if limit == 0 {
        diff.clear();
        return true;
    }
    let marker = if limit >= JOURNAL_DIFF_TRUNCATION_MARKER.len() {
        JOURNAL_DIFF_TRUNCATION_MARKER
    } else {
        ""
    };
    let mut boundary = limit.saturating_sub(marker.len());
    while !diff.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    diff.truncate(boundary);
    diff.push_str(marker);
    true
}

fn cap_journal_diff(diff: &mut String) -> bool {
    truncate_journal_diff(diff, MAX_JOURNAL_DIFF_BYTES)
}

/// Escape every untrusted control character while preserving printable
/// Unicode. Newlines used by the unified-diff grammar are added separately.
fn terminal_safe_diff_atom(value: &str) -> String {
    use std::fmt::Write as _;

    let mut safe = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\t' => safe.push_str("\\t"),
            '\r' => safe.push_str("\\r"),
            '\n' => safe.push_str("\\n"),
            character if character.is_control() => {
                let _ = write!(safe, "\\u{{{:x}}}", u32::from(character));
            }
            character => safe.push(character),
        }
    }
    safe
}

fn text_lines(bytes: &[u8]) -> Option<(Vec<&str>, bool)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let trailing_newline = text.ends_with('\n');
    let lines = if text.is_empty() {
        Vec::new()
    } else {
        text.split_terminator('\n').collect()
    };
    Some((lines, trailing_newline))
}

fn push_unified_line(
    output: &mut String,
    prefix: char,
    line: &str,
    is_final_line: bool,
    trailing_newline: bool,
) {
    output.push(prefix);
    output.push_str(&terminal_safe_diff_atom(line));
    output.push('\n');
    if is_final_line && !trailing_newline {
        output.push_str("\\ No newline at end of file\n");
    }
}

fn verified_change_diff(change: &JournalChange) -> (String, bool) {
    use std::fmt::Write as _;

    let path = terminal_safe_diff_atom(&change.path.to_string_lossy());
    let mut diff = String::new();
    if change.before.is_some() {
        let _ = writeln!(diff, "--- a/{path}");
    } else {
        diff.push_str("--- /dev/null\n");
    }
    let _ = writeln!(diff, "+++ b/{path}");

    let before_bytes = change
        .before
        .as_ref()
        .map(|snapshot| snapshot.content.as_slice())
        .unwrap_or_default();
    let Some((before, before_trailing_newline)) = text_lines(before_bytes) else {
        diff.push_str("Binary file changed (content omitted)\n");
        let truncated = cap_journal_diff(&mut diff);
        return (diff, truncated);
    };
    let Some((after, after_trailing_newline)) = text_lines(&change.after.content) else {
        diff.push_str("Binary file changed (content omitted)\n");
        let truncated = cap_journal_diff(&mut diff);
        return (diff, truncated);
    };

    let mut prefix = before
        .iter()
        .zip(after.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let newline_changed = before_trailing_newline != after_trailing_newline;
    if newline_changed && prefix == before.len().min(after.len()) && prefix > 0 {
        prefix -= 1;
    }
    let mut suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    if newline_changed {
        suffix = suffix.min(before.len().min(after.len()).saturating_sub(prefix + 1));
    }
    let old_changed_end = before.len().saturating_sub(suffix);
    let new_changed_end = after.len().saturating_sub(suffix);
    let context_start = prefix.saturating_sub(3);
    let old_context_end = old_changed_end.saturating_add(3).min(before.len());
    let new_context_end = new_changed_end.saturating_add(3).min(after.len());
    let old_count = old_context_end.saturating_sub(context_start);
    let new_count = new_context_end.saturating_sub(context_start);
    let old_start = if old_count == 0 { 0 } else { context_start + 1 };
    let new_start = if new_count == 0 { 0 } else { context_start + 1 };
    let _ = writeln!(
        diff,
        "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
    );

    for (index, line) in before[context_start..prefix].iter().enumerate() {
        let absolute = context_start + index;
        push_unified_line(
            &mut diff,
            ' ',
            line,
            absolute + 1 == before.len(),
            before_trailing_newline,
        );
    }
    for (index, line) in before[prefix..old_changed_end].iter().enumerate() {
        let absolute = prefix + index;
        push_unified_line(
            &mut diff,
            '-',
            line,
            absolute + 1 == before.len(),
            before_trailing_newline,
        );
    }
    for (index, line) in after[prefix..new_changed_end].iter().enumerate() {
        let absolute = prefix + index;
        push_unified_line(
            &mut diff,
            '+',
            line,
            absolute + 1 == after.len(),
            after_trailing_newline,
        );
    }
    for (index, line) in before[old_changed_end..old_context_end].iter().enumerate() {
        let absolute = old_changed_end + index;
        push_unified_line(
            &mut diff,
            ' ',
            line,
            absolute + 1 == before.len(),
            before_trailing_newline,
        );
    }

    let truncated = cap_journal_diff(&mut diff);
    (diff, truncated)
}

fn turn_report(turn: JournalTurn) -> JournalTurnReport {
    let mut changes = Vec::with_capacity(turn.changes.len());
    let mut unified_diff = String::new();
    let mut diff_truncated = false;
    let mut per_change_budget = MAX_JOURNAL_DIFF_BYTES;
    for change in turn.changes {
        let (change_diff, raw_change_truncated) = verified_change_diff(&change);
        if !unified_diff.is_empty() && !unified_diff.ends_with('\n') {
            unified_diff.push('\n');
        }
        unified_diff.push_str(&change_diff);
        diff_truncated |= raw_change_truncated;
        let mut reported_change_diff = change_diff;
        let budget_truncated = truncate_journal_diff(&mut reported_change_diff, per_change_budget);
        per_change_budget = per_change_budget.saturating_sub(reported_change_diff.len());
        changes.push(JournalChangeReport {
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
            unified_diff: reported_change_diff,
            diff_truncated: raw_change_truncated || budget_truncated,
        });
    }
    diff_truncated |= cap_journal_diff(&mut unified_diff);
    JournalTurnReport {
        id: turn.id,
        label: turn.label,
        changes,
        unified_diff,
        diff_truncated,
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
        RenameMode::Exchange => crate::rename_flags::RENAME_EXCHANGE,
        RenameMode::NoReplace => crate::rename_flags::RENAME_NOREPLACE,
    };
    unsafe {
        crate::rename_flags::renameat2(old_dir, old.as_ptr(), new_dir, new.as_ptr(), flags)
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

// Non-unix builds need this outside tests too: `atomic_replace_checked_portable`
// is the only replace path there, and it reads the current snapshot by path.
#[cfg(any(test, not(unix)))]
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
            "Create a UTF-8 text file, or replace one only when its exact previously-read \
                      content is supplied as expected_previous_content. Use fs_edit_file for \
                      focused edits. The parent directory must already exist. Returns the path, \
                      byte count, and observed change."
                .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to write (created or overwritten)"},
                "content": {"type": "string", "description": "Full UTF-8 file contents"},
                "expected_previous_content": {
                    "type": "string",
                    "description": "Exact full content previously read from an existing target. Required to replace it; omit only when creating a new file or when content is already unchanged."
                }
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

/// A chat-template token is leaked protocol debris only when it appears
/// OUTSIDE a string literal — a raw completion leaks it bare (`<|im_start|>assistant`
/// at line start), whereas legitimate source that lists these tokens as data
/// always quotes them (`"<|im_start|>"`). Treating a token as debris only when
/// it is NOT immediately preceded by a quote removes the false positive on any
/// file that mentions the markers as strings — e.g. kitsu's own token table or
/// debris-detector code (a real kimi-cycle-16 false positive: editing near the
/// `CHAT_TEMPLATE_TOKENS` list tripped the old naive `contains` check).
fn has_unquoted_chat_template_marker(content: &str) -> bool {
    for tok in CHAT_TEMPLATE_TOKENS {
        let mut from = 0;
        while let Some(rel) = content[from..].find(tok) {
            let idx = from + rel;
            let preceded_by_quote = content[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| c == '"' || c == '\'');
            if !preceded_by_quote {
                return true;
            }
            from = idx + tok.len();
        }
    }
    false
}

/// Best-effort detection of obviously-broken generated content: a model's raw
/// protocol tokens leaking into the file, or a would-be multi-line file whose
/// newlines never got un-escaped (literal `\n` two-char sequences instead of
/// real line breaks). Advisory only — never blocks the write — a heuristic
/// false positive on legitimately unusual content is far cheaper than a
/// silently corrupted file (a real dogfooding finding: a Python file written
/// with literal `\n` was a guaranteed SyntaxError, reported as a clean write).
fn content_quality_warning(content: &str) -> Option<&'static str> {
    if has_unquoted_chat_template_marker(content) {
        return Some(
            "content contains a raw model chat-template control marker — \
             the model likely emitted protocol text instead of real file content",
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
    if let Some(expected) = a.expected_previous_content.as_deref() {
        policy.check_write_size(expected.len())?;
    }
    let target = policy.check_write(Path::new(&a.path)).await?;
    let bytes = a.content.len();
    let quality_warning = content_quality_warning(&a.content);
    let display = target.display().to_string();
    let expected_previous = a.expected_previous_content.map(String::into_bytes);
    let content = a.content.into_bytes();
    let written = target.clone();
    let blocking_journal = journal.cloned();
    let tracked_mutation = journal.map(WriteJournal::begin_mutation);
    // The journal pre-image is part of a WRITE, so bound it by the write cap
    // (1 MiB), not the 64 KiB read cap — otherwise any file over the read cap
    // is uneditable (orchestrator.rs at 129 KiB could never be written).
    let max_bytes = policy.max_write_bytes();
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
            match (previous.as_ref(), expected_previous.as_deref()) {
                (Some(existing), Some(expected)) if existing.content == expected => {}
                (Some(_), Some(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "write conflict: existing file no longer matches expected_previous_content; file was not modified",
                    ));
                }
                (Some(_), None) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "write conflict: replacing an existing file requires expected_previous_content; read it first or use fs_edit_file",
                    ));
                }
                (None, Some(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "write conflict: expected an existing file but the target is absent; file was not created",
                    ));
                }
                (None, None) => {}
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
        description:
            "Replace exact UTF-8 text in an existing file at a read- and write-allowed path. \
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

/// Detect a `new_text` whose newlines arrived double-escaped, returning how
/// many literal `\n` sequences it carries.
///
/// A local model emitting `"...):\\n    if b == 0:\\n..."` decodes to genuine
/// backslash-`n` characters, which a faithful write turns into a single
/// corrupted line. Observed live: `divide()` was "successfully" rewritten into
/// one unparseable line, the checkpoint recorded it as a normal edit, and a
/// subagent then read the wreckage back and confirmed the fix. Silent
/// corruption that survives its own verification is far worse than a burnt
/// tool round, so this refuses the write and tells the model how to retry.
///
/// Deliberately narrow, because a literal `\n` is legitimate in source code
/// (`print("a\nb")`). All three conditions must hold:
/// - the region being replaced spans real lines, so the replacement should too;
/// - the replacement has NO real newline at all; and
/// - it carries at least two literal `\n`, which collapses the plausible case
///   of swapping a block for one line that happens to contain one escape.
///
/// A false positive costs one instructive rejection with the file intact; a
/// false negative costs a broken file that reads as a success.
fn double_escaped_newlines(old_text: &str, new_text: &str) -> Option<usize> {
    if !old_text.contains('\n') || new_text.contains('\n') {
        return None;
    }
    let escapes = new_text.matches("\\n").count();
    (escapes >= 2).then_some(escapes)
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
    if let Some(escapes) = double_escaped_newlines(&a.old_text, &a.new_text) {
        return Err(ToolError::InvalidArgs(format!(
            "new_text replaces a multi-line region but contains no real newline and {escapes} \
             literal \\n sequences — your newlines arrived double-escaped and would be written \
             verbatim, corrupting the file. Resend new_text with real line breaks; file was not \
             modified."
        )));
    }
    // Reject an obviously oversized replacement before doing any file IO. The
    // complete post-edit content is checked again below.
    policy.check_write_size(a.new_text.len())?;
    let requested = Path::new(&a.path);
    let authorized_read_target = policy.check_read(requested).await?;
    let target = policy.check_write(requested).await?;
    if authorized_read_target != target {
        return Err(ToolError::PolicyDenied(format!(
            "edit denied: target '{}' changed while read and write authority were checked",
            requested.display()
        )));
    }
    let display = target.display().to_string();
    let read_target = target.clone();
    // fs_edit_file reads the whole existing file to apply the replacement and
    // snapshot the pre-image; both are part of a write, so bound by the write
    // cap (1 MiB), not the 64 KiB read cap — a large source file must remain
    // editable even though a bare read of it would be capped.
    let max_bytes = policy.max_write_bytes();
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

    /// A real 3-row, 3-column spreadsheet (id,name,score) generated once with
    /// LibreOffice, embedded so the xlsx tests need no writer dependency.
    const SAMPLE_XLSX: &[u8] = include_bytes!("testdata/sample.xlsx");
    /// A real 120x80 8-bit RGB PNG, generated once and embedded.
    const SAMPLE_PNG: &[u8] = include_bytes!("testdata/sample.png");

    #[test]
    fn image_metadata_reports_format_and_dimensions() {
        let out = render_image_metadata("pic.png", SAMPLE_PNG);
        assert!(out.contains("Png"), "format: {out}");
        assert!(out.contains("120x80 px"), "dimensions: {out}");
        assert!(out.contains("metadata only"), "disclosure: {out}");
        // A non-image header does not panic and discloses it can't read dims.
        let out = render_image_metadata("x.png", b"this is not an image");
        assert!(
            out.contains("could not read dimensions") || out.contains("unrecognized"),
            "non-image: {out}"
        );
    }

    #[tokio::test]
    async fn read_image_end_to_end_and_via_dispatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("photo.png");
        std::fs::write(&path, SAMPLE_PNG).unwrap();

        // Direct fs_read_image.
        let out = read_image_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("image ok");
        assert!(out.contains("Png") && out.contains("120x80 px"), "{out}");

        // read_document routes an image to fs_read_image (real metadata now).
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("dispatch ok");
        assert!(out.contains("image → fs_read_image"), "route label: {out}");
        assert!(out.contains("120x80 px"), "dispatch metadata: {out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.png");
        std::fs::write(&bad, SAMPLE_PNG).unwrap();
        assert!(matches!(
            read_image_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    /// Build a minimal real `.docx` (a zip carrying `word/document.xml`).
    fn write_min_docx(path: &std::path::Path, document_xml: &str) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    /// Build a minimal real `.pptx` (a zip carrying `ppt/slides/slideN.xml`).
    fn write_min_pptx(path: &std::path::Path, slides: &[(u32, &str)]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (n, sp_tree_body) in slides {
            zip.start_file(
                format!("ppt/slides/slide{n}.xml"),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            let xml = format!(
                "<?xml version=\"1.0\"?><p:sld xmlns:a=\"x\"><p:cSld><p:spTree>{sp_tree_body}\
                 </p:spTree></p:cSld></p:sld>"
            );
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn pptx_paragraph_extraction_and_entities() {
        let xml = "<p:sld xmlns:a=\"x\">\
            <a:p><a:r><a:t>Hello</a:t></a:r></a:p>\
            <a:p><a:r><a:t>World &amp; co</a:t></a:r></a:p>\
            <a:p></a:p></p:sld>";
        let paras = parse_pptx_paragraphs(xml);
        let with_text: Vec<&String> = paras.iter().filter(|p| !p.trim().is_empty()).collect();
        assert_eq!(with_text.len(), 2, "{paras:?}");
        assert_eq!(with_text[0], "Hello");
        assert_eq!(with_text[1], "World & co"); // entity decoded
    }

    #[tokio::test]
    async fn read_pptx_end_to_end_ordering_and_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("deck.pptx");
        // Slides added out of lexical order to exercise the numeric sort
        // (lexically "slide10" precedes "slide2").
        write_min_pptx(
            &path,
            &[
                (2, "<a:p><a:r><a:t>Second slide</a:t></a:r></a:p>"),
                (10, "<a:p><a:r><a:t>Tenth slide</a:t></a:r></a:p>"),
                (1, "<a:p><a:r><a:t>First slide</a:t></a:r></a:p>"),
            ],
        );
        let out = read_pptx_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("pptx ok");
        assert!(out.contains("3 slide(s)"), "{out}");
        let (i1, i2, i10) = (
            out.find("--- slide 1 ---").expect("slide 1"),
            out.find("--- slide 2 ---").expect("slide 2"),
            out.find("--- slide 10 ---").expect("slide 10"),
        );
        assert!(i1 < i2 && i2 < i10, "numeric slide order: {out}");
        assert!(
            out.contains("First slide") && out.contains("Tenth slide"),
            "{out}"
        );

        // read_document routes a pptx to fs_read_pptx.
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("dispatch ok");
        assert!(out.contains("PPTX → fs_read_pptx"), "{out}");
        assert!(out.contains("First slide"), "{out}");

        // A zip WITHOUT ppt/slides/slideN.xml → honest error, not garbage.
        let notpptx = dir.path().join("weird.pptx");
        {
            let f = std::fs::File::create(&notpptx).unwrap();
            let mut z = zip::ZipWriter::new(f);
            z.start_file("other.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"not a slide deck").unwrap();
            z.finish().unwrap();
        }
        assert!(matches!(
            read_pptx_run(
                &policy,
                &serde_json::json!({"path": notpptx.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::ExecError(_))
        ));

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.pptx");
        write_min_pptx(&bad, &[(1, "<a:p><a:r><a:t>x</a:t></a:r></a:p>")]);
        assert!(matches!(
            read_pptx_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[test]
    fn docx_paragraph_extraction_and_entities() {
        let xml = "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>\
            <w:p><w:r><w:t>Hello world</w:t></w:r></w:p>\
            <w:p><w:r><w:t xml:space=\"preserve\">Second </w:t><w:t>paragraph &amp; more</w:t></w:r></w:p>\
            <w:p></w:p>\
            <w:p><w:r><w:t>Tab</w:t><w:tab/><w:t>after</w:t></w:r></w:p>\
            </w:body></w:document>";
        let out = render_docx("memo.docx", xml, 500, false);
        // Empty <w:p> skipped → 3 with text.
        assert!(out.contains("3 paragraph(s) with text"), "{out}");
        assert!(out.contains("Hello world"), "{out}");
        // Adjacent runs joined; &amp; decoded.
        assert!(out.contains("Second paragraph & more"), "{out}");
        // <w:tab/> becomes a tab.
        assert!(out.contains("Tab\tafter"), "{out}");
    }

    #[test]
    fn docx_empty_and_malformed_are_honest() {
        let out = render_docx(
            "x.docx",
            "<w:document><w:body></w:body></w:document>",
            500,
            false,
        );
        assert!(out.contains("no extractable text"), "{out}");
        // Unclosed tags must not panic; whatever text was gathered is surfaced.
        let out = render_docx("x.docx", "<w:p><w:t>partial", 500, false);
        assert!(
            out.contains("partial") || out.contains("no extractable text"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn read_docx_end_to_end_and_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("report.docx");
        write_min_docx(
            &path,
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>\
             <w:p><w:r><w:t>First para</w:t></w:r></w:p>\
             <w:p><w:r><w:t>Second para</w:t></w:r></w:p></w:body></w:document>",
        );
        let out = read_docx_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("docx ok");
        assert!(out.contains("2 paragraph(s) with text"), "{out}");
        assert!(
            out.contains("First para") && out.contains("Second para"),
            "{out}"
        );

        // A zip WITHOUT word/document.xml → honest error, not a panic or garbage.
        let notdocx = dir.path().join("weird.docx");
        {
            let f = std::fs::File::create(&notdocx).unwrap();
            let mut z = zip::ZipWriter::new(f);
            z.start_file("other.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"not a word doc").unwrap();
            z.finish().unwrap();
        }
        assert!(matches!(
            read_docx_run(
                &policy,
                &serde_json::json!({"path": notdocx.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::ExecError(_))
        ));

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.docx");
        write_min_docx(
            &bad,
            "<w:document><w:body><w:p><w:r><w:t>x</w:t></w:r></w:p></w:body></w:document>",
        );
        assert!(matches!(
            read_docx_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    const SAMPLE_AUDIT_JSON: &str = r#"{
      "vulnerabilities": {
        "found": true,
        "count": 1,
        "list": [
          {
            "advisory": {
              "id": "RUSTSEC-2021-0001",
              "title": "flate2 miscompilation",
              "aliases": ["CVE-2021-0001"]
            },
            "versions": { "patched": [">=1.0.14"], "unaffected": [] },
            "package": { "name": "flate2", "version": "1.0.13" }
          }
        ]
      },
      "warnings": {
        "unmaintained": [
          {
            "kind": "unmaintained",
            "package": { "name": "ansi_term", "version": "0.12.1" },
            "advisory": { "id": "RUSTSEC-2021-0139", "title": "ansi_term is unmaintained" }
          }
        ]
      }
    }"#;

    #[test]
    fn scan_secrets_detects_and_redacts_known_forms() {
        // Known token prefix with a long body.
        let line = "let key = \"sk-abcdefgh12345678\";";
        assert!(
            detect_secret_kinds(line).contains(&"token-prefix"),
            "{line}"
        );
        let red = redact_secrets_line(line);
        assert!(
            red.contains("<redacted>") && !red.contains("abcdefgh12345678"),
            "{red}"
        );
        // Bearer token.
        let line = "Authorization: Bearer abcdef.ghijkl.mnopqr";
        assert!(detect_secret_kinds(line).contains(&"bearer-token"));
        assert!(!redact_secrets_line(line).contains("abcdef.ghijkl.mnopqr"));
        // Secret assignment.
        let line = "password = hunter2hunter2";
        assert!(detect_secret_kinds(line).contains(&"secret-assignment"));
        assert!(!redact_secrets_line(line).contains("hunter2hunter2"));
        // URL user:pass@host.
        let line = "clone https://user:s3cr3tpass@example.com/repo.git";
        assert!(detect_secret_kinds(line).contains(&"url-credentials"));
        assert!(!redact_secrets_line(line).contains("s3cr3tpass"));
        // Benign: a short hf_ identifier and ordinary code are NOT flagged.
        assert!(detect_secret_kinds("let hf_model = load();").is_empty());
        assert!(detect_secret_kinds("let total = count + 1;").is_empty());
    }

    #[tokio::test]
    async fn scan_secrets_end_to_end_and_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("changes.diff");
        std::fs::write(
            &path,
            "+const A = 1;\n+let token = \"ghp_0123456789abcdefghij\";\n+normal line\n",
        )
        .unwrap();
        let out = scan_secrets_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("scan ok");
        assert!(out.contains("1 line(s) with apparent credentials"), "{out}");
        assert!(out.contains("2: ") && out.contains("token-prefix"), "{out}");
        assert!(
            out.contains("<redacted>") && !out.contains("0123456789abcdefghij"),
            "secret leaked: {out}"
        );

        // A clean file → honest "no apparent credentials".
        let clean = dir.path().join("clean.txt");
        std::fs::write(&clean, "just some code\nno secrets here\n").unwrap();
        let out = scan_secrets_run(
            &policy,
            &serde_json::json!({"path": clean.to_string_lossy()}).to_string(),
        )
        .await
        .expect("scan ok");
        assert!(out.contains("no apparent credentials found"), "{out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.diff");
        std::fs::write(&bad, "token = ghp_0123456789abcdefghij\n").unwrap();
        assert!(matches!(
            scan_secrets_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[test]
    fn audit_summary_ranked_view() {
        let out = render_audit_summary("audit.json", SAMPLE_AUDIT_JSON, 100);
        assert!(out.contains("1 vulnerability, 1 warning(s)"), "{out}");
        assert!(
            out.contains("[RUSTSEC-2021-0001] flate2 1.0.13 — flate2 miscompilation"),
            "{out}"
        );
        assert!(out.contains("fix: upgrade to >=1.0.14"), "{out}");
        assert!(out.contains("aka: CVE-2021-0001"), "{out}");
        assert!(
            out.contains("[unmaintained] ansi_term 0.12.1 — ansi_term is unmaintained"),
            "{out}"
        );
    }

    #[test]
    fn audit_summary_clean_no_patch_and_malformed() {
        // Clean report.
        let clean = r#"{"vulnerabilities":{"found":false,"count":0,"list":[]},"warnings":{}}"#;
        let out = render_audit_summary("a.json", clean, 100);
        assert!(out.contains("0 vulnerabilities, 0 warning(s)"), "{out}");
        assert!(out.contains("no known vulnerabilities"), "{out}");
        // A vuln with no patched release is disclosed honestly.
        let no_patch = r#"{"vulnerabilities":{"count":1,"list":[{"advisory":{"id":"RUSTSEC-2020-0002","title":"unfixed"},"versions":{"patched":[]},"package":{"name":"foo","version":"0.1.0"}}]},"warnings":{}}"#;
        let out = render_audit_summary("a.json", no_patch, 100);
        assert!(out.contains("fix: no patched release available"), "{out}");
        // Malformed JSON → disclosed, not guessed or panicked on.
        let out = render_audit_summary("a.json", "not json {{{", 100);
        assert!(out.contains("could not parse"), "{out}");
    }

    #[tokio::test]
    async fn audit_summary_end_to_end_and_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("audit.json");
        std::fs::write(&path, SAMPLE_AUDIT_JSON).unwrap();

        let out = audit_summary_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("audit ok");
        assert!(out.contains("1 vulnerability, 1 warning(s)"), "{out}");
        assert!(out.contains("[RUSTSEC-2021-0001]"), "{out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("audit.json");
        std::fs::write(&bad, SAMPLE_AUDIT_JSON).unwrap();
        assert!(matches!(
            audit_summary_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    const SAMPLE_NOTEBOOK: &str = r##"{
      "cells": [
        {"cell_type": "markdown", "source": ["# Title\n", "Some **text**\n"]},
        {"cell_type": "code", "execution_count": 1,
         "source": ["import numpy as np\n", "np.arange(3)\n"],
         "outputs": [
           {"output_type": "stream", "name": "stdout", "text": ["loading\n", "done\n"]},
           {"output_type": "execute_result", "data": {"text/plain": ["array([0, 1, 2])"]}},
           {"output_type": "error", "ename": "ValueError", "evalue": "bad input", "traceback": []}
         ]}
      ],
      "metadata": {"kernelspec": {"language": "python", "name": "python3"}},
      "nbformat": 4, "nbformat_minor": 5
    }"##;

    #[test]
    fn notebook_renders_cells_types_and_output_summaries() {
        let out = render_notebook("nb.ipynb", SAMPLE_NOTEBOOK, 100);
        assert!(out.contains("2 cell(s) (1 code, 1 markdown)"), "{out}");
        assert!(out.contains("[language: python]"), "{out}");
        assert!(out.contains("[1] markdown:"), "{out}");
        assert!(out.contains("# Title"), "{out}");
        assert!(out.contains("[2] code (exec 1):"), "{out}");
        assert!(out.contains("import numpy as np"), "{out}");
        // Output summaries — never the full payload.
        assert!(out.contains("→ stdout (2 lines)"), "{out}");
        assert!(out.contains("→ result: array([0, 1, 2])"), "{out}");
        assert!(out.contains("→ error: ValueError: bad input"), "{out}");
    }

    #[test]
    fn notebook_non_notebook_and_malformed_are_honest() {
        // Valid JSON but not a notebook (no cells array).
        let out = render_notebook("x.ipynb", r#"{"foo": 1}"#, 100);
        assert!(out.contains("not a Jupyter notebook"), "{out}");
        // Malformed JSON.
        let out = render_notebook("x.ipynb", "not json {{{", 100);
        assert!(out.contains("could not parse"), "{out}");
        // A cell whose `source` is a plain string (not an array) is handled.
        let one = r#"{"cells":[{"cell_type":"code","source":"print(1)\n"}]}"#;
        let out = render_notebook("x.ipynb", one, 100);
        assert!(out.contains("print(1)"), "{out}");
    }

    #[tokio::test]
    async fn read_notebook_end_to_end_dispatch_and_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("analysis.ipynb");
        std::fs::write(&path, SAMPLE_NOTEBOOK).unwrap();

        let out = read_notebook_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("nb ok");
        assert!(
            out.contains("2 cell(s)") && out.contains("import numpy"),
            "{out}"
        );

        // read_document routes an .ipynb to fs_read_notebook.
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("dispatch ok");
        assert!(out.contains("notebook → fs_read_notebook"), "{out}");
        assert!(out.contains("# Title"), "{out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.ipynb");
        std::fs::write(&bad, SAMPLE_NOTEBOOK).unwrap();
        assert!(matches!(
            read_notebook_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[test]
    fn html_to_text_strips_tags_scripts_and_decodes_entities() {
        let html = "<!DOCTYPE html><html><head><title>Doc</title>\
            <style>.c{color:red}</style></head><body>\
            <h1>Heading</h1><p>Hello &amp; <b>world</b> — 5 &lt; 6</p>\
            <script>var secret = a < b && c > d;</script>\
            <!-- a comment --><ul><li>one</li><li>two</li></ul>\
            <p>&#65; &#x42; &nbsp;end</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Heading"), "{text}");
        assert!(text.contains("Hello & world — 5 < 6"), "{text}");
        // <script>/<style> content removed wholesale (incl. the `a < b` that
        // would derail a naive tag parser).
        assert!(!text.contains("var secret"), "script leaked: {text}");
        assert!(!text.contains("color:red"), "style leaked: {text}");
        assert!(!text.contains("a comment"), "comment leaked: {text}");
        // Block tags become line breaks; list items land on their own lines.
        assert!(text.contains("one") && text.contains("two"), "{text}");
        // Entities: &#65;=A, &#x42;=B, &nbsp;=space.
        assert!(text.contains("A B end"), "entities: {text}");
    }

    #[tokio::test]
    async fn read_html_end_to_end_dispatch_and_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("page.html");
        std::fs::write(
            &path,
            "<html><body><h1>Report</h1><p>Body &amp; text</p>\
             <script>x<y</script></body></html>",
        )
        .unwrap();

        let out = read_html_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("html ok");
        assert!(
            out.contains("Report") && out.contains("Body & text"),
            "{out}"
        );
        assert!(!out.contains("x<y"), "script leaked: {out}");

        // read_document routes .html to fs_read_html.
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("dispatch ok");
        assert!(out.contains("HTML → fs_read_html"), "{out}");
        assert!(out.contains("Report"), "{out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.html");
        std::fs::write(&bad, "<p>hi</p>").unwrap();
        assert!(matches!(
            read_html_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[test]
    fn outline_lists_definitions_and_skips_noise() {
        let src = r#"// fn commented_out does nothing
use std::fmt;

pub struct Config {
    name: String,
}

const MAX: usize = 10;

impl Config {
    pub fn new(name: String) -> Self {
        let mut count = 0;
        Self { name }
    }
}

fn helper(x: u32) -> u32 { x + MAX as u32 }
"#;
        let out = render_outline("lib.rs", src, 500, false);
        assert!(out.contains("struct Config"), "{out}");
        assert!(out.contains("const MAX"), "{out}");
        assert!(out.contains("impl Config"), "{out}");
        assert!(out.contains("fn new"), "{out}");
        assert!(out.contains("fn helper"), "{out}");
        assert!(out.contains("5 definition(s)"), "{out}");
        // The commented `fn commented_out` is not a definition.
        assert!(!out.contains("commented_out"), "comment leaked: {out}");
        // A local `let mut count` binding is excluded.
        assert!(
            !out.contains("let mut count"),
            "local binding leaked: {out}"
        );
        // `use` is not a definition.
        assert!(!out.contains("use std::fmt"), "{out}");
    }

    #[test]
    fn outline_no_defs_is_honest() {
        let out = render_outline("x.txt", "just some prose\nno code here\n", 500, false);
        assert!(out.contains("(no top-level definitions detected)"), "{out}");
    }

    #[test]
    fn outline_rejects_keywords_in_strings_and_calls() {
        // A keyword mid-line (inside a string or a call argument) is NOT a
        // definition; only a keyword at the statement start (after modifiers) is.
        let src = "fn real_definition() {}\n\
                   describe(\"fn fake\", || {});\n\
                   let code = \"fn also_fake\";\n\
                   pub(crate) fn visible_helper() {}\n";
        let out = render_outline("t.rs", src, 500, false);
        assert!(out.contains("2 definition(s)"), "{out}");
        assert!(out.contains("fn real_definition"), "{out}");
        assert!(
            out.contains("fn visible_helper"),
            "pub(crate) missed: {out}"
        );
        assert!(!out.contains("fake"), "string/call keyword leaked: {out}");
    }

    #[tokio::test]
    async fn outline_file_end_to_end_and_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("mod.rs");
        std::fs::write(&path, "pub fn alpha() {}\nstruct Beta;\nfn gamma() {}\n").unwrap();

        let out = outline_file_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("outline ok");
        assert!(out.contains("3 definition(s)"), "{out}");
        assert!(
            out.contains("fn alpha") && out.contains("struct Beta") && out.contains("fn gamma"),
            "{out}"
        );
        assert!(out.contains("1: pub fn alpha"), "line numbers: {out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.rs");
        std::fs::write(&bad, "fn z() {}\n").unwrap();
        assert!(matches!(
            outline_file_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[test]
    fn manifest_cargo_summary() {
        let content = r#"
[package]
name = "kitsu-tools"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = "1"
serde = "1"

[dev-dependencies]
tempfile = "3"

[features]
extra = []
"#;
        let out = render_manifest("Cargo.toml", "Cargo.toml", content, 50);
        assert!(out.contains("Cargo (Rust) — kitsu-tools v0.1.0"), "{out}");
        assert!(out.contains("edition 2021"), "{out}");
        // Sorted + counted.
        assert!(out.contains("dependencies (2): serde, tokio"), "{out}");
        assert!(out.contains("dev-dependencies (1): tempfile"), "{out}");
        assert!(out.contains("features: 1"), "{out}");
    }

    #[test]
    fn manifest_package_json_summary() {
        let content = r#"{
  "name": "webapp",
  "version": "2.3.0",
  "dependencies": { "react": "^18", "zod": "^3" },
  "devDependencies": { "vitest": "^1" },
  "scripts": { "build": "vite build", "test": "vitest" }
}"#;
        let out = render_manifest("package.json", "package.json", content, 50);
        assert!(out.contains("package.json (Node) — webapp v2.3.0"), "{out}");
        assert!(out.contains("dependencies (2): react, zod"), "{out}");
        assert!(out.contains("devDependencies (1): vitest"), "{out}");
        assert!(out.contains("scripts (2): build, test"), "{out}");
    }

    #[test]
    fn manifest_pyproject_pep621_and_poetry() {
        let pep621 = r#"
[project]
name = "svc"
version = "0.4.1"
dependencies = ["requests>=2.0", "flask (==3.0)", "pydantic"]

[build-system]
build-backend = "hatchling.build"
"#;
        let out = render_manifest("pyproject.toml", "pyproject.toml", pep621, 50);
        assert!(
            out.contains("pyproject.toml (Python) — svc v0.4.1"),
            "{out}"
        );
        // Requirement names extracted from PEP 508 strings, sorted.
        assert!(
            out.contains("dependencies (3): flask, pydantic, requests"),
            "{out}"
        );
        assert!(out.contains("build-backend: hatchling.build"), "{out}");

        let poetry = r#"
[tool.poetry]
name = "svc2"
version = "1.0.0"

[tool.poetry.dependencies]
python = "^3.11"
httpx = "^0.27"
"#;
        let out = render_manifest("pyproject.toml", "pyproject.toml", poetry, 50);
        assert!(
            out.contains("pyproject.toml (Python) — svc2 v1.0.0"),
            "{out}"
        );
        // The `python` pin is not a dependency; httpx is.
        assert!(out.contains("dependencies (1): httpx"), "{out}");
    }

    #[test]
    fn manifest_requirement_name_and_bad_input() {
        assert_eq!(requirement_name("requests>=2.0"), "requests");
        assert_eq!(
            requirement_name("ruff (==0.1); python_version>'3.7'"),
            "ruff"
        );
        assert_eq!(requirement_name("  numpy  "), "numpy");
        // A malformed manifest is disclosed, not guessed or panicked on.
        let out = render_manifest("Cargo.toml", "Cargo.toml", "this is not = [valid toml", 50);
        assert!(out.contains("could not parse"), "{out}");
    }

    #[tokio::test]
    async fn read_manifest_end_to_end_and_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("Cargo.toml");
        std::fs::write(
            &path,
            "[package]\nname = \"demo\"\nversion = \"9.9.9\"\n\n[dependencies]\nanyhow = \"1\"\n",
        )
        .unwrap();

        let out = read_manifest_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("manifest ok");
        assert!(out.contains("demo v9.9.9"), "{out}");
        assert!(out.contains("dependencies (1): anyhow"), "{out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("Cargo.toml");
        std::fs::write(&bad, "[package]\nname = \"x\"\n").unwrap();
        assert!(matches!(
            read_manifest_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[test]
    fn symbol_whole_word_matching() {
        assert!(line_mentions_symbol("let foo = 1;", "foo"));
        assert!(line_mentions_symbol("a.foo()", "foo"));
        assert!(line_mentions_symbol("foo", "foo"));
        // Fragments of a larger identifier are NOT whole-word matches.
        assert!(!line_mentions_symbol("foobar", "foo"));
        assert!(!line_mentions_symbol("barfoo", "foo"));
        assert!(!line_mentions_symbol("foo_bar", "foo"));
        assert!(!line_mentions_symbol("nothing here", "foo"));
    }

    #[test]
    fn symbol_definition_classification() {
        // Definition forms across languages.
        assert!(line_defines_symbol("fn parse(input: &str) {", "parse"));
        assert!(line_defines_symbol("    pub struct Config {", "Config"));
        assert!(line_defines_symbol("def parse(self):", "parse"));
        assert!(line_defines_symbol("class Config:", "Config"));
        assert!(line_defines_symbol("let count = 0;", "count"));
        assert!(line_defines_symbol("let mut total = 0;", "total"));
        assert!(line_defines_symbol("type Alias = u32;", "Alias"));
        // Uses are NOT definitions.
        assert!(!line_defines_symbol("self.parse();", "parse"));
        assert!(!line_defines_symbol("use crate::parse;", "parse"));
        assert!(!line_defines_symbol("return parse(x)", "parse"));
        assert!(!line_defines_symbol("let n = count + 1;", "count"));
    }

    #[test]
    fn symbol_render_reports_not_found_cleanly() {
        let hits = SymbolHits::default();
        let out = render_symbol_hits("src", "missing", &hits);
        assert!(out.contains("0 definition(s), 0 reference(s)"), "{out}");
        assert!(out.contains("no definitions or usages"), "{out}");
        assert!(out.contains("heuristic text scan"), "{out}");
    }

    #[tokio::test]
    async fn find_symbol_locates_definition_and_references_across_a_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Arc::new(Permissions::for_roots(&[root], &[]).expect("perms"));

        // A small "repo": a definition in one file, two uses in a nested file.
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn widget() -> u32 { 42 }\nfn other() {}\n",
        )
        .unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(
            sub.join("main.rs"),
            "fn main() {\n    let n = widget();\n    println!(\"{}\", widget());\n}\n",
        )
        .unwrap();
        // A binary file must be skipped honestly, not garbled.
        std::fs::write(dir.path().join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        // A build dir must not be descended (else its generated def is counted).
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("gen.rs"), "fn widget() { /* generated */ }\n").unwrap();

        let out = find_symbol_run(
            policy.clone(),
            &serde_json::json!({"symbol": "widget", "path": dir.path().to_string_lossy()})
                .to_string(),
        )
        .await
        .expect("find ok");

        assert!(out.contains("definitions:"), "{out}");
        assert!(out.contains("lib.rs:1"), "def site: {out}");
        assert!(out.contains("references:"), "{out}");
        assert!(
            out.contains("1 definition(s), 2 reference(s)"),
            "counts: {out}"
        );
        // The build dir was skipped: the generated def is NOT counted.
        assert!(!out.contains("gen.rs"), "should skip build dirs: {out}");
        // The binary blob was skipped and disclosed.
        assert!(
            out.contains("binary/unreadable skipped"),
            "binary disclosure: {out}"
        );

        // Degrades cleanly for an absent symbol.
        let out = find_symbol_run(
            policy.clone(),
            &serde_json::json!({"symbol": "nonexistent", "path": dir.path().to_string_lossy()})
                .to_string(),
        )
        .await
        .expect("find ok");
        assert!(out.contains("no definitions or usages"), "{out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        assert!(matches!(
            find_symbol_run(
                policy,
                &serde_json::json!({"symbol": "widget", "path": other.path().to_string_lossy()})
                    .to_string(),
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn find_symbol_revalidates_a_file_replaced_after_the_path_check() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let private = dir.path().join("private");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&private).unwrap();
        let visible = allowed.join("visible.rs");
        let secret = private.join("session.rs");
        std::fs::write(&visible, "fn harmless() {}\n").unwrap();
        std::fs::write(&secret, "fn private_symbol() {}\n").unwrap();

        let root = allowed.to_string_lossy().into_owned();
        let policy = Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap();
        policy
            .protect_read_path(&private.to_string_lossy())
            .unwrap();

        let mut hits = SymbolHits::default();
        let mut budget = SymbolScanBudget::default();
        scan_one_file_with_hook(
            &policy,
            &allowed,
            &visible,
            "allowed",
            &mut hits,
            "private_symbol",
            SYMBOL_DEFAULT_MAX_REFS,
            &mut budget,
            || {
                std::fs::remove_file(&visible).unwrap();
                std::fs::hard_link(&secret, &visible).unwrap();
            },
        );

        assert_eq!(hits.total_defs, 0);
        assert_eq!(hits.total_refs, 0);
        assert_eq!(hits.files_scanned, 0);
        assert_eq!(hits.skipped_unreadable, 1);
    }

    #[test]
    fn config_diff_reports_added_removed_changed_on_sshd_shaped_files() {
        // sshd_config-shaped: `Key value`, # comments, case-insensitive keys.
        let reference = "\
# baseline sshd_config
Port 22
PermitRootLogin no
PasswordAuthentication yes
X11Forwarding no
";
        let current = "\
# hardened
Port 2222
permitrootlogin no
PasswordAuthentication no
UseDNS no
";
        let refc = parse_config(reference, true);
        let cur = parse_config(current, true);
        let out = render_config_diff(&cur, &refc, "current", "baseline");
        assert!(
            out.contains("1 added, 1 removed, 2 changed, 1 unchanged"),
            "summary: {out}"
        );
        // Port changed 22 → 2222.
        assert!(out.contains("~ Port: \"22\" → \"2222\""), "{out}");
        // PasswordAuthentication changed yes → no.
        assert!(out.contains("PasswordAuthentication"), "{out}");
        // UseDNS added in current; X11Forwarding removed from current.
        assert!(out.contains("+ UseDNS = \"no\""), "added: {out}");
        assert!(out.contains("- X11Forwarding = \"no\""), "removed: {out}");
        // permitrootlogin (case-differing) matched PermitRootLogin → unchanged
        // (both "no"), NOT reported as changed.
        assert!(
            !out.contains("PermitRootLogin: "),
            "case-insensitive match: {out}"
        );
    }

    #[test]
    fn config_diff_handles_equal_configs_and_multiple_delimiters() {
        // key=value and key: value styles both parse; identical → no drift.
        let a = "host=localhost\nport: 8080\ndebug true\n";
        let b = "port: 8080\nhost=localhost\ndebug true\n";
        let out = render_config_diff(&parse_config(a, false), &parse_config(b, false), "a", "b");
        assert!(
            out.contains("0 added, 0 removed, 0 changed, 3 unchanged"),
            "{out}"
        );
        assert!(out.contains("no drift"), "{out}");
    }

    #[tokio::test]
    async fn diff_config_end_to_end_and_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let cur = dir.path().join("app.conf");
        let refr = dir.path().join("app.conf.default");
        std::fs::write(&cur, "timeout=30\nretries=5\n").unwrap();
        std::fs::write(&refr, "timeout=10\nretries=5\n").unwrap();
        let out = diff_config_run(
            &policy,
            &serde_json::json!({"path": cur.to_string_lossy(), "reference": refr.to_string_lossy()})
                .to_string(),
        )
        .await
        .expect("diff ok");
        assert!(
            out.contains("0 added, 0 removed, 1 changed, 1 unchanged"),
            "{out}"
        );
        assert!(out.contains("~ timeout: \"10\" → \"30\""), "{out}");

        // Either path outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.conf");
        std::fs::write(&bad, "a=b\n").unwrap();
        assert!(matches!(
            diff_config_run(
                &policy,
                &serde_json::json!({"path": cur.to_string_lossy(), "reference": bad.to_string_lossy()})
                    .to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn scan_file_grep_returns_matches_with_served_of_total_disclosure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app.log");
        // 1000 lines; every 100th is an ERROR (10 total matches).
        let mut body = String::new();
        for i in 1..=1000 {
            if i % 100 == 0 {
                body.push_str(&format!("line {i}: ERROR disk full\n"));
            } else {
                body.push_str(&format!("line {i}: info ok\n"));
            }
        }
        std::fs::write(&path, &body).unwrap();
        let policy = Permissions::for_roots(&[dir.path().to_string_lossy().into_owned()], &[])
            .expect("perms");

        // Grep with a cap below the match count → served-of-total disclosure.
        let out = scan_file_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy(), "pattern": "ERROR", "max_matches": 3})
                .to_string(),
        )
        .await
        .expect("grep ok");
        assert!(out.contains("10 match(es) in 1000 lines scanned"), "{out}");
        assert!(
            out.contains("100: line 100: ERROR"),
            "line-numbered match: {out}"
        );
        assert!(
            out.contains("[showing 3 of 10 matches"),
            "cap disclosure: {out}"
        );

        // Case-insensitive matches lowercase too.
        std::fs::write(&path, "Warn: A\nwarn: b\nWARN: c\ninfo\n").unwrap();
        let out = scan_file_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy(), "pattern": "warn", "ignore_case": true})
                .to_string(),
        )
        .await
        .expect("ci ok");
        assert!(out.contains("3 match(es)"), "case-insensitive: {out}");

        // No match discloses zero, not an error.
        let out = scan_file_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy(), "pattern": "zzz"}).to_string(),
        )
        .await
        .expect("nomatch ok");
        assert!(
            out.contains("0 match(es)") && out.contains("no matching lines"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn scan_file_tail_returns_the_last_lines_via_seek() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.log");
        let mut body = String::new();
        for i in 1..=5000 {
            body.push_str(&format!("row {i}\n"));
        }
        std::fs::write(&path, &body).unwrap();
        let policy = Permissions::for_roots(&[dir.path().to_string_lossy().into_owned()], &[])
            .expect("perms");

        // Default tail (no pattern) returns the LAST lines, not the first.
        let out = scan_file_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy(), "tail_lines": 3}).to_string(),
        )
        .await
        .expect("tail ok");
        assert!(out.contains("tail — last 3 line(s)"), "{out}");
        assert!(out.contains("row 5000"), "last line present: {out}");
        assert!(out.contains("row 4998"), "{out}");
        assert!(!out.contains("row 1\n"), "must not include the head: {out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.log");
        std::fs::write(&bad, "secret\n").unwrap();
        assert!(matches!(
            scan_file_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn read_xlsx_lists_sheets_and_returns_a_bounded_cell_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("book.xlsx");
        std::fs::write(&path, SAMPLE_XLSX).unwrap();
        let policy = Permissions::for_roots(&[dir.path().to_string_lossy().into_owned()], &[])
            .expect("perms");

        // Default: first sheet, header + 3 data rows, 3 columns.
        let out = read_xlsx_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy()}).to_string(),
        )
        .await
        .expect("xlsx ok");
        assert!(out.contains("sheet(s):"), "sheet list: {out}");
        assert!(out.contains("cols"), "dims: {out}");
        assert!(
            out.contains("Ada") && out.contains("Grace") && out.contains("Alan"),
            "{out}"
        );
        assert!(out.contains("95"), "cell value: {out}");

        // Sheet selection by 0-based index string resolves to the first sheet.
        let out = read_xlsx_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy(), "sheet": "0"}).to_string(),
        )
        .await
        .expect("xlsx idx ok");
        assert!(out.contains("Ada"), "{out}");

        // A non-existent sheet is disclosed, not a panic.
        let out = read_xlsx_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy(), "sheet": "Nope"}).to_string(),
        )
        .await
        .expect("xlsx bad-sheet ok");
        assert!(out.contains("not found"), "{out}");

        // Paging: start_row past the end discloses an empty window.
        let out = read_xlsx_run(
            &policy,
            &serde_json::json!({"path": path.to_string_lossy(), "start_row": 999}).to_string(),
        )
        .await
        .expect("xlsx page ok");
        assert!(out.contains("no rows in window"), "{out}");

        // Outside the allowlist → PolicyDenied.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.xlsx");
        std::fs::write(&bad, SAMPLE_XLSX).unwrap();
        assert!(matches!(
            read_xlsx_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));

        // A non-spreadsheet surfaces a parse error, not a panic.
        let notxlsx = dir.path().join("plain.xlsx");
        std::fs::write(&notxlsx, b"not a spreadsheet").unwrap();
        assert!(matches!(
            read_xlsx_run(
                &policy,
                &serde_json::json!({"path": notxlsx.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::ExecError(_))
        ));
    }

    #[test]
    fn csv_reports_shape_types_and_bounded_window() {
        let mut body = String::from("id,name,price,active\n");
        for i in 1..=1000 {
            body.push_str(&format!("{i},Item{i},{}.5,true\n", i % 7));
        }
        let out = render_csv(&body, b',', false, 1, 20, false);
        assert!(out.contains("4 columns, 1000 data rows"), "shape: {out}");
        assert!(
            out.contains("id:int, name:string, price:float, active:bool"),
            "types: {out}"
        );
        assert!(out.contains("rows 1-20 of 1000:"), "window header: {out}");
        assert!(out.contains("[showing 20 of 1000 rows"), "more hint: {out}");
        // Exactly 20 data rows rendered in the window.
        let data_lines = out
            .lines()
            .filter(|l| l.starts_with("1 | ") || l.contains(" | id="))
            .count();
        assert!(data_lines >= 1);
        assert_eq!(out.lines().filter(|l| l.contains(" | id=")).count(), 20);
    }

    #[test]
    fn csv_paging_with_start_row() {
        let body = "a,b\n1,2\n3,4\n5,6\n7,8\n";
        let out = render_csv(body, b',', false, 3, 2, false);
        assert!(out.contains("rows 3-4 of 4:"), "{out}");
        assert!(out.contains("3 | a=5 | b=6"), "{out}");
        assert!(out.contains("4 | a=7 | b=8"), "{out}");
        assert!(
            !out.contains("a=1"),
            "row 1 should be outside the window: {out}"
        );
    }

    #[test]
    fn csv_discloses_malformed_rows_not_dropped() {
        let body = "a,b,c\n1,2,3\n4,5\n6,7,8,9\n10,11,12\n";
        let out = render_csv(body, b',', false, 1, 50, false);
        // 4 data rows counted (none silently dropped).
        assert!(out.contains("3 columns, 4 data rows"), "{out}");
        assert!(
            out.contains("2 row(s) with an unexpected field count"),
            "disclosure missing: {out}"
        );
        assert!(out.contains("row 2 (2 fields, expected 3)"), "{out}");
        assert!(out.contains("row 3 (4 fields, expected 3)"), "{out}");
    }

    #[test]
    fn csv_tsv_and_no_header_and_nullable_types() {
        // TSV, no header: columns get positional names; empty cells keep the
        // column's inferred type (nullable int column stays int).
        let body = "1\tx\n\ty\n3\tz\n";
        let out = render_csv(body, b'\t', true, 1, 50, false);
        assert!(out.contains("TSV: 2 columns, 3 data rows"), "{out}");
        assert!(out.contains("col1:int, col2:string"), "nullable int: {out}");
        assert!(out.contains("1 | col1=1 | col2=x"), "{out}");
    }

    /// Build an in-memory PDF: each entry is `Some(text)` for a text page or
    /// `None` for an image-only page (no text operators).
    fn build_pdf(pages_text: &[Option<&str>]) -> Vec<u8> {
        use lopdf::content::{Content, Operation};
        use lopdf::{Dictionary, Document, Object, Stream};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object({
            let mut d = Dictionary::new();
            d.set("Type", "Font");
            d.set("Subtype", "Type1");
            d.set("BaseFont", "Courier");
            d
        });
        let resources_id = doc.add_object({
            let mut fonts = Dictionary::new();
            fonts.set("F1", font_id);
            let mut res = Dictionary::new();
            res.set("Font", fonts);
            res
        });
        let mut kids: Vec<Object> = Vec::new();
        for text in pages_text {
            let ops = match text {
                Some(t) => vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 24.into()]),
                    Operation::new("Td", vec![72.into(), 720.into()]),
                    Operation::new("Tj", vec![Object::string_literal(*t)]),
                    Operation::new("ET", vec![]),
                ],
                None => vec![], // no text operators → image-only page
            };
            let content = Content { operations: ops };
            let content_id =
                doc.add_object(Stream::new(Dictionary::new(), content.encode().unwrap()));
            let mut page = Dictionary::new();
            page.set("Type", "Page");
            page.set("Parent", pages_id);
            page.set("Contents", content_id);
            kids.push(doc.add_object(page).into());
        }
        let count = kids.len() as i64;
        let mut pages = Dictionary::new();
        pages.set("Type", "Pages");
        pages.set("Kids", kids);
        pages.set("Count", count);
        pages.set("Resources", resources_id);
        pages.set("MediaBox", vec![0.into(), 0.into(), 595.into(), 842.into()]);
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let mut catalog = Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", pages_id);
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", catalog_id);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).expect("save pdf");
        buf
    }

    #[test]
    fn doc_type_detection_prefers_magic_then_extension() {
        use std::path::Path;
        // Magic wins for binary formats.
        assert_eq!(
            detect_doc_type(Path::new("x.dat"), b"%PDF-1.7"),
            DocType::Pdf
        );
        assert_eq!(
            detect_doc_type(Path::new("photo.bin"), &[0x89, b'P', b'N', b'G', 0, 0]),
            DocType::Image("PNG")
        );
        assert_eq!(
            detect_doc_type(Path::new("j"), &[0xFF, 0xD8, 0xFF, 0xE0]),
            DocType::Image("JPEG")
        );
        // Zip magic + extension distinguishes Office formats.
        assert_eq!(
            detect_doc_type(Path::new("book.xlsx"), &[b'P', b'K', 0x03, 0x04]),
            DocType::Office("XLSX")
        );
        assert_eq!(
            detect_doc_type(Path::new("memo.docx"), &[b'P', b'K', 0x03, 0x04]),
            DocType::Office("DOCX")
        );
        // Extension carries CSV/TSV (no magic number).
        assert_eq!(
            detect_doc_type(Path::new("data.csv"), b"a,b\n"),
            DocType::Csv
        );
        assert_eq!(
            detect_doc_type(Path::new("data.tsv"), b"a\tb\n"),
            DocType::Csv
        );
        // Unknown/plain → Text (UTF-8 fallback preserved).
        assert_eq!(
            detect_doc_type(Path::new("notes.md"), b"# hello"),
            DocType::Text
        );
        assert_eq!(
            detect_doc_type(Path::new("noext"), b"just words"),
            DocType::Text
        );
    }

    #[tokio::test]
    async fn read_document_routes_csv_pdf_text_and_discloses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");

        // CSV → structured table.
        let csv = dir.path().join("t.csv");
        std::fs::write(&csv, "id,name\n1,Ada\n2,Grace\n").unwrap();
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": csv.to_string_lossy()}).to_string(),
        )
        .await
        .expect("csv route");
        assert!(out.contains("read_document>"), "disclosure: {out}");
        assert!(out.contains("CSV/TSV → fs_read_csv"), "{out}");
        assert!(out.contains("2 data rows"), "delegated body: {out}");

        // Plain text → text read.
        let txt = dir.path().join("notes.md");
        std::fs::write(&txt, "line one\nline two\n").unwrap();
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": txt.to_string_lossy()}).to_string(),
        )
        .await
        .expect("text route");
        assert!(out.contains("text → fs_read_file"), "{out}");
        assert!(out.contains("line one"), "{out}");

        // PDF → text-layer extraction (build a real 1-page PDF).
        let pdf = dir.path().join("doc.pdf");
        std::fs::write(&pdf, build_pdf(&[Some("routed via read_document")])).unwrap();
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": pdf.to_string_lossy()}).to_string(),
        )
        .await
        .expect("pdf route");
        assert!(out.contains("PDF → fs_read_pdf"), "{out}");
        assert!(out.contains("routed via read_document"), "extracted: {out}");

        // PNG → routed to fs_read_image for real metadata (format + dims), no
        // garbage. Use a real PNG so dimensions come through.
        let png = dir.path().join("pic.png");
        std::fs::write(&png, SAMPLE_PNG).unwrap();
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": png.to_string_lossy()}).to_string(),
        )
        .await
        .expect("png route");
        assert!(out.contains("image → fs_read_image"), "{out}");
        assert!(out.contains("120x80 px"), "{out}");

        // xlsx (zip magic + extension) → routed to the real extractor.
        let xlsx = dir.path().join("book.xlsx");
        std::fs::write(&xlsx, SAMPLE_XLSX).unwrap();
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": xlsx.to_string_lossy()}).to_string(),
        )
        .await
        .expect("xlsx route");
        assert!(out.contains("XLSX → fs_read_xlsx"), "{out}");
        assert!(out.contains("Ada"), "extracted cell: {out}");

        // docx (a real minimal zip) → routed to fs_read_docx and text-extracted.
        let docx = dir.path().join("memo.docx");
        write_min_docx(
            &docx,
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>\
             <w:p><w:r><w:t>Routed body text</w:t></w:r></w:p></w:body></w:document>",
        );
        let out = read_document_run(
            &policy,
            &serde_json::json!({"path": docx.to_string_lossy()}).to_string(),
        )
        .await
        .expect("docx route");
        assert!(out.contains("DOCX → fs_read_docx"), "{out}");
        assert!(out.contains("Routed body text"), "{out}");

        // Outside the allowlist → PolicyDenied before any routing.
        let other = tempfile::tempdir().unwrap();
        let bad = other.path().join("x.csv");
        std::fs::write(&bad, "a,b\n1,2\n").unwrap();
        assert!(matches!(
            read_document_run(
                &policy,
                &serde_json::json!({"path": bad.to_string_lossy()}).to_string()
            )
            .await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[test]
    fn pdf_render_formats_pages_and_discloses_image_only() {
        let extracted = vec![
            (1u32, Ok("Hello from page one".to_string())),
            (2u32, Ok(String::new())), // image-only
            (3u32, Ok("Third page text".to_string())),
        ];
        let out = render_pdf(&extracted, 5, 1);
        assert!(out.contains("5 page(s), text-layer only (no OCR)"), "{out}");
        assert!(out.contains("pages 1-3 of 5:"), "{out}");
        assert!(out.contains("--- page 1 ---"), "{out}");
        assert!(out.contains("Hello from page one"), "{out}");
        assert!(
            out.contains("no extractable text on this page"),
            "image-only disclosure: {out}"
        );
        assert!(
            out.contains("1 page(s) in this window had no text layer"),
            "{out}"
        );
        assert!(out.contains("[showing pages 1-3 of 5"), "more hint: {out}");
    }

    #[test]
    fn pdf_render_empty_and_past_end() {
        assert!(render_pdf(&[], 0, 1).contains("may not be a valid PDF"));
        assert!(render_pdf(&[], 4, 99).contains("no pages in window"));
    }

    #[tokio::test]
    async fn read_pdf_end_to_end_extracts_text_and_flags_scanned_page() {
        // Page 1 has text, page 2 is image-only (no text operators).
        let bytes = build_pdf(&[Some("Kitsune runs on foxfire"), None]);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("doc.pdf");
        std::fs::write(&path, &bytes).expect("write pdf");
        let policy = Permissions::for_roots(&[dir.path().to_string_lossy().into_owned()], &[])
            .expect("perms");
        let args = serde_json::json!({"path": path.to_string_lossy()}).to_string();
        let out = read_pdf_run(&policy, &args).await.expect("pdf read ok");
        assert!(out.contains("2 page(s), text-layer only (no OCR)"), "{out}");
        assert!(
            out.contains("Kitsune runs on foxfire"),
            "extracted text: {out}"
        );
        assert!(
            out.contains("no extractable text on this page"),
            "scanned-page disclosure: {out}"
        );

        // Outside the allowlist → PolicyDenied, same guard as fs_read_file.
        let other = tempfile::tempdir().expect("other");
        let bad = other.path().join("x.pdf");
        std::fs::write(&bad, &bytes).expect("write");
        let bad_args = serde_json::json!({"path": bad.to_string_lossy()}).to_string();
        assert!(matches!(
            read_pdf_run(&policy, &bad_args).await,
            Err(ToolError::PolicyDenied(_))
        ));

        // A non-PDF file surfaces a parse error, not a panic or garbage.
        let notpdf = dir.path().join("plain.pdf");
        std::fs::write(&notpdf, b"this is not a pdf at all").expect("write");
        let np_args = serde_json::json!({"path": notpdf.to_string_lossy()}).to_string();
        assert!(matches!(
            read_pdf_run(&policy, &np_args).await,
            Err(ToolError::ExecError(_))
        ));
    }

    #[test]
    fn csv_no_header_accepts_stringy_booleans() {
        // Small models emit `"false"`/`"true"` as strings (live llama3.2
        // finding); all of these must parse rather than erroring the turn.
        for raw in [
            r#"{"path":"x","no_header":"false"}"#,
            r#"{"path":"x","no_header":false}"#,
            r#"{"path":"x","no_header":"true"}"#,
            r#"{"path":"x","no_header":1}"#,
            r#"{"path":"x"}"#,
        ] {
            let parsed: ReadCsvArgs = serde_json::from_str(raw).unwrap_or_else(|e| {
                panic!("should parse {raw}: {e}");
            });
            let _ = parsed.no_header;
        }
        let t: ReadCsvArgs = serde_json::from_str(r#"{"path":"x","no_header":"true"}"#).unwrap();
        assert!(t.no_header);
        let f: ReadCsvArgs = serde_json::from_str(r#"{"path":"x","no_header":"false"}"#).unwrap();
        assert!(!f.no_header);
    }

    #[test]
    fn csv_start_row_past_end_is_disclosed() {
        let body = "a,b\n1,2\n";
        let out = render_csv(body, b',', false, 99, 10, false);
        assert!(out.contains("no rows in window"), "{out}");
        assert!(
            out.contains("1 data rows") || out.contains("1 data row"),
            "{out}"
        );
    }

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

    #[test]
    fn private_artifact_writer_streams_and_retains_only_after_finish() {
        let dir = tempfile::tempdir().unwrap();
        let sequence = AtomicU64::new(0);
        let mut writer =
            create_unique_private_artifact_with_sequence(dir.path(), "command", &sequence).unwrap();
        let path = writer.path().to_path_buf();
        writer.write_all(b"stdout: first\n").unwrap();
        writer.write_all(b"stderr: second\n").unwrap();

        let retained = writer.finish().unwrap();
        assert_eq!(retained, path);
        assert_eq!(
            std::fs::read(&retained).unwrap(),
            b"stdout: first\nstderr: second\n"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(retained).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn unfinished_private_artifact_is_removed_on_drop_or_abort() {
        let dir = tempfile::tempdir().unwrap();
        let sequence = AtomicU64::new(0);
        let dropped_path = {
            let mut writer =
                create_unique_private_artifact_with_sequence(dir.path(), "drop", &sequence)
                    .unwrap();
            writer.write_all(b"partial").unwrap();
            let path = writer.path().to_path_buf();
            assert!(path.exists());
            path
        };
        assert!(!dropped_path.exists());

        let mut writer =
            create_unique_private_artifact_with_sequence(dir.path(), "abort", &sequence).unwrap();
        writer.write_all(b"partial").unwrap();
        let aborted_path = writer.path().to_path_buf();
        writer.abort().unwrap();
        assert!(!aborted_path.exists());
        assert_eq!(
            writer.write_all(b"late").unwrap_err().kind(),
            std::io::ErrorKind::Other
        );
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
    async fn read_csv_end_to_end_honors_allowlist_and_delimiter() {
        let (dir, root) = temp_with_file("data.tsv", "id\tname\n1\tAda\n2\tGrace\n");
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("data.tsv");
        let args =
            serde_json::json!({"path": path.to_string_lossy(), "delimiter": "\\t"}).to_string();
        let out = read_csv_run(&policy, &args).await.expect("csv ok");
        assert!(out.contains("TSV: 2 columns, 2 data rows"), "{out}");
        assert!(out.contains("id:int, name:string"), "{out}");
        assert!(out.contains("1 | id=1 | name=Ada"), "{out}");

        // Outside the allowlist → PolicyDenied (same guard as fs_read_file).
        let other = tempfile::tempdir().expect("other");
        let bad = other.path().join("x.csv");
        std::fs::write(&bad, "a,b\n1,2\n").expect("write");
        let bad_args = serde_json::json!({"path": bad.to_string_lossy()}).to_string();
        assert!(matches!(
            read_csv_run(&policy, &bad_args).await,
            Err(ToolError::PolicyDenied(_))
        ));

        // A multi-character delimiter is a clear InvalidArgs, not a panic.
        let two =
            serde_json::json!({"path": path.to_string_lossy(), "delimiter": "ab"}).to_string();
        assert!(matches!(
            read_csv_run(&policy, &two).await,
            Err(ToolError::InvalidArgs(_))
        ));

        // An empty delimiter degrades to the default comma (small-model
        // habit), so a comma file parses instead of erroring.
        let (dir2, root2) = temp_with_file("c.csv", "a,b\n1,2\n");
        let policy2 = Permissions::for_roots(&[root2], &[]).expect("perms");
        let empty = serde_json::json!({
            "path": dir2.path().join("c.csv").to_string_lossy(),
            "delimiter": ""
        })
        .to_string();
        let out = read_csv_run(&policy2, &empty)
            .await
            .expect("empty delim ok");
        assert!(out.contains("CSV: 2 columns, 1 data rows"), "{out}");
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
        // Ranged reads are line-number prefixed (R1.7) so citations are exact.
        assert_eq!(out, "2\tl2\n3\tl3");
    }

    #[tokio::test]
    async fn full_read_has_no_line_prefixes() {
        // A full read (no range) is verbatim — prefixes are ranged-only, so
        // the edit-context path is unchanged.
        let (dir, root) = temp_with_file("a.txt", "l1\nl2\nl3\n");
        let policy = Permissions::for_roots(&[root], &[]).expect("perms");
        let path = dir.path().join("a.txt");
        let args = serde_json::json!({"path": path.to_string_lossy()}).to_string();
        let out = read_file_run(&policy, &args).await.expect("read ok");
        assert_eq!(out, "l1\nl2\nl3\n");
        assert!(!out.contains('\t'), "no line-number prefix on a full read");
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

        // cap_output's generic marker is replaced by the served-of-total
        // disclosure appended after capping (cycle 31); the ~90-byte overflow
        // past the cap is deliberate — the disclosure is the point.
        assert!(
            output.contains("...[truncated: output exceeded byte cap"),
            "missing truncation marker: ...{}",
            &output[output.len().saturating_sub(160)..]
        );
        assert!(output.ends_with("re-read a line range for the rest]"));
        assert!(output.len() <= crate::permissions::DEFAULT_MAX_BYTES + 256);
    }

    /// The byte cap on a ranged read is range-relative: a range starting past
    /// the file's first 64 KiB serves the requested lines in full instead of
    /// the near-empty remainder of an absolute-prefix cap (kimi-k3 cycle-1).
    #[tokio::test]
    async fn read_file_range_past_the_byte_cap_serves_the_requested_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        // 4000 lines x ~30 bytes ≈ 120 KiB: line 3000 sits well past 64 KiB.
        let body: String = (1..=4000)
            .map(|n| format!("line-{n:05} {}\n", "x".repeat(18)))
            .collect();
        std::fs::write(&path, &body).unwrap();
        let policy =
            Permissions::for_roots(&[dir.path().to_string_lossy().into_owned()], &[]).unwrap();
        let args = serde_json::json!({
            "path": path,
            "start_line": 3000,
            "end_line": 3004
        })
        .to_string();

        let output = read_file_run(&policy, &args).await.unwrap();

        assert!(output.starts_with("3000\tline-03000"), "got: {output}");
        assert!(output.contains("3004\tline-03004"));
        assert_eq!(output.lines().count(), 5);
        assert!(
            !output.contains("truncated"),
            "range fits, no cap: {output}"
        );
    }

    /// A ranged read wider than the byte budget is capped relative to the
    /// range start and discloses the actually-served size.
    #[tokio::test]
    async fn read_file_wide_range_caps_relative_to_the_range_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let body: String = (1..=8000)
            .map(|n| format!("line-{n:05} {}\n", "x".repeat(18)))
            .collect();
        std::fs::write(&path, &body).unwrap();
        let policy =
            Permissions::for_roots(&[dir.path().to_string_lossy().into_owned()], &[]).unwrap();
        let args = serde_json::json!({"path": path, "start_line": 3000}).to_string();

        let output = read_file_run(&policy, &args).await.unwrap();

        // Serving starts at the requested line, fills the budget, and the
        // disclosure reports post-slice served bytes (~64 KiB), not the
        // pre-slice absolute read.
        assert!(output.starts_with("3000\tline-03000"), "got: {output}");
        assert!(output.contains("served 63 KiB") || output.contains("served 64 KiB"));
        assert!(output.ends_with("re-read a line range for the rest]"));
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

    #[cfg(unix)]
    #[tokio::test]
    async fn read_file_rejects_a_final_hard_link_swapped_after_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let private = dir.path().join("private");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::create_dir(&private).unwrap();
        let target = allowed.join("target.txt");
        let secret = private.join("session.json");
        std::fs::write(&target, "AUTHORIZED").unwrap();
        std::fs::write(&secret, "PRIVATE").unwrap();
        let policy =
            Permissions::for_roots(&[allowed.to_string_lossy().into_owned()], &[]).unwrap();
        policy
            .protect_read_path(&private.to_string_lossy())
            .unwrap();
        let args = serde_json::json!({"path": target}).to_string();

        let error = read_file_run_with_hook(&policy, &args, || {
            std::fs::remove_file(&target).unwrap();
            std::fs::hard_link(&secret, &target).unwrap();
            std::fs::remove_file(&secret).unwrap();
        })
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            ToolError::PolicyDenied(message) if message.contains("changed after authorization")
        ));
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
        let policy = Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());
        policy
            .protect_read_path(&private.to_string_lossy())
            .unwrap();
        policy
            .allow_internal_read_root(&artifacts.to_string_lossy())
            .unwrap();
        let artifact = artifacts.join("result.txt");
        std::fs::write(&artifact, "TRUSTED_RESULT").unwrap();

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
        #[cfg(unix)]
        {
            let grep = grep_run(
                policy.clone(),
                &serde_json::json!({"path": root, "pattern": "TRANSCRIPT_SECRET"}).to_string(),
            )
            .await
            .unwrap();
            assert_eq!(grep, "(no matches)");
            let artifact_grep = grep_run(
                policy.clone(),
                &serde_json::json!({"path": artifacts, "pattern": "TRUSTED_RESULT"}).to_string(),
            )
            .await
            .unwrap();
            assert!(artifact_grep.contains("result.txt:1:TRUSTED_RESULT"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn grep_escapes_protected_paths_with_glob_metacharacters() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("[private].toml");
        std::fs::write(&secret, "GLOB_META_SECRET").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "visible").unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());
        policy.protect_read_path(&secret.to_string_lossy()).unwrap();

        let output = grep_run(
            policy,
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
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());
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
            policy,
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
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let nested = allowed.join("nested");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(nested.join("source.txt"), "PINNED_AUTHORIZED").unwrap();
        std::fs::write(outside.join("source.txt"), "PINNED_OUTSIDE").unwrap();
        let policy = Arc::new(
            Permissions::for_roots(&[allowed.to_string_lossy().into_owned()], &[]).unwrap(),
        );
        let moved = allowed.join("moved-nested");
        let args = serde_json::json!({"path": nested, "pattern": "PINNED_"}).to_string();

        let output = grep_run_with_hook(policy, &args, || {
            std::fs::rename(&nested, &moved).unwrap();
            std::os::unix::fs::symlink(&outside, &nested).unwrap();
        })
        .await
        .unwrap();

        assert!(output.contains("PINNED_AUTHORIZED"), "{output}");
        assert!(!output.contains("PINNED_OUTSIDE"), "{output}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn grep_distinguishes_no_matches_from_process_errors() {
        let (dir, root) = temp_with_file("a.txt", "present\n");
        let policy = Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());

        let no_match = grep_run(
            policy.clone(),
            &serde_json::json!({"path": root, "pattern": "absent"}).to_string(),
        )
        .await
        .unwrap();
        assert_eq!(no_match, "(no matches)");

        let invalid = grep_run(
            policy,
            &serde_json::json!({"path": dir.path(), "pattern": "["}).to_string(),
        )
        .await
        .expect_err("invalid regex must be rejected, not reported as no matches");
        match invalid {
            ToolError::InvalidArgs(message) => {
                assert!(message.contains("invalid search pattern"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    /// A quoted scalar must not cost a tool round. Local models emit
    /// `"max_count": "100"` and `"literal": "true"` routinely; every other arg
    /// struct in this file already coerces them, and `repo_grep` is one of only
    /// three read tools a lean-tier registry offers, so a rejection here leaves
    /// a small model with no way to search.
    /// Regression for live corruption: qwen2.5-coder rewrote a Python function
    /// with double-escaped newlines, kitsu wrote them verbatim, the turn
    /// reported success, and a spawned subagent then "verified" the broken
    /// file. The edit must be refused with the file left intact.
    #[tokio::test]
    async fn edit_refuses_double_escaped_newlines_and_leaves_the_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let path = dir.path().join("calc.py");
        let original = "def divide(a, b):\n    return a / b\n";
        std::fs::write(&path, original).unwrap();
        let policy =
            Permissions::for_roots(std::slice::from_ref(&root), std::slice::from_ref(&root))
                .unwrap();

        let error = edit_file_run(
            &policy,
            &serde_json::json!({
                "path": path.to_string_lossy(),
                "old_text": "def divide(a, b):\n    return a / b",
                // Exactly what the model sent: literal backslash-n, no real breaks.
                "new_text": "def divide(a, b):\\n    if b == 0:\\n        raise ValueError(\"x\")\\n    return a / b"
            })
            .to_string(),
        )
        .await
        .expect_err("a double-escaped replacement must be refused");
        assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "the file must be untouched"
        );
    }

    /// The guard must not fire on legitimate content. A single-line replacement
    /// carrying escape sequences is normal source code, and any replacement
    /// that actually contains real newlines is fine no matter what it holds.
    #[tokio::test]
    async fn edit_allows_legitimate_escapes_and_real_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy =
            Permissions::for_roots(std::slice::from_ref(&root), std::slice::from_ref(&root))
                .unwrap();

        // (a) Multi-line -> multi-line that legitimately contains \n escapes.
        let path = dir.path().join("a.py");
        std::fs::write(&path, "def show():\n    pass\n").unwrap();
        edit_file_run(
            &policy,
            &serde_json::json!({
                "path": path.to_string_lossy(),
                "old_text": "def show():\n    pass",
                "new_text": "def show():\n    print(\"a\\nb\\nc\")"
            })
            .to_string(),
        )
        .await
        .expect("real newlines must be accepted even with escapes present");
        assert!(std::fs::read_to_string(&path).unwrap().contains("a\\nb"));

        // (b) Single-line -> single-line with escapes: old_text has no newline,
        // so the multi-line premise never applies.
        let path = dir.path().join("b.txt");
        std::fs::write(&path, "fmt=old\n").unwrap();
        edit_file_run(
            &policy,
            &serde_json::json!({
                "path": path.to_string_lossy(),
                "old_text": "fmt=old",
                "new_text": "fmt=%s\\n%s\\n"
            })
            .to_string(),
        )
        .await
        .expect("a single-line edit with escapes must be accepted");
    }

    #[tokio::test]
    async fn grep_accepts_quoted_scalars_from_small_models() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hay.txt");
        std::fs::write(&file, "alpha\nneedle\nomega\n").unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Arc::new(Permissions::for_roots(&[root], &[]).expect("perms"));

        let output = grep_run(
            policy.clone(),
            &serde_json::json!({
                "path": dir.path().to_string_lossy(),
                "pattern": "needle",
                "max_count": "5",
                "literal": "true"
            })
            .to_string(),
        )
        .await
        .expect("quoted scalars must parse");
        assert!(output.contains("needle"), "{output}");

        // Coercion is of representation only: a value that is not a number is
        // still an error, not a silent default.
        let error = grep_run(
            policy,
            &serde_json::json!({
                "path": dir.path().to_string_lossy(),
                "pattern": "needle",
                "max_count": "lots"
            })
            .to_string(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
    }

    #[tokio::test]
    async fn grep_rejects_unbounded_arguments_before_touching_the_filesystem() {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        for max_count in [0, GREP_MAX_MATCHES + 1] {
            let error = grep_run(
                policy.clone(),
                &serde_json::json!({
                    "path": "/does/not/exist",
                    "pattern": "x",
                    "max_count": max_count
                })
                .to_string(),
            )
            .await
            .unwrap_err();
            assert!(matches!(
                error,
                ToolError::InvalidArgs(message) if message.contains("max_count")
            ));
        }

        let error = grep_run(
            policy,
            &serde_json::json!({
                "path": "/does/not/exist",
                "pattern": "x".repeat(4 * 1024 + 1)
            })
            .to_string(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ToolError::InvalidArgs(message) if message.contains("4096-byte limit")
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_grep_is_deterministic_literal_and_glob_independent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(dir.path().join("z.txt"), "aaab\na+b\n").unwrap();
        std::fs::write(dir.path().join("a.txt"), "a+b\n").unwrap();
        std::fs::write(nested.join("m.txt"), "prefix a+b suffix\n").unwrap();
        std::fs::write(dir.path().join(".hidden.txt"), "a+b\n").unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());

        let output = grep_run(
            policy,
            &serde_json::json!({
                "path": root,
                "pattern": "a+b",
                "literal": true
            })
            .to_string(),
        )
        .await
        .unwrap();

        assert_eq!(
            output,
            "a.txt:1:a+b\nnested/m.txt:1:prefix a+b suffix\nz.txt:2:a+b"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_grep_skips_an_oversized_file_without_starving_later_sources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a-large.txt"),
            "x".repeat(GREP_MAX_FILE_BYTES + 1),
        )
        .unwrap();
        std::fs::write(dir.path().join("z-source.rs"), "NEEDLE\n").unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());

        let output = grep_run(
            policy,
            &serde_json::json!({"path": root, "pattern": "NEEDLE"}).to_string(),
        )
        .await
        .unwrap();

        assert_eq!(output, "z-source.rs:1:NEEDLE");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_grep_reports_when_an_explicit_file_is_too_large_to_search() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        std::fs::write(&path, "x".repeat(GREP_MAX_FILE_BYTES + 1)).unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Arc::new(Permissions::for_roots(&[root], &[]).unwrap());

        let output = grep_run(
            policy,
            &serde_json::json!({"path": path, "pattern": "x"}).to_string(),
        )
        .await
        .unwrap();

        assert_eq!(
            output,
            "(file not searched: file exceeds the 1 MiB per-file limit)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_grep_honors_cancellation_before_consuming_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.txt");
        std::fs::write(&path, "NEEDLE\n").unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Permissions::for_roots(&[root], &[]).unwrap();
        let grep_root = PinnedGrepRoot::open(&path).unwrap();
        let expression = Regex::new("NEEDLE").unwrap();
        let cancelled = AtomicBool::new(true);

        let error = native_grep(
            &policy,
            &expression,
            &path,
            grep_root,
            GREP_DEFAULT_MAX,
            policy.max_bytes(),
            &cancelled,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ToolError::ExecError(message) if message == "search cancelled"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_grep_applies_a_global_match_limit_with_a_typed_receipt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "MATCH one\nMATCH two\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "MATCH three\n").unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let policy = Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());

        let output = grep_run(
            policy,
            &serde_json::json!({"path": root, "pattern": "MATCH", "max_count": 1}).to_string(),
        )
        .await
        .unwrap();

        assert!(output.starts_with("a.txt:1:MATCH one\n"), "{output}");
        assert!(
            output.contains("search truncated: match limit reached"),
            "{output}"
        );
        assert!(output.contains("matches=1; files=1"), "{output}");
        assert!(!output.contains("MATCH two"), "{output}");
        assert!(!output.contains("MATCH three"), "{output}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_grep_skips_symlinks_and_protected_hard_link_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let private = allowed.join("private");
        let outside = dir.path().join("outside.txt");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(&outside, "OUTSIDE_SECRET\n").unwrap();
        std::os::unix::fs::symlink(&outside, allowed.join("outside-link.txt")).unwrap();
        let secret = private.join("session.json");
        std::fs::write(&secret, "PRIVATE_SECRET\n").unwrap();
        std::fs::write(allowed.join("visible.txt"), "VISIBLE\n").unwrap();
        let root = allowed.to_string_lossy().into_owned();
        let policy = Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());
        policy
            .protect_read_path(&private.to_string_lossy())
            .unwrap();
        // Create the alias after protection registration. Descriptor
        // validation must refresh the private identity set before reading it.
        let alias = allowed.join("innocent.txt");
        std::fs::hard_link(&secret, &alias).unwrap();

        let output = grep_run(
            policy,
            &serde_json::json!({"path": root, "pattern": "SECRET|VISIBLE"}).to_string(),
        )
        .await
        .unwrap();

        assert_eq!(output, "visible.txt:1:VISIBLE");
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
    async fn captured_command_reports_a_natural_exit_status() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("exit 7");

        let output = run_captured_command(&mut command, Duration::from_secs(1), 32)
            .await
            .unwrap();

        assert!(matches!(
            output.termination,
            ProcessTermination::Exited(status) if status.code() == Some(7)
        ));
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

    #[tokio::test]
    async fn write_tool_warning_is_quote_aware_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[], &[root]).expect("perms");

        // A source file that lists the markers as QUOTED data → no warning
        // (R1.8: the write succeeds cleanly, warning is null).
        let ok_path = dir.path().join("tokens.rs");
        let ok_args = serde_json::json!({
            "path": ok_path.to_string_lossy(),
            "content": "const T: &[&str] = &[\"<|im_start|>\", \"<|im_end|>\"];\n"
        })
        .to_string();
        let out = write_file_run(&policy, &ok_args).await.expect("write ok");
        assert!(
            out.contains("\"warning\":null"),
            "quoted table must not warn: {out}"
        );

        // A file whose content is a BARE leaked marker → warning fires.
        let bad_path = dir.path().join("leaked.txt");
        let bad_args = serde_json::json!({
            "path": bad_path.to_string_lossy(),
            "content": "<|im_start|>assistant\nsome leaked protocol text\n"
        })
        .to_string();
        let out = write_file_run(&policy, &bad_args).await.expect("write ok");
        assert!(
            out.contains("chat-template control marker"),
            "bare marker must still warn: {out}"
        );
    }

    #[test]
    fn content_quality_warning_flags_chat_template_tokens() {
        // A BARE marker (raw leaked completion) is flagged.
        let warning = content_quality_warning("<|im_start|>system\nhi")
            .expect("chat-template marker should be flagged");
        assert!(CHAT_TEMPLATE_TOKENS
            .iter()
            .all(|token| !warning.contains(token)));
        assert!(content_quality_warning("normal code, no tokens here").is_none());
    }

    #[test]
    fn content_quality_warning_ignores_quoted_marker_strings_in_source() {
        // R1.8: a file that lists the markers AS DATA (quoted string literals)
        // must NOT be flagged — this is the false positive Kimi hit editing the
        // debris-detector's own token table.
        let token_table = "const CHAT_TEMPLATE_TOKENS: &[&str] = &[\n    \
                           \"<|im_start|>\",\n    \"<|im_end|>\",\n    \"[INST]\",\n];\n";
        assert!(
            content_quality_warning(token_table).is_none(),
            "quoted marker literals in source must not warn"
        );
        // Single-quoted also counts as quoted.
        assert!(content_quality_warning("let c = '\u{0}'; let m = \"<|eot_id|>\";").is_none());
        // But a bare marker mixed into otherwise-quoted content still warns.
        assert!(
            content_quality_warning("\"<|im_start|>\" then bare <|im_end|> here").is_some(),
            "a bare (unquoted) marker is still leaked debris"
        );
    }

    #[test]
    fn content_quality_warning_flags_unescaped_literal_newlines() {
        let broken = "def f():\\n    return 1\\n\\ndef g():\\n    return 2\\n".repeat(6);
        assert!(content_quality_warning(&broken).is_some());

        // A real multi-line file (actual newlines) is never flagged, even if
        // it also happens to contain the literal two-char sequence "\n" as text
        // (e.g. documenting an escape sequence).
        let real_file =
            "fn main() {\n    println!(\"a literal \\\\n in a string\");\n}\n".repeat(6);
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
            "content": "new",
            "expected_previous_content": "old"
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
    async fn write_file_requires_an_exact_precondition_for_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[], &[root]).unwrap();
        let target = dir.path().join("existing.txt");
        std::fs::write(&target, "user-current").unwrap();

        let missing = serde_json::json!({
            "path": target,
            "content": "model-new"
        })
        .to_string();
        let error = write_file_run(&policy, &missing).await.unwrap_err();
        assert!(error.to_string().contains("expected_previous_content"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "user-current");

        let stale = serde_json::json!({
            "path": target,
            "content": "model-new",
            "expected_previous_content": "stale-model-read"
        })
        .to_string();
        let error = write_file_run(&policy, &stale).await.unwrap_err();
        assert!(error.to_string().contains("no longer matches"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "user-current");
    }

    #[tokio::test]
    async fn write_file_refuses_creation_when_existing_content_was_expected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[], &[root]).unwrap();
        let target = dir.path().join("missing.txt");
        let args = serde_json::json!({
            "path": target,
            "content": "model-new",
            "expected_previous_content": "previous"
        })
        .to_string();

        let error = write_file_run(&policy, &args).await.unwrap_err();
        assert!(error.to_string().contains("target is absent"));
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn edit_file_replaces_one_exact_match_and_reports_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy =
            Permissions::for_roots(std::slice::from_ref(&root), std::slice::from_ref(&root))
                .unwrap();
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
    async fn edit_and_write_handle_files_larger_than_the_read_cap() {
        // Regression: the journal pre-image used the 64 KiB READ cap, so any
        // file above it (e.g. orchestrator.rs at 129 KiB) was uneditable. The
        // write cap (1 MiB) is the correct bound. Build a ~200 KiB file — well
        // over the read cap, under the write cap — and edit + rewrite it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy =
            Permissions::for_roots(std::slice::from_ref(&root), std::slice::from_ref(&root))
                .unwrap();
        // The 200 KiB fixture straddles the caps: over the 64 KiB read cap,
        // under the 1 MiB write cap (DEFAULT_MAX_BYTES .. DEFAULT_MAX_WRITE_BYTES).
        let target = dir.path().join("big.rs");
        let mut body = "x".repeat(200 * 1024);
        body.push_str("\nOLD_MARKER\n");
        std::fs::write(&target, &body).unwrap();

        // fs_edit_file on a file over the read cap now succeeds.
        let args = serde_json::json!({
            "path": target,
            "old_text": "OLD_MARKER",
            "new_text": "NEW_MARKER"
        })
        .to_string();
        let result: serde_json::Value =
            serde_json::from_str(&edit_file_run(&policy, &args).await.unwrap()).unwrap();
        assert_eq!(result["change"], "updated", "edit result: {result}");
        assert!(std::fs::read_to_string(&target)
            .unwrap()
            .contains("NEW_MARKER"));

        // fs_write_file creating a fresh file larger than the read cap
        // succeeds (the write cap governs, not the read cap).
        let fresh = dir.path().join("fresh_big.rs");
        let content = "y".repeat(150 * 1024);
        let wargs = serde_json::json!({ "path": fresh, "content": content }).to_string();
        let wout = write_file_run(&policy, &wargs)
            .await
            .expect("write over-cap ok");
        assert!(
            wout.contains("created") || wout.contains("\"change\""),
            "{wout}"
        );
        assert_eq!(std::fs::read_to_string(&fresh).unwrap().len(), 150 * 1024);
    }

    #[tokio::test]
    async fn edit_file_requires_read_authority_for_the_write_target() {
        let readable = tempfile::tempdir().unwrap();
        let writable = tempfile::tempdir().unwrap();
        let read_root = readable.path().to_string_lossy().to_string();
        let write_root = writable.path().to_string_lossy().to_string();
        let policy = Permissions::for_roots(&[read_root], &[write_root]).unwrap();
        let target = writable.path().join("write-only.txt");
        let original = "private current contents";
        std::fs::write(&target, original).unwrap();
        let args = serde_json::json!({
            "path": target,
            "old_text": "private",
            "new_text": "exposed"
        })
        .to_string();

        let error = edit_file_run(&policy, &args).await.unwrap_err();

        assert!(matches!(error, ToolError::PolicyDenied(_)));
        assert!(error.to_string().contains("read denied"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
    }

    #[tokio::test]
    async fn edit_file_honors_read_revocation_before_inspecting_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy =
            Permissions::for_roots(std::slice::from_ref(&root), std::slice::from_ref(&root))
                .unwrap();
        let target = dir.path().join("revoked.txt");
        let original = "secret before revocation";
        std::fs::write(&target, original).unwrap();
        policy.revoke_read_root(&root).unwrap();
        let args = serde_json::json!({
            "path": target,
            "old_text": "secret",
            "new_text": "changed"
        })
        .to_string();

        let error = edit_file_run(&policy, &args).await.unwrap_err();

        assert!(matches!(error, ToolError::PolicyDenied(_)));
        assert!(error.to_string().contains("read denied"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
    }

    #[tokio::test]
    async fn edit_file_refuses_missing_or_ambiguous_preconditions_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let policy =
            Permissions::for_roots(std::slice::from_ref(&root), std::slice::from_ref(&root))
                .unwrap();
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
        let policy =
            Permissions::for_roots(std::slice::from_ref(&root), std::slice::from_ref(&root))
                .unwrap();
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
            "content": "after\nline two\n",
            "expected_previous_content": "before\nline two\n"
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
    async fn journal_report_contains_verified_bounded_unified_diff_and_preview_apis() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("review.txt");
        std::fs::write(&target, "before\nshared\n").unwrap();
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let args = serde_json::json!({
            "path": target,
            "content": "after\nshared\n",
            "expected_previous_content": "before\nshared\n"
        })
        .to_string();
        let report = run_journaled_write(&journal, &policy, "verified edit", &args).await;

        assert!(report.unified_diff.contains("--- a/"));
        assert!(report.unified_diff.contains("+++ b/"));
        assert!(report.unified_diff.contains("@@ -1,2 +1,2 @@"));
        assert!(report.unified_diff.contains("-before\n"));
        assert!(report.unified_diff.contains("+after\n"));
        assert!(!report.diff_truncated);
        assert_eq!(report.changes[0].unified_diff, report.unified_diff);

        let summaries = journal.checkpoint_summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, report.id);
        assert_eq!(summaries[0].change_count, 1);
        assert_eq!(summaries[0].paths, vec![target]);
        assert_eq!(journal.checkpoint_preview(report.id), Some(report.clone()));
        assert!(journal.checkpoint_preview(report.id + 1).is_none());
    }

    #[tokio::test]
    async fn journal_required_write_verification_is_exact_and_detects_later_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("required.txt");
        std::fs::write(&target, "before").unwrap();
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let args = serde_json::json!({
            "path": target,
            "content": "after",
            "expected_previous_content": "before"
        })
        .to_string();
        run_journaled_write(&journal, &policy, "required effect", &args).await;

        let verified = journal
            .verify_written_path(&target)
            .unwrap()
            .expect("recorded write");
        assert_eq!(verified.initial_bytes, Some(6));
        assert_eq!(verified.final_bytes, 5);
        assert!(verified.net_content_changed);
        assert!(verified.current_matches_latest_write);

        let (verified, observed) = journal
            .verify_written_path_with_current_content(&target, |content| {
                content.map(ToOwned::to_owned)
            })
            .unwrap()
            .expect("recorded write and same-snapshot observation");
        assert!(verified.current_matches_latest_write);
        assert_eq!(observed.as_deref(), Some(b"after".as_slice()));

        std::fs::write(&target, "other").unwrap();
        let conflicted = journal
            .verify_written_path(&target)
            .unwrap()
            .expect("record remains");
        assert!(conflicted.net_content_changed);
        assert!(!conflicted.current_matches_latest_write);
        let (conflicted, observed) = journal
            .verify_written_path_with_current_content(&target, |content| {
                content.map(ToOwned::to_owned)
            })
            .unwrap()
            .expect("record and current postimage remain observable");
        assert!(!conflicted.current_matches_latest_write);
        assert_eq!(observed.as_deref(), Some(b"other".as_slice()));
        assert!(journal
            .verify_written_path(&dir.path().join("never-written.txt"))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn journal_required_write_verification_includes_the_active_turn() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("active-required.txt");
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let start = journal.begin_attributed_turn("active exact repair");
        let args = serde_json::json!({
            "path": target,
            "content": "active postimage"
        })
        .to_string();
        journal
            .scope_turn(&start.token, async {
                write_file_run_with_journal(&policy, Some(&journal), &args)
                    .await
                    .unwrap();
            })
            .await;
        journal.drain_mutations().await;

        let (verified, observed) = journal
            .verify_written_path_with_current_content(&target, |content| {
                content.map(ToOwned::to_owned)
            })
            .unwrap()
            .expect("the active attributed write is immediately verifiable");
        assert!(verified.net_content_changed);
        assert!(verified.current_matches_latest_write);
        assert_eq!(observed.as_deref(), Some(b"active postimage".as_slice()));
        assert_eq!(journal.undo_depth(), 0, "the turn is not finalized yet");

        journal.finish_attributed_turn(&start.token).unwrap();
        assert_eq!(journal.undo_depth(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn required_write_evidence_never_follows_a_raced_final_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let authorized = dir.path().join("authorized.txt");
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "private outside bytes").unwrap();
        symlink(&outside, &authorized).unwrap();

        let error = read_verified_write_target(&authorized, 1024).unwrap_err();
        assert!(error.to_string().contains("authorized write target"));
    }

    #[tokio::test]
    async fn journal_diff_escapes_terminal_controls_and_is_checkpoint_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("large.txt");
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let mut content = "\u{1b}]0;forged-title\u{7}\u{009b}31m".to_string();
        content.push_str(&"x".repeat(MAX_JOURNAL_DIFF_BYTES * 2));
        let args = serde_json::json!({"path": target, "content": content}).to_string();
        let report = run_journaled_write(&journal, &policy, "large control edit", &args).await;

        assert!(report.diff_truncated);
        assert!(report.unified_diff.len() <= MAX_JOURNAL_DIFF_BYTES);
        assert!(report.unified_diff.contains("checkpoint diff truncated"));
        assert!(report.unified_diff.contains("\\u{1b}"));
        assert!(report.unified_diff.contains("\\u{7}"));
        assert!(report.unified_diff.contains("\\u{9b}"));
        assert!(!report
            .unified_diff
            .chars()
            .any(|character| character != '\n' && character.is_control()));
        let retained_change_diff_bytes: usize = report
            .changes
            .iter()
            .map(|change| change.unified_diff.len())
            .sum();
        assert!(retained_change_diff_bytes <= MAX_JOURNAL_DIFF_BYTES);
    }

    #[tokio::test]
    async fn journal_diff_represents_a_trailing_newline_only_change() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("newline.txt");
        std::fs::write(&target, "same line").unwrap();
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();
        let args = serde_json::json!({
            "path": target,
            "content": "same line\n",
            "expected_previous_content": "same line"
        })
        .to_string();
        let report = run_journaled_write(&journal, &policy, "newline", &args).await;

        assert!(report.unified_diff.contains("-same line\n"));
        assert!(report.unified_diff.contains("+same line\n"));
        assert!(report.unified_diff.contains("\\ No newline at end of file"));
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
        let args = serde_json::json!({
            "path": target,
            "content": "model",
            "expected_previous_content": "before"
        })
        .to_string();
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
        let args = serde_json::json!({
            "path": target,
            "content": "model",
            "expected_previous_content": "before"
        })
        .to_string();
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
        let args = serde_json::json!({
            "path": target,
            "content": "model",
            "expected_previous_content": "before"
        })
        .to_string();
        run_journaled_write(&journal, &policy, "model edit", &args).await;

        let old_identity = file_identity(&std::fs::metadata(&target).unwrap());
        // Replace the path with a DIFFERENT inode carrying identical bytes.
        //
        // The obvious spelling — remove then rewrite — is not deterministic:
        // the filesystem is free to hand the freed inode straight back, and
        // ext4 routinely does, which made this fixture fail roughly one run in
        // five under parallel load. Building the replacement while the original
        // is still allocated forces the kernel to pick a different inode, then
        // the rename installs it. The test is about undo refusing a stale
        // identity, so the fixture must establish that premise, not gamble on it.
        let replacement = dir.path().join("inode-conflict.replacement");
        std::fs::write(&replacement, "model").unwrap();
        std::fs::rename(&replacement, &target).unwrap();
        assert_ne!(
            old_identity,
            file_identity(&std::fs::metadata(&target).unwrap()),
            "fixture must install a distinct inode for the conflict to exist"
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
                let first = serde_json::json!({
                    "path": target,
                    "content": "foreground-1",
                    "expected_previous_content": "original"
                })
                .to_string();
                write_file_run_with_journal(&policy, Some(&journal), &first)
                    .await
                    .unwrap();

                let background_policy = policy.clone();
                let background_journal = journal.clone();
                let background = serde_json::json!({
                    "path": target,
                    "content": "background",
                    "expected_previous_content": "foreground-1"
                })
                .to_string();
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

                let last = serde_json::json!({
                    "path": target,
                    "content": "foreground-2",
                    "expected_previous_content": "background"
                })
                .to_string();
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
                let mut expected = "original";
                for content in ["first", "second"] {
                    let args = serde_json::json!({
                        "path": target,
                        "content": content,
                        "expected_previous_content": expected
                    })
                    .to_string();
                    write_file_run_with_journal(&policy, Some(&journal), &args)
                        .await
                        .unwrap();
                    expected = content;
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

    #[tokio::test]
    async fn selected_undo_restores_an_older_disjoint_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        std::fs::write(&first, "first-before").unwrap();
        std::fs::write(&second, "second-before").unwrap();
        let policy =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        let journal = WriteJournal::new();

        let first_args = serde_json::json!({
            "path": first,
            "content": "first-after",
            "expected_previous_content": "first-before"
        })
        .to_string();
        let first_report =
            run_journaled_write(&journal, &policy, "first change", &first_args).await;
        let second_args = serde_json::json!({
            "path": second,
            "content": "second-after",
            "expected_previous_content": "second-before"
        })
        .to_string();
        let second_report =
            run_journaled_write(&journal, &policy, "second change", &second_args).await;

        assert_eq!(
            journal
                .checkpoints()
                .iter()
                .map(|report| report.id)
                .collect::<Vec<_>>(),
            vec![first_report.id, second_report.id]
        );
        journal
            .undo_checkpoint(&policy, first_report.id)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "first-before");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "second-after");
        assert_eq!(journal.checkpoints(), vec![second_report.clone()]);

        journal
            .undo_checkpoint(&policy, second_report.id)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "second-before");
        assert!(journal.checkpoints().is_empty());
    }
}
