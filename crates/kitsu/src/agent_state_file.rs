//! The per-process agent state file: a small JSON hint, one file per running
//! kitsu, that an outside tool such as a terminal multiplexer can read to tell
//! whether this process is idle, working, or waiting on the user.
//!
//! kitsu runs its background agents in-process, so nothing outside the process
//! can observe them; without this file a herder has to classify kitsu as
//! `unknown`. The file is written best effort into a runtime directory and
//! deleted on exit, so it is **advisory, not authoritative**: it may be stale,
//! absent, or left behind by a process that died, and a reader must treat it as
//! a hint rather than a source of truth. Writing it must never break a turn,
//! so every failure is reported as an `Err(String)` the caller may ignore and
//! nothing here panics.

// Private writer staged ahead of its callers; the turn lifecycle, the config
// keys, and the reader that consume it are later cards.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Schema tag carried by every state file, so a reader can refuse a payload it
/// does not understand instead of guessing.
pub(crate) const AGENT_STATE_SCHEMA: &str = "kitsu.agents.v1";

/// The background-agent tallies a reader needs to describe the process without
/// understanding individual threads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub(crate) struct AgentStateCounts {
    pub(crate) running: usize,
    pub(crate) blocked: usize,
    pub(crate) unacknowledged: usize,
    pub(crate) terminal: usize,
    pub(crate) failed: usize,
}

/// The whole payload of one process's state file.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct AgentStateFile {
    pub(crate) schema: &'static str,
    pub(crate) pid: u32,
    pub(crate) cwd: String,
    /// The session this process is attached to, when it has one. Serialized
    /// even when absent: a reader distinguishes "no session" from "field gone"
    /// by the explicit `null`, so this field is never skipped.
    pub(crate) session_id: Option<String>,
    pub(crate) state: &'static str,
    pub(crate) counts: AgentStateCounts,
    pub(crate) threads: Vec<crate::subagent::thread::AgentThreadSnapshotDto>,
    pub(crate) updated_at: String,
}

/// True when an environment value is worth using as a directory root: present,
/// non-empty, and absolute. A relative or empty value falls through to the next
/// candidate rather than resolving against an arbitrary working directory.
fn usable(value: Option<&OsStr>) -> Option<&Path> {
    let path = Path::new(value?);
    (!path.as_os_str().is_empty() && path.is_absolute()).then_some(path)
}

/// Picks the directory that holds the state files, preferring the per-user
/// runtime directory. The `TMPDIR` and `/tmp` fallbacks are shared between
/// users, so they carry the uid in the directory name to keep two users from
/// colliding there.
pub(crate) fn state_dir_from(xdg: Option<&OsStr>, tmpdir: Option<&OsStr>, uid: u32) -> PathBuf {
    if let Some(xdg) = usable(xdg) {
        return xdg.join("kitsu");
    }
    let root = usable(tmpdir).unwrap_or(Path::new("/tmp"));
    root.join(format!("kitsu-{uid}"))
}

/// [`state_dir_from`] against the real environment and effective uid.
pub(crate) fn state_dir() -> PathBuf {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR");
    let tmpdir = std::env::var_os("TMPDIR");
    state_dir_from(xdg.as_deref(), tmpdir.as_deref(), effective_uid())
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` reads process credentials and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn effective_uid() -> u32 {
    0
}

/// The state file for one pid inside `dir`.
pub(crate) fn state_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.json"))
}

/// Collapses the agent tallies and the turn into the one word a reader shows.
/// Blocked outranks working, matching the precedence of the live status row's
/// tag: a process waiting on the user is the thing a herder must surface.
pub(crate) fn process_state(running: usize, blocked: usize, turn_active: bool) -> &'static str {
    if blocked > 0 {
        "blocked"
    } else if turn_active || running > 0 {
        "working"
    } else {
        "idle"
    }
}

/// Rejects a state directory that an attacker could have prepared: the
/// `/tmp` fallback lives under a world-writable parent, so an existing path
/// there must be a real directory (never a symlink to somewhere else, hence
/// `symlink_metadata`), owned by us, and not writable by group or world.
#[cfg(unix)]
fn ensure_safe_directory(dir: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "cannot inspect agent state directory '{}': {error}",
                dir.display()
            ))
        }
    };
    if !metadata.is_dir() {
        return Err(format!(
            "agent state directory '{}' is not a directory",
            dir.display()
        ));
    }
    if metadata.uid() != effective_uid() {
        return Err(format!(
            "agent state directory '{}' is not owned by this user",
            dir.display()
        ));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(format!(
            "agent state directory '{}' is group- or world-writable",
            dir.display()
        ));
    }
    Ok(true)
}

#[cfg(not(unix))]
fn ensure_safe_directory(dir: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "agent state directory '{}' is not a directory",
            dir.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "cannot inspect agent state directory '{}': {error}",
            dir.display()
        )),
    }
}

/// Creates the state directory when it is missing, owner-only. `create_dir_all`
/// does not reliably honour the umask here, so the mode is set explicitly
/// afterwards rather than trusted.
fn create_state_directory(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| {
        format!(
            "cannot create agent state directory '{}': {error}",
            dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "cannot restrict agent state directory '{}': {error}",
                dir.display()
            )
        })?;
    }
    Ok(())
}

/// Opens a fresh temporary file in `dir`, owner-only from creation so the
/// payload is never briefly readable by anyone else.
fn create_temporary(dir: &Path, pid: u32) -> Result<(PathBuf, std::fs::File), String> {
    let mut last = String::new();
    for attempt in 0..16u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0);
        let path = dir.join(format!(".{pid}.{nanos}.{attempt}.tmp"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last = error.to_string();
            }
            Err(error) => {
                return Err(format!(
                    "cannot create agent state temporary '{}': {error}",
                    path.display()
                ))
            }
        }
    }
    Err(format!(
        "cannot create a unique agent state temporary in '{}': {last}",
        dir.display()
    ))
}

/// Writes `payload` as the state file for `pid`, replacing any previous one
/// atomically: the directory is created owner-only when missing and refused
/// when it is unsafe, the payload goes into a fresh owner-only temporary in the
/// same directory, and a rename puts it in place. A failure anywhere returns an
/// `Err` and leaves no temporary behind, because this file is a hint and must
/// never cost the caller a turn.
pub(crate) fn write(dir: &Path, pid: u32, payload: &AgentStateFile) -> Result<(), String> {
    use std::io::Write as _;

    if !ensure_safe_directory(dir)? {
        create_state_directory(dir)?;
    }

    let encoded = serde_json::to_vec(payload)
        .map_err(|error| format!("cannot encode the agent state file: {error}"))?;
    let (temporary, mut file) = create_temporary(dir, pid)?;
    let path = state_path(dir, pid);
    let outcome = file
        .write_all(&encoded)
        .and_then(|()| file.flush())
        .map_err(|error| {
            format!(
                "cannot write agent state temporary '{}': {error}",
                temporary.display()
            )
        });
    drop(file);
    if let Err(error) = outcome {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "cannot publish agent state file '{}': {error}",
            path.display()
        ));
    }
    Ok(())
}

/// Deletes the state file for `pid`, leaving the directory for the other
/// processes that share it. Nothing here reports failure: an absent file is the
/// desired end state, and a stale one is a hint a reader already tolerates.
pub(crate) fn remove(dir: &Path, pid: u32) {
    let _ = std::fs::remove_file(state_path(dir, pid));
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::subagent::{self, AgentRegistry};

    fn payload(
        pid: u32,
        state: &'static str,
        updated_at: &str,
        threads: Vec<subagent::thread::AgentThreadSnapshotDto>,
    ) -> AgentStateFile {
        AgentStateFile {
            schema: AGENT_STATE_SCHEMA,
            pid,
            cwd: "/workspace/kitsu".to_string(),
            session_id: None,
            state,
            counts: AgentStateCounts {
                running: 1,
                blocked: 2,
                unacknowledged: 3,
                terminal: 4,
                failed: 5,
            },
            threads,
            updated_at: updated_at.to_string(),
        }
    }

    fn dto_from_registry() -> subagent::thread::AgentThreadSnapshotDto {
        let registry = AgentRegistry::default();
        let id = registry.start("summarize the diff".to_string()).unwrap();
        registry.finish(id, Ok("done".to_string()));
        let mut dtos = registry.thread_snapshot_dtos();
        assert_eq!(dtos.len(), 1, "one started agent must yield one thread dto");
        dtos.remove(0)
    }

    #[test]
    fn state_dir_from_prefers_an_absolute_xdg_runtime_dir() {
        let dir = state_dir_from(
            Some(OsStr::new("/run/user/1000")),
            Some(OsStr::new("/var/tmp")),
            1000,
        );
        assert_eq!(dir, PathBuf::from("/run/user/1000/kitsu"));
        assert!(
            !dir.to_string_lossy().contains("1000/kitsu-"),
            "the xdg path must not carry a uid suffix: {dir:?}"
        );
        assert_eq!(dir.file_name().unwrap(), OsStr::new("kitsu"));
    }

    #[test]
    fn state_dir_from_falls_through_an_empty_xdg_runtime_dir_to_tmpdir() {
        let dir = state_dir_from(Some(OsStr::new("")), Some(OsStr::new("/var/tmp")), 1000);
        assert_eq!(dir, PathBuf::from("/var/tmp/kitsu-1000"));
    }

    #[test]
    fn state_dir_from_falls_through_a_relative_xdg_runtime_dir_to_tmpdir() {
        let dir = state_dir_from(
            Some(OsStr::new("relative/run")),
            Some(OsStr::new("/var/tmp")),
            4242,
        );
        assert_eq!(dir, PathBuf::from("/var/tmp/kitsu-4242"));
    }

    #[test]
    fn state_dir_from_uses_tmpdir_with_the_uid_when_xdg_is_absent() {
        let dir = state_dir_from(None, Some(OsStr::new("/var/tmp")), 7);
        assert_eq!(dir, PathBuf::from("/var/tmp/kitsu-7"));
        assert!(dir.file_name().unwrap().to_string_lossy().contains("7"));
    }

    #[test]
    fn state_dir_from_falls_back_to_slash_tmp_when_neither_is_set() {
        let dir = state_dir_from(None, None, 1000);
        assert_eq!(dir, PathBuf::from("/tmp/kitsu-1000"));
    }

    #[test]
    fn state_dir_from_ignores_a_relative_or_empty_tmpdir() {
        assert_eq!(
            state_dir_from(None, Some(OsStr::new("tmp")), 1000),
            PathBuf::from("/tmp/kitsu-1000")
        );
        assert_eq!(
            state_dir_from(None, Some(OsStr::new("")), 1000),
            PathBuf::from("/tmp/kitsu-1000")
        );
    }

    #[test]
    fn state_dir_reads_the_real_environment_and_names_kitsu() {
        let dir = state_dir();
        assert!(dir.is_absolute(), "state dir must be absolute: {dir:?}");
        let last = dir.file_name().unwrap().to_string_lossy().to_string();
        assert!(last.contains("kitsu"), "last component was {last}");
    }

    #[test]
    fn state_path_composes_pid_dot_json() {
        assert_eq!(
            state_path(Path::new("/run/user/1000/kitsu"), 4321),
            PathBuf::from("/run/user/1000/kitsu/4321.json")
        );
    }

    #[test]
    fn process_state_puts_blocked_above_working_and_idle() {
        assert_eq!(process_state(0, 0, false), "idle");
        assert_eq!(process_state(0, 0, true), "working");
        assert_eq!(process_state(2, 0, false), "working");
        assert_eq!(process_state(2, 0, true), "working");
        assert_eq!(process_state(0, 1, false), "blocked");
        assert_eq!(process_state(3, 1, true), "blocked");
    }

    #[test]
    fn write_creates_a_private_directory_and_file_with_the_documented_json() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("kitsu-state");
        let dto = dto_from_registry();
        let payload = payload(4321, "working", "2026-10-13T12:00:00Z", vec![dto.clone()]);

        write(&dir, 4321, &payload).expect("write into a fresh directory must succeed");

        let path = state_path(&dir, 4321);
        assert!(path.is_file(), "state file must exist at {path:?}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["schema"], "kitsu.agents.v1");
        assert_eq!(value["pid"], 4321);
        assert_eq!(value["cwd"], "/workspace/kitsu");
        assert!(value["session_id"].is_null());
        assert_eq!(value["state"], "working");
        assert_eq!(value["counts"]["running"], 1);
        assert_eq!(value["counts"]["blocked"], 2);
        assert_eq!(value["counts"]["unacknowledged"], 3);
        assert_eq!(value["counts"]["terminal"], 4);
        assert_eq!(value["counts"]["failed"], 5);
        assert_eq!(value["updated_at"], "2026-10-13T12:00:00Z");
        assert_eq!(value["threads"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn embedded_thread_dtos_survive_a_round_trip_and_validate() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("kitsu-state");
        let dto = dto_from_registry();
        let payload = payload(99, "idle", "2026-10-13T12:00:01Z", vec![dto]);

        write(&dir, 99, &payload).unwrap();

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(state_path(&dir, 99)).unwrap()).unwrap();
        let threads = value["threads"].as_array().unwrap();
        assert!(!threads.is_empty(), "the fixture must embed a thread");
        for thread in threads {
            let decoded: subagent::thread::AgentThreadSnapshotDto =
                serde_json::from_value(thread.clone())
                    .expect("every embedded thread must deserialize as a thread dto");
            decoded
                .validate_default()
                .expect("every embedded thread must pass the reader's validation");
        }
    }

    #[test]
    fn schema_is_pinned_and_an_absent_session_id_serializes_as_null() {
        assert_eq!(AGENT_STATE_SCHEMA, "kitsu.agents.v1");
        let payload = payload(1, "idle", "2026-10-13T12:00:02Z", Vec::new());
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["schema"], "kitsu.agents.v1");
        assert!(
            value.get("session_id").is_some() && value["session_id"].is_null(),
            "session_id must be present and null when absent: {value}"
        );

        let mut with_session = payload;
        with_session.session_id = Some("sess-7".to_string());
        let value = serde_json::to_value(&with_session).unwrap();
        assert_eq!(value["session_id"], "sess-7");
    }

    #[test]
    fn write_replaces_atomically_and_leaves_no_temporary_behind() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("kitsu-state");

        write(&dir, 10, &payload(10, "idle", "first", Vec::new())).unwrap();
        write(&dir, 10, &payload(10, "blocked", "second", Vec::new())).unwrap();

        let entries = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec!["10.json".to_string()], "no residue allowed");

        let path = state_path(&dir, 10);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["updated_at"], "second");
        assert_eq!(value["state"], "blocked");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_refuses_a_group_or_world_writable_directory() {
        use std::os::unix::fs::PermissionsExt as _;

        for mode in [0o770u32, 0o707u32, 0o777u32] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            let error = write(dir.path(), 11, &payload(11, "idle", "now", Vec::new()));
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(
                error.is_err(),
                "mode {mode:o} must be refused, got {error:?}"
            );
            assert!(
                !state_path(dir.path(), 11).exists(),
                "a refused directory must not get a state file"
            );
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn write_refuses_a_regular_file_where_the_directory_belongs() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("not-a-directory");
        std::fs::write(&dir, b"occupied").unwrap();

        let error = write(&dir, 12, &payload(12, "idle", "now", Vec::new()));
        assert!(error.is_err(), "a regular file must be refused: {error:?}");
        assert_eq!(std::fs::read_to_string(&dir).unwrap(), "occupied");
    }

    #[test]
    fn remove_deletes_the_file_keeps_the_directory_and_is_idempotent() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("kitsu-state");
        write(&dir, 13, &payload(13, "idle", "now", Vec::new())).unwrap();
        let path = state_path(&dir, 13);
        assert!(path.exists());

        remove(&dir, 13);
        assert!(!path.exists(), "remove must delete the state file");
        assert!(dir.is_dir(), "remove must leave the directory in place");

        remove(&dir, 13);
        remove(&parent.path().join("absent"), 13);
    }

    #[cfg(unix)]
    #[test]
    fn write_returns_err_when_the_directory_cannot_be_created() {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("kitsu-state");
        let original = std::fs::metadata(parent.path()).unwrap().permissions();
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

        let error = write(&dir, 14, &payload(14, "idle", "now", Vec::new()));

        std::fs::set_permissions(parent.path(), original).unwrap();
        assert!(
            error.is_err(),
            "an unwritable parent must yield Err, got {error:?}"
        );
        assert!(!dir.exists());
    }
}
