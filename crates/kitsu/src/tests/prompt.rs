use super::*;

#[test]
fn system_prompt_names_git_only_when_the_tool_is_available() {
    let environment = PromptEnvironment::default();
    let enabled = chat_system_prompt(&["git".to_string()], &environment);
    assert!(enabled.contains("use the git tool with literal args and cwd"));
    assert!(!enabled.contains("commands are not enabled"));
    let disabled = chat_system_prompt(&["fs_read_file".to_string()], &environment);
    assert!(disabled.contains("kitsu chat --enable-commands"));
    assert!(!disabled.contains("use the git tool"));
    let commands_only = chat_system_prompt(&["command_exec".to_string()], &environment);
    assert!(!commands_only.contains("use the git tool"));
}

/// A read-only launch root made the model refuse edits and lecture about
/// grant flags, though interactive Ask mode would simply have prompted.
/// The prompt says so only when access really prompts.
#[test]
fn ask_mode_system_prompt_says_out_of_root_access_prompts_the_user() {
    let tools = ["fs_read_file".to_string(), "fs_write_file".to_string()];
    let asking = PromptEnvironment {
        access_prompts: true,
        ..PromptEnvironment::default()
    };
    assert!(chat_system_prompt(&tools, &asking).contains("the user is asked to approve"));
    assert!(!chat_system_prompt(&tools, &PromptEnvironment::default())
        .contains("the user is asked to approve"));
    assert!(!chat_system_prompt(&["command_exec".to_string()], &asking)
        .contains("the user is asked to approve"));
}

#[test]
fn write_only_system_prompt_does_not_claim_read_or_edit_authority() {
    let environment = PromptEnvironment::default();
    let prompt = chat_system_prompt(&["fs_write_file".to_string()], &environment);
    assert!(prompt.contains("write-only filesystem authority"));
    assert!(!prompt.contains("fs_read_file"));
    assert!(!prompt.contains("fs_edit_file"));

    let combined = chat_system_prompt(
        &[
            "fs_read_file".to_string(),
            "fs_write_file".to_string(),
            "fs_edit_file".to_string(),
        ],
        &environment,
    );
    assert!(combined.contains("fs_edit_file for exact localized edits"));
}

#[test]
fn system_prompt_names_the_cwd_and_every_granted_root_with_authority() {
    let environment = PromptEnvironment::from_grants(
        std::path::Path::new("/work/project"),
        &["/data/readonly".to_string(), "/shared".to_string()],
        &["/out/writeonly".to_string()],
        &["/work/project".to_string(), "/shared".to_string()],
    );
    let prompt = chat_system_prompt(
        &["fs_read_file".to_string(), "fs_write_file".to_string()],
        &environment,
    );
    assert!(prompt.contains("Current working directory: /work/project."));
    assert!(prompt.contains("/data/readonly (read-only)"));
    assert!(prompt.contains("/out/writeonly (write-only)"));
    assert!(prompt.contains("/work/project (read-write)"));
    // Read-write wins when a path appears in more than one grant list.
    assert!(prompt.contains("/shared (read-write)"));
    assert!(!prompt.contains("/shared (read-only)"));
    // The model is told to explore rather than ask for paths.
    assert!(prompt.contains("explore the granted roots"));
}

#[test]
fn system_prompt_without_grants_omits_the_roots_line_but_keeps_the_cwd() {
    let environment =
        PromptEnvironment::from_grants(std::path::Path::new("/work/project"), &[], &[], &[]);
    let prompt = chat_system_prompt(&["command_exec".to_string()], &environment);
    assert!(prompt.contains("Current working directory: /work/project."));
    assert!(!prompt.contains("Granted filesystem roots"));
}

/// Regression for cycle-8 audit gap 3: `--allow-command` pregrants must
/// be named verbatim (exact argv/cwd) in the system prompt sent to the
/// model, not just surfaced via `/permissions` — otherwise a tool-
/// capable model can only guess an invocation and risk a denied
/// round-trip.
#[test]
fn system_prompt_names_every_pregranted_command_verbatim() {
    let pregrants = vec![
        PregrantedCommand {
            argv: vec!["pytest".to_string(), "-v".to_string()],
            cwd: std::path::PathBuf::from("/work/project"),
            cwd_source: CwdSource::LaunchDirectory,
        },
        PregrantedCommand {
            argv: vec!["cargo".to_string(), "test".to_string()],
            cwd: std::path::PathBuf::from("/work/project"),
            cwd_source: CwdSource::LaunchDirectory,
        },
    ];
    let environment =
        PromptEnvironment::from_grants(std::path::Path::new("/work/project"), &[], &[], &[])
            .with_command_pregrants(&pregrants);
    let prompt = chat_system_prompt(&["command_exec".to_string()], &environment);
    assert!(prompt.contains("Pre-approved commands"), "{prompt}");
    assert!(
        prompt.contains(r#"["pytest","-v"] in /work/project"#),
        "{prompt}"
    );
    assert!(
        prompt.contains(r#"["cargo","test"] in /work/project"#),
        "{prompt}"
    );
    assert!(!prompt.contains("more pre-approved"));
}

#[test]
fn system_prompt_without_commands_tool_omits_pregrant_disclosure() {
    let pregrants = vec![PregrantedCommand {
        argv: vec!["pytest".to_string()],
        cwd: std::path::PathBuf::from("/work/project"),
        cwd_source: CwdSource::LaunchDirectory,
    }];
    let environment =
        PromptEnvironment::from_grants(std::path::Path::new("/work/project"), &[], &[], &[])
            .with_command_pregrants(&pregrants);
    // No command_exec tool granted: never mention command pregrants even
    // though they're attached to the environment.
    let prompt = chat_system_prompt(&["fs_read_file".to_string()], &environment);
    assert!(!prompt.contains("Pre-approved commands"));
    assert!(!prompt.contains("pytest"));
}

#[test]
fn system_prompt_caps_pregrant_disclosure_and_notes_the_overflow_count() {
    let pregrants: Vec<PregrantedCommand> = (0..(PROMPT_PREGRANT_CAP + 3))
        .map(|index| PregrantedCommand {
            argv: vec!["tool".to_string(), index.to_string()],
            cwd: std::path::PathBuf::from("/work/project"),
            cwd_source: CwdSource::LaunchDirectory,
        })
        .collect();
    let environment =
        PromptEnvironment::from_grants(std::path::Path::new("/work/project"), &[], &[], &[])
            .with_command_pregrants(&pregrants);
    let prompt = chat_system_prompt(&["command_exec".to_string()], &environment);
    assert!(prompt.contains("+3 more pre-approved"), "{prompt}");
    // The very last grant beyond the cap must not be listed verbatim.
    assert!(!prompt.contains(&format!("\"{}\"", PROMPT_PREGRANT_CAP + 2)));
}

#[test]
fn system_prompt_with_spawn_agent_bg_scopes_use_to_parallel_or_long_running_work() {
    let environment = PromptEnvironment::default();
    let prompt = chat_system_prompt(&["spawn_agent_bg".to_string()], &environment);
    assert!(prompt.contains("Use spawn_agent_bg only for explicitly parallel or long-running work"));
    assert!(prompt.contains("answer simple requests directly"));

    // Verify spawn guidance is absent when tool is not registered
    let prompt_without = chat_system_prompt(&["fs_read_file".to_string()], &environment);
    assert!(!prompt_without.contains("spawn_agent_bg"));
}

#[test]
fn system_prompt_with_workspace_diff_teaches_fetching_verified_diffs() {
    let environment = PromptEnvironment::default();
    let prompt = chat_system_prompt(&["workspace_diff".to_string()], &environment);
    assert!(
        prompt.contains("Use workspace_diff to fetch the user's verified git diff"),
        "{prompt}"
    );
    assert!(
        prompt.contains("do not ask them to paste diffs or spawn agents to run git"),
        "{prompt}"
    );

    let prompt_without = chat_system_prompt(&["fs_read_file".to_string()], &environment);
    assert!(!prompt_without.contains("workspace_diff"));
}

#[test]
fn interactive_prompt_labels_the_cwd_read_write_not_read_only() {
    // The fix for "the model declares the project read-only and declines to
    // edit": an interactive grant must surface cwd as read-write in the
    // prompt, so the model edits directly.
    let cwd = std::path::Path::new("/work/project");
    let env = PromptEnvironment::from_grants(
        cwd,
        &[],                            // allow_paths
        &[],                            // allow_write
        &["/work/project".to_string()], // read_write (what interactive now feeds)
    );
    let (_, authority) = env
        .roots
        .iter()
        .find(|(p, _)| p == "/work/project")
        .expect("cwd is a listed root");
    assert_eq!(
        *authority, "read-write",
        "cwd must be read-write, not read-only"
    );
}
