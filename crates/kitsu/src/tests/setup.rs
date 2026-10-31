use super::*;

#[test]
fn base_tool_duplicate_maps_to_user_error_before_runtime_entry() {
    let permissions = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
    let skillset = Arc::new(skills::SkillSet::default());
    let journal = kitsu_tools::builtins::fs::WriteJournal::new();
    let mut tools = Registry::new();
    let (descriptor, handler) = builtins::fs::read_file_tool(permissions.clone());
    tools
        .register_with_descriptor(descriptor, handler)
        .expect("fixture registration succeeds");
    let descriptors_before = tools.descriptors().to_vec();

    let error = register_base_tools(
        &mut tools,
        &permissions,
        &skillset,
        true,
        false,
        &journal,
        None,
        kitsu_tools::ToolTier::Full,
    )
    .expect_err("base composition rejects duplicate");

    assert_eq!(
        error,
        kitsu_tools::RegistryError::DuplicateName {
            name: "fs_read_file".to_string(),
        }
    );
    assert_eq!(tools.descriptors(), descriptors_before);
    let cli_error = CliError::from(error);
    assert_eq!(cli_error.exit_code(), EXIT_USER);
    assert_eq!(
        cli_error.to_string(),
        "tool registry composition failed: duplicate tool registration: fs_read_file"
    );
}

#[test]
fn unknown_cli_context_uses_the_documented_checked_default() {
    let cli = Cli::try_parse_from(["kitsu", "ask", "hello"]).unwrap();
    let cfg = build_agent_config(
        "model".to_string(),
        &cli,
        None,
        None,
        ArtifactStoragePolicy::Automatic,
    );
    assert_eq!(cfg.context_window, Some(DEFAULT_CONTEXT_WINDOW));
}

#[test]
fn command_only_registry_does_not_advertise_readable_offload_pointers() {
    assert!(!can_resolve_offload_pointers(&["command".to_string()]));
    assert!(can_resolve_offload_pointers(&[
        "command".to_string(),
        "fs_read_file".to_string(),
    ]));
}

#[test]
fn explicit_adapter_selects_wire_while_legacy_flavors_stay_compatible() {
    let gateway = |flavor: &str, models_path: Option<&str>| Gateway {
        flavor: flavor.to_string(),
        adapter: config::GatewayAdapter::CompatibleChat,
        base_url: "https://user:secret@example.invalid/?token=hidden".to_string(),
        chat_path: "/v1/chat/completions".to_string(),
        models_path: models_path.map(str::to_string),
        model: Some("display-model".to_string()),
        context_window: None,
        auth: config::AuthConfig::default(),
    };
    let config_path = std::path::Path::new("/workspace/kitsu/config.toml");
    let reference = current_provider_route_declaration(
        config_path,
        "primary",
        &gateway("openai", Some("/v1/models")),
    )
    .unwrap();

    for flavor in ["openai", "anthropic", "ollama"] {
        let declaration = current_provider_route_declaration(
            config_path,
            "primary",
            &gateway(flavor, Some("/v1/models")),
        )
        .unwrap();
        assert_eq!(
            declaration.adapter(),
            kitsu_providers::ProviderAdapterId::OpenAiCompatible,
            "the {flavor} display label must not claim a native adapter"
        );
        assert_eq!(
            declaration.wire_revision(),
            kitsu_providers::ProviderWireRevision::OpenAiChatCompletionsV1
        );
        assert_eq!(declaration.profile(), reference.profile());
        assert!(declaration
            .capabilities()
            .supports(kitsu_providers::ProviderCapability::ModelCatalog));
        assert!(!declaration
            .capabilities()
            .supports(kitsu_providers::ProviderCapability::ImageInput));
    }

    let without_catalog =
        current_provider_route_declaration(config_path, "primary", &gateway("anthropic", None))
            .unwrap();
    assert!(!without_catalog
        .capabilities()
        .supports(kitsu_providers::ProviderCapability::ModelCatalog));
    assert!(!without_catalog.profile().as_str().contains("primary"));
    assert!(!without_catalog.profile().as_str().contains("secret"));
    assert!(!without_catalog.profile().as_str().contains("display-model"));

    let mut native = gateway("anthropic", None);
    native.adapter = config::GatewayAdapter::AnthropicMessages;
    native.chat_path = "/v1/messages".to_string();
    let native_declaration =
        current_provider_route_declaration(config_path, "primary", &native).unwrap();
    assert_eq!(
        native_declaration.adapter(),
        kitsu_providers::ProviderAdapterId::AnthropicMessages
    );
    assert_eq!(
        native_declaration.wire_revision(),
        kitsu_providers::ProviderWireRevision::AnthropicMessages2023_06_01
    );
    assert!(native_declaration
        .capabilities()
        .supports(kitsu_providers::ProviderCapability::Streaming));
    assert!(!native_declaration
        .capabilities()
        .supports(kitsu_providers::ProviderCapability::ModelCatalog));
    assert_eq!(native_declaration.profile(), reference.profile());

    let mut changed_route_contents = gateway("ollama", None);
    changed_route_contents.base_url = "http://different.invalid/private".to_string();
    changed_route_contents.chat_path = "/different/chat".to_string();
    changed_route_contents.model = Some("different-model".to_string());
    changed_route_contents.auth.token = Some("different-secret".to_string());
    let changed =
        current_provider_route_declaration(config_path, "primary", &changed_route_contents)
            .unwrap();
    assert_eq!(changed.profile(), reference.profile());
    assert_ne!(
        current_provider_route_declaration(config_path, "secondary", &changed_route_contents,)
            .unwrap()
            .profile(),
        reference.profile()
    );
}

#[test]
#[serial]
fn provider_auth_source_matches_the_credential_that_route_freezing_selects() {
    const ROUTE_AUTH_ENV: &str = "KITSU_TEST_ROUTE_AUTH_SOURCE";
    let _guard = EnvVarGuard::set(ROUTE_AUTH_ENV, "environment-token");
    let mut gateway = Gateway {
        flavor: "openai".to_string(),
        adapter: config::GatewayAdapter::CompatibleChat,
        base_url: "https://example.invalid".to_string(),
        chat_path: "/v1/chat/completions".to_string(),
        models_path: None,
        model: Some("model".to_string()),
        context_window: None,
        auth: config::AuthConfig::default(),
    };
    gateway.auth.env = ROUTE_AUTH_ENV.to_string();

    assert_eq!(
        current_provider_auth_source(&gateway, true),
        ProviderAuthSource::Environment
    );
    std::env::set_var(ROUTE_AUTH_ENV, " \t");
    assert_eq!(
        current_provider_auth_source(&gateway, true),
        ProviderAuthSource::ManagedCredential,
        "whitespace-only environment values select managed fallback"
    );

    gateway.auth.method = "static".to_string();
    assert_eq!(
        current_provider_auth_source(&gateway, false),
        ProviderAuthSource::StaticConfiguration
    );
    gateway.auth.method = "none".to_string();
    assert_eq!(
        current_provider_auth_source(&gateway, false),
        ProviderAuthSource::None
    );
}

#[test]
#[cfg(unix)]
fn provider_profile_identity_uses_the_canonical_config_namespace() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("kitsu.toml");
    let alias_path = temp.path().join("alias.toml");
    std::fs::write(&config_path, "[gateways]\n").unwrap();
    symlink(&config_path, &alias_path).unwrap();
    let gateway = Gateway {
        flavor: "openai".to_string(),
        adapter: config::GatewayAdapter::CompatibleChat,
        base_url: "https://example.invalid".to_string(),
        chat_path: "/v1/chat/completions".to_string(),
        models_path: None,
        model: Some("model".to_string()),
        context_window: None,
        auth: config::AuthConfig::default(),
    };

    let direct = current_provider_route_declaration(&config_path, "primary", &gateway).unwrap();
    let aliased = current_provider_route_declaration(&alias_path, "primary", &gateway).unwrap();
    assert_eq!(direct.profile(), aliased.profile());
}

#[cfg(unix)]
#[test]
fn resume_revalidates_loaded_skill_snapshots_and_replaces_stale_text() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let skill_dir = root.path().join(".kitsu/skills/review");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let manifest = skill_dir.join("SKILL.md");
    std::fs::write(
        &manifest,
        "---\nname: review\ndescription: review code\n---\nOLD_SKILL_BODY\n",
    )
    .unwrap();
    let permissions = Permissions::for_roots(&[], &[]).unwrap();
    let before = skills::SkillSet::discover(root.path(), false, &permissions);
    let loaded = before.load_with_snapshot("review").unwrap();
    let mut messages = vec![
        ChatMessage::system("fresh system"),
        loaded_skill_message("review", &loaded),
        ChatMessage::user("ordinary conversation"),
    ];

    std::fs::write(
        &manifest,
        "---\nname: review\ndescription: review code\n---\nNEW_SKILL_BODY\n",
    )
    .unwrap();
    let current = skills::SkillSet::discover(root.path(), false, &permissions);
    let notices = refresh_loaded_skill_messages(&mut messages, &current);

    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("manifest_hash"));
    assert!(messages[1].content.contains("NEW_SKILL_BODY"));
    assert!(!messages[1].content.contains("OLD_SKILL_BODY"));
    assert_eq!(messages[2].content, "ordinary conversation");

    std::fs::remove_dir_all(skill_dir).unwrap();
    let absent = skills::SkillSet::discover(root.path(), false, &permissions);
    let notices = refresh_loaded_skill_messages(&mut messages, &absent);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("removed saved loaded skill"));
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].content, "ordinary conversation");
}
