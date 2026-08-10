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
        _request: &CommandArtifactRequest,
    ) -> Result<Arc<dyn CommandOutputArtifactWriter>, ToolError> {
        let directory = self
            .directory
            .read()
            .expect("command artifact directory")
            .clone()
            .ok_or_else(|| {
                ToolError::ExecError("protected command artifact directory is unavailable".into())
            })?;
        let mut writer =
            create_unique_private_artifact(&directory, "command-output").map_err(|error| {
                ToolError::ExecError(format!("cannot create command artifact: {error}"))
            })?;
        write_record(
            &mut writer,
            &serde_json::json!({
                "format": "kitsu.command-output.v1",
                "event": "started",
            }),
        )
        .map_err(|error| {
            ToolError::ExecError(format!("cannot initialize command artifact: {error}"))
        })?;
        Ok(Arc::new(CommandArtifact {
            writer: Mutex::new(Some(writer)),
        }))
    }
}

struct CommandArtifact {
    writer: Mutex<Option<PrivateArtifactWriter>>,
}

impl CommandOutputArtifactWriter for CommandArtifact {
    fn write_chunk(&self, stream: CommandOutputStream, chunk: &[u8]) -> std::io::Result<()> {
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("command artifact lock was poisoned"))?;
        let writer = guard
            .as_mut()
            .ok_or_else(|| std::io::Error::other("command artifact is already closed"))?;
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
        write_record(writer, &record)
    }

    fn complete(
        &self,
        completion: &CommandArtifactCompletion,
    ) -> std::io::Result<CommandOutputArtifact> {
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("command artifact lock was poisoned"))?;
        let mut writer = guard
            .take()
            .ok_or_else(|| std::io::Error::other("command artifact is already closed"))?;
        write_record(
            &mut writer,
            &serde_json::json!({
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
            }),
        )?;
        let path = writer.finish()?;
        Ok(CommandOutputArtifact {
            reference: path.to_string_lossy().into_owned(),
        })
    }

    fn abort(&self) -> std::io::Result<()> {
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("command artifact lock was poisoned"))?;
        if let Some(mut writer) = guard.take() {
            writer.abort()
        } else {
            Ok(())
        }
    }
}

fn write_record(
    writer: &mut PrivateArtifactWriter,
    record: &serde_json::Value,
) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, record).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")
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
}
