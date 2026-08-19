use kitsu_providers::{
    ProviderAdapterId, ProviderCapabilities, ProviderCapability, ProviderProfileId,
    ProviderRouteContractError, ProviderRouteGeneration, ProviderRouteIdentity,
    ProviderWireRevision,
};

fn chat_route() -> ProviderRouteIdentity {
    let capabilities = ProviderCapabilities::new(
        1,
        [
            ProviderCapability::Streaming,
            ProviderCapability::Tools,
            ProviderCapability::Usage,
            ProviderCapability::ModelCatalog,
        ],
    )
    .expect("fixture capabilities are valid");
    ProviderRouteIdentity::new(
        ProviderAdapterId::OpenAiCompatible,
        ProviderProfileId::new("profile-v1-0123456789abcdef0123456789abcdef")
            .expect("fixture profile is valid"),
        ProviderRouteGeneration::new(7).expect("fixture generation is nonzero"),
        ProviderWireRevision::OpenAiChatCompletionsV1,
        capabilities,
    )
    .expect("fixture route is internally consistent")
}

#[test]
fn route_identity_has_a_stable_secret_free_v1_vector() {
    let route = chat_route();
    let encoded = serde_json::to_string(&route).expect("route serializes");
    assert_eq!(
        encoded,
        r#"{"contract_revision":1,"adapter":"openai_compatible","profile":"profile-v1-0123456789abcdef0123456789abcdef","generation":7,"wire_revision":"openai_chat_completions_v1","capabilities":{"revision":1,"supported":["streaming","tools","usage","model_catalog"]}}"#
    );
    assert!(!encoded.contains("http"));
    assert!(!encoded.contains("token"));
    assert!(!encoded.contains("auth"));

    let decoded: ProviderRouteIdentity =
        serde_json::from_str(&encoded).expect("stable vector deserializes");
    assert_eq!(decoded, route);
    assert_eq!(decoded.contract_revision(), 1);
    assert_eq!(
        decoded.profile().as_str(),
        "profile-v1-0123456789abcdef0123456789abcdef"
    );
    assert_eq!(decoded.generation().get(), 7);
}

#[test]
fn identifiers_and_revisions_reject_ambiguous_or_unbounded_values() {
    assert_eq!(
        ProviderProfileId::new("").unwrap_err(),
        ProviderRouteContractError::EmptyProfileId
    );
    assert!(matches!(
        ProviderProfileId::new("profile with spaces").unwrap_err(),
        ProviderRouteContractError::InvalidProfileIdCharacter { .. }
    ));
    assert!(matches!(
        ProviderProfileId::new("x".repeat(ProviderProfileId::MAX_BYTES + 1)).unwrap_err(),
        ProviderRouteContractError::ProfileIdTooLong { .. }
    ));
    assert_eq!(
        ProviderRouteGeneration::new(0).unwrap_err(),
        ProviderRouteContractError::ZeroGeneration
    );
    assert_eq!(
        ProviderCapabilities::new(0, []).unwrap_err(),
        ProviderRouteContractError::ZeroCapabilityRevision
    );
    assert_eq!(
        ProviderCapabilities::new(1, [ProviderCapability::Tools, ProviderCapability::Tools])
            .unwrap_err(),
        ProviderRouteContractError::DuplicateCapability {
            capability: ProviderCapability::Tools,
        }
    );
}

#[test]
fn adapter_wire_pairing_is_validated_on_construction_and_deserialization() {
    let capabilities = ProviderCapabilities::new(1, [ProviderCapability::Streaming]).unwrap();
    let error = ProviderRouteIdentity::new(
        ProviderAdapterId::AnthropicMessages,
        ProviderProfileId::new("profile-v1-fedcba9876543210fedcba9876543210").unwrap(),
        ProviderRouteGeneration::new(1).unwrap(),
        ProviderWireRevision::OpenAiChatCompletionsV1,
        capabilities,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ProviderRouteContractError::AdapterWireMismatch {
            adapter: ProviderAdapterId::AnthropicMessages,
            wire_revision: ProviderWireRevision::OpenAiChatCompletionsV1,
        }
    );

    let mut value = serde_json::to_value(chat_route()).unwrap();
    value["adapter"] = serde_json::json!("anthropic_messages");
    assert!(serde_json::from_value::<ProviderRouteIdentity>(value).is_err());
}

#[test]
fn future_unknown_and_duplicate_serialized_values_fail_closed() {
    let mut future = serde_json::to_value(chat_route()).unwrap();
    future["contract_revision"] = serde_json::json!(2);
    assert!(serde_json::from_value::<ProviderRouteIdentity>(future).is_err());

    let mut unknown = serde_json::to_value(chat_route()).unwrap();
    unknown["capabilities"]["supported"] = serde_json::json!(["streaming", "telepathy"]);
    assert!(serde_json::from_value::<ProviderRouteIdentity>(unknown).is_err());

    let mut duplicate = serde_json::to_value(chat_route()).unwrap();
    duplicate["capabilities"]["supported"] = serde_json::json!(["streaming", "tools", "tools"]);
    assert!(serde_json::from_value::<ProviderRouteIdentity>(duplicate).is_err());

    let mut extra = serde_json::to_value(chat_route()).unwrap();
    extra["endpoint"] = serde_json::json!("https://secret.invalid/?token=do-not-retain");
    assert!(serde_json::from_value::<ProviderRouteIdentity>(extra).is_err());

    let mut zero_generation = serde_json::to_value(chat_route()).unwrap();
    zero_generation["generation"] = serde_json::json!(0);
    assert!(serde_json::from_value::<ProviderRouteIdentity>(zero_generation).is_err());

    let mut zero_capability_revision = serde_json::to_value(chat_route()).unwrap();
    zero_capability_revision["capabilities"]["revision"] = serde_json::json!(0);
    assert!(serde_json::from_value::<ProviderRouteIdentity>(zero_capability_revision).is_err());

    let mut malformed_profile = serde_json::to_value(chat_route()).unwrap();
    malformed_profile["profile"] = serde_json::json!("profile-v1-0123456789ABCDEF0123456789ABCDEF");
    assert!(serde_json::from_value::<ProviderRouteIdentity>(malformed_profile).is_err());
}

#[test]
fn stale_or_unsupported_route_validation_prevents_the_send_boundary() {
    let route = chat_route();
    let mut sends = 0_u32;

    let stale = route.validate_for_send(
        ProviderRouteGeneration::new(8).unwrap(),
        &[ProviderCapability::Streaming],
    );
    if stale.is_ok() {
        sends += 1;
    }
    assert_eq!(
        stale.unwrap_err(),
        ProviderRouteContractError::StaleGeneration {
            bound: ProviderRouteGeneration::new(7).unwrap(),
            current: ProviderRouteGeneration::new(8).unwrap(),
        }
    );

    let unsupported = route.validate_for_send(
        ProviderRouteGeneration::new(7).unwrap(),
        &[ProviderCapability::ImageInput],
    );
    if unsupported.is_ok() {
        sends += 1;
    }
    assert_eq!(
        unsupported.unwrap_err(),
        ProviderRouteContractError::UnsupportedCapability {
            capability: ProviderCapability::ImageInput,
        }
    );
    assert_eq!(sends, 0, "no physical send follows failed route admission");
}

#[test]
fn capability_membership_and_order_are_explicit_and_deterministic() {
    let capabilities = ProviderCapabilities::new(
        3,
        [
            ProviderCapability::ModelCatalog,
            ProviderCapability::StructuredOutput,
            ProviderCapability::ImageInput,
            ProviderCapability::Tools,
            ProviderCapability::Usage,
            ProviderCapability::Streaming,
        ],
    )
    .unwrap();
    assert_eq!(capabilities.revision(), 3);
    assert!(capabilities.supports(ProviderCapability::Streaming));
    assert!(capabilities.supports(ProviderCapability::StructuredOutput));
    assert_eq!(
        capabilities.supported().collect::<Vec<_>>(),
        vec![
            ProviderCapability::Streaming,
            ProviderCapability::Tools,
            ProviderCapability::ImageInput,
            ProviderCapability::StructuredOutput,
            ProviderCapability::Usage,
            ProviderCapability::ModelCatalog,
        ]
    );
    assert_eq!(
        serde_json::to_string(&capabilities).unwrap(),
        r#"{"revision":3,"supported":["streaming","tools","image_input","structured_output","usage","model_catalog"]}"#
    );
}
