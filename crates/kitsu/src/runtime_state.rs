//! Private runtime-state locations and profile namespaces.

pub(crate) fn runtime_state_base() -> std::path::PathBuf {
    let base = kitsu_tools::host_home_dir()
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".kitsu"))
        .unwrap_or_else(|| {
            #[cfg(unix)]
            let owner = unsafe { libc::geteuid() }.to_string();
            #[cfg(not(unix))]
            let owner = std::process::id().to_string();
            std::env::temp_dir().join(format!("kitsu-state-{owner}"))
        });
    base
}

pub(crate) fn stable_profile_key(config_path: &std::path::Path) -> String {
    // FNV-1a/128 is deliberately spelled out and versioned: unlike
    // `DefaultHasher`, its output is stable across Rust releases. This is an
    // on-disk identity, not a security primitive; 128 bits keeps accidental
    // profile collisions impractical without adding a crypto dependency.
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    #[cfg(unix)]
    let encoded = {
        use std::os::unix::ffi::OsStrExt;
        config_path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let encoded = {
        use std::os::windows::ffi::OsStrExt;
        config_path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    #[cfg(not(any(unix, windows)))]
    let encoded = config_path.to_string_lossy().as_bytes().to_vec();

    let digest = encoded.into_iter().fold(OFFSET, |hash, byte| {
        (hash ^ u128::from(byte)).wrapping_mul(PRIME)
    });
    format!("v1-fnv128-{digest:032x}")
}

/// Location used by builds before profile keys became a stable on-disk
/// contract. Kept only so this release can migrate existing development data.
pub(crate) fn legacy_profile_dir(
    base: &std::path::Path,
    config_path: &std::path::Path,
) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config_path.hash(&mut hasher);
    base.join("profiles")
        .join(format!("{:016x}", hasher.finish()))
}

/// Resolve a config-scoped state namespace without creating, renaming, or
/// otherwise migrating anything. Provider-free inspection and doctor use this
/// so merely looking at state remains read-only.
pub(crate) fn runtime_state_dir_read_only(config_path: &std::path::Path) -> std::path::PathBuf {
    let base = runtime_state_base();
    let config = std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let default_config = base.join("config.toml");
    let default_config = std::fs::canonicalize(&default_config).unwrap_or(default_config);
    if config == default_config {
        return base;
    }

    let stable = base.join("profiles").join(stable_profile_key(&config));
    if std::fs::symlink_metadata(&stable).is_ok() {
        return stable;
    }
    let legacy = legacy_profile_dir(&base, &config);
    let legacy_is_safe_dir = std::fs::symlink_metadata(&legacy)
        .map(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false);
    if legacy_is_safe_dir {
        legacy
    } else {
        stable
    }
}

pub(crate) fn runtime_state_dir(config_path: &std::path::Path) -> std::path::PathBuf {
    let base = runtime_state_base();
    let config = std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let default_config = base.join("config.toml");
    let default_config = std::fs::canonicalize(&default_config).unwrap_or(default_config);
    if config == default_config {
        return base;
    }

    let stable = base.join("profiles").join(stable_profile_key(&config));
    if std::fs::symlink_metadata(&stable).is_ok() {
        return stable;
    }

    // Best-effort, same-parent migration. If HOME is read-only, retain the
    // usable legacy location; stateless commands must not fail merely because
    // migration cannot be performed.
    let legacy = legacy_profile_dir(&base, &config);
    let legacy_is_safe_dir = std::fs::symlink_metadata(&legacy)
        .map(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false);
    if legacy_is_safe_dir {
        if std::fs::rename(&legacy, &stable).is_ok() {
            return stable;
        }
        // Another Kitsu process may have won the migration race between our
        // metadata check and rename. Follow it instead of recreating a fresh,
        // empty legacy profile beside the migrated state.
        if std::fs::symlink_metadata(&stable).is_ok() {
            return stable;
        }
        if std::fs::symlink_metadata(&legacy)
            .map(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return legacy;
        }
    }
    stable
}

pub(crate) fn prepare_runtime_state_dir(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create runtime state {}: {error}", dir.display()))?;
    let metadata = std::fs::symlink_metadata(dir)
        .map_err(|error| format!("cannot inspect runtime state {}: {error}", dir.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(format!(
            "runtime state '{}' is not a regular directory",
            dir.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure runtime state {}: {error}", dir.display()))?;
    }
    Ok(())
}
