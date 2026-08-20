use kitsu_providers::{
    ProviderAdapterId, ProviderCapability, ProviderProfileId, ProviderRouteContractError,
    ProviderRouteDeclaration, ProviderWireRevision,
};

#[test]
fn profile_derivation_is_stable_domain_separated_and_opaque() {
    let profile = ProviderProfileId::derive_v1(b"/workspace/kitsu/config.toml", b"primary")
        .expect("bounded identity inputs derive");
    assert_eq!(
        profile.as_str(),
        "profile-v1-f4af3c09e6bb1fc3010385f98425f2cf"
    );
    assert!(!profile.as_str().contains("workspace"));
    assert!(!profile.as_str().contains("primary"));

    assert_eq!(
        profile,
        ProviderProfileId::derive_v1(b"/workspace/kitsu/config.toml", b"primary").unwrap()
    );
    assert_ne!(
        profile,
        ProviderProfileId::derive_v1(b"/workspace/kitsu/other.toml", b"primary").unwrap()
    );
    assert_ne!(
        profile,
        ProviderProfileId::derive_v1(b"/workspace/kitsu/config.toml", b"secondary").unwrap()
    );
    assert_ne!(
        ProviderProfileId::derive_v1(b"ab", b"c").unwrap(),
        ProviderProfileId::derive_v1(b"a", b"bc").unwrap(),
        "length framing prevents concatenation ambiguity"
    );
}

#[test]
fn profile_derivation_rejects_empty_or_unbounded_inputs() {
    assert_eq!(
        ProviderProfileId::derive_v1([], b"primary").unwrap_err(),
        ProviderRouteContractError::EmptyProfileNamespace
    );
    assert_eq!(
        ProviderProfileId::derive_v1(b"namespace", []).unwrap_err(),
        ProviderRouteContractError::EmptyProfileName
    );
    assert!(matches!(
        ProviderProfileId::derive_v1(
            vec![b'x'; ProviderProfileId::MAX_NAMESPACE_BYTES + 1],
            b"primary"
        )
        .unwrap_err(),
        ProviderRouteContractError::ProfileNamespaceTooLong { .. }
    ));
    assert!(matches!(
        ProviderProfileId::derive_v1(
            b"namespace",
            vec![b'x'; ProviderProfileId::MAX_PROFILE_NAME_BYTES + 1]
        )
        .unwrap_err(),
        ProviderRouteContractError::ProfileNameTooLong { .. }
    ));
}

#[test]
fn current_compatible_chat_declaration_has_truthful_explicit_capabilities() {
    let profile = ProviderProfileId::derive_v1(b"namespace", b"primary").unwrap();
    let without_catalog = ProviderRouteDeclaration::compatible_chat_v1(profile.clone(), false);
    assert_eq!(
        without_catalog.adapter(),
        ProviderAdapterId::OpenAiCompatible
    );
    assert_eq!(
        without_catalog.wire_revision(),
        ProviderWireRevision::OpenAiChatCompletionsV1
    );
    assert_eq!(without_catalog.profile(), &profile);
    assert_eq!(without_catalog.capabilities().revision(), 1);
    assert_eq!(
        without_catalog
            .capabilities()
            .supported()
            .collect::<Vec<_>>(),
        vec![
            ProviderCapability::Streaming,
            ProviderCapability::Tools,
            ProviderCapability::StructuredOutput,
            ProviderCapability::Usage,
        ]
    );
    assert!(!without_catalog
        .capabilities()
        .supports(ProviderCapability::ImageInput));
    assert!(!without_catalog
        .capabilities()
        .supports(ProviderCapability::ModelCatalog));

    let with_catalog = ProviderRouteDeclaration::compatible_chat_v1(profile, true);
    assert!(with_catalog
        .capabilities()
        .supports(ProviderCapability::ModelCatalog));

    let catalog_only = ProviderRouteDeclaration::compatible_catalog_v1(
        ProviderProfileId::derive_v1(b"namespace", b"catalog").unwrap(),
    );
    assert_eq!(
        catalog_only.capabilities().supported().collect::<Vec<_>>(),
        vec![ProviderCapability::ModelCatalog]
    );

    let mismatch = ProviderRouteDeclaration::new(
        ProviderAdapterId::AnthropicMessages,
        ProviderProfileId::derive_v1(b"namespace", b"other").unwrap(),
        ProviderWireRevision::OpenAiChatCompletionsV1,
        without_catalog.capabilities().clone(),
    )
    .unwrap_err();
    assert_eq!(
        mismatch,
        ProviderRouteContractError::AdapterWireMismatch {
            adapter: ProviderAdapterId::AnthropicMessages,
            wire_revision: ProviderWireRevision::OpenAiChatCompletionsV1,
        }
    );
}

#[test]
fn native_messages_declaration_is_truthful_and_narrow() {
    let declaration = ProviderRouteDeclaration::native_messages_v1(
        ProviderProfileId::derive_v1(b"namespace", b"native-messages").unwrap(),
    );
    assert_eq!(declaration.adapter(), ProviderAdapterId::AnthropicMessages);
    assert_eq!(
        declaration.wire_revision(),
        ProviderWireRevision::AnthropicMessages2023_06_01
    );
    assert_eq!(
        declaration.capabilities().supported().collect::<Vec<_>>(),
        vec![
            ProviderCapability::Streaming,
            ProviderCapability::Tools,
            ProviderCapability::Usage,
        ]
    );
    assert!(!declaration
        .capabilities()
        .supports(ProviderCapability::StructuredOutput));
    assert!(!declaration
        .capabilities()
        .supports(ProviderCapability::ModelCatalog));
}
