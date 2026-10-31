use super::*;

#[test]
fn auto_mode_resets_with_the_session_and_shows_in_the_label() {
    let root = tempfile::tempdir().unwrap();
    let policy = Arc::new(
        kitsu_tools::CommandPolicy::new(
            &[root.path().to_string_lossy().to_string()],
            kitsu_tools::CommandMode::Ask,
        )
        .unwrap(),
    );
    // Label starts at the real mode and follows the auto flag live.
    assert_eq!(command_mode_label(Some(&policy)), Some("ask"));
    assert!(policy.set_session_auto(true));
    assert!(command_mode_label(Some(&policy))
        .unwrap()
        .starts_with("auto"));
    // /new and /resume clear auto mode with the rest of the session.
    reset_command_session(Some(&policy), &[]);
    assert!(!policy.session_auto());
    assert_eq!(command_mode_label(Some(&policy)), Some("ask"));
    assert_eq!(command_mode_label(None), None);
}

#[test]
fn session_switch_drops_live_command_grants_but_restores_cli_pregrants() {
    let root = tempfile::tempdir().unwrap();
    let policy = Arc::new(
        kitsu_tools::CommandPolicy::new(
            &[root.path().to_string_lossy().to_string()],
            kitsu_tools::CommandMode::Deny,
        )
        .unwrap(),
    );
    let explicit = vec![PregrantedCommand {
        argv: vec!["cargo".to_string(), "test".to_string()],
        cwd: root.path().canonicalize().unwrap(),
        cwd_source: CwdSource::LaunchDirectory,
    }];
    let live_only = vec!["cargo".to_string(), "clippy".to_string()];
    policy
        .grant_command(&explicit[0].argv, root.path())
        .unwrap();
    policy.grant_command(&live_only, root.path()).unwrap();

    reset_command_session(Some(&policy), &explicit);

    assert!(policy.is_granted(&explicit[0].argv, root.path()));
    assert!(!policy.is_granted(&live_only, root.path()));
}

#[test]
fn session_switch_restores_pregrants_against_their_own_cwd() {
    // /new must not silently rebind an explicit-cwd pregrant to the launch
    // directory: the grant is exact argv *and* exact cwd.
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let policy = Arc::new(
        kitsu_tools::CommandPolicy::new(
            &[root.path().to_string_lossy().to_string()],
            kitsu_tools::CommandMode::Deny,
        )
        .unwrap(),
    );
    let argv = vec!["cargo".to_string(), "test".to_string()];
    let explicit = vec![PregrantedCommand {
        argv: argv.clone(),
        cwd: nested.canonicalize().unwrap(),
        cwd_source: CwdSource::Explicit,
    }];
    policy.grant_command(&argv, &nested).unwrap();

    reset_command_session(Some(&policy), &explicit);

    assert!(policy.is_granted(&argv, &nested));
    assert!(!policy.is_granted(&argv, root.path()));
}

#[test]
fn legacy_pregrant_outside_roots_names_the_object_syntax() {
    let grant = PregrantedCommand {
        argv: vec!["pwd".to_string()],
        cwd: std::path::PathBuf::from("/launch"),
        cwd_source: CwdSource::LaunchDirectory,
    };
    let message = pregrant_error(
        &grant,
        &["/proj".to_string()],
        "policy denied: command cwd '/launch' is outside the approved command roots",
    );
    assert!(message.contains("\"cwd\":\"/proj\""));
    assert!(message.contains("/launch is outside the approved command roots"));

    // An explicit cwd that fails is reported as-is — the suggestion would be noise.
    let explicit = PregrantedCommand {
        cwd_source: CwdSource::Explicit,
        ..grant
    };
    let message = pregrant_error(
        &explicit,
        &["/proj".to_string()],
        "policy denied: command cwd '/launch' is outside the approved command roots",
    );
    assert!(!message.contains("pass {"));
}

#[test]
fn dangerous_global_bypass_refuses_misleading_root_revocation() {
    for access in ["read", "write"] {
        let error = dangerous_bypass_revocation_error(Mode::AutoAllow, access).unwrap();
        assert!(error.contains(&format!("cannot revoke {access} roots")));
        assert!(error.contains("--dangerously-allow-all-filesystem"));
        assert!(error.contains("restart without the dangerous flag"));
    }
    assert!(dangerous_bypass_revocation_error(Mode::Ask, "read").is_none());
    assert!(dangerous_bypass_revocation_error(Mode::Scoped, "write").is_none());
    assert!(dangerous_bypass_revocation_error(Mode::Deny, "read").is_none());
}
