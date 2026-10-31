use super::*;

#[test]
#[cfg(unix)]
fn profile_key_is_versioned_and_stable() {
    assert_eq!(
        stable_profile_key(std::path::Path::new("/workspace/kitsu/kitsu.toml")),
        "v1-fnv128-9f1392a8a96c8bcf6b4866d572b912eb"
    );
}

#[test]
#[serial]
fn project_local_config_uses_private_home_state() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let config = project.path().join("kitsu.toml");
    std::fs::write(&config, "[gateways]\n").unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let state = runtime_state_dir(&config);
    assert!(state.starts_with(home.path().join(".kitsu/profiles")));
    assert!(!state.starts_with(project.path()));
    assert!(state
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("v1-fnv128-"));
    prepare_runtime_state_dir(&state).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(state).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
#[serial]
fn project_profile_migrates_the_legacy_unstable_key() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let config = project.path().join("kitsu.toml");
    std::fs::write(&config, "[gateways]\n").unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let canonical = std::fs::canonicalize(&config).unwrap();
    let base = home.path().join(".kitsu");
    let legacy = legacy_profile_dir(&base, &canonical);
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("history"), "preserved").unwrap();

    let state = runtime_state_dir(&config);

    assert!(state
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("v1-fnv128-"));
    assert_eq!(
        std::fs::read_to_string(state.join("history")).unwrap(),
        "preserved"
    );
    assert!(!legacy.exists());
}
