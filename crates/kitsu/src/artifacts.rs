//! Cross-run inventory and pruning of on-disk command-artifact storage.
//!
//! Two namespaces accumulate generation directories inside Kitsu-private state:
//!
//! * `<state>/artifacts/runtime-<pid>-<nanos>-<gen>` — stateless runs with the
//!   command tool enabled. These deliberately survive process exit so a one-shot
//!   automation run can still read the full captured output afterwards, which
//!   also means nothing ever removed them.
//! * `<state>/sessions/<id>.artifacts` — a durable session's sidecar. When a
//!   chat dies before committing its first snapshot the sidecar outlives the
//!   `<id>.json` that would have owned it.
//!
//! Everything here is host-only. `kitsu artifacts` runs outside any chat, no
//! model authority is involved, and none is created: these paths are protected
//! read roots that the model never learns exist.
//!
//! Safety posture, in priority order: **when liveness is ambiguous, keep the
//! data.** Removal requires (a) an exact name match against the generation
//! patterns above, (b) a real directory, never a symlink, and (c) proof that no
//! live process holds the directory — an advisory `flock` the kernel releases on
//! process death, including `SIGKILL`, rather than a pid check (pids are reused)
//! or a marker file (a killed process never removes one).

use std::path::{Path, PathBuf};

/// Lock file each live generation directory holds open for its lifetime.
pub const GENERATION_LOCK_NAME: &str = ".lock";
const RUNTIME_PREFIX: &str = "runtime-";
const ARTIFACTS_SUFFIX: &str = ".artifacts";
const LOCKS_DIR_NAME: &str = ".locks";

/// Generations without a lock file predate the lock and cannot be distinguished
/// from live ones by inspection. Treat them as prunable only once they are old
/// enough that a live owner is implausible.
pub const UNLOCKED_GRACE_SECS: u64 = 3600;

/// Window between `create_dir` and the lock being taken by the owning process.
/// An unlocked directory younger than this may belong to a process that is
/// still starting up, so even an empty one is left alone.
pub const UNLOCKED_ALLOCATION_GRACE_SECS: u64 = 60;

/// Startup says something only when the litter is worth a line.
pub const NOTICE_GENERATION_THRESHOLD: usize = 16;
pub const NOTICE_BYTE_THRESHOLD: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    /// `<state>/artifacts` — stateless command runs.
    Runtime,
    /// `<state>/sessions` — durable session sidecars.
    Sessions,
}

impl Namespace {
    pub fn label(self) -> &'static str {
        match self {
            Namespace::Runtime => "runtime",
            Namespace::Sessions => "sessions",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// A live process holds the generation lock.
    Live,
    /// The lock exists and is free.
    Free,
    /// No lock file: a pre-upgrade generation, or one whose owner never got far
    /// enough to create it.
    Unlocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Nothing owns it any more.
    Orphaned,
    /// A durable session still refers to this sidecar.
    SessionExists,
    /// A live process is using it right now.
    InUse,
}

impl Retention {
    pub fn label(self) -> &'static str {
        match self {
            Retention::Orphaned => "orphaned",
            Retention::SessionExists => "session exists",
            Retention::InUse => "in use by a live process",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Generation {
    pub id: String,
    pub path: PathBuf,
    pub namespace: Namespace,
    pub files: usize,
    pub bytes: u64,
    /// Seconds since the directory was last modified, or `None` when unknown.
    pub age_secs: Option<u64>,
    pub liveness: Liveness,
    pub retention: Retention,
}

impl Generation {
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }

    /// Provably safe to remove without consulting the user: nothing is in it,
    /// nothing owns it, and no live process holds it.
    ///
    /// A directory with no lock file yet may be one a live process created
    /// microseconds ago and has not locked; a short grace period closes that
    /// window without keeping worthless directories around for an hour.
    pub fn is_empty_orphan(&self) -> bool {
        self.is_empty()
            && self.retention == Retention::Orphaned
            && match self.liveness {
                Liveness::Live => false,
                Liveness::Free => true,
                Liveness::Unlocked => self
                    .age_secs
                    .is_some_and(|age| age >= UNLOCKED_ALLOCATION_GRACE_SECS),
            }
    }

    /// Safe to remove on an explicit `kitsu artifacts prune`. Ambiguity always
    /// resolves to keeping the data.
    pub fn prunable(&self) -> bool {
        if self.retention != Retention::Orphaned {
            return false;
        }
        match self.liveness {
            Liveness::Live => false,
            Liveness::Free => true,
            // A pre-upgrade generation is only prunable once it is old enough
            // that a live owner is implausible; an unknown age never is.
            Liveness::Unlocked => self.age_secs.is_some_and(|age| age >= UNLOCKED_GRACE_SECS),
        }
    }
}

/// `runtime-<pid>-<nanos>-<gen>`: three numeric fields, nothing else. Anything
/// that does not match is not ours and is never touched.
fn is_runtime_generation_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(RUNTIME_PREFIX) else {
        return false;
    };
    let fields: Vec<&str> = rest.split('-').collect();
    fields.len() == 3
        && fields
            .iter()
            .all(|field| !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()))
}

/// `<id>.artifacts` where `<id>` is a valid session id.
fn sidecar_session_id(name: &str) -> Option<&str> {
    let id = name.strip_suffix(ARTIFACTS_SUFFIX)?;
    let valid = !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    valid.then_some(id)
}

fn age_secs(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    std::time::SystemTime::now()
        .duration_since(modified)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// Count regular files and their bytes one level deep, never following symlinks
/// and never counting the lock file itself as content.
fn measure(path: &Path) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return (0, 0);
    };
    let mut files = 0;
    let mut bytes = 0;
    for entry in entries.flatten() {
        if entry.file_name() == std::ffi::OsStr::new(GENERATION_LOCK_NAME) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() {
            files += 1;
            bytes += metadata.len();
        } else if metadata.is_dir() {
            let (nested_files, nested_bytes) = measure(&entry.path());
            files += nested_files;
            bytes += nested_bytes;
        }
    }
    (files, bytes)
}

/// Try to take the exclusive lock at `path` without blocking. `Free` means we
/// got it (and immediately released it by dropping the handle); `Live` means
/// somebody else holds it; a missing file means `Unlocked`.
#[cfg(unix)]
fn probe_lock(path: &Path) -> Liveness {
    use std::os::unix::io::AsRawFd as _;

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Liveness::Unlocked,
    };
    if !metadata.file_type().is_file() {
        // Not the lock we wrote. Refuse to reason about it.
        return Liveness::Live;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    else {
        return Liveness::Live;
    };
    // A plain `flock` would be wrong here: a sibling thread between `fork` and
    // `execve` holds a duplicate of whatever lock descriptors this process has
    // open, so the first `EWOULDBLOCK` does not mean the lock is held. See
    // `crate::session::flock_exclusive_with_fork_retry` for the mechanism and
    // the retry budget.
    if crate::session::flock_exclusive_with_fork_retry(file.as_raw_fd()).is_ok() {
        // SAFETY: same descriptor, still open.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        Liveness::Free
    } else {
        Liveness::Live
    }
}

#[cfg(not(unix))]
fn probe_lock(path: &Path) -> Liveness {
    if std::fs::symlink_metadata(path).is_ok() {
        // Without a portable non-blocking lock probe, assume the safest answer.
        Liveness::Live
    } else {
        Liveness::Unlocked
    }
}

/// Acquire and hold the generation lock. The returned handle must live as long
/// as the directory is in use; the kernel releases the lock on process death.
#[cfg(unix)]
pub fn hold_generation_lock(directory: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::io::AsRawFd as _;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(directory.join(GENERATION_LOCK_NAME))
        .ok()?;
    // A plain `flock` would be wrong here: a sibling thread's fork-to-exec
    // window keeps a duplicate of our lock descriptors alive, so the first
    // `EWOULDBLOCK` is not contention. See
    // `crate::session::flock_exclusive_with_fork_retry`.
    crate::session::flock_exclusive_with_fork_retry(file.as_raw_fd())
        .ok()
        .map(|()| file)
}

#[cfg(not(unix))]
pub fn hold_generation_lock(directory: &Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(GENERATION_LOCK_NAME))
        .ok()
}

fn scan_namespace(
    root: &Path,
    namespace: Namespace,
    generations: &mut Vec<Generation>,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot inspect artifact namespace {}: {error}",
                root.display()
            ))
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let session_id = match namespace {
            Namespace::Runtime => {
                if !is_runtime_generation_name(name) {
                    continue;
                }
                None
            }
            Namespace::Sessions => match sidecar_session_id(name) {
                Some(id) => Some(id.to_string()),
                None => continue,
            },
        };
        let path = entry.path();
        // Never follow a symlink that merely wears a generation name.
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            continue;
        }
        let liveness = match namespace {
            Namespace::Runtime => probe_lock(&path.join(GENERATION_LOCK_NAME)),
            // A sidecar's owner is the session, and the session lock is the
            // one Kitsu already takes for the whole store.
            Namespace::Sessions => {
                let id = session_id.as_deref().unwrap_or_default();
                probe_lock(&root.join(LOCKS_DIR_NAME).join(format!("{id}.lock")))
            }
        };
        let session_exists = session_id
            .as_deref()
            .is_some_and(|id| root.join(format!("{id}.json")).exists());
        let retention = if liveness == Liveness::Live {
            Retention::InUse
        } else if session_exists {
            Retention::SessionExists
        } else {
            Retention::Orphaned
        };
        let (files, bytes) = measure(&path);
        generations.push(Generation {
            id: name.to_string(),
            path,
            namespace,
            files,
            bytes,
            age_secs: age_secs(&entry.path()),
            liveness,
            retention,
        });
    }
    Ok(())
}

/// Inventory both namespaces under `state_dir`. Read-only: it never creates a
/// directory, so merely inspecting artifacts cannot materialize a profile.
pub fn scan(state_dir: &Path) -> Result<Vec<Generation>, String> {
    let mut generations = Vec::new();
    scan_namespace(
        &state_dir.join("artifacts"),
        Namespace::Runtime,
        &mut generations,
    )?;
    scan_namespace(
        &state_dir.join("sessions"),
        Namespace::Sessions,
        &mut generations,
    )?;
    generations.sort_by(|left, right| {
        right
            .age_secs
            .cmp(&left.age_secs)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(generations)
}

/// Remove one generation directory. Re-verifies the name and the symlink status
/// immediately before removal, and refuses anything that is not exactly the
/// generation we scanned.
pub fn remove(generation: &Generation) -> Result<(), String> {
    let name = generation
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let recognized = match generation.namespace {
        Namespace::Runtime => is_runtime_generation_name(name),
        Namespace::Sessions => sidecar_session_id(name).is_some(),
    };
    if !recognized {
        return Err(format!(
            "refusing to remove '{}': not an artifact generation",
            generation.path.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(&generation.path)
        .map_err(|error| format!("cannot inspect {}: {error}", generation.path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(format!(
            "refusing to remove '{}': not a regular directory",
            generation.path.display()
        ));
    }
    std::fs::remove_dir_all(&generation.path)
        .map_err(|error| format!("cannot remove {}: {error}", generation.path.display()))
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SweepReport {
    pub removed: usize,
    pub retained_generations: usize,
    pub retained_bytes: u64,
}

impl SweepReport {
    /// A notice, not a deletion: non-empty orphans are never removed without an
    /// explicit command, but silence about them is how they reached gigabytes.
    pub fn notice(&self) -> Option<String> {
        if self.retained_generations <= NOTICE_GENERATION_THRESHOLD
            && self.retained_bytes <= NOTICE_BYTE_THRESHOLD
        {
            return None;
        }
        Some(format!(
            "~ {} retained command-artifact generation(s) ({}) from earlier runs; inspect with 'kitsu artifacts list', remove with 'kitsu artifacts prune all'",
            self.retained_generations,
            human_bytes(self.retained_bytes),
        ))
    }
}

/// Remove only provably empty orphans. Zero disclosure risk: an empty directory
/// holds no output, and an orphan has no owner that could still be filling it.
pub fn sweep_empty_orphans(state_dir: &Path) -> SweepReport {
    let Ok(generations) = scan(state_dir) else {
        return SweepReport::default();
    };
    let mut report = SweepReport::default();
    for generation in &generations {
        if generation.is_empty_orphan() {
            if remove(generation).is_ok() {
                report.removed += 1;
            }
            continue;
        }
        if generation.retention == Retention::Orphaned {
            report.retained_generations += 1;
            report.retained_bytes += generation.bytes;
        }
    }
    report
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn human_age(age_secs: Option<u64>) -> String {
    let Some(age) = age_secs else {
        return "unknown".to_string();
    };
    if age < 60 {
        format!("{age}s ago")
    } else if age < 3600 {
        format!("{}m ago", age / 60)
    } else if age < 86_400 {
        format!("{}h ago", age / 3600)
    } else {
        format!("{}d ago", age / 86_400)
    }
}

/// Parse a duration selector such as `7d`, `12h`, `30m`, or `90s`.
pub fn parse_duration_secs(value: &str) -> Result<u64, String> {
    let value = value.trim();
    let (digits, multiplier) = match value.chars().last() {
        Some('d') => (&value[..value.len() - 1], 86_400),
        Some('h') => (&value[..value.len() - 1], 3_600),
        Some('m') => (&value[..value.len() - 1], 60),
        Some('s') => (&value[..value.len() - 1], 1),
        _ => (value, 86_400),
    };
    digits
        .parse::<u64>()
        .map_err(|_| format!("invalid duration '{value}'; use forms like 7d, 12h, 30m, 90s"))
        .map(|number| number.saturating_mul(multiplier))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation_named(state_dir: &Path, id: &str) -> Generation {
        scan(state_dir)
            .unwrap()
            .into_iter()
            .find(|generation| generation.id == id)
            .unwrap_or_else(|| panic!("no generation named {id}"))
    }

    #[test]
    fn only_exact_generation_names_are_recognized() {
        assert!(is_runtime_generation_name("runtime-123-456-0"));
        assert!(!is_runtime_generation_name("runtime-123-456"));
        assert!(!is_runtime_generation_name("runtime-123-456-0-extra"));
        assert!(!is_runtime_generation_name("runtime-abc-456-0"));
        assert!(!is_runtime_generation_name("not-an-artifact"));
        assert!(!is_runtime_generation_name("runtime-"));

        assert_eq!(sidecar_session_id("s-1-2-0.artifacts"), Some("s-1-2-0"));
        assert_eq!(sidecar_session_id("s-1-2-0.json"), None);
        assert_eq!(sidecar_session_id(".artifacts"), None);
        assert_eq!(sidecar_session_id("history"), None);
    }

    #[test]
    fn scan_ignores_foreign_entries_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let artifacts = root.path().join("artifacts");
        let sessions = root.path().join("sessions");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir(artifacts.join("runtime-1-2-0")).unwrap();
        std::fs::write(artifacts.join("runtime-1-2-0/command-output-0.txt"), "x").unwrap();
        std::fs::create_dir(artifacts.join("not-an-artifact")).unwrap();
        std::fs::write(sessions.join("s-1-2-0.json"), "{}").unwrap();
        std::fs::create_dir(sessions.join("s-1-2-0.artifacts")).unwrap();
        std::fs::create_dir(sessions.join("s-9-9-9.artifacts")).unwrap();
        std::fs::create_dir(sessions.join(LOCKS_DIR_NAME)).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path(), artifacts.join("runtime-9-9-9")).unwrap();

        let generations = scan(root.path()).unwrap();
        let ids: Vec<&str> = generations
            .iter()
            .map(|generation| generation.id.as_str())
            .collect();
        assert!(ids.contains(&"runtime-1-2-0"));
        assert!(ids.contains(&"s-1-2-0.artifacts"));
        assert!(ids.contains(&"s-9-9-9.artifacts"));
        assert!(!ids.contains(&"not-an-artifact"));
        assert!(!ids.contains(&"runtime-9-9-9"), "symlink must be skipped");

        let owned = generations
            .iter()
            .find(|generation| generation.id == "s-1-2-0.artifacts")
            .unwrap();
        assert_eq!(owned.retention, Retention::SessionExists);
        let orphan = generations
            .iter()
            .find(|generation| generation.id == "s-9-9-9.artifacts")
            .unwrap();
        assert_eq!(orphan.retention, Retention::Orphaned);
        assert_eq!(orphan.liveness, Liveness::Unlocked);
        assert!(
            !orphan.is_empty_orphan(),
            "a freshly created unlocked directory is inside the allocation grace window"
        );
    }

    #[test]
    fn sweep_removes_only_empty_orphans() {
        let root = tempfile::tempdir().unwrap();
        let artifacts = root.path().join("artifacts");
        let sessions = root.path().join("sessions");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir(artifacts.join("runtime-1-1-0")).unwrap();
        // A finished run leaves its (free) lock file behind.
        std::fs::write(
            artifacts.join("runtime-1-1-0").join(GENERATION_LOCK_NAME),
            "",
        )
        .unwrap();
        std::fs::create_dir(artifacts.join("runtime-2-2-0")).unwrap();
        std::fs::write(artifacts.join("runtime-2-2-0/command-output-0.txt"), "keep").unwrap();
        std::fs::create_dir(sessions.join("s-1-1-0.artifacts")).unwrap();
        std::fs::write(sessions.join("s-2-2-0.json"), "{}").unwrap();
        std::fs::create_dir(sessions.join("s-2-2-0.artifacts")).unwrap();

        let report = sweep_empty_orphans(root.path());

        assert_eq!(report.removed, 1);
        assert!(!artifacts.join("runtime-1-1-0").exists());
        // The sidecar has no lock file and was created moments ago: a process
        // may still be starting up inside it, so it survives this sweep.
        assert!(sessions.join("s-1-1-0.artifacts").exists());
        assert!(artifacts.join("runtime-2-2-0").exists());
        assert!(sessions.join("s-2-2-0.artifacts").exists());
        assert!(sessions.join("s-2-2-0.json").exists());
        assert_eq!(report.retained_generations, 2);
        assert!(report.notice().is_none());

        let never_locked = generation_named(root.path(), "s-1-1-0.artifacts");
        assert!(
            !never_locked.is_empty_orphan(),
            "an unlocked directory inside the allocation grace window is kept"
        );
        assert!(Generation {
            age_secs: Some(UNLOCKED_ALLOCATION_GRACE_SECS + 1),
            ..never_locked
        }
        .is_empty_orphan());
    }

    #[cfg(unix)]
    #[test]
    fn a_held_lock_marks_the_generation_live_and_unprunable() {
        let root = tempfile::tempdir().unwrap();
        let artifacts = root.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let live = artifacts.join("runtime-3-3-0");
        std::fs::create_dir(&live).unwrap();
        let _guard = hold_generation_lock(&live).expect("hold the generation lock");

        let generations = scan(root.path()).unwrap();
        let generation = generations
            .iter()
            .find(|generation| generation.id == "runtime-3-3-0")
            .unwrap();
        assert_eq!(generation.liveness, Liveness::Live);
        assert_eq!(generation.retention, Retention::InUse);
        assert!(!generation.is_empty_orphan());
        assert!(!generation.prunable());

        // The startup sweep must leave it exactly where it is.
        assert_eq!(sweep_empty_orphans(root.path()).removed, 0);
        assert!(live.exists());
    }

    #[test]
    fn unlocked_generations_are_kept_until_the_grace_period_passes() {
        let generation = Generation {
            id: "runtime-1-2-0".to_string(),
            path: PathBuf::from("/tmp/runtime-1-2-0"),
            namespace: Namespace::Runtime,
            files: 1,
            bytes: 10,
            age_secs: Some(5),
            liveness: Liveness::Unlocked,
            retention: Retention::Orphaned,
        };
        assert!(!generation.prunable());
        let old = Generation {
            age_secs: Some(UNLOCKED_GRACE_SECS + 1),
            ..generation.clone()
        };
        assert!(old.prunable());
        let unknown_age = Generation {
            age_secs: None,
            ..generation
        };
        assert!(
            !unknown_age.prunable(),
            "an unknown age must not authorize deletion"
        );
    }

    #[test]
    fn durations_accept_the_documented_suffixes() {
        assert_eq!(parse_duration_secs("7d").unwrap(), 604_800);
        assert_eq!(parse_duration_secs("12h").unwrap(), 43_200);
        assert_eq!(parse_duration_secs("30m").unwrap(), 1_800);
        assert_eq!(parse_duration_secs("90s").unwrap(), 90);
        assert_eq!(parse_duration_secs("2").unwrap(), 172_800);
        assert!(parse_duration_secs("soon").is_err());
    }

    /// `flock` locks live on the open file description and `O_CLOEXEC` closes
    /// only at `exec`, so a sibling thread sitting between `fork` and `execve`
    /// holds a duplicate of any lock descriptor open in this process. A probe
    /// that trusts the first `EWOULDBLOCK` therefore reports a free lock as
    /// held, and a reclaimable generation is never reclaimed.
    #[cfg(unix)]
    #[test]
    fn probe_lock_is_not_spuriously_live_while_another_thread_forks() {
        use std::process::{Command, Stdio};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        const SPAWNERS: usize = 4;
        const ATTEMPTS: usize = 2000;

        let root = tempfile::tempdir().unwrap();
        let generation = root.path().join("runtime-8-8-0");
        std::fs::create_dir(&generation).unwrap();
        let lock = generation.join(GENERATION_LOCK_NAME);

        let stop = Arc::new(AtomicBool::new(false));
        let spawners: Vec<_> = (0..SPAWNERS)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let Ok(mut child) = Command::new("/bin/true")
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()
                        else {
                            return;
                        };
                        let _ = child.wait();
                    }
                })
            })
            .collect();

        let mut failure = None;
        for attempt in 0..ATTEMPTS {
            // A run takes the generation lock and finishes: the handle drops,
            // its descriptor closes, and the directory is reclaimable. This is
            // the leak source a sweep meets in production -- a fork that caught
            // the descriptor keeps the lock alive past the holder's exit. (If
            // this acquisition itself trips, that is the sibling defect, not
            // this one, so the attempt is skipped rather than failed.)
            let Some(held) = hold_generation_lock(&generation) else {
                continue;
            };
            drop(held);
            let liveness = probe_lock(&lock);
            if liveness != Liveness::Free {
                failure = Some((attempt, liveness));
                break;
            }
        }

        stop.store(true, Ordering::Relaxed);
        for spawner in spawners {
            spawner.join().unwrap();
        }

        if let Some((attempt, liveness)) = failure {
            panic!(
                "an unheld generation lock probed as {liveness:?} on attempt {attempt} \
                 of {ATTEMPTS} while another thread was forking"
            );
        }
    }

    /// The same fork window makes `hold_generation_lock` report "could not
    /// acquire" for a lock nobody holds, and the caller then skips work it
    /// should have done.
    #[cfg(unix)]
    #[test]
    fn generation_lock_is_not_spuriously_unavailable_while_another_thread_forks() {
        use std::process::{Command, Stdio};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        const SPAWNERS: usize = 4;
        const ATTEMPTS: usize = 2000;

        let root = tempfile::tempdir().unwrap();
        let generation = root.path().join("runtime-9-9-0");
        std::fs::create_dir(&generation).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let spawners: Vec<_> = (0..SPAWNERS)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let Ok(mut child) = Command::new("/bin/true")
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()
                        else {
                            return;
                        };
                        let _ = child.wait();
                    }
                })
            })
            .collect();

        let mut failure = None;
        for attempt in 0..ATTEMPTS {
            match hold_generation_lock(&generation) {
                // Dropping the handle closes the descriptor and releases the
                // lock, so every attempt is uncontended.
                Some(held) => drop(held),
                None => {
                    failure = Some(attempt);
                    break;
                }
            }
        }

        stop.store(true, Ordering::Relaxed);
        for spawner in spawners {
            spawner.join().unwrap();
        }

        if let Some(attempt) = failure {
            panic!(
                "an uncontended generation lock could not be acquired on attempt {attempt} \
                 of {ATTEMPTS} while another thread was forking"
            );
        }
    }
}
