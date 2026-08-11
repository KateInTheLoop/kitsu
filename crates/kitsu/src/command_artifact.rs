//! Protected, host-owned full-output artifacts for developer commands.
//!
//! Command output is stored as JSON Lines so terminal controls remain escaped
//! and non-UTF-8 chunks remain exactly recoverable as hexadecimal bytes. The
//! model never chooses a path: the frontend supplies one protected directory
//! and the filesystem layer allocates a unique `0600` file without following
//! symlinks.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kitsu_tools::builtins::fs::{create_unique_private_artifact, PrivateArtifactWriter};
use kitsu_tools::{
    CommandArtifactCompletion, CommandArtifactRequest, CommandOutputArtifact,
    CommandOutputArtifactSink, CommandOutputArtifactWriter, CommandOutputStream, ToolError,
};

/// Space held back so the bounded terminal record can always fit inside the
/// hard serialized artifact ceiling.
const FINISHED_RECORD_RESERVE_BYTES: u64 = 1024;
/// Default number of retained plus in-flight command artifacts per sink.
pub const DEFAULT_COMMAND_ARTIFACT_COUNT: usize = 32;
/// Default aggregate serialized bytes retained or reserved by one sink.
pub const DEFAULT_COMMAND_ARTIFACT_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandArtifactOverflowPolicy {
    /// Remove the lowest allocation id first until the new reservation fits.
    PruneOldest,
    /// Preserve every retained artifact and refuse the new capture.
    RefuseNew,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandArtifactQuota {
    pub max_count: usize,
    pub max_total_bytes: u64,
    pub overflow: CommandArtifactOverflowPolicy,
}

impl CommandArtifactQuota {
    pub fn new(
        max_count: usize,
        max_total_bytes: u64,
        overflow: CommandArtifactOverflowPolicy,
    ) -> Result<Self, ToolError> {
        if max_count == 0 {
            return Err(ToolError::InvalidArgs(
                "aggregate command artifact count must be greater than zero".to_string(),
            ));
        }
        if max_total_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "aggregate command artifact byte quota must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            max_count,
            max_total_bytes,
            overflow,
        })
    }
}

impl Default for CommandArtifactQuota {
    fn default() -> Self {
        Self {
            max_count: DEFAULT_COMMAND_ARTIFACT_COUNT,
            max_total_bytes: DEFAULT_COMMAND_ARTIFACT_TOTAL_BYTES,
            overflow: CommandArtifactOverflowPolicy::PruneOldest,
        }
    }
}

/// One completed artifact retained under the aggregate quota.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedCommandArtifact {
    /// Monotonic allocation order; the lowest id is deterministically oldest.
    pub id: u64,
    pub reference: PathBuf,
    pub artifact_bytes: u64,
    pub captured_bytes: u64,
    pub truncated: bool,
    pub limit_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandArtifactUsage {
    pub retained_count: usize,
    pub retained_bytes: u64,
    pub in_flight_count: usize,
    pub reserved_bytes: u64,
    pub max_count: usize,
    pub max_total_bytes: u64,
}

impl CommandArtifactUsage {
    pub fn available_count(self) -> usize {
        self.max_count
            .saturating_sub(self.retained_count.saturating_add(self.in_flight_count))
    }

    pub fn available_bytes(self) -> u64 {
        self.max_total_bytes
            .saturating_sub(self.retained_bytes.saturating_add(self.reserved_bytes))
    }
}

struct ArtifactSinkState {
    directory: Option<PathBuf>,
    quota: CommandArtifactQuota,
    next_id: u64,
    retained: Vec<RetainedCommandArtifact>,
    reservations: Vec<(u64, u64)>,
}

impl ArtifactSinkState {
    fn usage(&self) -> CommandArtifactUsage {
        CommandArtifactUsage {
            retained_count: self.retained.len(),
            retained_bytes: self.retained.iter().fold(0_u64, |total, artifact| {
                total.saturating_add(artifact.artifact_bytes)
            }),
            in_flight_count: self.reservations.len(),
            reserved_bytes: self
                .reservations
                .iter()
                .fold(0_u64, |total, (_, bytes)| total.saturating_add(*bytes)),
            max_count: self.quota.max_count,
            max_total_bytes: self.quota.max_total_bytes,
        }
    }
}

/// A sink whose destination can follow `/new` and `/resume` session switches.
#[derive(Clone)]
pub struct CommandArtifactSink {
    state: Arc<Mutex<ArtifactSinkState>>,
}

impl Default for CommandArtifactSink {
    fn default() -> Self {
        Self::with_quota(CommandArtifactQuota::default())
    }
}

impl CommandArtifactSink {
    pub fn with_quota(quota: CommandArtifactQuota) -> Self {
        Self {
            state: Arc::new(Mutex::new(ArtifactSinkState {
                directory: None,
                quota,
                next_id: 0,
                retained: Vec::new(),
                reservations: Vec::new(),
            })),
        }
    }

    pub fn set_directory(&self, directory: Option<PathBuf>) {
        self.state
            .lock()
            .expect("command artifact registry")
            .directory = directory;
    }

    pub fn quota(&self) -> CommandArtifactQuota {
        self.state.lock().expect("command artifact registry").quota
    }

    pub fn usage(&self) -> CommandArtifactUsage {
        self.state
            .lock()
            .expect("command artifact registry")
            .usage()
    }

    /// Snapshot retained artifacts in deterministic oldest-first order.
    pub fn list(&self) -> Vec<RetainedCommandArtifact> {
        self.state
            .lock()
            .expect("command artifact registry")
            .retained
            .clone()
    }

    /// Remove exactly one retained artifact and return its metadata. Missing
    /// files count as successful cleanup because no persistent bytes remain.
    pub fn prune(&self, id: u64) -> std::io::Result<Option<RetainedCommandArtifact>> {
        let mut state = self.state.lock().expect("command artifact registry");
        let Some(index) = state.retained.iter().position(|artifact| artifact.id == id) else {
            return Ok(None);
        };
        remove_retained_file(&state.retained[index])?;
        Ok(Some(state.retained.remove(index)))
    }

    /// Remove all retained artifacts in deterministic oldest-first order.
    /// Stops at the first failed removal and leaves that record plus every
    /// newer record inspectable for a later retry.
    pub fn prune_all(&self) -> std::io::Result<Vec<RetainedCommandArtifact>> {
        let mut state = self.state.lock().expect("command artifact registry");
        let mut removed = Vec::new();
        while let Some(oldest) = state.retained.first() {
            remove_retained_file(oldest)?;
            removed.push(state.retained.remove(0));
        }
        Ok(removed)
    }

    #[cfg(test)]
    pub(crate) fn directory(&self) -> Option<PathBuf> {
        self.state
            .lock()
            .expect("command artifact registry")
            .directory
            .clone()
    }
}

fn reserve_artifact(
    registry: &Arc<Mutex<ArtifactSinkState>>,
    limit_bytes: u64,
) -> Result<u64, ToolError> {
    let mut state = registry.lock().expect("command artifact registry");
    if limit_bytes > state.quota.max_total_bytes {
        return Err(ToolError::ExecError(format!(
            "command artifact reservation {limit_bytes} exceeds aggregate byte quota {}",
            state.quota.max_total_bytes
        )));
    }
    loop {
        let usage = state.usage();
        let count_fits =
            usage.retained_count.saturating_add(usage.in_flight_count) < state.quota.max_count;
        let bytes_fit = usage
            .retained_bytes
            .saturating_add(usage.reserved_bytes)
            .saturating_add(limit_bytes)
            <= state.quota.max_total_bytes;
        if count_fits && bytes_fit {
            break;
        }
        if state.quota.overflow == CommandArtifactOverflowPolicy::RefuseNew
            || state.retained.is_empty()
        {
            return Err(ToolError::ExecError(format!(
                "aggregate command artifact quota exhausted ({} retained, {} in flight, {} retained bytes, {} reserved bytes; limits {} artifacts / {} bytes)",
                usage.retained_count,
                usage.in_flight_count,
                usage.retained_bytes,
                usage.reserved_bytes,
                state.quota.max_count,
                state.quota.max_total_bytes
            )));
        }
        remove_retained_file(&state.retained[0]).map_err(|error| {
            ToolError::ExecError(format!(
                "cannot prune oldest command artifact #{} for quota admission: {error}",
                state.retained[0].id
            ))
        })?;
        state.retained.remove(0);
    }
    state.next_id = state.next_id.checked_add(1).ok_or_else(|| {
        ToolError::ExecError("command artifact allocation id exhausted".to_string())
    })?;
    let id = state.next_id;
    state.reservations.push((id, limit_bytes));
    state.reservations.sort_by_key(|(id, _)| *id);
    Ok(id)
}

fn release_reservation(registry: &Arc<Mutex<ArtifactSinkState>>, id: u64) {
    let mut state = registry.lock().expect("command artifact registry");
    if let Some(index) = state
        .reservations
        .iter()
        .position(|(candidate, _)| *candidate == id)
    {
        state.reservations.remove(index);
    }
}

fn retain_completed_artifact(
    registry: &Arc<Mutex<ArtifactSinkState>>,
    artifact: RetainedCommandArtifact,
) -> std::io::Result<()> {
    let mut state = registry.lock().expect("command artifact registry");
    let Some(index) = state
        .reservations
        .iter()
        .position(|(id, _)| *id == artifact.id)
    else {
        return Err(std::io::Error::other(
            "command artifact reservation is no longer registered",
        ));
    };
    let reserved = state.reservations[index].1;
    if artifact.artifact_bytes > reserved {
        return Err(std::io::Error::other(format!(
            "command artifact used {} bytes beyond its {reserved}-byte reservation",
            artifact.artifact_bytes
        )));
    }
    state.reservations.remove(index);
    let insertion = state
        .retained
        .binary_search_by_key(&artifact.id, |candidate| candidate.id)
        .unwrap_or_else(|index| index);
    state.retained.insert(insertion, artifact);
    Ok(())
}

fn remove_retained_file(artifact: &RetainedCommandArtifact) -> std::io::Result<()> {
    match std::fs::remove_file(&artifact.reference) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

impl CommandOutputArtifactSink for CommandArtifactSink {
    fn begin(
        &self,
        request: &CommandArtifactRequest,
    ) -> Result<Arc<dyn CommandOutputArtifactWriter>, ToolError> {
        let directory = self
            .state
            .lock()
            .expect("command artifact registry")
            .directory
            .clone()
            .ok_or_else(|| {
                ToolError::ExecError("protected command artifact directory is unavailable".into())
            })?;
        let started = record_bytes(&serde_json::json!({
            "format": "kitsu.command-output.v1",
            "event": "started",
            "limit_bytes": request.limit_bytes,
        }))
        .map_err(|error| {
            ToolError::ExecError(format!("cannot initialize command artifact: {error}"))
        })?;
        let started_bytes = u64::try_from(started.len()).unwrap_or(u64::MAX);
        if started_bytes.saturating_add(FINISHED_RECORD_RESERVE_BYTES) > request.limit_bytes {
            return Err(ToolError::ExecError(format!(
                "command artifact limit {} is too small for protected framing metadata",
                request.limit_bytes
            )));
        }
        // Admission precedes allocation, so a quota refusal cannot leave an
        // untracked temporary file if cleanup itself fails.
        let reservation_id = reserve_artifact(&self.state, request.limit_bytes)?;
        let mut writer = match create_unique_private_artifact(&directory, "command-output") {
            Ok(writer) => writer,
            Err(error) => {
                release_reservation(&self.state, reservation_id);
                return Err(ToolError::ExecError(format!(
                    "cannot create command artifact: {error}"
                )));
            }
        };
        if let Err(error) = writer.write_all(&started) {
            let cleanup = writer.abort();
            if cleanup.is_ok() {
                release_reservation(&self.state, reservation_id);
            }
            return Err(ToolError::ExecError(match cleanup {
                Ok(()) => format!("cannot initialize command artifact: {error}"),
                Err(cleanup) => format!(
                    "cannot initialize command artifact: {error}; artifact cleanup failed: {cleanup}"
                ),
            }));
        }
        Ok(Arc::new(CommandArtifact {
            state: Mutex::new(Some(CommandArtifactState {
                writer,
                bytes_written: started_bytes,
                captured_bytes: 0,
                truncated: false,
                limit_bytes: request.limit_bytes,
                reservation_id,
                registry: self.state.clone(),
            })),
            #[cfg(test)]
            fail_completion_once: std::sync::atomic::AtomicBool::new(false),
        }))
    }
}

struct CommandArtifact {
    state: Mutex<Option<CommandArtifactState>>,
    #[cfg(test)]
    fail_completion_once: std::sync::atomic::AtomicBool,
}

struct CommandArtifactState {
    writer: PrivateArtifactWriter,
    bytes_written: u64,
    captured_bytes: u64,
    truncated: bool,
    limit_bytes: u64,
    reservation_id: u64,
    registry: Arc<Mutex<ArtifactSinkState>>,
}

impl CommandOutputArtifactWriter for CommandArtifact {
    fn write_chunk(&self, stream: CommandOutputStream, chunk: &[u8]) -> std::io::Result<()> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| std::io::Error::other("command artifact lock was poisoned"))?;
        let state = guard
            .as_mut()
            .ok_or_else(|| std::io::Error::other("command artifact is already closed"))?;
        if state.truncated {
            return Ok(());
        }
        let stream = match stream {
            CommandOutputStream::Stdout => "stdout",
            CommandOutputStream::Stderr => "stderr",
        };
        let record = match std::str::from_utf8(chunk) {
            Ok(text) => serde_json::json!({
                "event": "chunk",
                "stream": stream,
                "encoding": "utf8",
                "bytes": chunk.len(),
                "data": text,
            }),
            Err(_) => serde_json::json!({
                "event": "chunk",
                "stream": stream,
                "encoding": "hex",
                "bytes": chunk.len(),
                "data": encode_hex(chunk),
            }),
        };
        let encoded = record_bytes(&record)?;
        let encoded_bytes = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        if state
            .bytes_written
            .saturating_add(encoded_bytes)
            .saturating_add(FINISHED_RECORD_RESERVE_BYTES)
            > state.limit_bytes
        {
            state.truncated = true;
            return Ok(());
        }
        state.writer.write_all(&encoded)?;
        state.bytes_written = state.bytes_written.saturating_add(encoded_bytes);
        state.captured_bytes = state
            .captured_bytes
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    fn complete(
        &self,
        completion: &CommandArtifactCompletion,
    ) -> std::io::Result<CommandOutputArtifact> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| std::io::Error::other("command artifact lock was poisoned"))?;
        let mut state = guard
            .take()
            .ok_or_else(|| std::io::Error::other("command artifact is already closed"))?;
        state.truncated |= completion.truncated;
        let mut artifact_bytes = state.bytes_written;
        let mut finished = Vec::new();
        for _ in 0..8 {
            finished = record_bytes(&serde_json::json!({
                "event": "finished",
                "termination": completion.termination,
                "success": completion.success,
                "exit_code": completion.exit_code,
                "signal": completion.signal,
                "elapsed_ms": completion.elapsed_ms,
                "stdout_bytes": completion.stdout_bytes,
                "stderr_bytes": completion.stderr_bytes,
                "stdout_capture_complete": completion.stdout_capture_complete,
                "stderr_capture_complete": completion.stderr_capture_complete,
                "captured_bytes": state.captured_bytes,
                "truncated": state.truncated,
                "limit_bytes": state.limit_bytes,
                "artifact_bytes": artifact_bytes,
            }))?;
            let next = state
                .bytes_written
                .saturating_add(u64::try_from(finished.len()).unwrap_or(u64::MAX));
            if next == artifact_bytes {
                break;
            }
            artifact_bytes = next;
        }
        if state
            .bytes_written
            .saturating_add(u64::try_from(finished.len()).unwrap_or(u64::MAX))
            != artifact_bytes
            || artifact_bytes > state.limit_bytes
        {
            *guard = Some(state);
            return Err(std::io::Error::other(
                "command artifact terminal metadata exceeded its reserved byte budget",
            ));
        }
        #[cfg(test)]
        if self
            .fail_completion_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            *guard = Some(state);
            return Err(std::io::Error::other(
                "injected command artifact finalization failure",
            ));
        }
        if let Err(error) = state.writer.write_all(&finished) {
            *guard = Some(state);
            return Err(error);
        }
        state.bytes_written = artifact_bytes;
        let path = match state.writer.finish_in_place() {
            Ok(path) => path,
            Err(error) => {
                *guard = Some(state);
                return Err(error);
            }
        };
        let retained = RetainedCommandArtifact {
            id: state.reservation_id,
            reference: path.clone(),
            captured_bytes: state.captured_bytes,
            truncated: state.truncated,
            limit_bytes: state.limit_bytes,
            artifact_bytes: state.bytes_written,
        };
        if let Err(error) = retain_completed_artifact(&state.registry, retained) {
            let cleanup = std::fs::remove_file(&path);
            if cleanup.is_ok()
                || cleanup
                    .as_ref()
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                release_reservation(&state.registry, state.reservation_id);
            }
            return Err(std::io::Error::other(match cleanup {
                Ok(()) => format!(
                    "cannot retain completed command artifact in aggregate registry: {error}; new artifact was removed"
                ),
                Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => format!(
                    "cannot retain completed command artifact in aggregate registry: {error}; new artifact was already absent"
                ),
                Err(cleanup) => format!(
                    "cannot retain completed command artifact in aggregate registry: {error}; cleanup failed: {cleanup}"
                ),
            }));
        }
        Ok(CommandOutputArtifact {
            reference: path.to_string_lossy().into_owned(),
            captured_bytes: state.captured_bytes,
            truncated: state.truncated,
            limit_bytes: state.limit_bytes,
            artifact_bytes: state.bytes_written,
        })
    }

    fn abort(&self) -> std::io::Result<()> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| std::io::Error::other("command artifact lock was poisoned"))?;
        let Some(mut state) = guard.take() else {
            return Ok(());
        };
        match state.writer.abort() {
            Ok(()) => {
                release_reservation(&state.registry, state.reservation_id);
                Ok(())
            }
            Err(error) => {
                // Preserve the armed writer for a later explicit retry and its
                // final Drop cleanup. `Ok` is reserved for verified removal.
                *guard = Some(state);
                Err(error)
            }
        }
    }
}

impl Drop for CommandArtifact {
    fn drop(&mut self) {
        let Ok(mut guard) = self.state.lock() else {
            return;
        };
        let Some(mut state) = guard.take() else {
            return;
        };
        if state.writer.abort().is_ok() {
            release_reservation(&state.registry, state.reservation_id);
        }
        // On failed cleanup the reservation intentionally remains charged: the
        // sink cannot truthfully claim those persistent bytes were removed.
    }
}

fn record_bytes(record: &serde_json::Value) -> std::io::Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitsu_tools::{CommandExecutableIdentity, CommandExecutableResolution, CommandTermination};

    fn request() -> CommandArtifactRequest {
        CommandArtifactRequest {
            argv: vec!["test".to_string()],
            cwd: "/tmp".to_string(),
            timeout_ms: 1000,
            executable: CommandExecutableIdentity {
                invocation: "test".to_string(),
                resolution: CommandExecutableResolution::AmbientPath,
                observed_path: None,
                pinned: false,
            },
            limit_bytes: 4096,
        }
    }

    fn completion() -> CommandArtifactCompletion {
        CommandArtifactCompletion {
            termination: CommandTermination::Exited,
            success: true,
            exit_code: Some(0),
            signal: None,
            elapsed_ms: 12,
            stdout_bytes: 5,
            stderr_bytes: 2,
            stdout_capture_complete: true,
            stderr_capture_complete: true,
            captured_bytes: 7,
            truncated: false,
            limit_bytes: 1024,
        }
    }

    fn complete_artifact(
        sink: &CommandArtifactSink,
        limit_bytes: u64,
        payload: &[u8],
    ) -> CommandOutputArtifact {
        let mut request = request();
        request.limit_bytes = limit_bytes;
        let writer = sink.begin(&request).unwrap();
        writer
            .write_chunk(CommandOutputStream::Stdout, payload)
            .unwrap();
        writer.complete(&completion()).unwrap()
    }

    #[test]
    fn missing_directory_fails_before_allocating() {
        let sink = CommandArtifactSink::default();
        assert_eq!(sink.directory(), None);
        let error = match sink.begin(&request()) {
            Ok(_) => panic!("missing artifact directory must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("directory is unavailable"));
    }

    #[test]
    fn completed_artifact_is_jsonl_and_binary_chunks_are_exact_hex() {
        let directory = tempfile::tempdir().unwrap();
        let sink = CommandArtifactSink::default();
        sink.set_directory(Some(directory.path().to_path_buf()));
        let writer = sink.begin(&request()).unwrap();
        writer
            .write_chunk(CommandOutputStream::Stdout, b"hello")
            .unwrap();
        writer
            .write_chunk(CommandOutputStream::Stderr, &[0xff, b'\n'])
            .unwrap();
        let artifact = writer.complete(&completion()).unwrap();
        let text = std::fs::read_to_string(&artifact.reference).unwrap();
        let records = text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 4);
        assert_eq!(records[1]["data"], "hello");
        assert_eq!(records[2]["encoding"], "hex");
        assert_eq!(records[2]["data"], "ff0a");
        assert_eq!(records[3]["termination"], "exited");
        assert_eq!(records[3]["captured_bytes"], 7);
        assert_eq!(records[3]["truncated"], false);
        assert_eq!(records[3]["limit_bytes"], 4096);
        assert_eq!(records[3]["artifact_bytes"], artifact.artifact_bytes);
        assert_eq!(artifact.captured_bytes, 7);
        assert!(!artifact.truncated);
        assert_eq!(artifact.limit_bytes, 4096);
        assert_eq!(
            artifact.artifact_bytes,
            std::fs::metadata(&artifact.reference).unwrap().len()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&artifact.reference)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn abort_removes_the_unfinished_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let sink = CommandArtifactSink::default();
        sink.set_directory(Some(directory.path().to_path_buf()));
        let writer = sink.begin(&request()).unwrap();
        writer
            .write_chunk(CommandOutputStream::Stdout, b"partial")
            .unwrap();
        writer.abort().unwrap();
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn serialized_ceiling_counts_tiny_hex_records_and_framing() {
        let directory = tempfile::tempdir().unwrap();
        let sink = CommandArtifactSink::default();
        sink.set_directory(Some(directory.path().to_path_buf()));
        let mut request = request();
        request.limit_bytes = 2048;
        let writer = sink.begin(&request).unwrap();
        for _ in 0..1000 {
            writer
                .write_chunk(CommandOutputStream::Stdout, &[0xff])
                .unwrap();
        }
        let artifact = writer.complete(&completion()).unwrap();

        let file_bytes = std::fs::metadata(&artifact.reference).unwrap().len();
        assert_eq!(artifact.artifact_bytes, file_bytes);
        assert!(file_bytes <= request.limit_bytes);
        assert!(artifact.truncated);
        assert!(artifact.captured_bytes > 0);
        assert!(artifact.captured_bytes < 1000);
        let records = std::fs::read_to_string(&artifact.reference)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(records[1..records.len() - 1]
            .iter()
            .all(|record| record["encoding"] == "hex" && record["data"] == "ff"));
        let finished = records.last().unwrap();
        assert_eq!(finished["artifact_bytes"], file_bytes);
        assert_eq!(finished["captured_bytes"], artifact.captured_bytes);
        assert_eq!(finished["truncated"], true);
    }

    #[test]
    fn aggregate_count_quota_prunes_deterministic_oldest_and_exposes_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let quota =
            CommandArtifactQuota::new(2, 16 * 1024, CommandArtifactOverflowPolicy::PruneOldest)
                .unwrap();
        let sink = CommandArtifactSink::with_quota(quota);
        sink.set_directory(Some(directory.path().to_path_buf()));
        assert_eq!(sink.quota(), quota);

        let first = complete_artifact(&sink, 4096, b"first");
        let second = complete_artifact(&sink, 4096, b"second");
        let third = complete_artifact(&sink, 4096, b"third");

        assert!(!PathBuf::from(&first.reference).exists());
        assert!(PathBuf::from(&second.reference).exists());
        assert!(PathBuf::from(&third.reference).exists());
        let retained = sink.list();
        assert_eq!(
            retained
                .iter()
                .map(|artifact| artifact.id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(
            retained
                .iter()
                .map(|artifact| artifact.reference.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from(&second.reference),
                PathBuf::from(&third.reference)
            ]
        );
        let usage = sink.usage();
        assert_eq!(usage.retained_count, 2);
        assert_eq!(usage.in_flight_count, 0);
        assert_eq!(usage.available_count(), 0);
        assert_eq!(
            usage.retained_bytes,
            retained
                .iter()
                .map(|artifact| artifact.artifact_bytes)
                .sum::<u64>()
        );
        assert!(usage.retained_bytes <= quota.max_total_bytes);

        let removed = sink.prune(2).unwrap().unwrap();
        assert_eq!(removed.id, 2);
        assert!(!removed.reference.exists());
        assert_eq!(sink.prune(2).unwrap(), None);
        let remaining = sink.prune_all().unwrap();
        assert_eq!(
            remaining
                .iter()
                .map(|artifact| artifact.id)
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert!(sink.list().is_empty());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn aggregate_byte_quota_prunes_oldest_before_reserving_new_capture() {
        let directory = tempfile::tempdir().unwrap();
        let quota =
            CommandArtifactQuota::new(3, 4096, CommandArtifactOverflowPolicy::PruneOldest).unwrap();
        let sink = CommandArtifactSink::with_quota(quota);
        sink.set_directory(Some(directory.path().to_path_buf()));

        let first = complete_artifact(&sink, 2048, &vec![b'a'; 600]);
        let second = complete_artifact(&sink, 2048, &vec![b'b'; 600]);
        assert!(PathBuf::from(&first.reference).exists());
        let third = complete_artifact(&sink, 2048, &vec![b'c'; 600]);

        assert!(!PathBuf::from(&first.reference).exists());
        assert!(PathBuf::from(&second.reference).exists());
        assert!(PathBuf::from(&third.reference).exists());
        assert_eq!(
            sink.list()
                .iter()
                .map(|artifact| artifact.id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        let usage = sink.usage();
        assert_eq!(usage.in_flight_count, 0);
        assert!(usage.retained_bytes <= quota.max_total_bytes);
        assert_eq!(
            usage.available_bytes(),
            quota.max_total_bytes - usage.retained_bytes
        );
    }

    #[test]
    fn refusal_quota_cleans_unadmitted_file_and_preserves_retained_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let quota =
            CommandArtifactQuota::new(1, 4096, CommandArtifactOverflowPolicy::RefuseNew).unwrap();
        let sink = CommandArtifactSink::with_quota(quota);
        sink.set_directory(Some(directory.path().to_path_buf()));
        let first = complete_artifact(&sink, 2048, b"retained");

        let mut next = request();
        next.limit_bytes = 2048;
        let error = match sink.begin(&next) {
            Ok(_) => panic!("full refusal quota must reject the new artifact"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("quota exhausted"));
        assert!(PathBuf::from(&first.reference).exists());
        assert_eq!(sink.list().len(), 1);
        assert_eq!(sink.usage().in_flight_count, 0);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn abort_releases_aggregate_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let sink = CommandArtifactSink::default();
        sink.set_directory(Some(directory.path().to_path_buf()));
        let writer = sink.begin(&request()).unwrap();
        assert_eq!(sink.usage().in_flight_count, 1);
        assert_eq!(sink.usage().reserved_bytes, request().limit_bytes);
        writer.abort().unwrap();
        assert_eq!(sink.usage().in_flight_count, 0);
        assert_eq!(sink.usage().reserved_bytes, 0);
    }

    #[cfg(unix)]
    #[test]
    fn failed_completion_and_cleanup_leave_writer_recoverable_for_retry() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::sync::atomic::Ordering;

        let directory = tempfile::tempdir().unwrap();
        let request = request();
        let started = record_bytes(&serde_json::json!({
            "format": "kitsu.command-output.v1",
            "event": "started",
            "limit_bytes": request.limit_bytes,
        }))
        .unwrap();
        let mut private =
            create_unique_private_artifact(directory.path(), "command-output").unwrap();
        private.write_all(&started).unwrap();
        let path = private.path().to_path_buf();
        let registry = Arc::new(Mutex::new(ArtifactSinkState {
            directory: Some(directory.path().to_path_buf()),
            quota: CommandArtifactQuota::default(),
            next_id: 1,
            retained: Vec::new(),
            reservations: vec![(1, request.limit_bytes)],
        }));
        let writer = CommandArtifact {
            state: Mutex::new(Some(CommandArtifactState {
                writer: private,
                bytes_written: started.len() as u64,
                captured_bytes: 0,
                truncated: false,
                limit_bytes: request.limit_bytes,
                reservation_id: 1,
                registry: registry.clone(),
            })),
            fail_completion_once: std::sync::atomic::AtomicBool::new(true),
        };

        let error = writer.complete(&completion()).unwrap_err();
        assert!(error.to_string().contains("injected"));
        assert!(
            path.exists(),
            "failed completion must retain cleanup authority"
        );
        assert!(!writer.fail_completion_once.load(Ordering::SeqCst));

        let original = std::fs::metadata(directory.path()).unwrap().permissions();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let cleanup = writer.abort();
        std::fs::set_permissions(directory.path(), original).unwrap();
        assert!(cleanup.is_err(), "unwritable parent must make unlink fail");
        assert!(path.exists(), "failed cleanup must retain the armed writer");
        writer.abort().unwrap();
        assert!(!path.exists());
        assert!(registry.lock().unwrap().reservations.is_empty());
    }
}
