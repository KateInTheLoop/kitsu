//! Private create-new config publication adapter.
//!
//! The adapter remains deliberately unmounted: these types own filesystem
//! truth for a future coordinator without exposing any product/UI/CLI path.

use super::onboarding::{
    CredentialPlan, NoClobberConfigTarget, NormalizedSetupPreview, OwnedEffectKind,
    OwnedEffectReceipt, SetupAdapter, SetupSelectionKind, MAX_OWNED_EFFECTS,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(not(unix))]
use std::ffi::OsString;
#[cfg(unix)]
use std::ffi::{CStr, CString};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

pub(super) const MAX_RENDERED_CONFIG_BYTES: usize = 16 * 1024;
static PUBLICATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Bytes can enter the publication port only through the closed preview
/// renderer. Debug deliberately reveals neither route fields nor serialized
/// content.
pub(super) struct RenderedConfigBytes(Vec<u8>);

impl fmt::Debug for RenderedConfigBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderedConfigBytes")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ConfigRenderError {
    Encode,
    TooLarge,
    InvalidConfig,
    Invariant,
}

impl RenderedConfigBytes {
    pub(super) fn render(preview: &NormalizedSetupPreview) -> Result<Self, ConfigRenderError> {
        let mut auth = toml::Table::new();
        auth.insert(
            "method".to_string(),
            toml::Value::String(
                match preview.credential() {
                    CredentialPlan::None => "none",
                    CredentialPlan::ManagedBearer => "bearer-env",
                }
                .to_string(),
            ),
        );

        let mut gateway = toml::Table::new();
        gateway.insert(
            "flavor".to_string(),
            toml::Value::String(
                match (preview.selection(), preview.adapter()) {
                    (_, SetupAdapter::AnthropicMessages) => "anthropic",
                    (SetupSelectionKind::Local, SetupAdapter::CompatibleChat) => "ollama",
                    _ => "openai",
                }
                .to_string(),
            ),
        );
        gateway.insert(
            "adapter".to_string(),
            toml::Value::String(
                match preview.adapter() {
                    SetupAdapter::CompatibleChat => "compatible-chat",
                    SetupAdapter::AnthropicMessages => "anthropic-messages",
                }
                .to_string(),
            ),
        );
        gateway.insert(
            "base_url".to_string(),
            toml::Value::String(preview.endpoint().to_string()),
        );
        gateway.insert(
            "chat_path".to_string(),
            toml::Value::String(preview.chat_path().to_string()),
        );
        if let Some(models_path) = preview.models_path() {
            gateway.insert(
                "models_path".to_string(),
                toml::Value::String(models_path.to_string()),
            );
        }
        gateway.insert(
            "model".to_string(),
            toml::Value::String(preview.model().to_string()),
        );
        gateway.insert("auth".to_string(), toml::Value::Table(auth));

        let mut gateways = toml::Table::new();
        gateways.insert(preview.profile().to_string(), toml::Value::Table(gateway));
        let mut root = toml::Table::new();
        root.insert(
            "default_gateway".to_string(),
            toml::Value::String(preview.profile().to_string()),
        );
        root.insert("gateways".to_string(), toml::Value::Table(gateways));

        let encoded =
            toml::to_string(&toml::Value::Table(root)).map_err(|_| ConfigRenderError::Encode)?;
        if encoded.len() > MAX_RENDERED_CONFIG_BYTES {
            return Err(ConfigRenderError::TooLarge);
        }
        let parsed: super::Config =
            toml::from_str(&encoded).map_err(|_| ConfigRenderError::InvalidConfig)?;
        let gateway = parsed
            .gateways
            .get(preview.profile())
            .ok_or(ConfigRenderError::Invariant)?;
        if parsed.default_gateway.as_deref() != Some(preview.profile())
            || parsed.gateways.len() != 1
            || gateway.auth.token.is_some()
            || gateway.auth.method
                != match preview.credential() {
                    CredentialPlan::None => "none",
                    CredentialPlan::ManagedBearer => "bearer-env",
                }
        {
            return Err(ConfigRenderError::Invariant);
        }
        Ok(Self(encoded.into_bytes()))
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfigPublicationFailureKind {
    ParentMissing,
    UnsafeParent,
    WriterBusy,
    InvalidTarget,
    StageCreate,
    Write,
    FileSync,
    InstallLink,
    StageCleanup,
    DirectorySync,
    ReceiptCapture,
    Unsupported,
}

pub(super) enum ConfigPublicationOutcome {
    Created(InstalledConfigReceipt),
    Collision,
    Failed {
        kind: ConfigPublicationFailureKind,
        residue: Option<ConfigPublicationResidue>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfigResidueKind {
    UninstalledStageCleanup,
    InstalledUnconfirmed,
    RemovedUnconfirmed,
    StageCleanup,
    Mismatch,
    DisplacedEntry,
    RollbackFailure,
    Unsupported,
}

pub(super) enum ConfigPublicationResidue {
    UninstalledStageCleanup(UninstalledStageCleanupReceipt),
    InstalledUnconfirmed(InstalledUnconfirmedReceipt),
    StageCleanup(StageCleanupReceipt),
}

impl ConfigPublicationResidue {
    pub(super) const fn kind(&self) -> ConfigResidueKind {
        match self {
            Self::UninstalledStageCleanup(_) => ConfigResidueKind::UninstalledStageCleanup,
            Self::InstalledUnconfirmed(_) => ConfigResidueKind::InstalledUnconfirmed,
            Self::StageCleanup(_) => ConfigResidueKind::StageCleanup,
        }
    }
}

impl fmt::Debug for ConfigPublicationResidue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ConfigPublicationResidue")
            .field(&self.kind())
            .finish()
    }
}

pub(super) struct InstalledConfigReceipt {
    target: PinnedConfigTarget,
    file: File,
    expected: Option<ConfigFileSnapshot>,
    #[cfg(unix)]
    stage_name: Option<CString>,
    #[cfg(not(unix))]
    stage_name: Option<OsString>,
}

impl fmt::Debug for InstalledConfigReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstalledConfigReceipt")
            .field("identity_confirmed", &self.expected.is_some())
            .field("stage_retained", &self.stage_name.is_some())
            .finish_non_exhaustive()
    }
}

impl InstalledConfigReceipt {
    pub(super) fn installed_size(&self) -> u64 {
        self.expected.as_ref().map_or(0, |expected| expected.size)
    }

    #[cfg(target_os = "linux")]
    fn refresh_after_stage_removal(&mut self) -> Result<(), ()> {
        let prior = self.expected.as_ref().ok_or(())?;
        let (current, _) = ConfigFileSnapshot::read_owned(&self.file).map_err(|_| ())?;
        if !prior.same_owned_inode_after_link_removal(&current) {
            return Err(());
        }
        self.expected = Some(current);
        Ok(())
    }
}

pub(super) struct InstalledUnconfirmedReceipt(InstalledConfigReceipt);
pub(super) struct StageCleanupReceipt(InstalledConfigReceipt);
pub(super) struct RemovedUnconfirmedReceipt(InstalledConfigReceipt);

pub(super) struct UninstalledStageCleanupReceipt {
    target: PinnedConfigTarget,
    file: File,
    #[cfg(unix)]
    stage_name: Option<CString>,
    #[cfg(not(unix))]
    stage_name: Option<OsString>,
    expected: Option<ConfigFileSnapshot>,
}

pub(super) struct DisplacedEntryReceipt {
    installed: InstalledConfigReceipt,
    #[cfg(unix)]
    displaced_name: CString,
}

#[derive(Debug)]
struct ConfigFileSnapshot {
    size: u64,
    digest: String,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    user: u32,
    #[cfg(unix)]
    group: u32,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(unix)]
    links: u64,
    #[cfg(not(unix))]
    readonly: bool,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

impl ConfigFileSnapshot {
    fn from_file(file: &File, digest: String) -> std::io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_RENDERED_CONFIG_BYTES as u64 {
            return Err(std::io::Error::other(
                "config receipt is not a bounded regular file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                size: metadata.len(),
                digest,
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                user: metadata.uid(),
                group: metadata.gid(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
                links: metadata.nlink(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                size: metadata.len(),
                digest,
                readonly: metadata.permissions().readonly(),
                modified: metadata.modified().ok(),
            })
        }
    }

    fn matches(&self, other: &Self) -> bool {
        if self.size != other.size || self.digest != other.digest {
            return false;
        }
        #[cfg(unix)]
        {
            self.device == other.device
                && self.inode == other.inode
                && self.mode == other.mode
                && self.user == other.user
                && self.group == other.group
                && self.changed_seconds == other.changed_seconds
                && self.changed_nanoseconds == other.changed_nanoseconds
                && self.links == other.links
        }
        #[cfg(not(unix))]
        {
            self.readonly == other.readonly && self.modified == other.modified
        }
    }

    #[cfg(target_os = "linux")]
    fn same_owned_inode_after_link_removal(&self, other: &Self) -> bool {
        self.size == other.size
            && self.digest == other.digest
            && self.device == other.device
            && self.inode == other.inode
            && self.mode == other.mode
            && self.user == other.user
            && self.group == other.group
            && self.links == other.links.saturating_add(1)
    }

    #[cfg(target_os = "linux")]
    fn same_owned_inode_after_rename(&self, other: &Self) -> bool {
        // The owned rename advances ctime. The tracked identity, content,
        // mode, uid/gid, and link-count facts remain exact across it.
        self.size == other.size
            && self.digest == other.digest
            && self.device == other.device
            && self.inode == other.inode
            && self.mode == other.mode
            && self.user == other.user
            && self.group == other.group
            && self.links == other.links
    }

    #[cfg(target_os = "linux")]
    fn read_owned(file: &File) -> std::io::Result<(Self, Vec<u8>)> {
        use std::os::unix::fs::FileExt;

        let before_metadata = file.metadata()?;
        if !before_metadata.is_file() || before_metadata.len() > MAX_RENDERED_CONFIG_BYTES as u64 {
            return Err(std::io::Error::other(
                "config receipt is not a bounded regular file",
            ));
        }

        let mut bytes = Vec::with_capacity(before_metadata.len() as usize);
        let mut offset = 0_u64;
        while bytes.len() <= MAX_RENDERED_CONFIG_BYTES {
            let mut chunk = [0_u8; 4096];
            let remaining = MAX_RENDERED_CONFIG_BYTES + 1 - bytes.len();
            let chunk_limit = remaining.min(chunk.len());
            let read = file.read_at(&mut chunk[..chunk_limit], offset)?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
            offset += read as u64;
        }
        if bytes.len() > MAX_RENDERED_CONFIG_BYTES {
            return Err(std::io::Error::other("config receipt exceeded its bound"));
        }

        let digest = crate::skills::content_sha256_hex(&bytes);
        let before = Self::from_metadata(&before_metadata, digest.clone())?;
        let after = Self::from_file(file, digest)?;
        if bytes.len() as u64 != before_metadata.len() || !before.matches(&after) {
            return Err(std::io::Error::other(
                "config receipt changed while it was captured",
            ));
        }
        Ok((after, bytes))
    }

    #[cfg(target_os = "linux")]
    fn from_metadata(metadata: &std::fs::Metadata, digest: String) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt;

        if !metadata.is_file() || metadata.len() > MAX_RENDERED_CONFIG_BYTES as u64 {
            return Err(std::io::Error::other(
                "config receipt is not a bounded regular file",
            ));
        }
        Ok(Self {
            size: metadata.len(),
            digest,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            user: metadata.uid(),
            group: metadata.gid(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            links: metadata.nlink(),
        })
    }
}

struct PinnedConfigTarget {
    path: PathBuf,
    #[cfg(unix)]
    parent: File,
    #[cfg(unix)]
    name: CString,
}

impl fmt::Debug for PinnedConfigTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PinnedConfigTarget")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PinTargetError {
    Missing,
    Unsafe,
    Busy,
    Invalid,
}

impl PinnedConfigTarget {
    #[cfg(unix)]
    fn pin(target: &NoClobberConfigTarget) -> Result<Self, PinTargetError> {
        use std::os::unix::fs::OpenOptionsExt;

        let absolute = if target.path().is_absolute() {
            target.path().to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|_| PinTargetError::Unsafe)?
                .join(target.path())
        };
        let name = absolute
            .file_name()
            .ok_or(PinTargetError::Invalid)
            .and_then(|name| CString::new(name.as_bytes()).map_err(|_| PinTargetError::Invalid))?;
        let parent_path = absolute.parent().ok_or(PinTargetError::Invalid)?;
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut parent = options
            .open(Path::new("/"))
            .map_err(|_| PinTargetError::Unsafe)?;
        for component in parent_path.components() {
            use std::path::Component;
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(component) => {
                    let component =
                        CString::new(component.as_bytes()).map_err(|_| PinTargetError::Invalid)?;
                    let fd = unsafe {
                        libc::openat(
                            parent.as_raw_fd(),
                            component.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                        )
                    };
                    if fd < 0 {
                        let error = std::io::Error::last_os_error();
                        return Err(if error.kind() == std::io::ErrorKind::NotFound {
                            PinTargetError::Missing
                        } else {
                            PinTargetError::Unsafe
                        });
                    }
                    parent = unsafe { File::from_raw_fd(fd) };
                }
                Component::ParentDir | Component::Prefix(_) => return Err(PinTargetError::Invalid),
            }
        }
        #[cfg(target_os = "linux")]
        {
            // Advisory serialization covers Kitsu config publishers that use
            // this adapter. It cannot exclude non-cooperating or privileged
            // same-UID writers from the directory namespace.
            let result = unsafe { libc::flock(parent.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
                    PinTargetError::Busy
                } else {
                    PinTargetError::Unsafe
                });
            }
        }
        Ok(Self {
            path: absolute,
            parent,
            name,
        })
    }

    #[cfg(not(unix))]
    fn pin(target: &NoClobberConfigTarget) -> Result<Self, PinTargetError> {
        let absolute = if target.path().is_absolute() {
            target.path().to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|_| PinTargetError::Unsafe)?
                .join(target.path())
        };
        if absolute.file_name().is_none() {
            return Err(PinTargetError::Invalid);
        }
        let parent = absolute.parent().ok_or(PinTargetError::Invalid)?;
        let metadata = std::fs::symlink_metadata(parent).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                PinTargetError::Missing
            } else {
                PinTargetError::Unsafe
            }
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(PinTargetError::Unsafe);
        }
        Ok(Self { path: absolute })
    }

    #[cfg(unix)]
    fn create_stage(&self) -> std::io::Result<(CString, File)> {
        use std::os::unix::fs::PermissionsExt;

        for _ in 0..32 {
            let sequence = PUBLICATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = CString::new(format!(
                ".kitsu-config-{}-{sequence}.tmp",
                std::process::id()
            ))
            .expect("generated stage name contains no NUL");
            let fd = unsafe {
                libc::openat(
                    self.parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd >= 0 {
                let file = unsafe { File::from_raw_fd(fd) };
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                return Ok((name, file));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate config stage",
        ))
    }

    #[cfg(not(unix))]
    fn create_stage(&self) -> std::io::Result<(OsString, File)> {
        let parent = self.path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "target has no parent")
        })?;
        for _ in 0..32 {
            let sequence = PUBLICATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = OsString::from(format!(
                ".kitsu-config-{}-{sequence}.tmp",
                std::process::id()
            ));
            let path = parent.join(&name);
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(file) => return Ok((name, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate config stage",
        ))
    }

    #[cfg(unix)]
    fn link_stage(&self, stage: &CStr) -> std::io::Result<()> {
        let result = unsafe {
            libc::linkat(
                self.parent.as_raw_fd(),
                stage.as_ptr(),
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                0,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(not(unix))]
    fn link_stage(&self, stage: &std::ffi::OsStr) -> std::io::Result<()> {
        let parent = self.path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "target has no parent")
        })?;
        std::fs::hard_link(parent.join(stage), &self.path)
    }

    #[cfg(unix)]
    fn unlink(&self, name: &CStr) -> std::io::Result<()> {
        let result = unsafe { libc::unlinkat(self.parent.as_raw_fd(), name.as_ptr(), 0) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(not(unix))]
    fn unlink(&self, name: &std::ffi::OsStr) -> std::io::Result<()> {
        let parent = self.path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "target has no parent")
        })?;
        std::fs::remove_file(parent.join(name))
    }

    fn sync_parent(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            self.parent.sync_all()
        }
        #[cfg(not(unix))]
        {
            let Some(parent) = self.path.parent() else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "target has no parent",
                ));
            };
            match File::open(parent) {
                Ok(directory) => directory.sync_all(),
                Err(_) => Ok(()),
            }
        }
    }

    #[cfg(unix)]
    fn read_named_snapshot(&self, name: &CStr) -> std::io::Result<Option<ConfigFileSnapshot>> {
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
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
        let mut file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_RENDERED_CONFIG_BYTES as u64 {
            return Err(std::io::Error::other(
                "config entry is not a bounded regular file",
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        (&mut file)
            .take(MAX_RENDERED_CONFIG_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RENDERED_CONFIG_BYTES {
            return Err(std::io::Error::other("config entry exceeded its bound"));
        }
        let digest = crate::skills::content_sha256_hex(&bytes);
        ConfigFileSnapshot::from_file(&file, digest).map(Some)
    }

    #[cfg(target_os = "linux")]
    fn rename_noreplace(&self, old: &CStr, new: &CStr) -> std::io::Result<()> {
        unsafe {
            kitsu_tools::rename_flags::renameat2(
                self.parent.as_raw_fd(),
                old.as_ptr(),
                self.parent.as_raw_fd(),
                new.as_ptr(),
                kitsu_tools::rename_flags::RENAME_NOREPLACE,
            )
        }
    }

    #[cfg(target_os = "linux")]
    fn quarantine(&self, name: &CStr) -> std::io::Result<CString> {
        for _ in 0..32 {
            let sequence = PUBLICATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let quarantine = CString::new(format!(
                ".kitsu-config-rollback-{}-{sequence}.tmp",
                std::process::id()
            ))
            .expect("generated quarantine name contains no NUL");
            match self.rename_noreplace(name, &quarantine) {
                Ok(()) => return Ok(quarantine),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate rollback quarantine",
        ))
    }

    #[cfg(unix)]
    fn named_path(&self, name: &CStr) -> PathBuf {
        let bytes = name.to_bytes().to_vec();
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(std::ffi::OsString::from_vec(bytes))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PublicationFaultPoint {
    Write,
    FileSync,
    InstallLink,
    UninstalledStageCleanup,
    UninstalledDirectorySync,
    StageCleanup,
    ReceiptCapture,
    DirectorySync,
}

#[cfg(test)]
struct PublicationFaultPlan {
    generation: u64,
    points: Vec<PublicationFaultPoint>,
}

#[cfg(test)]
struct RollbackFaultPlan {
    generation: u64,
    point: RollbackFaultPoint,
}

#[cfg(test)]
struct PublicationHookPlan {
    generation: u64,
    hook: PublicationHook,
}

#[cfg(test)]
thread_local! {
    static NEXT_PUBLICATION_FAULTS: std::cell::RefCell<Option<PublicationFaultPlan>> = const {
        std::cell::RefCell::new(None)
    };
    static NEXT_ROLLBACK_FAULT: std::cell::RefCell<Option<RollbackFaultPlan>> = const {
        std::cell::RefCell::new(None)
    };
    static NEXT_PUBLICATION_HOOK: std::cell::RefCell<Option<PublicationHookPlan>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
static TEST_FIXTURE_GENERATION: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
fn next_test_fixture_generation() -> u64 {
    let generation = TEST_FIXTURE_GENERATION.fetch_add(1, Ordering::Relaxed);
    assert_ne!(generation, u64::MAX, "test fixture generation exhausted");
    generation
}

#[cfg(test)]
#[must_use = "retain the publication fault guard through the intended operation"]
pub(super) struct PublicationFaultFixtureGuard {
    generation: u64,
}

#[cfg(test)]
impl Drop for PublicationFaultFixtureGuard {
    fn drop(&mut self) {
        NEXT_PUBLICATION_FAULTS.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|plan| plan.generation == self.generation)
            {
                *slot = None;
            }
        });
    }
}

#[cfg(test)]
#[must_use = "retain the rollback fault guard through the intended operation"]
pub(super) struct RollbackFaultFixtureGuard {
    generation: u64,
}

#[cfg(test)]
impl Drop for RollbackFaultFixtureGuard {
    fn drop(&mut self) {
        NEXT_ROLLBACK_FAULT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|plan| plan.generation == self.generation)
            {
                *slot = None;
            }
        });
    }
}

#[cfg(test)]
#[must_use = "retain the publication hook guard through the intended operation"]
pub(super) struct PublicationHookFixtureGuard {
    generation: u64,
}

#[cfg(test)]
impl Drop for PublicationHookFixtureGuard {
    fn drop(&mut self) {
        NEXT_PUBLICATION_HOOK.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|plan| plan.generation == self.generation)
            {
                *slot = None;
            }
        });
    }
}

#[cfg(test)]
pub(super) enum PublicationHook {
    BeforeInstall(Box<dyn FnOnce()>),
}

#[cfg(test)]
pub(super) fn inject_publication_hook(hook: PublicationHook) -> PublicationHookFixtureGuard {
    let generation = next_test_fixture_generation();
    NEXT_PUBLICATION_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none());
        *slot = Some(PublicationHookPlan { generation, hook });
    });
    PublicationHookFixtureGuard { generation }
}

#[cfg(test)]
pub(super) fn inject_next_publication_fault(
    point: PublicationFaultPoint,
) -> PublicationFaultFixtureGuard {
    inject_publication_faults(&[point])
}

#[cfg(test)]
pub(super) fn inject_publication_faults(
    points: &[PublicationFaultPoint],
) -> PublicationFaultFixtureGuard {
    let generation = next_test_fixture_generation();
    NEXT_PUBLICATION_FAULTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none());
        *slot = Some(PublicationFaultPlan {
            generation,
            points: points.to_vec(),
        });
    });
    PublicationFaultFixtureGuard { generation }
}

fn publication_fault(point: PublicationFaultPoint) -> bool {
    #[cfg(test)]
    {
        NEXT_PUBLICATION_FAULTS.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(plan) = slot.as_mut() else {
                return false;
            };
            if let Some(index) = plan.points.iter().position(|pending| *pending == point) {
                plan.points.remove(index);
                if plan.points.is_empty() {
                    *slot = None;
                }
                true
            } else {
                false
            }
        })
    }
    #[cfg(not(test))]
    {
        let _ = point;
        false
    }
}

fn run_before_install_hook() {
    #[cfg(test)]
    NEXT_PUBLICATION_HOOK.with(|slot| {
        let hook = slot.borrow_mut().take().map(|plan| plan.hook);
        if let Some(PublicationHook::BeforeInstall(hook)) = hook {
            hook();
        }
    });
}

#[cfg(target_os = "linux")]
fn capture_publication_snapshot(
    file: &File,
    rendered: &RenderedConfigBytes,
) -> (Option<ConfigFileSnapshot>, bool) {
    let Ok((mut snapshot, bytes)) = ConfigFileSnapshot::read_owned(file) else {
        return (None, false);
    };
    let exact = bytes.as_slice() == rendered.as_bytes();
    if !exact {
        // Keep an explicit negative proof: rollback may compare the raced inode
        // with the bytes the adapter wrote, but must not bless the raced bytes.
        snapshot.size = rendered.as_bytes().len() as u64;
        snapshot.digest = crate::skills::content_sha256_hex(rendered.as_bytes());
    }
    (Some(snapshot), exact)
}

#[cfg(target_os = "linux")]
fn cleanup_uninstalled_stage(
    target: PinnedConfigTarget,
    file: File,
    stage_name: CString,
) -> Option<UninstalledStageCleanupReceipt> {
    let expected = ConfigFileSnapshot::read_owned(&file)
        .ok()
        .map(|(snapshot, _)| snapshot);
    let mut receipt = UninstalledStageCleanupReceipt {
        target,
        file,
        stage_name: Some(stage_name),
        expected,
    };
    if publication_fault(PublicationFaultPoint::UninstalledStageCleanup) {
        return Some(receipt);
    }

    let stage = receipt
        .stage_name
        .as_ref()
        .expect("new uninstalled stage retains its name")
        .clone();
    let removal = quarantine_verified_name(
        &receipt.target,
        &receipt.file,
        receipt.expected.as_ref(),
        &stage,
        false,
        &mut |_| {},
    );
    match removal {
        QuarantineRemoval::Removed => receipt.stage_name = None,
        QuarantineRemoval::Displaced(displaced_name) => {
            receipt.stage_name = Some(displaced_name);
            return Some(receipt);
        }
        QuarantineRemoval::Mismatch
        | QuarantineRemoval::Failure
        | QuarantineRemoval::Unsupported => return Some(receipt),
    }
    if publication_fault(PublicationFaultPoint::UninstalledDirectorySync)
        || receipt.target.sync_parent().is_err()
    {
        Some(receipt)
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn publish_create_new(
    _target: &NoClobberConfigTarget,
    _rendered: &RenderedConfigBytes,
) -> ConfigPublicationOutcome {
    ConfigPublicationOutcome::Failed {
        kind: ConfigPublicationFailureKind::Unsupported,
        residue: None,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn publish_create_new(
    target: &NoClobberConfigTarget,
    rendered: &RenderedConfigBytes,
) -> ConfigPublicationOutcome {
    let pinned = match PinnedConfigTarget::pin(target) {
        Ok(pinned) => pinned,
        Err(PinTargetError::Missing) => {
            return ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::ParentMissing,
                residue: None,
            }
        }
        Err(PinTargetError::Unsafe) => {
            return ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::UnsafeParent,
                residue: None,
            }
        }
        Err(PinTargetError::Busy) => {
            return ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::WriterBusy,
                residue: None,
            }
        }
        Err(PinTargetError::Invalid) => {
            return ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::InvalidTarget,
                residue: None,
            }
        }
    };
    let (stage_name, mut file) = match pinned.create_stage() {
        Ok(stage) => stage,
        Err(_) => {
            return ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::StageCreate,
                residue: None,
            }
        }
    };
    if publication_fault(PublicationFaultPoint::Write)
        || file.write_all(rendered.as_bytes()).is_err()
    {
        let residue = cleanup_uninstalled_stage(pinned, file, stage_name)
            .map(ConfigPublicationResidue::UninstalledStageCleanup);
        return ConfigPublicationOutcome::Failed {
            kind: ConfigPublicationFailureKind::Write,
            residue,
        };
    }
    if publication_fault(PublicationFaultPoint::FileSync) || file.sync_all().is_err() {
        let residue = cleanup_uninstalled_stage(pinned, file, stage_name)
            .map(ConfigPublicationResidue::UninstalledStageCleanup);
        return ConfigPublicationOutcome::Failed {
            kind: ConfigPublicationFailureKind::FileSync,
            residue,
        };
    }

    run_before_install_hook();
    if publication_fault(PublicationFaultPoint::InstallLink) {
        let residue = cleanup_uninstalled_stage(pinned, file, stage_name)
            .map(ConfigPublicationResidue::UninstalledStageCleanup);
        return ConfigPublicationOutcome::Failed {
            kind: ConfigPublicationFailureKind::InstallLink,
            residue,
        };
    }
    if let Err(error) = pinned.link_stage(&stage_name) {
        let residue = cleanup_uninstalled_stage(pinned, file, stage_name)
            .map(ConfigPublicationResidue::UninstalledStageCleanup);
        return if error.kind() == std::io::ErrorKind::AlreadyExists && residue.is_none() {
            ConfigPublicationOutcome::Collision
        } else {
            ConfigPublicationOutcome::Failed {
                kind: if error.kind() == std::io::ErrorKind::AlreadyExists {
                    ConfigPublicationFailureKind::StageCleanup
                } else {
                    ConfigPublicationFailureKind::InstallLink
                },
                residue,
            }
        };
    }

    let (expected, exact_bytes) = if publication_fault(PublicationFaultPoint::ReceiptCapture) {
        (None, false)
    } else {
        capture_publication_snapshot(&file, rendered)
    };
    let mut installed = InstalledConfigReceipt {
        target: pinned,
        file,
        expected,
        stage_name: Some(stage_name),
    };
    if !exact_bytes {
        return ConfigPublicationOutcome::Failed {
            kind: ConfigPublicationFailureKind::ReceiptCapture,
            residue: Some(ConfigPublicationResidue::InstalledUnconfirmed(
                InstalledUnconfirmedReceipt(installed),
            )),
        };
    }
    if publication_fault(PublicationFaultPoint::StageCleanup) {
        return ConfigPublicationOutcome::Failed {
            kind: ConfigPublicationFailureKind::StageCleanup,
            residue: Some(ConfigPublicationResidue::StageCleanup(StageCleanupReceipt(
                installed,
            ))),
        };
    }
    let stage = installed
        .stage_name
        .as_ref()
        .expect("newly installed receipt retains its stage");
    if installed.target.unlink(stage).is_err() {
        return ConfigPublicationOutcome::Failed {
            kind: ConfigPublicationFailureKind::StageCleanup,
            residue: Some(ConfigPublicationResidue::StageCleanup(StageCleanupReceipt(
                installed,
            ))),
        };
    }
    installed.stage_name = None;
    let (expected, exact_bytes) = capture_publication_snapshot(&installed.file, rendered);
    installed.expected = expected;
    if !exact_bytes {
        return ConfigPublicationOutcome::Failed {
            kind: ConfigPublicationFailureKind::ReceiptCapture,
            residue: Some(ConfigPublicationResidue::InstalledUnconfirmed(
                InstalledUnconfirmedReceipt(installed),
            )),
        };
    }
    if publication_fault(PublicationFaultPoint::DirectorySync)
        || installed.target.sync_parent().is_err()
    {
        return ConfigPublicationOutcome::Failed {
            kind: ConfigPublicationFailureKind::DirectorySync,
            residue: Some(ConfigPublicationResidue::InstalledUnconfirmed(
                InstalledUnconfirmedReceipt(installed),
            )),
        };
    }
    ConfigPublicationOutcome::Created(installed)
}

enum LedgerEntry {
    Installed(InstalledConfigReceipt),
    InstalledUnconfirmed(InstalledUnconfirmedReceipt),
    StageCleanup(StageCleanupReceipt),
    RemovedUnconfirmed(RemovedUnconfirmedReceipt),
    Mismatch(InstalledConfigReceipt),
    DisplacedEntry(DisplacedEntryReceipt),
    RollbackFailure(InstalledConfigReceipt),
    Unsupported(InstalledConfigReceipt),
}

impl LedgerEntry {
    const fn residue_kind(&self) -> Option<ConfigResidueKind> {
        match self {
            Self::Installed(_) => None,
            Self::InstalledUnconfirmed(_) => Some(ConfigResidueKind::InstalledUnconfirmed),
            Self::StageCleanup(_) => Some(ConfigResidueKind::StageCleanup),
            Self::RemovedUnconfirmed(_) => Some(ConfigResidueKind::RemovedUnconfirmed),
            Self::Mismatch(_) => Some(ConfigResidueKind::Mismatch),
            Self::DisplacedEntry(_) => Some(ConfigResidueKind::DisplacedEntry),
            Self::RollbackFailure(_) => Some(ConfigResidueKind::RollbackFailure),
            Self::Unsupported(_) => Some(ConfigResidueKind::Unsupported),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfigLedgerError {
    InvalidTransaction,
    UnknownEffect { effect_id: u64 },
    UnknownStageCleanup { cleanup_id: u64 },
    UnresolvedStageCleanup { cleanup_id: u64 },
    MissingConcreteEffect { effect_id: u64 },
    UnexpectedConcreteEffect { effect_id: u64 },
    WrongTransaction { effect_id: u64 },
    InvalidLogicalEffect { effect_id: u64 },
    DuplicateLogicalEffect { effect_id: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigLedgerRegistrationFailureKind {
    ZeroEffect,
    DuplicateEffect,
    Full,
}

pub(super) struct ConfigLedgerRegistrationError {
    kind: ConfigLedgerRegistrationFailureKind,
    retained: Box<LedgerEntry>,
}

impl ConfigLedgerRegistrationError {
    pub(super) fn retry(
        self,
        ledger: &mut ConfigPublicationLedger,
        effect_id: u64,
    ) -> Result<OwnedEffectReceipt, Self> {
        ledger.register(effect_id, *self.retained)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct UninstalledStageCleanupId(u64);

pub(super) struct ConfigStageRegistrationError {
    kind: ConfigLedgerRegistrationFailureKind,
    retained: Box<UninstalledStageCleanupReceipt>,
}

impl fmt::Debug for ConfigStageRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigStageRegistrationError")
            .field("kind", &self.kind)
            .field("retained", &ConfigResidueKind::UninstalledStageCleanup)
            .finish()
    }
}

impl ConfigStageRegistrationError {
    pub(super) fn retry(
        self,
        ledger: &mut ConfigPublicationLedger,
        cleanup_id: u64,
    ) -> Result<UninstalledStageCleanupId, Self> {
        ledger.register_uninstalled_stage(cleanup_id, *self.retained)
    }
}

#[derive(Debug)]
pub(super) enum RegisteredConfigPublicationResidue {
    OwnedEffect(OwnedEffectReceipt),
    UninstalledStage(UninstalledStageCleanupId),
}

#[derive(Debug)]
pub(super) enum ConfigResidueRegistrationError {
    OwnedEffect(ConfigLedgerRegistrationError),
    UninstalledStage(ConfigStageRegistrationError),
}

impl fmt::Debug for ConfigLedgerRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigLedgerRegistrationError")
            .field("kind", &self.kind)
            .field("retained", &self.retained.residue_kind())
            .finish()
    }
}

pub(super) struct ConfigPublicationLedger {
    transaction_id: u64,
    entries: BTreeMap<u64, LedgerEntry>,
    stage_cleanups: BTreeMap<u64, UninstalledStageCleanupReceipt>,
}

pub(super) struct ConfigRecoveryTransferError {
    error: ConfigLedgerError,
    ledger: Box<ConfigPublicationLedger>,
    logical_effects: Vec<OwnedEffectReceipt>,
}

impl fmt::Debug for ConfigRecoveryTransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigRecoveryTransferError")
            .field("error", &self.error)
            .field(
                "logical_effects",
                &self
                    .logical_effects
                    .iter()
                    .map(OwnedEffectReceipt::id)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl ConfigRecoveryTransferError {
    pub(super) const fn error(&self) -> ConfigLedgerError {
        self.error
    }

    pub(super) fn retry_with(
        self,
        logical_effects: Vec<OwnedEffectReceipt>,
    ) -> Result<ConfigPublicationRecovery, Self> {
        (*self.ledger).into_recovery(logical_effects)
    }
}

impl fmt::Debug for ConfigPublicationLedger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigPublicationLedger")
            .field("transaction_id", &self.transaction_id)
            .field("effects", &self.entries.keys().collect::<Vec<_>>())
            .field(
                "stage_cleanups",
                &self.stage_cleanups.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ConfigPublicationLedger {
    pub(super) fn new(transaction_id: u64) -> Result<Self, ConfigLedgerError> {
        if transaction_id == 0 {
            return Err(ConfigLedgerError::InvalidTransaction);
        }
        Ok(Self {
            transaction_id,
            entries: BTreeMap::new(),
            stage_cleanups: BTreeMap::new(),
        })
    }

    pub(super) const fn capacity(&self) -> usize {
        MAX_OWNED_EFFECTS
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.stage_cleanups.is_empty()
    }

    fn register(
        &mut self,
        effect_id: u64,
        entry: LedgerEntry,
    ) -> Result<OwnedEffectReceipt, ConfigLedgerRegistrationError> {
        let failure = if effect_id == 0 {
            Some(ConfigLedgerRegistrationFailureKind::ZeroEffect)
        } else if self.entries.contains_key(&effect_id)
            || self.stage_cleanups.contains_key(&effect_id)
        {
            Some(ConfigLedgerRegistrationFailureKind::DuplicateEffect)
        } else if self.entries.len() + self.stage_cleanups.len() == MAX_OWNED_EFFECTS {
            Some(ConfigLedgerRegistrationFailureKind::Full)
        } else {
            None
        };
        if let Some(kind) = failure {
            return Err(ConfigLedgerRegistrationError {
                kind,
                retained: Box::new(entry),
            });
        }
        self.entries.insert(effect_id, entry);
        Ok(
            OwnedEffectReceipt::new(self.transaction_id, effect_id, OwnedEffectKind::ConfigFile)
                .expect("validated transaction and effect IDs form a logical receipt"),
        )
    }

    pub(super) fn register_created(
        &mut self,
        effect_id: u64,
        receipt: InstalledConfigReceipt,
    ) -> Result<OwnedEffectReceipt, ConfigLedgerRegistrationError> {
        self.register(effect_id, LedgerEntry::Installed(receipt))
    }

    pub(super) fn register_residue(
        &mut self,
        effect_id: u64,
        residue: ConfigPublicationResidue,
    ) -> Result<RegisteredConfigPublicationResidue, ConfigResidueRegistrationError> {
        let entry = match residue {
            ConfigPublicationResidue::UninstalledStageCleanup(receipt) => {
                return self
                    .register_uninstalled_stage(effect_id, receipt)
                    .map(RegisteredConfigPublicationResidue::UninstalledStage)
                    .map_err(ConfigResidueRegistrationError::UninstalledStage);
            }
            ConfigPublicationResidue::InstalledUnconfirmed(receipt) => {
                LedgerEntry::InstalledUnconfirmed(receipt)
            }
            ConfigPublicationResidue::StageCleanup(receipt) => LedgerEntry::StageCleanup(receipt),
        };
        self.register(effect_id, entry)
            .map(RegisteredConfigPublicationResidue::OwnedEffect)
            .map_err(ConfigResidueRegistrationError::OwnedEffect)
    }

    fn register_uninstalled_stage(
        &mut self,
        cleanup_id: u64,
        receipt: UninstalledStageCleanupReceipt,
    ) -> Result<UninstalledStageCleanupId, ConfigStageRegistrationError> {
        let failure = if cleanup_id == 0 {
            Some(ConfigLedgerRegistrationFailureKind::ZeroEffect)
        } else if self.entries.contains_key(&cleanup_id)
            || self.stage_cleanups.contains_key(&cleanup_id)
        {
            Some(ConfigLedgerRegistrationFailureKind::DuplicateEffect)
        } else if self.entries.len() + self.stage_cleanups.len() == MAX_OWNED_EFFECTS {
            Some(ConfigLedgerRegistrationFailureKind::Full)
        } else {
            None
        };
        if let Some(kind) = failure {
            return Err(ConfigStageRegistrationError {
                kind,
                retained: Box::new(receipt),
            });
        }
        self.stage_cleanups.insert(cleanup_id, receipt);
        Ok(UninstalledStageCleanupId(cleanup_id))
    }

    pub(super) fn residue_kind(&self, effect_id: u64) -> Option<ConfigResidueKind> {
        self.entries
            .get(&effect_id)
            .and_then(LedgerEntry::residue_kind)
    }

    pub(super) fn validate_terminal_residue(
        &self,
        retained: &[OwnedEffectReceipt],
    ) -> Result<(), ConfigLedgerError> {
        if let Some(cleanup_id) = self.stage_cleanups.keys().next() {
            return Err(ConfigLedgerError::UnresolvedStageCleanup {
                cleanup_id: *cleanup_id,
            });
        }
        let mut logical = BTreeSet::new();
        for effect in retained {
            if effect.kind() != OwnedEffectKind::ConfigFile {
                continue;
            }
            if effect.transaction_id() != self.transaction_id {
                return Err(ConfigLedgerError::WrongTransaction {
                    effect_id: effect.id(),
                });
            }
            if effect.id() == 0 {
                return Err(ConfigLedgerError::InvalidLogicalEffect {
                    effect_id: effect.id(),
                });
            }
            if !logical.insert(effect.id()) {
                return Err(ConfigLedgerError::DuplicateLogicalEffect {
                    effect_id: effect.id(),
                });
            }
            if !self.entries.contains_key(&effect.id()) {
                return Err(ConfigLedgerError::MissingConcreteEffect {
                    effect_id: effect.id(),
                });
            }
        }
        if let Some(effect_id) = self
            .entries
            .keys()
            .find(|effect_id| !logical.contains(effect_id))
        {
            return Err(ConfigLedgerError::UnexpectedConcreteEffect {
                effect_id: *effect_id,
            });
        }
        Ok(())
    }

    pub(super) fn into_recovery(
        self,
        retained: Vec<OwnedEffectReceipt>,
    ) -> Result<ConfigPublicationRecovery, ConfigRecoveryTransferError> {
        match self.validate_terminal_residue(&retained) {
            Ok(()) => Ok(ConfigPublicationRecovery {
                ledger: self,
                logical_effects: retained,
            }),
            Err(error) => Err(ConfigRecoveryTransferError {
                error,
                ledger: Box::new(self),
                logical_effects: retained,
            }),
        }
    }

    pub(super) fn rollback(
        &mut self,
        effect_id: u64,
    ) -> Result<ConfigRollbackResult, ConfigLedgerError> {
        self.rollback_internal(effect_id, |_| {})
    }

    pub(super) fn rollback_uninstalled_stage(
        &mut self,
        cleanup_id: UninstalledStageCleanupId,
    ) -> Result<ConfigRollbackResult, ConfigLedgerError> {
        let receipt = self.stage_cleanups.remove(&cleanup_id.0).ok_or(
            ConfigLedgerError::UnknownStageCleanup {
                cleanup_id: cleanup_id.0,
            },
        )?;
        let (result, retained) = rollback_uninstalled_stage(receipt);
        if let Some(retained) = retained {
            self.stage_cleanups.insert(cleanup_id.0, retained);
        }
        Ok(result)
    }

    #[cfg(test)]
    pub(super) fn rollback_with_hook<F>(
        &mut self,
        effect_id: u64,
        hook: F,
    ) -> Result<ConfigRollbackResult, ConfigLedgerError>
    where
        F: FnMut(RollbackHookPoint<'_>),
    {
        self.rollback_internal(effect_id, hook)
    }

    fn rollback_internal<F>(
        &mut self,
        effect_id: u64,
        mut hook: F,
    ) -> Result<ConfigRollbackResult, ConfigLedgerError>
    where
        F: FnMut(RollbackHookPoint<'_>),
    {
        let entry = self
            .entries
            .remove(&effect_id)
            .ok_or(ConfigLedgerError::UnknownEffect { effect_id })?;
        let (result, retained) = rollback_entry(entry, &mut hook);
        if let Some(retained) = retained {
            self.entries.insert(effect_id, retained);
        }
        Ok(result)
    }
}

pub(super) struct ConfigPublicationRecovery {
    ledger: ConfigPublicationLedger,
    logical_effects: Vec<OwnedEffectReceipt>,
}

impl ConfigPublicationRecovery {
    pub(super) fn effect_ids(&self) -> Vec<u64> {
        self.logical_effects
            .iter()
            .filter(|effect| effect.kind() == OwnedEffectKind::ConfigFile)
            .map(OwnedEffectReceipt::id)
            .collect()
    }

    pub(super) fn rollback(
        &mut self,
        effect_id: u64,
    ) -> Result<ConfigRollbackResult, ConfigLedgerError> {
        let result = self.ledger.rollback(effect_id)?;
        if result == ConfigRollbackResult::Removed {
            self.logical_effects
                .retain(|effect| effect.id() != effect_id);
        }
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfigRollbackResult {
    Removed,
    Incomplete(ConfigResidueKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RollbackFaultPoint {
    DirectorySync,
}

#[cfg(test)]
pub(super) fn inject_next_rollback_fault(point: RollbackFaultPoint) -> RollbackFaultFixtureGuard {
    let generation = next_test_fixture_generation();
    NEXT_ROLLBACK_FAULT.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none());
        *slot = Some(RollbackFaultPlan { generation, point });
    });
    RollbackFaultFixtureGuard { generation }
}

fn rollback_fault(point: RollbackFaultPoint) -> bool {
    #[cfg(test)]
    {
        NEXT_ROLLBACK_FAULT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|plan| plan.point == point) {
                *slot = None;
                true
            } else {
                false
            }
        })
    }
    #[cfg(not(test))]
    {
        let _ = point;
        false
    }
}

pub(super) enum RollbackHookPoint<'a> {
    TargetVerified { target: &'a Path },
    Quarantined { quarantine: &'a Path },
    QuarantineVerified { quarantine: &'a Path },
}

fn rollback_entry<F>(
    entry: LedgerEntry,
    hook: &mut F,
) -> (ConfigRollbackResult, Option<LedgerEntry>)
where
    F: FnMut(RollbackHookPoint<'_>),
{
    match entry {
        LedgerEntry::RemovedUnconfirmed(receipt) => {
            if rollback_fault(RollbackFaultPoint::DirectorySync)
                || receipt.0.target.sync_parent().is_err()
            {
                (
                    ConfigRollbackResult::Incomplete(ConfigResidueKind::RemovedUnconfirmed),
                    Some(LedgerEntry::RemovedUnconfirmed(receipt)),
                )
            } else {
                (ConfigRollbackResult::Removed, None)
            }
        }
        LedgerEntry::Mismatch(receipt) => (
            ConfigRollbackResult::Incomplete(ConfigResidueKind::Mismatch),
            Some(LedgerEntry::Mismatch(receipt)),
        ),
        LedgerEntry::DisplacedEntry(receipt) => {
            let _ = &receipt.installed;
            #[cfg(unix)]
            let _ = &receipt.displaced_name;
            (
                ConfigRollbackResult::Incomplete(ConfigResidueKind::DisplacedEntry),
                Some(LedgerEntry::DisplacedEntry(receipt)),
            )
        }
        LedgerEntry::RollbackFailure(receipt) => (
            ConfigRollbackResult::Incomplete(ConfigResidueKind::RollbackFailure),
            Some(LedgerEntry::RollbackFailure(receipt)),
        ),
        LedgerEntry::Unsupported(receipt) => (
            ConfigRollbackResult::Incomplete(ConfigResidueKind::Unsupported),
            Some(LedgerEntry::Unsupported(receipt)),
        ),
        LedgerEntry::Installed(receipt) => rollback_installed(receipt, hook),
        LedgerEntry::InstalledUnconfirmed(InstalledUnconfirmedReceipt(receipt)) => {
            rollback_installed(receipt, hook)
        }
        LedgerEntry::StageCleanup(StageCleanupReceipt(receipt)) => {
            rollback_installed(receipt, hook)
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn rollback_uninstalled_stage(
    receipt: UninstalledStageCleanupReceipt,
) -> (ConfigRollbackResult, Option<UninstalledStageCleanupReceipt>) {
    (
        ConfigRollbackResult::Incomplete(ConfigResidueKind::Unsupported),
        Some(receipt),
    )
}

#[cfg(target_os = "linux")]
fn rollback_uninstalled_stage(
    mut receipt: UninstalledStageCleanupReceipt,
) -> (ConfigRollbackResult, Option<UninstalledStageCleanupReceipt>) {
    let _ = &receipt.file;
    if let Some(stage) = receipt.stage_name.take() {
        match quarantine_verified_name(
            &receipt.target,
            &receipt.file,
            receipt.expected.as_ref(),
            &stage,
            false,
            &mut |_| {},
        ) {
            QuarantineRemoval::Removed => {}
            QuarantineRemoval::Displaced(displaced_name) => {
                receipt.stage_name = Some(displaced_name);
                return (
                    ConfigRollbackResult::Incomplete(ConfigResidueKind::UninstalledStageCleanup),
                    Some(receipt),
                );
            }
            QuarantineRemoval::Mismatch
            | QuarantineRemoval::Failure
            | QuarantineRemoval::Unsupported => {
                receipt.stage_name = Some(stage);
                return (
                    ConfigRollbackResult::Incomplete(ConfigResidueKind::UninstalledStageCleanup),
                    Some(receipt),
                );
            }
        }
    }
    if rollback_fault(RollbackFaultPoint::DirectorySync) || receipt.target.sync_parent().is_err() {
        (
            ConfigRollbackResult::Incomplete(ConfigResidueKind::UninstalledStageCleanup),
            Some(receipt),
        )
    } else {
        (ConfigRollbackResult::Removed, None)
    }
}

#[cfg(not(target_os = "linux"))]
fn rollback_installed<F>(
    receipt: InstalledConfigReceipt,
    _hook: &mut F,
) -> (ConfigRollbackResult, Option<LedgerEntry>)
where
    F: FnMut(RollbackHookPoint<'_>),
{
    (
        ConfigRollbackResult::Incomplete(ConfigResidueKind::Unsupported),
        Some(LedgerEntry::Unsupported(receipt)),
    )
}

#[cfg(target_os = "linux")]
fn rollback_installed<F>(
    mut receipt: InstalledConfigReceipt,
    hook: &mut F,
) -> (ConfigRollbackResult, Option<LedgerEntry>)
where
    F: FnMut(RollbackHookPoint<'_>),
{
    if let Some(stage) = receipt.stage_name.take() {
        match quarantine_verified_name(
            &receipt.target,
            &receipt.file,
            receipt.expected.as_ref(),
            &stage,
            false,
            hook,
        ) {
            QuarantineRemoval::Removed => {
                if receipt.refresh_after_stage_removal().is_err() {
                    return (
                        ConfigRollbackResult::Incomplete(ConfigResidueKind::RollbackFailure),
                        Some(LedgerEntry::RollbackFailure(receipt)),
                    );
                }
            }
            QuarantineRemoval::Mismatch => {
                receipt.stage_name = Some(stage);
                return (
                    ConfigRollbackResult::Incomplete(ConfigResidueKind::Mismatch),
                    Some(LedgerEntry::Mismatch(receipt)),
                );
            }
            QuarantineRemoval::Displaced(displaced_name) => {
                receipt.stage_name = Some(stage);
                return (
                    ConfigRollbackResult::Incomplete(ConfigResidueKind::DisplacedEntry),
                    Some(LedgerEntry::DisplacedEntry(DisplacedEntryReceipt {
                        installed: receipt,
                        displaced_name,
                    })),
                );
            }
            QuarantineRemoval::Failure => {
                receipt.stage_name = Some(stage);
                return (
                    ConfigRollbackResult::Incomplete(ConfigResidueKind::RollbackFailure),
                    Some(LedgerEntry::RollbackFailure(receipt)),
                );
            }
            QuarantineRemoval::Unsupported => {
                receipt.stage_name = Some(stage);
                return (
                    ConfigRollbackResult::Incomplete(ConfigResidueKind::Unsupported),
                    Some(LedgerEntry::Unsupported(receipt)),
                );
            }
        }
    }

    let target_name = receipt.target.name.clone();
    match quarantine_verified_name(
        &receipt.target,
        &receipt.file,
        receipt.expected.as_ref(),
        &target_name,
        true,
        hook,
    ) {
        QuarantineRemoval::Removed => {
            if rollback_fault(RollbackFaultPoint::DirectorySync)
                || receipt.target.sync_parent().is_err()
            {
                (
                    ConfigRollbackResult::Incomplete(ConfigResidueKind::RemovedUnconfirmed),
                    Some(LedgerEntry::RemovedUnconfirmed(RemovedUnconfirmedReceipt(
                        receipt,
                    ))),
                )
            } else {
                (ConfigRollbackResult::Removed, None)
            }
        }
        QuarantineRemoval::Mismatch => (
            ConfigRollbackResult::Incomplete(ConfigResidueKind::Mismatch),
            Some(LedgerEntry::Mismatch(receipt)),
        ),
        QuarantineRemoval::Displaced(displaced_name) => (
            ConfigRollbackResult::Incomplete(ConfigResidueKind::DisplacedEntry),
            Some(LedgerEntry::DisplacedEntry(DisplacedEntryReceipt {
                installed: receipt,
                displaced_name,
            })),
        ),
        QuarantineRemoval::Failure => (
            ConfigRollbackResult::Incomplete(ConfigResidueKind::RollbackFailure),
            Some(LedgerEntry::RollbackFailure(receipt)),
        ),
        QuarantineRemoval::Unsupported => (
            ConfigRollbackResult::Incomplete(ConfigResidueKind::Unsupported),
            Some(LedgerEntry::Unsupported(receipt)),
        ),
    }
}

#[cfg(target_os = "linux")]
enum QuarantineRemoval {
    Removed,
    Mismatch,
    Displaced(CString),
    Failure,
    Unsupported,
}

#[cfg(target_os = "linux")]
fn classify_quarantine_location(
    target: &PinnedConfigTarget,
    file: &File,
    baseline: &ConfigFileSnapshot,
    original: &CStr,
    quarantine: &CStr,
) -> QuarantineRemoval {
    let Ok((current, _)) = ConfigFileSnapshot::read_owned(file) else {
        return QuarantineRemoval::Failure;
    };
    let Ok(original) = target.read_named_snapshot(original) else {
        return QuarantineRemoval::Failure;
    };
    let Ok(displaced) = target.read_named_snapshot(quarantine) else {
        return QuarantineRemoval::Failure;
    };

    if displaced
        .as_ref()
        .is_some_and(|displaced| current.matches(displaced))
    {
        return QuarantineRemoval::Displaced(quarantine.to_owned());
    }
    if original
        .as_ref()
        .is_some_and(|original| current.matches(original))
    {
        return QuarantineRemoval::Mismatch;
    }
    if baseline.same_owned_inode_after_link_removal(&current) {
        return QuarantineRemoval::Removed;
    }
    QuarantineRemoval::Failure
}

#[cfg(target_os = "linux")]
fn quarantine_verified_name<F>(
    target: &PinnedConfigTarget,
    file: &File,
    expected: Option<&ConfigFileSnapshot>,
    name: &CStr,
    observe: bool,
    hook: &mut F,
) -> QuarantineRemoval
where
    F: FnMut(RollbackHookPoint<'_>),
{
    let Some(expected) = expected else {
        return QuarantineRemoval::Failure;
    };
    let before = match target.read_named_snapshot(name) {
        Ok(Some(before)) => before,
        Ok(None) => return QuarantineRemoval::Mismatch,
        Err(_) => return QuarantineRemoval::Failure,
    };
    if !expected.matches(&before) {
        return QuarantineRemoval::Mismatch;
    }
    if observe {
        hook(RollbackHookPoint::TargetVerified {
            target: &target.path,
        });
    }
    let quarantine = match target.quarantine(name) {
        Ok(quarantine) => quarantine,
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)
            ) =>
        {
            return QuarantineRemoval::Unsupported
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return QuarantineRemoval::Mismatch
        }
        Err(_) => return QuarantineRemoval::Failure,
    };
    let quarantine_path = target.named_path(&quarantine);
    let renamed = match target.read_named_snapshot(&quarantine) {
        Ok(Some(renamed)) if expected.same_owned_inode_after_rename(&renamed) => renamed,
        _ => {
            let _ = target.rename_noreplace(&quarantine, name);
            return classify_quarantine_location(target, file, expected, name, &quarantine);
        }
    };
    if observe {
        hook(RollbackHookPoint::Quarantined {
            quarantine: &quarantine_path,
        });
    }
    let after = target.read_named_snapshot(&quarantine);
    if after
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .is_some_and(|after| renamed.matches(after))
    {
        if observe {
            hook(RollbackHookPoint::QuarantineVerified {
                quarantine: &quarantine_path,
            });
        }
        let _ = target.unlink(&quarantine);
        return classify_quarantine_location(target, file, &renamed, name, &quarantine);
    }

    let _ = target.rename_noreplace(&quarantine, name);
    classify_quarantine_location(target, file, &renamed, name, &quarantine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::onboarding::{
        AcquisitionReceipt, CatalogIntent, CredentialPlan, GuidedSetupTransaction,
        NormalizedSetupPreview, OwnedEffectKind, PublicationReceipt, RollbackReceipt, SetupAdapter,
        SetupDirective, SetupFailureKind, SetupFence, SetupProposalInput, SetupRouteInput,
        SetupSelection, ValidationReceipt, MAX_OWNED_EFFECTS,
    };
    use crate::config::{Config, GatewayAdapter};
    use std::path::{Path, PathBuf};

    const SECRET_SHAPED_MODEL: &str = "hf_secret_shaped_model_marker";

    fn preview(target: impl Into<PathBuf>, model: &str) -> NormalizedSetupPreview {
        NormalizedSetupPreview::normalize(SetupProposalInput::new(
            target,
            SetupSelection::Custom {
                route: SetupRouteInput::new(
                    "example",
                    "https://api.example.test",
                    "/v1/messages",
                    Some("/v1/models"),
                    model,
                    CatalogIntent::CatalogAfterConsent,
                ),
                adapter: SetupAdapter::AnthropicMessages,
                credential: CredentialPlan::ManagedBearer,
            },
        ))
        .unwrap()
    }

    fn render(target: &Path) -> (NormalizedSetupPreview, RenderedConfigBytes) {
        let preview = preview(target, "model-v1");
        let rendered = RenderedConfigBytes::render(&preview).unwrap();
        (preview, rendered)
    }

    #[cfg(target_os = "linux")]
    fn snapshot_facts(
        snapshot: &ConfigFileSnapshot,
    ) -> (u64, u64, u64, u32, u32, u32, i64, i64, u64) {
        (
            snapshot.size,
            snapshot.device,
            snapshot.inode,
            snapshot.mode,
            snapshot.user,
            snapshot.group,
            snapshot.changed_seconds,
            snapshot.changed_nanoseconds,
            snapshot.links,
        )
    }

    #[cfg(target_os = "linux")]
    fn snapshot_result_facts(snapshot: std::io::Result<Option<ConfigFileSnapshot>>) -> String {
        match snapshot {
            Ok(Some(snapshot)) => format!("present:{:?}", snapshot_facts(&snapshot)),
            Ok(None) => "missing".to_string(),
            Err(error) => format!("error:{:?}:{:?}", error.kind(), error.raw_os_error()),
        }
    }

    #[cfg(target_os = "linux")]
    fn installed_receipt_facts(receipt: &InstalledConfigReceipt) -> String {
        let expected = receipt.expected.as_ref().map(snapshot_facts);
        let target =
            snapshot_result_facts(receipt.target.read_named_snapshot(&receipt.target.name));
        let stage = receipt.stage_name.as_ref().map_or_else(
            || "none".to_string(),
            |stage| snapshot_result_facts(receipt.target.read_named_snapshot(stage)),
        );
        format!(
            "expected={expected:?},target={target},stage_retained={},stage={stage}",
            receipt.stage_name.is_some()
        )
    }

    #[cfg(target_os = "linux")]
    fn directory_entries(directory: &Path) -> Vec<String> {
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    #[cfg(target_os = "linux")]
    fn outcome_facts(outcome: &ConfigPublicationOutcome, directory: &Path) -> String {
        let outcome = match outcome {
            ConfigPublicationOutcome::Created(receipt) => {
                format!("created: {}", installed_receipt_facts(receipt))
            }
            ConfigPublicationOutcome::Collision => "collision".to_string(),
            ConfigPublicationOutcome::Failed { kind, residue } => format!(
                "failed:{kind:?}:residue={:?}",
                residue.as_ref().map(ConfigPublicationResidue::kind)
            ),
        };
        format!("{outcome},entries={:?}", directory_entries(directory))
    }

    #[cfg(target_os = "linux")]
    fn ledger_entry_facts(ledger: &ConfigPublicationLedger, effect_id: u64) -> String {
        let Some(entry) = ledger.entries.get(&effect_id) else {
            return "entry=missing".to_string();
        };
        let receipt = match entry {
            LedgerEntry::Installed(receipt)
            | LedgerEntry::Mismatch(receipt)
            | LedgerEntry::RollbackFailure(receipt)
            | LedgerEntry::Unsupported(receipt) => receipt,
            LedgerEntry::InstalledUnconfirmed(InstalledUnconfirmedReceipt(receipt))
            | LedgerEntry::StageCleanup(StageCleanupReceipt(receipt))
            | LedgerEntry::RemovedUnconfirmed(RemovedUnconfirmedReceipt(receipt)) => receipt,
            LedgerEntry::DisplacedEntry(receipt) => &receipt.installed,
        };
        format!(
            "entry={:?},{}",
            entry.residue_kind(),
            installed_receipt_facts(receipt)
        )
    }

    #[cfg(target_os = "linux")]
    fn rollback_removed(
        ledger: &mut ConfigPublicationLedger,
        directory: &Path,
    ) -> Result<(), String> {
        let mut phases = Vec::new();
        let rollback = ledger
            .rollback_with_hook(1, |phase| {
                phases.push(match phase {
                    RollbackHookPoint::TargetVerified { .. } => "after-target-verification",
                    RollbackHookPoint::Quarantined { .. } => "after-quarantine",
                    RollbackHookPoint::QuarantineVerified { .. } => "after-quarantine-verification",
                });
            })
            .unwrap();
        if rollback == ConfigRollbackResult::Removed {
            Ok(())
        } else {
            Err(format!(
                "rollback={rollback:?},phases={phases:?},{},entries={:?}",
                ledger_entry_facts(ledger, 1),
                directory_entries(directory)
            ))
        }
    }

    fn created_receipt(target: &Path) -> InstalledConfigReceipt {
        let (preview, rendered) = render(target);
        match publish_create_new(preview.target(), &rendered) {
            ConfigPublicationOutcome::Created(receipt) => receipt,
            ConfigPublicationOutcome::Collision => panic!("fresh target collided"),
            ConfigPublicationOutcome::Failed { kind, .. } => {
                panic!("fresh publication failed: {kind:?}")
            }
        }
    }

    fn publish_into_ledger(
        transaction_id: u64,
        effect_id: u64,
        target: &Path,
    ) -> ConfigPublicationLedger {
        let receipt = created_receipt(target);
        let mut ledger = ConfigPublicationLedger::new(transaction_id).unwrap();
        let logical = ledger.register_created(effect_id, receipt).unwrap();
        assert_eq!(logical.kind(), OwnedEffectKind::ConfigFile);
        ledger
    }

    fn publication_fence(transaction: &GuidedSetupTransaction) -> SetupFence {
        match transaction.directive() {
            SetupDirective::Publish { fence, .. } => fence,
            other => panic!("expected publication fence, got {other:?}"),
        }
    }

    #[test]
    fn adapter_render_is_bounded_parseable_and_secret_free() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let preview = preview(&target, SECRET_SHAPED_MODEL);
        let rendered = RenderedConfigBytes::render(&preview).unwrap();

        assert!(rendered.as_bytes().len() <= MAX_RENDERED_CONFIG_BYTES);
        let text = std::str::from_utf8(rendered.as_bytes()).unwrap();
        let parsed: Config = toml::from_str(text).unwrap();
        assert_eq!(parsed.default_gateway.as_deref(), Some("example"));
        let gateway = &parsed.gateways["example"];
        assert_eq!(gateway.adapter, GatewayAdapter::AnthropicMessages);
        assert_eq!(gateway.model.as_deref(), Some(SECRET_SHAPED_MODEL));
        assert_eq!(gateway.auth.method, "bearer-env");
        assert!(gateway.auth.token.is_none());
        assert!(!text.contains("token ="));

        let debug = format!("{rendered:?}");
        assert!(
            !debug.contains(SECRET_SHAPED_MODEL),
            "bytes leaked: {debug}"
        );
        assert!(
            !debug.contains("api.example.test"),
            "endpoint leaked: {debug}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_parent_is_refused_and_never_created() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("missing");
        let target = parent.join("config.toml");
        let (preview, rendered) = render(&target);

        assert!(matches!(
            publish_create_new(preview.target(), &rendered),
            ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::ParentMissing,
                residue: None,
            }
        ));
        assert!(!parent.exists(), "publication created its parent");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn symlink_parent_is_refused_and_final_symlink_is_a_collision() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let real_parent = directory.path().join("real");
        std::fs::create_dir(&real_parent).unwrap();
        let linked_parent = directory.path().join("linked");
        symlink(&real_parent, &linked_parent).unwrap();
        let linked_target = linked_parent.join("config.toml");
        let (preview, rendered) = render(&linked_target);
        assert!(matches!(
            publish_create_new(preview.target(), &rendered),
            ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::UnsafeParent,
                residue: None,
            }
        ));
        assert!(!real_parent.join("config.toml").exists());

        let final_target = real_parent.join("config.toml");
        let outside = directory.path().join("outside");
        std::fs::write(&outside, b"preexisting").unwrap();
        symlink(&outside, &final_target).unwrap();
        let (preview, rendered) = render(&final_target);
        assert!(matches!(
            publish_create_new(preview.target(), &rendered),
            ConfigPublicationOutcome::Collision
        ));
        assert_eq!(std::fs::read(&outside).unwrap(), b"preexisting");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn preexisting_target_and_raced_final_install_are_no_overwrite_collisions() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        std::fs::write(&target, b"preexisting").unwrap();
        let (preview, rendered) = render(&target);
        assert!(matches!(
            publish_create_new(preview.target(), &rendered),
            ConfigPublicationOutcome::Collision
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"preexisting");

        std::fs::remove_file(&target).unwrap();
        let _hook_scope = inject_publication_hook(PublicationHook::BeforeInstall(Box::new({
            let target = target.clone();
            move || std::fs::write(&target, b"raced replacement").unwrap()
        })));
        assert!(matches!(
            publish_create_new(preview.target(), &rendered),
            ConfigPublicationOutcome::Collision
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"raced replacement");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn successful_publication_is_exact_mode_single_link_and_stage_free() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let receipt = created_receipt(&target);
        let metadata = std::fs::symlink_metadata(&target).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.len(), receipt.installed_size());
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            1,
            "same-directory stage was not cleaned"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn write_file_sync_and_link_cuts_never_claim_an_installed_receipt() {
        for fault in [
            PublicationFaultPoint::Write,
            PublicationFaultPoint::FileSync,
            PublicationFaultPoint::InstallLink,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let target = directory.path().join("config.toml");
            let (preview, rendered) = render(&target);
            let _fault_scope = inject_next_publication_fault(fault);
            let outcome = publish_create_new(preview.target(), &rendered);
            assert!(matches!(
                outcome,
                ConfigPublicationOutcome::Failed { residue: None, .. }
            ));
            assert!(!target.exists(), "pre-install cut created the target");
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn post_link_stage_cleanup_and_directory_sync_cuts_are_typed_residue() {
        let cases = [
            (
                PublicationFaultPoint::StageCleanup,
                ConfigResidueKind::StageCleanup,
            ),
            (
                PublicationFaultPoint::DirectorySync,
                ConfigResidueKind::InstalledUnconfirmed,
            ),
        ];
        for (fault, expected) in cases {
            let directory = tempfile::tempdir().unwrap();
            let target = directory.path().join("config.toml");
            let (preview, rendered) = render(&target);
            let _fault_scope = inject_next_publication_fault(fault);
            let residue = match publish_create_new(preview.target(), &rendered) {
                ConfigPublicationOutcome::Failed {
                    residue: Some(residue),
                    ..
                } => residue,
                _ => panic!("post-install cut must retain concrete residue"),
            };
            assert_eq!(residue.kind(), expected);
            assert!(target.exists());

            let mut ledger = ConfigPublicationLedger::new(83).unwrap();
            ledger.register_residue(1, residue).unwrap();
            assert_eq!(ledger.residue_kind(1), Some(expected));
            assert_eq!(ledger.rollback(1).unwrap(), ConfigRollbackResult::Removed);
            assert!(!target.exists());
            assert!(ledger.is_empty());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parallel_publication_receipts_remain_exact_until_rollback() {
        const WORKERS: usize = 8;
        const ROUNDS: usize = 4;
        let armed = std::sync::Arc::new(std::sync::Barrier::new(WORKERS));
        let consume = std::sync::Arc::new(std::sync::Barrier::new(WORKERS));
        let workers = (0..WORKERS)
            .map(|worker| {
                let armed = std::sync::Arc::clone(&armed);
                let consume = std::sync::Arc::clone(&consume);
                std::thread::spawn(move || {
                    let directory = tempfile::tempdir().unwrap();
                    let mut failures = Vec::new();

                    for round in 0..ROUNDS {
                        let retained_stage = (worker + round) % 2 == 1;
                        let target = directory.path().join(format!("receipt-{round}.toml"));
                        let (preview, rendered) = render(&target);
                        let _fault_scope = retained_stage.then(|| {
                            inject_next_publication_fault(PublicationFaultPoint::StageCleanup)
                        });
                        if worker == 0 {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                        armed.wait();

                        let ledger = if retained_stage {
                            match publish_create_new(preview.target(), &rendered) {
                                ConfigPublicationOutcome::Failed {
                                    kind: ConfigPublicationFailureKind::StageCleanup,
                                    residue: Some(residue),
                                } if residue.kind() == ConfigResidueKind::StageCleanup => {
                                    let mut ledger = ConfigPublicationLedger::new(303).unwrap();
                                    ledger.register_residue(1, residue).unwrap();
                                    Ok(ledger)
                                }
                                outcome => Err(outcome_facts(&outcome, directory.path())),
                            }
                        } else {
                            match publish_create_new(preview.target(), &rendered) {
                                ConfigPublicationOutcome::Created(receipt) => {
                                    let mut ledger = ConfigPublicationLedger::new(301).unwrap();
                                    ledger.register_created(1, receipt).unwrap();
                                    Ok(ledger)
                                }
                                outcome => Err(outcome_facts(&outcome, directory.path())),
                            }
                        };
                        consume.wait();

                        match ledger {
                            Ok(mut ledger) => {
                                if let Err(facts) = rollback_removed(&mut ledger, directory.path()) {
                                    failures.push(format!(
                                        "worker={worker},round={round},stage={retained_stage},{facts}"
                                    ));
                                }
                            }
                            Err(facts) => failures.push(format!(
                                "worker={worker},round={round},stage={retained_stage},outcome={facts}"
                            )),
                        }
                    }
                    failures
                })
            })
            .collect::<Vec<_>>();

        let failures = workers
            .into_iter()
            .flat_map(|worker| match worker.join() {
                Ok(failures) => failures,
                Err(_) => vec!["parallel publication worker panicked".to_string()],
            })
            .collect::<Vec<_>>();
        assert!(failures.is_empty(), "failures={failures:#?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn post_link_stage_mutation_is_never_accepted_as_created() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let (preview, rendered) = render(&target);
        let parent = directory.path().to_path_buf();
        let _hook_scope =
            inject_publication_hook(PublicationHook::BeforeInstall(Box::new(move || {
                let stage = std::fs::read_dir(&parent)
                    .unwrap()
                    .map(Result::unwrap)
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".kitsu-config-")
                    })
                    .expect("same-directory stage");
                std::fs::write(stage.path(), b"same-user stage mutation").unwrap();
            })));

        let residue = match publish_create_new(preview.target(), &rendered) {
            ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::ReceiptCapture,
                residue: Some(ConfigPublicationResidue::InstalledUnconfirmed(receipt)),
            } => ConfigPublicationResidue::InstalledUnconfirmed(receipt),
            _ => panic!("mutated installed inode must remain installed-unconfirmed"),
        };
        assert_eq!(std::fs::read(&target).unwrap(), b"same-user stage mutation");
        let mut ledger = ConfigPublicationLedger::new(85).unwrap();
        assert!(matches!(
            ledger.register_residue(1, residue).unwrap(),
            RegisteredConfigPublicationResidue::OwnedEffect(_)
        ));
        assert_eq!(
            ledger.rollback(1).unwrap(),
            ConfigRollbackResult::Incomplete(ConfigResidueKind::Mismatch)
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"same-user stage mutation");
        assert_eq!(ledger.residue_kind(1), Some(ConfigResidueKind::Mismatch));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn preinstall_cleanup_failures_have_a_nonlogical_ledger_channel() {
        for primary in [
            PublicationFaultPoint::Write,
            PublicationFaultPoint::FileSync,
            PublicationFaultPoint::InstallLink,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let target = directory.path().join("config.toml");
            let (preview, rendered) = render(&target);
            let _fault_scope = inject_publication_faults(&[
                primary,
                PublicationFaultPoint::UninstalledStageCleanup,
            ]);
            let residue = match publish_create_new(preview.target(), &rendered) {
                ConfigPublicationOutcome::Failed {
                    residue: Some(ConfigPublicationResidue::UninstalledStageCleanup(receipt)),
                    ..
                } => ConfigPublicationResidue::UninstalledStageCleanup(receipt),
                _ => panic!("failed preinstall cleanup must retain its stage"),
            };
            assert!(!target.exists());
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

            let mut ledger = ConfigPublicationLedger::new(86).unwrap();
            let registration = ledger.register_residue(1, residue).unwrap();
            let RegisteredConfigPublicationResidue::UninstalledStage(cleanup_id) = registration
            else {
                panic!("uninstalled stage must not become a logical config effect")
            };
            assert_eq!(
                ledger.validate_terminal_residue(&[]),
                Err(ConfigLedgerError::UnresolvedStageCleanup { cleanup_id: 1 })
            );
            assert_eq!(
                ledger.rollback_uninstalled_stage(cleanup_id).unwrap(),
                ConfigRollbackResult::Removed
            );
            assert!(ledger.is_empty());
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        std::fs::write(&target, b"preexisting").unwrap();
        let (preview, rendered) = render(&target);
        let _fault_scope =
            inject_next_publication_fault(PublicationFaultPoint::UninstalledStageCleanup);
        let residue = match publish_create_new(preview.target(), &rendered) {
            ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::StageCleanup,
                residue: Some(ConfigPublicationResidue::UninstalledStageCleanup(receipt)),
            } => ConfigPublicationResidue::UninstalledStageCleanup(receipt),
            _ => panic!("collision is pure only after its stage cleanup is durable"),
        };
        let mut collision_ledger = ConfigPublicationLedger::new(87).unwrap();
        let RegisteredConfigPublicationResidue::UninstalledStage(cleanup_id) =
            collision_ledger.register_residue(1, residue).unwrap()
        else {
            panic!("collision cleanup must not become a logical config effect")
        };
        assert_eq!(
            collision_ledger
                .rollback_uninstalled_stage(cleanup_id)
                .unwrap(),
            ConfigRollbackResult::Removed
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"preexisting");
        assert!(collision_ledger.is_empty());

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let (preview, rendered) = render(&target);
        let _fault_scope = inject_publication_faults(&[
            PublicationFaultPoint::Write,
            PublicationFaultPoint::UninstalledDirectorySync,
        ]);
        let residue = match publish_create_new(preview.target(), &rendered) {
            ConfigPublicationOutcome::Failed {
                residue: Some(ConfigPublicationResidue::UninstalledStageCleanup(receipt)),
                ..
            } => receipt,
            _ => panic!("directory-sync uncertainty must retain stage cleanup ownership"),
        };
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        let mut ledger = ConfigPublicationLedger::new(88).unwrap();
        let registration = ledger
            .register_residue(
                0,
                ConfigPublicationResidue::UninstalledStageCleanup(residue),
            )
            .unwrap_err();
        let ConfigResidueRegistrationError::UninstalledStage(registration) = registration else {
            panic!("stage registration error lost its concrete owner")
        };
        let cleanup_id = registration.retry(&mut ledger, 1).unwrap();
        assert_eq!(
            ledger.rollback_uninstalled_stage(cleanup_id).unwrap(),
            ConfigRollbackResult::Removed
        );
        assert!(ledger.is_empty());
    }

    #[test]
    fn transaction_scoped_ledger_is_bounded_by_the_transaction_effect_cap() {
        let ledger = ConfigPublicationLedger::new(87).unwrap();
        assert_eq!(ledger.capacity(), MAX_OWNED_EFFECTS);
        assert!(ConfigPublicationLedger::new(0).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_owned_registration_can_be_consumed_and_retried_without_loss() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let receipt = created_receipt(&target);
        let mut ledger = ConfigPublicationLedger::new(88).unwrap();
        let registration = ledger.register_created(0, receipt).unwrap_err();
        let logical = registration.retry(&mut ledger, 1).unwrap();
        assert_eq!(logical.id(), 1);
        assert_eq!(ledger.rollback(1).unwrap(), ConfigRollbackResult::Removed);
        assert!(!target.exists());

        let transfer_target = directory.path().join("transfer.toml");
        let receipt = created_receipt(&transfer_target);
        let mut ledger = ConfigPublicationLedger::new(90).unwrap();
        let logical = ledger.register_created(1, receipt).unwrap();
        let wrong = OwnedEffectReceipt::new(90, 2, OwnedEffectKind::ConfigFile).unwrap();
        let transfer = match ledger.into_recovery(vec![wrong]) {
            Err(transfer) => transfer,
            Ok(_) => panic!("mismatched terminal residue unexpectedly transferred"),
        };
        assert_eq!(
            transfer.error(),
            ConfigLedgerError::MissingConcreteEffect { effect_id: 2 }
        );
        let mut recovery = transfer.retry_with(vec![logical]).unwrap();
        assert_eq!(recovery.rollback(1).unwrap(), ConfigRollbackResult::Removed);
        assert!(!transfer_target.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exact_rollback_removes_only_the_receipted_entry() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let mut ledger = publish_into_ledger(89, 1, &target);
        assert_eq!(ledger.rollback(1).unwrap(), ConfigRollbackResult::Removed);
        assert!(!target.exists());
        assert!(ledger.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owned_quarantine_rename_rebaselines_its_own_ctime_change() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let mut ledger = publish_into_ledger(91, 1, &target);
        let mut phases = Vec::new();

        let rollback = ledger
            .rollback_with_hook(1, |phase| {
                phases.push(match phase {
                    RollbackHookPoint::TargetVerified { .. } => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        "after-target-verification"
                    }
                    RollbackHookPoint::Quarantined { .. } => "after-quarantine",
                    RollbackHookPoint::QuarantineVerified { .. } => "after-quarantine-verification",
                });
            })
            .unwrap();

        assert_eq!(
            rollback,
            ConfigRollbackResult::Removed,
            "phases={phases:?},{},entries={:?}",
            ledger_entry_facts(&ledger, 1),
            directory_entries(directory.path())
        );
        assert_eq!(
            phases,
            [
                "after-target-verification",
                "after-quarantine",
                "after-quarantine-verification"
            ]
        );
        assert!(!target.exists());
        assert!(ledger.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn post_quarantine_ctime_only_mutation_remains_a_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let mut ledger = publish_into_ledger(93, 1, &target);

        let rollback = ledger
            .rollback_with_hook(1, |phase| {
                if let RollbackHookPoint::Quarantined { quarantine } = phase {
                    let permissions = std::fs::symlink_metadata(quarantine).unwrap().permissions();
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    std::fs::set_permissions(quarantine, permissions).unwrap();
                }
            })
            .unwrap();

        assert_eq!(
            rollback,
            ConfigRollbackResult::Incomplete(ConfigResidueKind::Mismatch)
        );
        assert!(target.exists());
        assert_eq!(ledger.residue_kind(1), Some(ConfigResidueKind::Mismatch));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_receipt_blocks_a_second_kitsu_writer_until_rollback_finishes() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let contender = directory.path().join("contender.toml");
        let mut ledger = publish_into_ledger(95, 1, &target);
        let observed = std::rc::Rc::new(std::cell::RefCell::new(None));
        let hook_observed = std::rc::Rc::clone(&observed);

        assert_eq!(
            ledger
                .rollback_with_hook(1, |phase| {
                    if matches!(phase, RollbackHookPoint::TargetVerified { .. })
                        && hook_observed.borrow().is_none()
                    {
                        let (preview, rendered) = render(&contender);
                        *hook_observed.borrow_mut() =
                            Some(publish_create_new(preview.target(), &rendered));
                    }
                })
                .unwrap(),
            ConfigRollbackResult::Removed
        );

        let outcome = observed
            .borrow_mut()
            .take()
            .expect("contending Kitsu writer was attempted");
        let facts = outcome_facts(&outcome, directory.path());
        assert!(
            matches!(
                outcome,
                ConfigPublicationOutcome::Failed {
                    kind: ConfigPublicationFailureKind::WriterBusy,
                    residue: None,
                }
            ),
            "{facts}"
        );
        assert!(!contender.exists(), "busy writer had a visible effect");

        let mut contender_ledger = publish_into_ledger(97, 1, &contender);
        assert_eq!(
            contender_ledger.rollback(1).unwrap(),
            ConfigRollbackResult::Removed
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_quarantine_after_verification_is_removed_not_displaced() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let mut ledger = publish_into_ledger(99, 1, &target);
        let mut cut_reached = false;

        let rollback = ledger
            .rollback_with_hook(1, |phase| {
                if let RollbackHookPoint::QuarantineVerified { quarantine } = phase {
                    cut_reached = true;
                    std::fs::remove_file(quarantine).unwrap();
                }
            })
            .unwrap();

        assert!(cut_reached);
        assert_eq!(rollback, ConfigRollbackResult::Removed);
        assert!(ledger.is_empty());
        assert!(directory_entries(directory.path()).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn generic_unlink_failure_is_displaced_only_when_the_receipt_name_is_retained() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let mut ledger = publish_into_ledger(101, 1, &target);
        let original_permissions = std::fs::symlink_metadata(directory.path())
            .unwrap()
            .permissions();
        let parent = directory.path().to_path_buf();

        let rollback = ledger
            .rollback_with_hook(1, |phase| {
                if matches!(phase, RollbackHookPoint::QuarantineVerified { .. }) {
                    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500))
                        .unwrap();
                }
            })
            .unwrap();
        std::fs::set_permissions(directory.path(), original_permissions).unwrap();

        assert_eq!(
            rollback,
            ConfigRollbackResult::Incomplete(ConfigResidueKind::DisplacedEntry)
        );
        let LedgerEntry::DisplacedEntry(displaced) = ledger.entries.get(&1).unwrap() else {
            panic!("generic unlink failure lost its exact displaced receipt")
        };
        let named = displaced
            .installed
            .target
            .read_named_snapshot(&displaced.displaced_name)
            .unwrap()
            .expect("displaced name remains present");
        let (owned, _) = ConfigFileSnapshot::read_owned(&displaced.installed.file).unwrap();
        assert!(owned.matches(&named));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_restore_source_with_receipt_at_unknown_name_is_a_failure() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let unknown = directory.path().join("actor-moved-receipt.toml");
        let mut ledger = publish_into_ledger(103, 1, &target);

        let rollback = ledger
            .rollback_with_hook(1, |phase| {
                if let RollbackHookPoint::Quarantined { quarantine } = phase {
                    std::fs::rename(quarantine, &unknown).unwrap();
                }
            })
            .unwrap();

        assert_eq!(
            rollback,
            ConfigRollbackResult::Incomplete(ConfigResidueKind::RollbackFailure)
        );
        assert_eq!(
            ledger.residue_kind(1),
            Some(ConfigResidueKind::RollbackFailure)
        );
        assert!(!target.exists());
        assert!(unknown.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hardlink_and_content_changes_are_mismatches_and_remain_present() {
        let hardlink_dir = tempfile::tempdir().unwrap();
        let hardlink_target = hardlink_dir.path().join("config.toml");
        let alias = hardlink_dir.path().join("alias.toml");
        let mut hardlink_ledger = publish_into_ledger(97, 1, &hardlink_target);
        std::fs::hard_link(&hardlink_target, &alias).unwrap();
        assert_eq!(
            hardlink_ledger.rollback(1).unwrap(),
            ConfigRollbackResult::Incomplete(ConfigResidueKind::Mismatch)
        );
        assert!(hardlink_target.exists());
        assert!(alias.exists());

        let content_dir = tempfile::tempdir().unwrap();
        let content_target = content_dir.path().join("config.toml");
        let mut content_ledger = publish_into_ledger(101, 1, &content_target);
        std::fs::write(&content_target, b"changed after publication").unwrap();
        assert_eq!(
            content_ledger.rollback(1).unwrap(),
            ConfigRollbackResult::Incomplete(ConfigResidueKind::Mismatch)
        );
        assert_eq!(
            std::fs::read(&content_target).unwrap(),
            b"changed after publication"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn replacement_between_precheck_and_quarantine_is_restored_and_never_unlinked() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let displaced_owned = directory.path().join("owned-moved-by-racer.toml");
        let mut ledger = publish_into_ledger(103, 1, &target);

        let outcome = ledger
            .rollback_with_hook(1, |point| {
                if matches!(point, RollbackHookPoint::TargetVerified { .. }) {
                    std::fs::rename(&target, &displaced_owned).unwrap();
                    std::fs::write(&target, b"raced replacement").unwrap();
                }
            })
            .unwrap();
        assert_eq!(
            outcome,
            ConfigRollbackResult::Incomplete(ConfigResidueKind::RollbackFailure)
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"raced replacement");
        assert!(displaced_owned.exists());
        assert_eq!(
            ledger.residue_kind(1),
            Some(ConfigResidueKind::RollbackFailure)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn post_quarantine_mismatch_with_failed_restore_retains_displaced_entry() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let mut ledger = publish_into_ledger(107, 1, &target);

        let outcome = ledger
            .rollback_with_hook(1, |point| match point {
                RollbackHookPoint::TargetVerified { .. } => {}
                RollbackHookPoint::Quarantined { quarantine } => {
                    std::fs::write(quarantine, b"mutated quarantined entry").unwrap();
                    std::fs::write(&target, b"second raced replacement").unwrap();
                }
                RollbackHookPoint::QuarantineVerified { .. } => {}
            })
            .unwrap();
        assert_eq!(
            outcome,
            ConfigRollbackResult::Incomplete(ConfigResidueKind::DisplacedEntry)
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"second raced replacement");
        assert_eq!(
            ledger.residue_kind(1),
            Some(ConfigResidueKind::DisplacedEntry)
        );
        assert!(
            std::fs::read_dir(directory.path()).unwrap().count() >= 2,
            "the no-overwrite restoration failure lost the quarantined entry"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rollback_directory_sync_cut_is_removed_unconfirmed_and_retryable() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let mut ledger = publish_into_ledger(109, 1, &target);
        let _fault_scope = inject_next_rollback_fault(RollbackFaultPoint::DirectorySync);
        assert_eq!(
            ledger.rollback(1).unwrap(),
            ConfigRollbackResult::Incomplete(ConfigResidueKind::RemovedUnconfirmed)
        );
        assert!(!target.exists());
        assert_eq!(
            ledger.residue_kind(1),
            Some(ConfigResidueKind::RemovedUnconfirmed)
        );
        assert_eq!(ledger.rollback(1).unwrap(), ConfigRollbackResult::Removed);
        assert!(ledger.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn publication_fault_fixture_scope_clears_an_unconsumed_future_point() {
        {
            let directory = tempfile::tempdir().unwrap();
            let missing_parent = directory.path().join("missing");
            let target = missing_parent.join("config.toml");
            let (preview, rendered) = render(&target);
            let _fault_scope = inject_next_publication_fault(PublicationFaultPoint::Write);

            assert!(matches!(
                publish_create_new(preview.target(), &rendered),
                ConfigPublicationOutcome::Failed {
                    kind: ConfigPublicationFailureKind::ParentMissing,
                    residue: None,
                }
            ));
        }

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        std::fs::write(&target, b"preexisting").unwrap();
        let (preview, rendered) = render(&target);

        assert!(matches!(
            publish_create_new(preview.target(), &rendered),
            ConfigPublicationOutcome::Collision
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"preexisting");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rollback_fault_fixture_scope_clears_an_unconsumed_future_point() {
        {
            let mut empty = ConfigPublicationLedger::new(89).unwrap();
            let _fault_scope = inject_next_rollback_fault(RollbackFaultPoint::DirectorySync);
            assert_eq!(
                empty.rollback_uninstalled_stage(UninstalledStageCleanupId(1)),
                Err(ConfigLedgerError::UnknownStageCleanup { cleanup_id: 1 })
            );
        }

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let (preview, rendered) = render(&target);
        let _publication_fault_scope = inject_publication_faults(&[
            PublicationFaultPoint::Write,
            PublicationFaultPoint::UninstalledStageCleanup,
        ]);
        let residue = match publish_create_new(preview.target(), &rendered) {
            ConfigPublicationOutcome::Failed {
                residue: Some(ConfigPublicationResidue::UninstalledStageCleanup(receipt)),
                ..
            } => ConfigPublicationResidue::UninstalledStageCleanup(receipt),
            _ => panic!("injected write cleanup cut must retain its concrete stage"),
        };
        let mut ledger = ConfigPublicationLedger::new(90).unwrap();
        let RegisteredConfigPublicationResidue::UninstalledStage(cleanup_id) =
            ledger.register_residue(1, residue).unwrap()
        else {
            panic!("uninstalled stage must remain outside the logical effect ledger")
        };

        assert_eq!(
            ledger.rollback_uninstalled_stage(cleanup_id).unwrap(),
            ConfigRollbackResult::Removed
        );
        assert!(ledger.is_empty());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn publication_fault_fixture_scope_clears_during_unwind() {
        let unwind = std::panic::catch_unwind(|| {
            let _fault_scope = inject_next_publication_fault(PublicationFaultPoint::Write);
            panic!("deliberate fixture unwind");
        });
        assert!(unwind.is_err());

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        std::fs::write(&target, b"preexisting").unwrap();
        let (preview, rendered) = render(&target);
        assert!(matches!(
            publish_create_new(preview.target(), &rendered),
            ConfigPublicationOutcome::Collision
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"preexisting");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn publication_hook_fixture_scope_clears_an_unconsumed_hook() {
        {
            let directory = tempfile::tempdir().unwrap();
            let target = directory.path().join("missing/config.toml");
            let (preview, rendered) = render(&target);
            let _hook_scope =
                inject_publication_hook(PublicationHook::BeforeInstall(Box::new(|| {
                    panic!("stale publication hook escaped its fixture scope")
                })));

            assert!(matches!(
                publish_create_new(preview.target(), &rendered),
                ConfigPublicationOutcome::Failed {
                    kind: ConfigPublicationFailureKind::ParentMissing,
                    residue: None,
                }
            ));
        }

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let (preview, rendered) = render(&target);
        assert!(matches!(
            publish_create_new(preview.target(), &rendered),
            ConfigPublicationOutcome::Created(_)
        ));
        assert!(target.exists());
    }

    #[test]
    fn older_fixture_guards_cannot_clear_newer_active_generations() {
        let older_publication = inject_next_publication_fault(PublicationFaultPoint::Write);
        assert!(publication_fault(PublicationFaultPoint::Write));
        let newer_publication = inject_next_publication_fault(PublicationFaultPoint::FileSync);
        drop(older_publication);
        assert!(publication_fault(PublicationFaultPoint::FileSync));
        drop(newer_publication);

        let older_rollback = inject_next_rollback_fault(RollbackFaultPoint::DirectorySync);
        assert!(rollback_fault(RollbackFaultPoint::DirectorySync));
        let newer_rollback = inject_next_rollback_fault(RollbackFaultPoint::DirectorySync);
        drop(older_rollback);
        assert!(rollback_fault(RollbackFaultPoint::DirectorySync));
        drop(newer_rollback);

        let older_hook = inject_publication_hook(PublicationHook::BeforeInstall(Box::new(|| {})));
        run_before_install_hook();
        let newer_hook_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_observation = std::sync::Arc::clone(&newer_hook_ran);
        let newer_hook =
            inject_publication_hook(PublicationHook::BeforeInstall(Box::new(move || {
                hook_observation.store(true, Ordering::SeqCst);
            })));
        drop(older_hook);
        run_before_install_hook();
        assert!(newer_hook_ran.load(Ordering::SeqCst));
        drop(newer_hook);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_platform_fails_closed_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let (preview, rendered) = render(&target);
        assert!(matches!(
            publish_create_new(preview.target(), &rendered),
            ConfigPublicationOutcome::Failed {
                kind: ConfigPublicationFailureKind::Unsupported,
                residue: None,
            }
        ));
        assert!(!target.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn terminal_logical_residue_requires_transfer_of_the_matching_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let preview = preview(&target, "model-v1");
        let mut transaction = GuidedSetupTransaction::new(127, preview).unwrap();
        transaction.consent().unwrap();
        let acquire_fence = match transaction.directive() {
            SetupDirective::Acquire { fence, .. } => fence,
            other => panic!("expected acquire, got {other:?}"),
        };
        transaction
            .accept_acquisition(AcquisitionReceipt::acquired(acquire_fence, Vec::new()).unwrap())
            .unwrap();
        let validate_fence = match transaction.directive() {
            SetupDirective::Validate { fence, .. } => fence,
            other => panic!("expected validate, got {other:?}"),
        };
        transaction
            .accept_validation(ValidationReceipt::validated(validate_fence))
            .unwrap();

        let receipt = created_receipt(&target);
        let mut ledger = ConfigPublicationLedger::new(127).unwrap();
        let logical = ledger.register_created(7, receipt).unwrap();
        let publish_fence = publication_fence(&transaction);
        transaction
            .accept_publication(
                PublicationReceipt::failed(
                    publish_fence,
                    SetupFailureKind::PublicationIo,
                    Some(logical),
                )
                .unwrap(),
            )
            .unwrap();
        let rollback_fence = match transaction.directive() {
            SetupDirective::Rollback { fence, .. } => fence,
            other => panic!("expected rollback, got {other:?}"),
        };
        transaction
            .accept_rollback(RollbackReceipt::new(rollback_fence, vec![7]).unwrap())
            .unwrap();
        let (_, _, retained) = transaction.into_terminal().unwrap().into_parts();

        let mut empty = ConfigPublicationLedger::new(127).unwrap();
        assert_eq!(
            empty.validate_terminal_residue(&retained),
            Err(ConfigLedgerError::MissingConcreteEffect { effect_id: 7 })
        );
        assert_eq!(
            empty.rollback(7),
            Err(ConfigLedgerError::UnknownEffect { effect_id: 7 }),
            "a logical effect ID alone must not authorize filesystem rollback"
        );

        let mut recovery = ledger.into_recovery(retained).unwrap();
        assert_eq!(recovery.effect_ids(), vec![7]);
        assert_eq!(recovery.rollback(7).unwrap(), ConfigRollbackResult::Removed);
        assert!(!target.exists());
    }
}
