//! Protected, host-owned full-output artifacts for developer commands.
//!
//! Command output is stored as JSON Lines so terminal controls remain escaped
//! and non-UTF-8 chunks remain exactly recoverable as hexadecimal bytes. The
//! model never chooses a path: the frontend supplies one protected directory
//! and the filesystem layer allocates a unique `0600` file without following
//! symlinks.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use kitsu_tools::builtins::fs::{create_unique_private_artifact, PrivateArtifactWriter};
use kitsu_tools::{
    CommandArtifactCompletion, CommandArtifactRequest, CommandOutputArtifact,
    CommandOutputArtifactSink, CommandOutputArtifactWriter, CommandOutputStream, ToolError,
};

/// Space held back so the bounded terminal record can always fit inside the
/// hard serialized artifact ceiling.
const FINISHED_RECORD_RESERVE_BYTES: u64 = 1024;

/// A sink whose destination can follow `/new` and `/resume` session switches.
#[derive(Clone, Default)]
pub struct CommandArtifactSink {
    directory: Arc<RwLock<Option<PathBuf>>>,
}

impl CommandArtifactSink {
    pub fn set_directory(&self, directory: Option<PathBuf>) {
        *self.directory.write().expect("command artifact directory") = directory;
    }

    #[cfg(test)]
    pub(crate) fn directory(&self) -> Option<PathBuf> {
        self.directory
            .read()
            .expect("command artifact directory")
            .clone()
    }
}

impl CommandOutputArtifactSink for CommandArtifactSink {
    fn begin(
        &self,
        request: &CommandArtifactRequest,
    ) -> Result<Arc<dyn CommandOutputArtifactWriter>, ToolError> {
        let directory = self
            .directory
            .read()
            .expect("command artifact directory")
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
        let mut writer =
            create_unique_private_artifact(&directory, "command-output").map_err(|error| {
                ToolError::ExecError(format!("cannot create command artifact: {error}"))
            })?;
        if let Err(error) = writer.write_all(&started) {
            let cleanup = writer.abort();
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
            Ok(()) => Ok(()),
            Err(error) => {
                // Preserve the armed writer for a later explicit retry and its
                // final Drop cleanup. `Ok` is reserved for verified removal.
                *guard = Some(state);
                Err(error)
            }
        }
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

    #[test]
    fn missing_directory_fails_before_allocating() {
        let error = match CommandArtifactSink::default().begin(&request()) {
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
        let writer = CommandArtifact {
            state: Mutex::new(Some(CommandArtifactState {
                writer: private,
                bytes_written: started.len() as u64,
                captured_bytes: 0,
                truncated: false,
                limit_bytes: request.limit_bytes,
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
    }
}
