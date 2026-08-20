use std::sync::Arc;
use std::time::Duration;

use kitsu_providers::{
    ProviderAuthSource, ProviderCatalogPolicy, ProviderProfileId, ProviderRouteContractError,
    ProviderRouteDeclaration, ProviderRoutePolicy, ProviderRouteSnapshot,
};

fn declaration(catalog: bool) -> ProviderRouteDeclaration {
    ProviderRouteDeclaration::compatible_chat_v1(
        ProviderProfileId::derive_v1(b"snapshot-test", b"primary").unwrap(),
        catalog,
    )
}

fn catalog_declaration() -> ProviderRouteDeclaration {
    ProviderRouteDeclaration::compatible_catalog_v1(
        ProviderProfileId::derive_v1(b"snapshot-test", b"primary").unwrap(),
    )
}

fn policy(catalog: ProviderCatalogPolicy) -> ProviderRoutePolicy {
    ProviderRoutePolicy::new(
        2,
        0,
        Duration::from_secs(120),
        Duration::from_secs(120),
        Duration::from_secs(2),
        catalog,
    )
    .unwrap()
}

fn snapshot_error(
    result: Result<ProviderRouteSnapshot<()>, ProviderRouteContractError>,
) -> ProviderRouteContractError {
    match result {
        Ok(_) => panic!("snapshot construction unexpectedly succeeded"),
        Err(error) => error,
    }
}

#[test]
fn initial_snapshot_owns_one_coherent_runtime_generation() {
    let inference = Arc::new("inference-runtime");
    let catalog = Arc::new("catalog-runtime");
    let snapshot = ProviderRouteSnapshot::initial(
        declaration(true),
        "model-a",
        ProviderAuthSource::Environment,
        policy(ProviderCatalogPolicy::Configured),
        inference.clone(),
        catalog.clone(),
    )
    .unwrap();

    assert_eq!(snapshot.identity().generation().get(), 1);
    assert_eq!(snapshot.model(), Some("model-a"));
    assert_eq!(snapshot.auth_source(), ProviderAuthSource::Environment);
    assert_eq!(snapshot.policy().inference_retries(), 2);
    assert_eq!(snapshot.policy().catalog_retries(), 0);
    assert_eq!(
        snapshot.policy().inference_timeout(),
        Duration::from_secs(120)
    );
    assert_eq!(
        snapshot.policy().catalog_timeout(),
        Duration::from_secs(120)
    );
    assert_eq!(
        snapshot.policy().advisory_discovery_timeout(),
        Duration::from_secs(2)
    );
    assert!(Arc::ptr_eq(
        snapshot.inference_runtime().unwrap(),
        &inference
    ));
    assert!(Arc::ptr_eq(snapshot.catalog_runtime(), &catalog));
}

#[test]
fn catalog_only_snapshot_has_no_inference_binding_or_invented_model() {
    let catalog = Arc::new("catalog-runtime");
    let snapshot = ProviderRouteSnapshot::initial_catalog_only(
        catalog_declaration(),
        ProviderAuthSource::Environment,
        policy(ProviderCatalogPolicy::Configured),
        catalog.clone(),
    )
    .unwrap();

    assert_eq!(snapshot.identity().generation().get(), 1);
    assert_eq!(snapshot.model(), None);
    assert_eq!(snapshot.inference_runtime(), None);
    assert!(Arc::ptr_eq(snapshot.catalog_runtime(), &catalog));
}

#[test]
fn snapshot_rejects_model_and_catalog_ambiguity_before_publication() {
    let runtime = Arc::new(());
    assert!(matches!(
        snapshot_error(ProviderRouteSnapshot::initial_catalog_only(
            declaration(true),
            ProviderAuthSource::None,
            policy(ProviderCatalogPolicy::Configured),
            runtime.clone(),
        )),
        ProviderRouteContractError::CatalogOnlyCapabilityMismatch { .. }
    ));
    assert_eq!(
        snapshot_error(ProviderRouteSnapshot::initial(
            declaration(false),
            "",
            ProviderAuthSource::None,
            policy(ProviderCatalogPolicy::Unavailable),
            runtime.clone(),
            runtime.clone(),
        )),
        ProviderRouteContractError::EmptyModelId
    );
    assert!(matches!(
        snapshot_error(ProviderRouteSnapshot::initial(
            declaration(false),
            "m".repeat(ProviderRouteSnapshot::<()>::MAX_MODEL_ID_BYTES + 1),
            ProviderAuthSource::None,
            policy(ProviderCatalogPolicy::Unavailable),
            runtime.clone(),
            runtime.clone(),
        )),
        ProviderRouteContractError::ModelIdTooLong { .. }
    ));
    assert_eq!(
        snapshot_error(ProviderRouteSnapshot::initial(
            declaration(false),
            "model-a",
            ProviderAuthSource::None,
            policy(ProviderCatalogPolicy::Configured),
            runtime.clone(),
            runtime.clone(),
        )),
        ProviderRouteContractError::CatalogCapabilityMismatch {
            declared: false,
            configured: true,
        }
    );
    assert_eq!(
        snapshot_error(ProviderRouteSnapshot::initial(
            declaration(true),
            "model-a",
            ProviderAuthSource::None,
            policy(ProviderCatalogPolicy::Unavailable),
            runtime.clone(),
            runtime,
        )),
        ProviderRouteContractError::CatalogCapabilityMismatch {
            declared: true,
            configured: false,
        }
    );
}

#[test]
fn route_policy_rejects_zero_time_budgets() {
    for (inference, catalog, discovery) in [
        (
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(1),
        ),
        (
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::from_secs(1),
        ),
        (
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::ZERO,
        ),
    ] {
        assert!(matches!(
            ProviderRoutePolicy::new(
                2,
                0,
                inference,
                catalog,
                discovery,
                ProviderCatalogPolicy::Configured,
            ),
            Err(ProviderRouteContractError::ZeroRouteTimeout { .. })
        ));
    }
}
