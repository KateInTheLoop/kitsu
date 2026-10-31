use super::*;

#[test]
fn review_request_parser_supports_scopes_and_literal_backcompat_paths() {
    assert_eq!(
        parse_review_request("").unwrap(),
        ReviewRequest {
            scope: ReviewScope::Working,
            path: None,
        }
    );
    assert_eq!(
        parse_review_request("src/a file.rs").unwrap(),
        ReviewRequest {
            scope: ReviewScope::Working,
            path: Some("src/a file.rs".to_string()),
        }
    );
    assert_eq!(
        parse_review_request("working src/lib.rs").unwrap(),
        ReviewRequest {
            scope: ReviewScope::Working,
            path: Some("src/lib.rs".to_string()),
        }
    );
    assert_eq!(
        parse_review_request("staged src/a file.rs").unwrap(),
        ReviewRequest {
            scope: ReviewScope::Staged,
            path: Some("src/a file.rs".to_string()),
        }
    );
    assert_eq!(
        parse_review_request("unstaged").unwrap(),
        ReviewRequest {
            scope: ReviewScope::Unstaged,
            path: None,
        }
    );
    assert_eq!(
        parse_review_request("base origin/main crates/kitsu").unwrap(),
        ReviewRequest {
            scope: ReviewScope::Base("origin/main".to_string()),
            path: Some("crates/kitsu".to_string()),
        }
    );
    assert_eq!(
        parse_review_request("commit HEAD~2").unwrap(),
        ReviewRequest {
            scope: ReviewScope::Commit("HEAD~2".to_string()),
            path: None,
        }
    );
    assert_eq!(
        parse_review_request("checkpoint 42 src/lib.rs").unwrap(),
        ReviewRequest {
            scope: ReviewScope::Checkpoint(42),
            path: Some("src/lib.rs".to_string()),
        }
    );

    assert!(parse_review_request("base").is_err());
    assert!(parse_review_request("commit").is_err());
    assert!(parse_review_request("checkpoint nope").is_err());
    assert!(parse_review_request("checkpoint 0").is_err());
}

#[test]
fn checkpoint_command_ids_are_exact_and_review_prompt_has_no_tools() {
    assert_eq!(parse_exact_checkpoint_id("7").unwrap(), 7);
    assert_eq!(parse_optional_checkpoint_id("").unwrap(), None);
    assert_eq!(parse_optional_checkpoint_id("7").unwrap(), Some(7));
    assert!(parse_exact_checkpoint_id("7 trailing").is_err());
    let invalid = parse_checkpoint_id("7\u{1b}]0;owned\u{7}").unwrap_err();
    assert!(!invalid.contains('\u{1b}'));
    assert!(!invalid.contains('\u{7}'));

    let evidence = workspace::ReviewEvidence {
        target: "base:main".to_string(),
        resolved_revision: Some("a".repeat(40)),
        merge_base: Some("b".repeat(40)),
        diff: "diff --git a/a b/a\n-old\n+new".to_string(),
        truncated: false,
    };
    let prompt = SubmittedPrompt::review(verified_review_prompt(&evidence));
    assert_eq!(prompt.tool_mode, TurnToolMode::None);
    assert!(prompt.text.contains("target base:main"));
    assert!(prompt.text.contains("merge base"));
    assert!(prompt.text.contains("-old\n+new"));
}

#[test]
#[cfg(unix)]
fn private_path_identity_recognizes_a_symlink_alias() {
    let directory = tempfile::tempdir().unwrap();
    let private = directory.path().join("private");
    let alias = directory.path().join("alias");
    std::fs::create_dir(&private).unwrap();
    std::fs::write(private.join("session.json"), "secret").unwrap();
    std::os::unix::fs::symlink(&private, &alias).unwrap();

    assert!(path_is_within(&private.join("session.json"), &alias));
    assert!(!path_is_within(
        directory.path().join("public.txt").as_path(),
        &alias
    ));
}

#[test]
fn workspace_diff_args_parse_every_documented_scope() {
    let working = parse_workspace_diff_request("").unwrap();
    assert_eq!(working.scope, ReviewScope::Working);
    assert_eq!(working.path, None);
    let defaulted = parse_workspace_diff_request("{}").unwrap();
    assert_eq!(defaulted.scope, ReviewScope::Working);
    let explicit = parse_workspace_diff_request(r#"{"scope":"working"}"#).unwrap();
    assert_eq!(explicit.scope, ReviewScope::Working);
    let staged = parse_workspace_diff_request(r#"{"scope":"staged","path":"src"}"#).unwrap();
    assert_eq!(staged.scope, ReviewScope::Staged);
    assert_eq!(staged.path.as_deref(), Some("src"));
    let unstaged = parse_workspace_diff_request(r#"{"scope":"unstaged"}"#).unwrap();
    assert_eq!(unstaged.scope, ReviewScope::Unstaged);
    let base = parse_workspace_diff_request(r#"{"scope":{"base":"main"}}"#).unwrap();
    assert_eq!(base.scope, ReviewScope::Base("main".to_string()));
    let commit = parse_workspace_diff_request(r#"{"scope":{"commit":"HEAD~1"}}"#).unwrap();
    assert_eq!(commit.scope, ReviewScope::Commit("HEAD~1".to_string()));
    // Null and blank optionals degrade to the defaults, not errors.
    let nulls = parse_workspace_diff_request(r#"{"scope":null,"path":null}"#).unwrap();
    assert_eq!(nulls.scope, ReviewScope::Working);
    assert_eq!(nulls.path, None);
    let blank_path = parse_workspace_diff_request(r#"{"path":"  "}"#).unwrap();
    assert_eq!(blank_path.path, None);
}

#[test]
fn workspace_diff_args_reject_malformed_scopes_with_named_errors() {
    // There is no model-facing checkpoint scope: checkpoints replay
    // Kitsu's own write journal and stay a human surface.
    let checkpoint = parse_workspace_diff_request(r#"{"scope":"checkpoint"}"#).unwrap_err();
    assert!(checkpoint.contains("unknown scope"), "{checkpoint}");
    let unknown = parse_workspace_diff_request(r#"{"scope":"everything"}"#).unwrap_err();
    assert!(
        unknown.contains("unknown scope \"everything\""),
        "{unknown}"
    );
    let both = parse_workspace_diff_request(r#"{"scope":{"base":"a","commit":"b"}}"#).unwrap_err();
    assert!(both.contains("exactly one key"), "{both}");
    let wrong_key = parse_workspace_diff_request(r#"{"scope":{"branch":"main"}}"#).unwrap_err();
    assert!(
        wrong_key.contains("unknown scope key \"branch\""),
        "{wrong_key}"
    );
    let empty_rev = parse_workspace_diff_request(r#"{"scope":{"base":"  "}}"#).unwrap_err();
    assert!(empty_rev.contains("non-empty revision"), "{empty_rev}");
    let numeric_scope = parse_workspace_diff_request(r#"{"scope":7}"#).unwrap_err();
    assert!(numeric_scope.contains("invalid scope"), "{numeric_scope}");
    let extra_key =
        parse_workspace_diff_request(r#"{"scope":"working","paths":"src"}"#).unwrap_err();
    assert!(
        extra_key.contains("unknown argument 'paths'"),
        "{extra_key}"
    );
    let non_object = parse_workspace_diff_request(r#""working""#).unwrap_err();
    assert!(non_object.contains("JSON object"), "{non_object}");
    let bad_json = parse_workspace_diff_request("scope=working").unwrap_err();
    assert!(bad_json.contains("invalid JSON arguments"), "{bad_json}");
}

#[test]
fn workspace_diff_tool_registers_only_inside_a_worktree_outside_private_state() {
    let perms = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
    let live_mw: subagent::LiveModelWindow =
        Arc::new(std::sync::Mutex::new(("test-model".to_string(), None)));

    // Not a git worktree: no tool.
    let plain = tempfile::tempdir().unwrap();
    assert!(
        workspace_diff_tool(plain.path(), Vec::new(), &perms, &live_mw).is_none(),
        "registered outside a worktree"
    );

    // A worktree inside protected Kitsu state: refused, no tool.
    let protected = tempfile::tempdir().unwrap();
    perms
        .protect_read_path(&protected.path().to_string_lossy())
        .unwrap();
    let inside = protected.path().join("repo");
    std::fs::create_dir_all(&inside).unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(&inside)
        .args(["init", "--quiet"])
        .status()
        .unwrap()
        .success());
    assert!(
        workspace_diff_tool(&inside, Vec::new(), &perms, &live_mw).is_none(),
        "registered inside Kitsu-private state"
    );

    // An ordinary worktree registers, and the descriptor mirrors /diff's
    // scopes (minus checkpoint).
    let repo = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["init", "--quiet"])
        .status()
        .unwrap()
        .success());
    let (descriptor, _handler) = workspace_diff_tool(repo.path(), Vec::new(), &perms, &live_mw)
        .expect("worktree registers workspace_diff");
    assert_eq!(descriptor.name, "workspace_diff");
    let schema = descriptor.parameters.to_string();
    for scope in ["working", "staged", "unstaged", "base", "commit"] {
        assert!(schema.contains(scope), "schema misses {scope}: {schema}");
    }
    assert!(!schema.contains("checkpoint"), "{schema}");
}

#[tokio::test]
async fn workspace_diff_tool_returns_bounded_evidence_and_excludes_private_paths() {
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "--quiet"]);
    std::fs::write(repo.path().join("code.txt"), "before\n").unwrap();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--quiet",
        "-m",
        "base",
    ]);
    std::fs::write(repo.path().join("code.txt"), "after\n").unwrap();
    // A Kitsu-private file inside the worktree must never appear in the
    // disclosed evidence.
    let private_dir = repo.path().join(".kitsu-state");
    std::fs::create_dir_all(&private_dir).unwrap();
    std::fs::write(private_dir.join("secret.txt"), "SECRET-BYTES\n").unwrap();

    let perms = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
    let live_mw: subagent::LiveModelWindow =
        Arc::new(std::sync::Mutex::new(("test-model".to_string(), None)));
    let (_descriptor, handler) =
        workspace_diff_tool(repo.path(), vec![private_dir.clone()], &perms, &live_mw)
            .expect("worktree registers workspace_diff");

    let result = handler(r#"{"scope":"working"}"#.to_string()).await.unwrap();
    let payload: serde_json::Value = serde_json::from_str(&result).unwrap();
    let diff = payload["diff"].as_str().unwrap();
    assert!(diff.contains("+after"), "{diff}");
    assert!(!diff.contains("SECRET-BYTES"), "{diff}");
    assert!(!diff.contains("secret.txt"), "{diff}");
    assert_eq!(payload["truncated"], false);
    assert_eq!(payload["target"], "working_tree");

    // The path argument narrows inside the repository; escapes are
    // rejected before any I/O with an InvalidArgs error.
    let filtered = handler(r#"{"scope":"working","path":"code.txt"}"#.to_string())
        .await
        .unwrap();
    let filtered: serde_json::Value = serde_json::from_str(&filtered).unwrap();
    assert!(filtered["diff"].as_str().unwrap().contains("+after"));
    let escape = handler(r#"{"path":"../outside"}"#.to_string())
        .await
        .unwrap_err();
    assert!(
        matches!(escape, kitsu_tools::ToolError::InvalidArgs(_)),
        "{escape}"
    );
}

#[tokio::test]
async fn every_review_disclosure_surface_uses_the_same_auditable_shape() {
    let evidence = workspace::ReviewEvidence {
        target: "working_tree".to_string(),
        resolved_revision: None,
        merge_base: None,
        truncated: false,
        diff: "diff --git a/x b/x\n".to_string(),
    };
    let review = evidence_disclosure_payload(&evidence);
    let top_level = top_level_review_disclosure_event(&evidence);
    let tool = workspace_evidence_disclosure_event(&evidence);
    // One auditable shape across all disclosure surfaces: identical
    // fields except the discriminators.
    let review_fields: std::collections::BTreeSet<&str> = review
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "kind")
        .collect();
    let top_level_fields: std::collections::BTreeSet<&str> = top_level
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "source")
        .collect();
    let tool_fields: std::collections::BTreeSet<&str> = tool
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "source")
        .collect();
    assert_eq!(review_fields, top_level_fields);
    assert_eq!(review_fields, tool_fields);
    assert_eq!(top_level["source"], "top_level_review");
    assert_eq!(tool["source"], "workspace_diff");
    assert_eq!(tool["model_disclosed"], true);
    assert_eq!(tool["model_read_root_granted"], false);
    assert_eq!(tool["evidence_bytes"], evidence.diff.len());
}
