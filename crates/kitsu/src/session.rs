//! Durable local chat-session snapshots.
//!
//! Kitsu does not serialize auth configuration or permission grants. Session
//! transcripts do contain user/model/tool text, however, and that text may
//! itself include sensitive material. Permission grants are intentionally not
//! restored: resuming context must not silently restore filesystem authority.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{SecondsFormat, Utc};
use kitsu_providers::ChatMessage;
use serde::{Deserialize, Serialize};

const SESSION_VERSION: u32 = 1;
const MAX_SESSION_BYTES: u64 = 16 * 1024 * 1024;
static SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub version: u32,
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub directory: String,
    pub gateway: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_context: Option<String>,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub updated_at: String,
    pub model: String,
    pub gateway: String,
    pub directory: String,
    pub preview: String,
    pub turns: usize,
}

#[derive(Debug)]
pub struct SessionStore {
    dir: PathBuf,
    id: String,
    created_at: String,
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("cannot read session '{}': {error}", path.display()))?;
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

impl SessionStore {
    pub fn create(dir: PathBuf) -> Result<Self, String> {
        prepare_dir(&dir)?;
        let created_at = now();
        Ok(Self {
            dir,
            id: fresh_id(),
            created_at,
        })
    }

    pub fn from_saved(dir: PathBuf, saved: &SavedSession) -> Result<Self, String> {
        if !valid_id(&saved.id) {
            return Err("saved session has an invalid id".to_string());
        }
        prepare_dir(&dir)?;
        Ok(Self {
            dir,
            id: saved.id.clone(),
            created_at: saved.created_at.clone(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.dir.join(format!("{}.artifacts", self.id))
    }

    pub fn reset(&mut self) {
        self.id = fresh_id();
        self.created_at = now();
    }

    fn path(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.id))
    }

    pub fn save(
        &self,
        directory: &Path,
        gateway: &str,
        model: &str,
        project_context: Option<&Path>,
        messages: &[ChatMessage],
    ) -> Result<(), String> {
        let saved = SavedSession {
            version: SESSION_VERSION,
            id: self.id.clone(),
            created_at: self.created_at.clone(),
            updated_at: now(),
            directory: directory.display().to_string(),
            gateway: gateway.to_string(),
            model: model.to_string(),
            project_context: project_context.map(|path| path.display().to_string()),
            messages: messages.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&saved)
            .map_err(|error| format!("cannot encode session: {error}"))?;
        if bytes.len() as u64 > MAX_SESSION_BYTES {
            return Err("session snapshot exceeds the 16 MiB safety limit".to_string());
        }
        prepare_dir(&self.dir)?;
        let target = self.path();
        let temporary = self.dir.join(format!(".{}.{}.tmp", self.id, fresh_id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("cannot create session {}: {error}", target.display()))?;
        let write_result = file.write_all(&bytes).and_then(|_| file.sync_all());
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!(
                "cannot write session {}: {error}",
                target.display()
            ));
        }
        drop(file);
        if let Err(error) = std::fs::rename(&temporary, &target) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!(
                "cannot finalize session {}: {error}",
                target.display()
            ));
        }
        // Make the rename durable as well as atomic on filesystems that support
        // syncing directory handles.
        if let Ok(directory) = std::fs::File::open(&self.dir) {
            directory
                .sync_all()
                .map_err(|error| format!("cannot sync session directory: {error}"))?;
        }
        Ok(())
    }

    pub fn load(dir: &Path, selector: &str) -> Result<SavedSession, String> {
        let id = if selector == "last" || selector.is_empty() {
            Self::list(dir)?
                .into_iter()
                .next()
                .map(|summary| summary.id)
                .ok_or_else(|| "no saved sessions in this config directory".to_string())?
        } else if valid_id(selector) {
            selector.to_string()
        } else {
            return Err(format!("invalid session id '{selector}'"));
        };
        let path = dir.join(format!("{id}.json"));
        read_saved_file(&path, &id)
    }

    pub fn list(dir: &Path) -> Result<Vec<SessionSummary>, String> {
        match std::fs::symlink_metadata(dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() => {
                return Err(format!(
                    "session directory '{}' is not a regular directory",
                    dir.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(format!(
                    "cannot inspect session directory {}: {error}",
                    dir.display()
                ));
            }
            _ => {}
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(format!(
                    "cannot list session directory {}: {error}",
                    dir.display()
                ));
            }
        };
        let mut summaries = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|part| part.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|part| part.to_str()) else {
                continue;
            };
            if !valid_id(id) {
                continue;
            }
            let Ok(saved) = read_saved_file(&path, id) else {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        store
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
            assert_eq!(session_dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
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

        store.reset();
        assert_ne!(store.id(), original_id);
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
        symlink(&outside, root.path().join("safe-id.json")).unwrap();
        let error = SessionStore::load(root.path(), "safe-id").unwrap_err();
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
}
