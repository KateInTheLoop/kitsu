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

/// Whether this process should write a state file at all.
///
/// `Off` never writes and `On` always does, regardless of how inference was
/// carried. `Auto` writes only for an interactive carrier, and an unknown
/// carrier — the early-startup case — fails closed, so a scripted, piped or
/// JSONL run makes no state write while the integration is at its default.
pub(crate) fn should_write(
    mode: crate::config::StateFileMode,
    carrier: Option<crate::process_exit::InferenceCarrier>,
) -> bool {
    match mode {
        crate::config::StateFileMode::Off => false,
        crate::config::StateFileMode::On => true,
        crate::config::StateFileMode::Auto => {
            carrier.is_some_and(crate::process_exit::InferenceCarrier::is_interactive)
        }
    }
}

/// Assembles the payload from the live registry so the call sites stay thin:
/// they supply only the identity and the turn, and every tally and thread comes
/// from one consistent read of the registry.
pub(crate) fn build(
    registry: &crate::subagent::AgentRegistry,
    pid: u32,
    cwd: String,
    session_id: Option<String>,
    turn_active: bool,
    updated_at: String,
) -> AgentStateFile {
    let snapshot = registry.snapshot();
    AgentStateFile {
        schema: AGENT_STATE_SCHEMA,
        pid,
        cwd,
        session_id,
        state: process_state(snapshot.running, snapshot.blocked, turn_active),
        counts: AgentStateCounts {
            running: snapshot.running,
            blocked: snapshot.blocked,
            unacknowledged: snapshot.unacknowledged,
            terminal: snapshot.terminal,
            failed: snapshot.failed,
        },
        threads: registry.thread_snapshot_dtos(),
        updated_at,
    }
}

/// Removes this process's state file however the owning scope ends, so no
/// return path, `?`, or panic can leave a stale hint behind. The guard creates
/// nothing on drop: a run that never wrote stays invisible.
pub(crate) struct StateFileGuard {
    dir: PathBuf,
    pid: u32,
}

impl StateFileGuard {
    pub(crate) fn new(dir: PathBuf, pid: u32) -> Self {
        Self { dir, pid }
    }
}

impl Drop for StateFileGuard {
    fn drop(&mut self) {
        remove(&self.dir, self.pid);
    }
}

/// Whether a process with this pid currently exists.
///
/// Signal 0 performs the permission and existence checks without delivering
/// anything. `EPERM` counts as alive: the process exists but belongs to
/// another user, and reporting another user's live kitsu as stale would be
/// worse than saying nothing. Only `ESRCH` (and an unrepresentable pid) means
/// dead.
#[cfg(unix)]
pub(crate) fn process_is_alive(pid: u32) -> bool {
    // pid 0 addresses the caller's whole process group, never a process, and
    // no state file legitimately carries it.
    if pid == 0 {
        return false;
    }
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: `kill` with signal 0 delivers nothing; it only reports whether
    // the pid exists and may be signalled. It cannot touch this process's
    // memory.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// On a non-unix host there is no cheap, non-invasive liveness check, so every
/// pid is reported alive: a stale row the user can dismiss beats hiding a
/// running process.
#[cfg(not(unix))]
pub(crate) fn process_is_alive(_pid: u32) -> bool {
    true
}

/// How much of one state file this binary could make sense of.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentStateEntryStatus {
    /// A `kitsu.agents.v1` body this binary understands.
    Ok,
    /// A readable JSON object carrying a schema tag this binary does not know,
    /// which is what a newer kitsu running alongside looks like.
    SchemaUnsupported,
    /// Unparseable bytes, a non-object body, or a pid that disagrees with the
    /// filename. Only the pid and its liveness are known.
    Unreadable,
}

/// One row of `kitsu agents`: what one state file says, or how far reading it
/// got. Every optional field is serialized even when absent, so a JSON
/// consumer tells "absent" from "field gone" by the explicit `null`.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct AgentStateEntry {
    /// Taken from the filename, which is the only pid the reader trusts.
    pub(crate) pid: u32,
    pub(crate) alive: bool,
    pub(crate) status: AgentStateEntryStatus,
    pub(crate) schema: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) counts: Option<AgentStateCounts>,
    pub(crate) updated_at: Option<String>,
    pub(crate) threads: Vec<crate::subagent::thread::AgentThreadSnapshotDto>,
    /// Thread records that failed to deserialize or validate. They cost
    /// themselves and nothing else, so a newer sibling never blanks the row.
    pub(crate) unsupported_threads: usize,
}

/// The owned, deliberately permissive counterpart of [`AgentStateFile`] for
/// the read path. [`AgentStateFile`] is `Serialize`-only — its `schema` is a
/// `&'static str` — so reading needs its own type, and that type must be
/// tolerant: no `deny_unknown_fields` (a v1 body from a newer kitsu may carry
/// extra keys and is still a v1 body), every field optional, and the threads
/// left as raw values so one unusable record cannot fail the whole entry.
#[derive(Debug, Default, serde::Deserialize)]
struct AgentStateFileRead {
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    counts: Option<AgentStateCountsRead>,
    #[serde(default)]
    threads: Vec<serde_json::Value>,
    #[serde(default)]
    updated_at: Option<String>,
}

/// The tallies as read: each one defaults, so an older or newer writer that
/// omits a counter yields a zero rather than an unreadable entry.
#[derive(Debug, Default, serde::Deserialize)]
struct AgentStateCountsRead {
    #[serde(default)]
    running: usize,
    #[serde(default)]
    blocked: usize,
    #[serde(default)]
    unacknowledged: usize,
    #[serde(default)]
    terminal: usize,
    #[serde(default)]
    failed: usize,
}

impl From<AgentStateCountsRead> for AgentStateCounts {
    fn from(read: AgentStateCountsRead) -> Self {
        Self {
            running: read.running,
            blocked: read.blocked,
            unacknowledged: read.unacknowledged,
            terminal: read.terminal,
            failed: read.failed,
        }
    }
}

/// The pid a directory entry's name encodes, when the name is exactly
/// `<digits>.json`. Everything else — the writer's own `.pid.nanos.n.tmp`
/// temporaries, a backup copy, a non-numeric name, a pid too large for `u32` —
/// yields `None` and is ignored silently: a listing that mistook a temporary
/// for a payload would be racy by construction.
fn pid_from_state_file_name(name: &std::ffi::OsStr) -> Option<u32> {
    let digits = name.to_str()?.strip_suffix(".json")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok()
}

/// An entry carrying only what the filename and a liveness check established.
fn unreadable_entry(pid: u32, alive: bool, status: AgentStateEntryStatus) -> AgentStateEntry {
    AgentStateEntry {
        pid,
        alive,
        status,
        schema: None,
        cwd: None,
        session_id: None,
        state: None,
        counts: None,
        updated_at: None,
        threads: Vec::new(),
        unsupported_threads: 0,
    }
}

/// Turns one file's bytes into an entry. Every failure becomes a status rather
/// than an error: the listing's job is to describe what is there.
fn entry_from_bytes(pid: u32, alive: bool, bytes: &[u8]) -> AgentStateEntry {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return unreadable_entry(pid, alive, AgentStateEntryStatus::Unreadable);
    };
    if !value.is_object() {
        return unreadable_entry(pid, alive, AgentStateEntryStatus::Unreadable);
    }
    let Ok(body) = serde_json::from_value::<AgentStateFileRead>(value) else {
        return unreadable_entry(pid, alive, AgentStateEntryStatus::Unreadable);
    };
    // A body whose pid disagrees with its filename is a copied or corrupted
    // file; trusting either number would be a guess, so neither is used.
    if body.pid != Some(pid) {
        return unreadable_entry(pid, alive, AgentStateEntryStatus::Unreadable);
    }
    if body.schema.as_deref() != Some(AGENT_STATE_SCHEMA) {
        let mut entry = unreadable_entry(pid, alive, AgentStateEntryStatus::SchemaUnsupported);
        // Reported verbatim: a newer kitsu alongside this one is precisely
        // what the user needs told, by name.
        entry.schema = body.schema;
        return entry;
    }

    let mut threads = Vec::new();
    let mut unsupported_threads = 0usize;
    for raw in body.threads {
        match serde_json::from_value::<crate::subagent::thread::AgentThreadSnapshotDto>(raw) {
            Ok(dto) if dto.validate_default().is_ok() => threads.push(dto),
            _ => unsupported_threads += 1,
        }
    }
    AgentStateEntry {
        pid,
        alive,
        status: AgentStateEntryStatus::Ok,
        schema: body.schema,
        cwd: body.cwd,
        session_id: body.session_id,
        state: body.state,
        counts: body.counts.map(AgentStateCounts::from),
        updated_at: body.updated_at,
        threads,
        unsupported_threads,
    }
}

/// Every state file in `dir`, sorted by pid ascending.
///
/// This is a pure read: nothing is created (not even `dir`), nothing is
/// deleted — not even an obviously stale file, because reaping would race a
/// live process whose pid was reused — and nothing is rewritten. A missing or
/// unreadable directory is an empty answer rather than an error, and no input
/// can make this panic: every fallible step either yields a status or drops
/// the file. `ensure_safe_directory` is deliberately *not* applied here; the
/// read mutates nothing, so refusing a directory on ownership grounds would
/// only discard information.
///
/// `is_alive` is injected so the caller's tests need not depend on the pids
/// this machine happens to be running; production passes
/// [`process_is_alive`]. A dead pid is dropped unless `include_dead`.
pub(crate) fn list_entries(
    dir: &Path,
    include_dead: bool,
    is_alive: &dyn Fn(u32) -> bool,
) -> Vec<AgentStateEntry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for item in read_dir.flatten() {
        let Some(pid) = pid_from_state_file_name(&item.file_name()) else {
            continue;
        };
        // A *directory* named `13.json` is not a state file either; reading it
        // would otherwise masquerade as a corrupt payload.
        if !item.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let alive = is_alive(pid);
        if !alive && !include_dead {
            continue;
        }
        entries.push(match std::fs::read(item.path()) {
            Ok(bytes) => entry_from_bytes(pid, alive, &bytes),
            Err(_) => unreadable_entry(pid, alive, AgentStateEntryStatus::Unreadable),
        });
    }
    entries.sort_by_key(|entry| entry.pid);
    entries
}

/// The human listing. Only the header glyph may be non-ASCII: these rows skip
/// `sanitize_plain_inline`, and the `--plain` tier promises zero non-ASCII
/// bytes, so every piece of static text below is ASCII.
pub(crate) fn format_agents_listing(entries: &[AgentStateEntry]) -> String {
    let mut rendered =
        kitsu_render::deck_header(&format!("kitsu agents — {} process(es)", entries.len()));
    if entries.is_empty() {
        rendered.push_str("\n  no kitsu process is publishing agent state");
        return rendered;
    }
    for entry in entries {
        let mut fields = Vec::new();
        if !entry.alive {
            fields.push("not running".to_string());
        }
        match entry.status {
            AgentStateEntryStatus::Ok => {
                if let Some(state) = &entry.state {
                    fields.push(state.clone());
                }
                if let Some(counts) = &entry.counts {
                    fields.push(format!("running {}", counts.running));
                    fields.push(format!("blocked {}", counts.blocked));
                    fields.push(format!("unacknowledged {}", counts.unacknowledged));
                }
                if let Some(cwd) = &entry.cwd {
                    fields.push(cwd.clone());
                }
            }
            AgentStateEntryStatus::SchemaUnsupported => match &entry.schema {
                Some(schema) => fields.push(format!("unsupported schema {schema}")),
                None => fields.push("unsupported schema".to_string()),
            },
            AgentStateEntryStatus::Unreadable => fields.push("unreadable state file".to_string()),
        }
        rendered.push_str("\n  pid ");
        rendered.push_str(&entry.pid.to_string());
        for field in fields {
            rendered.push_str(" | ");
            rendered.push_str(&field);
        }
    }
    rendered
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

    /// Every one of the seven carriers against every mode. `Auto` must fail
    /// closed: only a real interactive session writes, so a scripted, piped or
    /// one-shot `ask` run makes no state write at all, which is the roadmap
    /// invariant for an optional integration left disabled.
    #[test]
    fn should_write_only_opts_an_interactive_carrier_into_auto() {
        use crate::config::StateFileMode;
        use crate::process_exit::InferenceCarrier;

        const ALL: [InferenceCarrier; 7] = [
            InferenceCarrier::AskHuman,
            InferenceCarrier::AskJsonl,
            InferenceCarrier::PipedHuman,
            InferenceCarrier::PipedJsonl,
            InferenceCarrier::ScriptHuman,
            InferenceCarrier::ScriptJsonl,
            InferenceCarrier::InteractiveHuman,
        ];

        for carrier in ALL {
            assert!(
                !should_write(StateFileMode::Off, Some(carrier)),
                "off must never write, carrier {carrier:?}"
            );
            assert!(
                should_write(StateFileMode::On, Some(carrier)),
                "on must always write, carrier {carrier:?}"
            );
            assert_eq!(
                should_write(StateFileMode::Auto, Some(carrier)),
                carrier == InferenceCarrier::InteractiveHuman,
                "auto must follow interactivity only, carrier {carrier:?}"
            );
        }

        // Spelled out rather than only derived from the loop, so a change to
        // `is_interactive` cannot quietly take a non-interactive carrier with it.
        for carrier in [
            InferenceCarrier::ScriptHuman,
            InferenceCarrier::ScriptJsonl,
            InferenceCarrier::PipedHuman,
            InferenceCarrier::PipedJsonl,
            InferenceCarrier::AskHuman,
            InferenceCarrier::AskJsonl,
        ] {
            assert!(
                !should_write(StateFileMode::Auto, Some(carrier)),
                "auto must not write for {carrier:?}"
            );
        }
        assert!(should_write(
            StateFileMode::Auto,
            Some(InferenceCarrier::InteractiveHuman)
        ));
        assert!(should_write(
            StateFileMode::On,
            Some(InferenceCarrier::ScriptJsonl)
        ));
        assert!(!should_write(
            StateFileMode::Off,
            Some(InferenceCarrier::InteractiveHuman)
        ));
    }

    /// A missing carrier is the early-startup case, and it must fail closed
    /// under `Auto` while still honouring an explicit `On`/`Off`.
    #[test]
    fn should_write_fails_closed_when_the_carrier_is_unknown() {
        use crate::config::StateFileMode;

        assert!(
            !should_write(StateFileMode::Auto, None),
            "auto with no carrier must not write"
        );
        assert!(should_write(StateFileMode::On, None));
        assert!(!should_write(StateFileMode::Off, None));
    }

    #[test]
    fn build_carries_the_registry_counts_threads_and_identity_verbatim() {
        let registry = AgentRegistry::default();
        let id = registry.start("summarize the diff".to_string()).unwrap();
        registry.finish(id, Ok("done".to_string()));

        let payload = build(
            &registry,
            4321,
            "/workspace/kitsu".to_string(),
            Some("sess-7".to_string()),
            false,
            "2026-10-13T12:00:00Z".to_string(),
        );

        let snapshot = registry.snapshot();
        assert_eq!(payload.counts.running, snapshot.running);
        assert_eq!(payload.counts.blocked, snapshot.blocked);
        assert_eq!(payload.counts.unacknowledged, snapshot.unacknowledged);
        assert_eq!(payload.counts.terminal, snapshot.terminal);
        assert_eq!(payload.counts.failed, snapshot.failed);

        assert_eq!(payload.threads.len(), registry.thread_snapshot_dtos().len());
        assert_eq!(payload.threads.len(), 1, "one retained agent, one thread");
        for thread in &payload.threads {
            thread
                .validate_default()
                .expect("every built thread must pass the reader's validation");
        }

        assert_eq!(payload.schema, AGENT_STATE_SCHEMA);
        assert_eq!(payload.pid, 4321);
        assert_eq!(payload.cwd, "/workspace/kitsu");
        assert_eq!(payload.session_id.as_deref(), Some("sess-7"));
        assert_eq!(payload.updated_at, "2026-10-13T12:00:00Z");
        assert_eq!(
            payload.state,
            process_state(snapshot.running, snapshot.blocked, false),
            "state must be derived from the snapshot and the turn"
        );
    }

    /// The turn alone decides between `idle` and `working` when no agent is
    /// running. `blocked` is asserted through [`process_state`] directly
    /// because `AgentRegistrySnapshot::blocked` is still reserved and always
    /// zero, so no registry can currently produce that case.
    #[test]
    fn build_derives_idle_and_working_from_the_turn() {
        let registry = AgentRegistry::default();

        let working = build(
            &registry,
            1,
            "/tmp".to_string(),
            None,
            true,
            "now".to_string(),
        );
        assert_eq!(working.state, "working");
        assert!(working.threads.is_empty());
        assert_eq!(working.counts, AgentStateCounts::default());

        let idle = build(
            &registry,
            1,
            "/tmp".to_string(),
            None,
            false,
            "now".to_string(),
        );
        assert_eq!(idle.state, "idle");
        assert!(idle.session_id.is_none());

        assert_eq!(process_state(0, 1, false), "blocked");
        assert_eq!(process_state(0, 1, true), "blocked");
    }

    #[test]
    fn state_file_guard_removes_the_file_on_drop_and_keeps_the_directory() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("kitsu-state");
        write(&dir, 777, &payload(777, "idle", "now", Vec::new())).unwrap();
        let path = state_path(&dir, 777);
        assert!(path.is_file());

        {
            let _guard = StateFileGuard::new(dir.clone(), 777);
            assert!(path.is_file(), "the guard must not remove anything early");
        }

        assert!(!path.exists(), "dropping the guard must remove the file");
        assert!(dir.is_dir(), "the shared directory must survive the drop");
    }

    #[test]
    fn state_file_guard_drop_is_harmless_when_the_file_is_already_gone() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("kitsu-state");

        drop(StateFileGuard::new(dir.clone(), 1234));
        assert!(!dir.exists(), "the guard must not create the directory");

        std::fs::create_dir_all(&dir).unwrap();
        drop(StateFileGuard::new(dir.clone(), 1234));
        assert!(dir.is_dir());
        assert!(!state_path(&dir, 1234).exists());
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

    // ---------------------------------------------------------------------
    // Reader: `kitsu agents` (requirement H1).
    //
    // The reader is deliberately exercised through hand-written bytes rather
    // than through `write`: a reader whose only fixtures come from this
    // binary's own writer cannot show that a *foreign* file - a newer kitsu,
    // a half-copied file, a stray temporary - is tolerated, and tolerating
    // those is the whole job.
    // ---------------------------------------------------------------------

    /// A state directory with `name` holding exactly `body`. The bytes are
    /// written verbatim so a malformed fixture stays malformed.
    fn write_raw(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    /// A well-formed `kitsu.agents.v1` body, as JSON text.
    fn valid_body(pid: u32, state: &str, threads: Vec<serde_json::Value>) -> String {
        serde_json::to_string_pretty(&serde_json::json!({
            "schema": AGENT_STATE_SCHEMA,
            "pid": pid,
            "cwd": "/workspace/kitsu",
            "session_id": "sess-7",
            "state": state,
            "counts": {
                "running": 1,
                "blocked": 2,
                "unacknowledged": 3,
                "terminal": 4,
                "failed": 5,
            },
            "threads": threads,
            "updated_at": "2026-10-13T12:00:00Z",
        }))
        .unwrap()
    }

    /// Every pid is alive. Liveness is injected everywhere below so no test
    /// depends on the pids this machine happens to be running.
    fn all_alive(_pid: u32) -> bool {
        true
    }

    /// A pid that cannot be running. Linux caps pids at `pid_max`, which is at
    /// most 2^22, so `i32::MAX` is above every assignable pid and `kill` must
    /// answer `ESRCH` for it. Picking a number rather than a recently-exited
    /// child's pid avoids the pid-reuse race entirely: a pid that can never be
    /// allocated can never be recycled either.
    const IMPOSSIBLE_PID: u32 = i32::MAX as u32;

    /// The directory listing, sorted, so a before/after comparison is stable.
    fn listing(dir: &Path) -> Vec<String> {
        let mut names = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn list_entries_carries_every_field_of_a_well_formed_live_file() {
        let dir = tempfile::tempdir().unwrap();
        let dto = dto_from_registry();
        let thread_id = dto.thread.id;
        write_raw(
            dir.path(),
            "4321.json",
            &valid_body(4321, "working", vec![serde_json::to_value(&dto).unwrap()]),
        );

        let entries = list_entries(dir.path(), false, &all_alive);

        assert_eq!(entries.len(), 1, "one state file must yield one entry");
        let entry = &entries[0];
        assert_eq!(entry.pid, 4321);
        assert!(entry.alive);
        assert_eq!(entry.status, AgentStateEntryStatus::Ok);
        assert_eq!(entry.schema.as_deref(), Some(AGENT_STATE_SCHEMA));
        assert_eq!(entry.cwd.as_deref(), Some("/workspace/kitsu"));
        assert_eq!(entry.session_id.as_deref(), Some("sess-7"));
        assert_eq!(entry.state.as_deref(), Some("working"));
        assert_eq!(
            entry.counts,
            Some(AgentStateCounts {
                running: 1,
                blocked: 2,
                unacknowledged: 3,
                terminal: 4,
                failed: 5,
            })
        );
        assert_eq!(entry.updated_at.as_deref(), Some("2026-10-13T12:00:00Z"));
        assert_eq!(entry.threads.len(), 1, "the embedded thread must survive");
        assert_eq!(entry.threads[0].thread.id, thread_id);
        assert_eq!(entry.unsupported_threads, 0);
    }

    /// A dead pid is the stale-file case. It is noise by default and evidence
    /// under `--all`, and either way the file itself is left alone.
    #[test]
    fn list_entries_skips_a_dead_pid_unless_include_dead_is_set() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(
            dir.path(),
            "4321.json",
            &valid_body(4321, "idle", Vec::new()),
        );
        let dead = |_pid: u32| false;

        assert!(
            list_entries(dir.path(), false, &dead).is_empty(),
            "a dead pid must be skipped by default"
        );

        let entries = list_entries(dir.path(), true, &dead);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pid, 4321);
        assert!(!entries[0].alive, "include_dead must report alive: false");
        assert_eq!(entries[0].status, AgentStateEntryStatus::Ok);

        assert_eq!(listing(dir.path()), vec!["4321.json".to_string()]);
    }

    #[test]
    fn list_entries_sorts_by_pid_ascending_whatever_the_directory_order() {
        let dir = tempfile::tempdir().unwrap();
        for pid in [30u32, 4, 200] {
            write_raw(
                dir.path(),
                &format!("{pid}.json"),
                &valid_body(pid, "idle", Vec::new()),
            );
        }

        let pids = list_entries(dir.path(), false, &all_alive)
            .iter()
            .map(|entry| entry.pid)
            .collect::<Vec<_>>();

        assert_eq!(pids, vec![4, 30, 200], "output must be deterministic");
    }

    /// Only `<digits>.json` is a state file. The writer's own temporaries live
    /// in this directory, so mistaking one for a payload would make every
    /// listing racy.
    #[test]
    fn list_entries_ignores_everything_that_is_not_a_digits_dot_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let body = valid_body(12, "idle", Vec::new());
        write_raw(dir.path(), ".12.99.0.tmp", &body);
        write_raw(dir.path(), "notanumber.json", &body);
        write_raw(dir.path(), "12", &body);
        write_raw(dir.path(), "12.json.bak", &body);
        std::fs::create_dir_all(dir.path().join("13.json")).unwrap();

        assert!(
            list_entries(dir.path(), true, &all_alive).is_empty(),
            "no non-state-file may produce an entry"
        );
    }

    #[test]
    fn list_entries_reports_unparseable_bytes_as_one_unreadable_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(dir.path(), "21.json", "{not json at all");

        let entries = list_entries(dir.path(), false, &all_alive);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pid, 21);
        assert!(entries[0].alive);
        assert_eq!(entries[0].status, AgentStateEntryStatus::Unreadable);
        assert!(entries[0].schema.is_none());
        assert!(entries[0].cwd.is_none());
        assert!(entries[0].state.is_none());
        assert!(entries[0].counts.is_none());
        assert!(entries[0].threads.is_empty());
    }

    #[test]
    fn list_entries_reports_a_json_array_body_as_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(dir.path(), "22.json", "[{\"schema\":\"kitsu.agents.v1\"}]");

        let entries = list_entries(dir.path(), false, &all_alive);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pid, 22);
        assert_eq!(entries[0].status, AgentStateEntryStatus::Unreadable);
    }

    /// A body whose pid disagrees with its filename is a copied or corrupted
    /// file. Both numbers are then suspect, so the reader refuses to pick one.
    #[test]
    fn list_entries_refuses_a_body_whose_pid_disagrees_with_the_filename() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(dir.path(), "31.json", &valid_body(32, "idle", Vec::new()));

        let entries = list_entries(dir.path(), false, &all_alive);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pid, 31, "the filename is the only pid shown");
        assert_eq!(entries[0].status, AgentStateEntryStatus::Unreadable);
        assert!(entries[0].state.is_none());
    }

    /// A newer kitsu running alongside is exactly what the user must be told
    /// about, so an unknown schema is surfaced by name instead of dropped.
    #[test]
    fn list_entries_surfaces_an_unknown_schema_by_name_without_dropping_it() {
        let dir = tempfile::tempdir().unwrap();
        let body = serde_json::to_string(&serde_json::json!({
            "schema": "kitsu.agents.v2",
            "pid": 41,
            "cwd": "/workspace/kitsu",
            "state": "whatever-v2-calls-it",
        }))
        .unwrap();
        write_raw(dir.path(), "41.json", &body);

        let entries = list_entries(dir.path(), false, &all_alive);
        assert_eq!(entries.len(), 1, "a newer schema must not be dropped");
        assert_eq!(entries[0].pid, 41);
        assert!(entries[0].alive);
        assert_eq!(entries[0].status, AgentStateEntryStatus::SchemaUnsupported);
        assert_eq!(
            entries[0].schema.as_deref(),
            Some("kitsu.agents.v2"),
            "the schema found must be reported verbatim"
        );
    }

    /// The forward-compatibility property that matters most: a thread record
    /// this binary cannot validate costs that one record and nothing else. A
    /// newer sibling must never blank out the listing.
    #[test]
    fn list_entries_keeps_valid_threads_and_counts_only_the_rejected_ones() {
        let dir = tempfile::tempdir().unwrap();
        let dto = dto_from_registry();
        let thread_id = dto.thread.id;
        let valid = serde_json::to_value(&dto).unwrap();
        let future_version = serde_json::json!({
            "schema_version": 99,
            "thread": valid["thread"].clone(),
        });
        let structurally_wrong = serde_json::json!({ "not": "a thread at all" });
        write_raw(
            dir.path(),
            "51.json",
            &valid_body(
                51,
                "working",
                vec![valid.clone(), future_version, structurally_wrong],
            ),
        );

        let entries = list_entries(dir.path(), false, &all_alive);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(
            entry.status,
            AgentStateEntryStatus::Ok,
            "an unusable thread must not fail the entry"
        );
        assert_eq!(entry.threads.len(), 1, "exactly the valid thread survives");
        assert_eq!(
            entry.threads[0].thread.id, thread_id,
            "the surviving thread must be the valid one"
        );
        assert_eq!(entry.unsupported_threads, 2);
        assert_eq!(entry.state.as_deref(), Some("working"));
    }

    /// No kitsu is running, or none is publishing: that is an empty answer,
    /// not an error, and the reader must not helpfully create the directory.
    #[test]
    fn list_entries_returns_empty_for_a_missing_directory_and_creates_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("absent");

        assert!(list_entries(&dir, true, &all_alive).is_empty());

        assert!(!dir.exists(), "the reader must not create {dir:?}");
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    /// `kitsu agents` is a read: it reclaims nothing, not even an obviously
    /// stale file belonging to a pid that is gone. Reaping is another story's
    /// job, and doing it here would race a live process whose pid was reused.
    #[test]
    fn list_entries_never_mutates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_raw(dir.path(), "61.json", &valid_body(61, "idle", Vec::new()));
        write_raw(dir.path(), "62.json", &valid_body(62, "idle", Vec::new()));
        write_raw(dir.path(), "63.json", "{corrupt");
        write_raw(dir.path(), ".62.1.0.tmp", "half written");
        let before = listing(dir.path());

        // 62 is the stale one: a well-formed file whose process is gone.
        let is_alive = |pid: u32| pid != 62;
        let _ = list_entries(dir.path(), false, &is_alive);
        let _ = list_entries(dir.path(), true, &is_alive);

        assert_eq!(
            listing(dir.path()),
            before,
            "the reader must leave the directory untouched"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("63.json")).unwrap(),
            "{corrupt"
        );
    }

    fn ok_entry(pid: u32, alive: bool) -> AgentStateEntry {
        AgentStateEntry {
            pid,
            alive,
            status: AgentStateEntryStatus::Ok,
            schema: Some(AGENT_STATE_SCHEMA.to_string()),
            cwd: Some("/workspace/kitsu".to_string()),
            session_id: Some("sess-7".to_string()),
            state: Some("blocked".to_string()),
            counts: Some(AgentStateCounts {
                running: 1,
                blocked: 2,
                unacknowledged: 3,
                terminal: 4,
                failed: 5,
            }),
            updated_at: Some("2026-10-13T12:00:00Z".to_string()),
            threads: Vec::new(),
            unsupported_threads: 0,
        }
    }

    fn bare_entry(
        pid: u32,
        status: AgentStateEntryStatus,
        schema: Option<&str>,
    ) -> AgentStateEntry {
        AgentStateEntry {
            pid,
            alive: true,
            status,
            schema: schema.map(str::to_string),
            cwd: None,
            session_id: None,
            state: None,
            counts: None,
            updated_at: None,
            threads: Vec::new(),
            unsupported_threads: 0,
        }
    }

    /// These rows do not pass through `sanitize_plain_inline`, so their static
    /// text must already be ASCII in every tier; only the header glyph may be
    /// non-ASCII, and a non-TTY never gets escape bytes.
    #[test]
    fn format_agents_listing_renders_a_row_per_entry_in_pure_ascii() {
        let entries = vec![
            ok_entry(4321, true),
            ok_entry(4322, false),
            bare_entry(
                41,
                AgentStateEntryStatus::SchemaUnsupported,
                Some("kitsu.agents.v2"),
            ),
            bare_entry(21, AgentStateEntryStatus::Unreadable, None),
        ];

        let rendered = format_agents_listing(&entries);

        assert!(!rendered.contains('\u{1b}'), "{rendered}");
        if kitsu_render::plain_mode() {
            assert!(rendered.starts_with("# kitsu agents"), "{rendered}");
        } else {
            assert!(rendered.starts_with("\u{25c8} kitsu agents"), "{rendered}");
        }
        assert!(rendered.contains("4 process(es)"), "{rendered}");

        // Everything after the header line is static text plus payload, and
        // all of it must be ASCII.
        let body = rendered
            .split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or("");
        assert!(body.is_ascii(), "rows must be pure ASCII: {body:?}");

        assert!(
            rendered.contains(
                "  pid 4321 | blocked | running 1 | blocked 2 | unacknowledged 3 | /workspace/kitsu"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("  pid 4322 | not running | blocked | running 1 | blocked 2 | unacknowledged 3 | /workspace/kitsu"),
            "a dead entry included by --all must be marked: {rendered}"
        );
        assert!(
            rendered.contains("  pid 41 | unsupported schema kitsu.agents.v2"),
            "{rendered}"
        );
        assert!(
            rendered.contains("  pid 21 | unreadable state file"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("no kitsu process is publishing agent state"),
            "{rendered}"
        );
    }

    #[test]
    fn format_agents_listing_says_so_when_nothing_is_publishing() {
        let rendered = format_agents_listing(&[]);

        assert!(!rendered.contains('\u{1b}'), "{rendered}");
        assert!(rendered.contains("0 process(es)"), "{rendered}");
        assert!(
            rendered.contains("  no kitsu process is publishing agent state"),
            "{rendered}"
        );
        assert!(!rendered.contains("pid "), "no agent rows: {rendered}");
        let body = rendered
            .split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or("");
        assert!(body.is_ascii(), "{body:?}");
    }

    /// The one test that must touch real pids, because the point is the real
    /// syscall. `EPERM` counts as alive too: a state file owned by another
    /// user's live kitsu must not be reported as stale.
    #[test]
    fn process_is_alive_answers_for_this_process_and_for_an_unassignable_pid() {
        assert!(
            process_is_alive(std::process::id()),
            "this very process must be reported alive"
        );
        assert!(
            !process_is_alive(IMPOSSIBLE_PID),
            "pid {IMPOSSIBLE_PID} is above every assignable pid and must be dead"
        );
    }

    /// The envelope's own forward compatibility, the mirror of the thread-level
    /// test above: a `kitsu.agents.v1` file from a newer kitsu that carries one
    /// extra top-level key is still a v1 file and must still read. A strict
    /// body - `deny_unknown_fields` on the read type - would report it as
    /// `Unreadable` and blind the listing to the very sibling process it exists
    /// to show, so this test fails loudly if anyone reaches for that.
    #[test]
    fn list_entries_tolerates_an_unknown_top_level_field_on_a_v1_body() {
        let dir = tempfile::tempdir().unwrap();
        let dto = dto_from_registry();
        let thread_id = dto.thread.id;
        let mut body: serde_json::Value = serde_json::from_str(&valid_body(
            71,
            "working",
            vec![serde_json::to_value(&dto).unwrap()],
        ))
        .unwrap();
        body.as_object_mut().unwrap().insert(
            "future_field".to_string(),
            serde_json::json!({ "anything": 1 }),
        );
        write_raw(
            dir.path(),
            "71.json",
            &serde_json::to_string_pretty(&body).unwrap(),
        );

        let entries = list_entries(dir.path(), false, &all_alive);

        assert_eq!(entries.len(), 1, "an extra key must not drop the entry");
        let entry = &entries[0];
        assert_eq!(
            entry.status,
            AgentStateEntryStatus::Ok,
            "a v1 body with an unknown key is still readable"
        );
        assert_eq!(entry.pid, 71);
        assert!(entry.alive);
        assert_eq!(entry.schema.as_deref(), Some(AGENT_STATE_SCHEMA));
        assert_eq!(entry.cwd.as_deref(), Some("/workspace/kitsu"));
        assert_eq!(entry.session_id.as_deref(), Some("sess-7"));
        assert_eq!(entry.state.as_deref(), Some("working"));
        assert_eq!(
            entry.counts,
            Some(AgentStateCounts {
                running: 1,
                blocked: 2,
                unacknowledged: 3,
                terminal: 4,
                failed: 5,
            })
        );
        assert_eq!(entry.updated_at.as_deref(), Some("2026-10-13T12:00:00Z"));
        assert_eq!(entry.threads.len(), 1, "the valid thread must be kept");
        assert_eq!(entry.threads[0].thread.id, thread_id);
        assert_eq!(entry.unsupported_threads, 0);
    }
}
