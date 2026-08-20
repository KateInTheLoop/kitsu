use std::sync::Arc;
use std::time::Duration;

use kitsu_providers::{
    ProviderAuthSource, ProviderCatalogPolicy, ProviderProfileId, ProviderRouteCurrent,
    ProviderRouteDeclaration, ProviderRoutePolicy, ProviderRouteReceipt, ProviderRouteSnapshot,
};

fn inference_snapshot() -> ProviderRouteSnapshot<String> {
    let declaration = ProviderRouteDeclaration::compatible_chat_v1(
        ProviderProfileId::new("profile-v1-0123456789abcdef0123456789abcdef").unwrap(),
        true,
    );
    let policy = ProviderRoutePolicy::new(
        2,
        1,
        Duration::from_millis(120_250),
        Duration::from_secs(30),
        Duration::from_millis(2_500),
        ProviderCatalogPolicy::Configured,
    )
    .unwrap();
    let secret_runtime =
        Arc::new("https://user:secret@private.invalid/?token=never-serialize".to_string());
    ProviderRouteSnapshot::initial(
        declaration,
        "model-a",
        ProviderAuthSource::Environment,
        policy,
        secret_runtime.clone(),
        secret_runtime,
    )
    .unwrap()
}

#[test]
fn route_receipt_has_a_stable_complete_secret_free_v1_vector() {
    let receipt = inference_snapshot().receipt();
    let encoded = serde_json::to_string(&receipt).unwrap();
    assert_eq!(
        encoded,
        r#"{"receipt_revision":1,"route":{"contract_revision":1,"adapter":"openai_compatible","profile":"profile-v1-0123456789abcdef0123456789abcdef","generation":1,"wire_revision":"openai_chat_completions_v1","capabilities":{"revision":1,"supported":["streaming","tools","structured_output","usage","model_catalog"]}},"model":"model-a","auth_source":"environment","policy":{"inference_retries":2,"catalog_retries":1,"inference_timeout":{"seconds":120,"nanoseconds":250000000},"catalog_timeout":{"seconds":30,"nanoseconds":0},"advisory_discovery_timeout":{"seconds":2,"nanoseconds":500000000},"catalog":"configured","retry_generation":"bound_generation","semantic_replay_generation":"current_generation","fallback":"disabled"}}"#
    );
    for secret in ["private.invalid", "secret", "token", "user:"] {
        assert!(
            !encoded.contains(secret),
            "receipt leaked {secret}: {encoded}"
        );
    }

    let decoded: ProviderRouteReceipt = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, receipt);
    assert_eq!(decoded.receipt_revision(), 1);
    assert_eq!(decoded.model(), Some("model-a"));
    assert_eq!(decoded.auth_source(), ProviderAuthSource::Environment);
    assert_eq!(decoded.policy().inference_retries(), 2);
}

#[test]
fn receipt_decode_rejects_future_unknown_and_contradictory_facts() {
    let value = serde_json::to_value(inference_snapshot().receipt()).unwrap();

    let mut future = value.clone();
    future["receipt_revision"] = serde_json::json!(2);
    assert!(serde_json::from_value::<ProviderRouteReceipt>(future).is_err());

    let mut endpoint = value.clone();
    endpoint["endpoint"] = serde_json::json!("https://secret.invalid/?token=no");
    assert!(serde_json::from_value::<ProviderRouteReceipt>(endpoint).is_err());

    let mut zero_timeout = value.clone();
    zero_timeout["policy"]["inference_timeout"] =
        serde_json::json!({"seconds": 0, "nanoseconds": 0});
    assert!(serde_json::from_value::<ProviderRouteReceipt>(zero_timeout).is_err());

    let mut invalid_duration = value.clone();
    invalid_duration["policy"]["catalog_timeout"]["nanoseconds"] =
        serde_json::json!(1_000_000_000_u64);
    assert!(serde_json::from_value::<ProviderRouteReceipt>(invalid_duration).is_err());

    let mut oversized_model = value.clone();
    oversized_model["model"] = serde_json::json!("m".repeat(4 * 1024 + 1));
    assert!(serde_json::from_value::<ProviderRouteReceipt>(oversized_model).is_err());

    let mut catalog_mismatch = value;
    catalog_mismatch["policy"]["catalog"] = serde_json::json!("unavailable");
    assert!(serde_json::from_value::<ProviderRouteReceipt>(catalog_mismatch).is_err());
}

#[test]
fn catalog_only_receipt_has_no_invented_model_or_inference_policy() {
    let declaration = ProviderRouteDeclaration::compatible_catalog_v1(
        ProviderProfileId::new("profile-v1-fedcba9876543210fedcba9876543210").unwrap(),
    );
    let policy = ProviderRoutePolicy::new(
        0,
        0,
        Duration::from_secs(5),
        Duration::from_secs(7),
        Duration::from_secs(2),
        ProviderCatalogPolicy::Configured,
    )
    .unwrap();
    let snapshot = ProviderRouteSnapshot::initial_catalog_only(
        declaration,
        ProviderAuthSource::ManagedCredential,
        policy,
        Arc::new("secret-runtime".to_string()),
    )
    .unwrap();
    let receipt = snapshot.receipt();

    assert_eq!(receipt.model(), None);
    assert_eq!(receipt.policy().catalog_retries(), 0);
    assert!(serde_json::to_value(&receipt).unwrap()["model"].is_null());
}

#[test]
fn bound_and_current_receipts_report_their_exact_immutable_generations() {
    let current = ProviderRouteCurrent::new(inference_snapshot());
    let first = current.bind().unwrap();
    let first_generation = first.identity().generation();
    let declaration =
        ProviderRouteDeclaration::compatible_chat_v1(first.identity().profile().clone(), true);
    let runtime = Arc::new("replacement-secret-runtime".to_string());
    let replacement = ProviderRouteSnapshot::successor(
        first.identity(),
        declaration,
        "model-b",
        ProviderAuthSource::ManagedCredential,
        first.policy(),
        runtime.clone(),
        runtime,
    )
    .unwrap();
    current.replace(first_generation, replacement).unwrap();

    assert_eq!(first.receipt().route().generation().get(), 1);
    assert_eq!(first.receipt().model(), Some("model-a"));
    let current_receipt = current.receipt().unwrap();
    assert_eq!(current_receipt.route().generation().get(), 2);
    assert_eq!(current_receipt.model(), Some("model-b"));
    assert_eq!(
        current_receipt.auth_source(),
        ProviderAuthSource::ManagedCredential
    );
}
