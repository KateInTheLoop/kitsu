use crate::{
    ContextSnapshotInput, ContextSourcesSnapshot, ContextSourcesSnapshotInput, ContextWindowSource,
    CurrencyAmount, CurrentSessionSnapshot, CurrentSessionSnapshotInput, EffectiveAuthorityInput,
    EffectiveAuthorityMode, ProviderUsageCoverageSnapshot, RunProviderUsageCoverageSnapshot,
    RunUsageSnapshot, SessionCostInput, SessionId, SessionUsageInput, TelemetryProvenance,
    TelemetryValue, TruthClass, UsageSnapshot,
};
use kitsu_providers::{ProviderAdapterId, ProviderRouteReceipt, ProviderWireRevision};

fn route_receipt() -> ProviderRouteReceipt {
    serde_json::from_value(serde_json::json!({
        "receipt_revision": 1,
        "route": {
            "contract_revision": 1,
            "adapter": "anthropic_messages",
            "profile": "profile-v1-0123456789abcdef0123456789abcdef",
            "generation": 7,
            "wire_revision": "anthropic_messages_2023_06_01",
            "capabilities": {
                "revision": 1,
                "supported": ["streaming", "tools", "usage"]
            }
        },
        "model": "claude-fixture",
        "auth_source": "environment",
        "policy": {
            "inference_retries": 1,
            "catalog_retries": 0,
            "inference_timeout": {"seconds": 30, "nanoseconds": 0},
            "catalog_timeout": {"seconds": 10, "nanoseconds": 0},
            "advisory_discovery_timeout": {"seconds": 2, "nanoseconds": 0},
            "catalog": "unavailable",
            "retry_generation": "bound_generation",
            "semantic_replay_generation": "current_generation",
            "fallback": "disabled"
        }
    }))
    .expect("provider-free route receipt fixture")
}

fn authority() -> EffectiveAuthorityInput {
    EffectiveAuthorityInput {
        mode: EffectiveAuthorityMode::Scoped,
        read_roots: 2,
        write_roots: 1,
        protected_roots: 1,
        exact_command_grants: 0,
        commands_enabled: false,
    }
}

fn anchored_context() -> ContextSnapshotInput {
    ContextSnapshotInput {
        window_tokens: TelemetryValue::authoritative(
            32_768,
            TelemetryProvenance::ContextWindow(ContextWindowSource::GatewayProfile),
        ),
        effective_tokens: TelemetryValue::derived(
            30_720,
            TelemetryProvenance::ContextBudgetArithmetic,
        ),
        used_tokens: TelemetryValue::estimated(
            12_345,
            TelemetryProvenance::ProviderAnchoredContextMeter,
        ),
    }
}

#[test]
fn context_sources_keep_estimates_budget_arithmetic_and_unavailable_buckets_distinct() {
    let sources = ContextSourcesSnapshot::from_input(ContextSourcesSnapshotInput {
        core_instructions_tokens: 1_200,
        project_context_tokens: 0,
        skill_context_tokens: 320,
        tool_schema_tokens: 640,
        runtime_system_message_tokens: 18,
        user_message_tokens: 2_100,
        assistant_message_tokens: 900,
        tool_result_tokens: 450,
        prompt_budget_tokens: 30_720,
        output_reserve_tokens: 2_048,
        trim_target_tokens: 28_672,
        estimate_buffer_tokens: 2_048,
        compactions: 2,
        exchanges_removed: 3,
        tool_results_elided: 4,
        compaction_tokens_saved: 5_000,
    });

    assert_eq!(sources.core_instructions.value(), Some(&1_200));
    assert_eq!(sources.project_context.value(), Some(&0));
    assert_eq!(
        sources.project_context.truth_class(),
        TruthClass::Estimated,
        "owned startup absence is an estimated zero, not unavailable"
    );
    assert_eq!(
        sources.core_instructions.provenance(),
        TelemetryProvenance::RequestAssemblyEstimate
    );
    assert_eq!(
        sources.user_messages.provenance(),
        TelemetryProvenance::ConversationMessageEstimate
    );
    for unavailable in [
        &sources.attachments,
        &sources.on_demand_skill_bodies,
        &sources.memory,
        &sources.response_schema,
    ] {
        assert_eq!(unavailable.value(), None);
        assert_eq!(unavailable.truth_class(), TruthClass::Unavailable);
        assert_eq!(
            unavailable.provenance(),
            TelemetryProvenance::ContextSourceNotSeparatelyTracked
        );
    }
    for derived in [
        &sources.prompt_budget,
        &sources.output_reserve,
        &sources.trim_target,
        &sources.estimate_buffer,
    ] {
        assert_eq!(derived.truth_class(), TruthClass::Derived);
        assert_eq!(
            derived.provenance(),
            TelemetryProvenance::ContextBudgetArithmetic
        );
    }
    assert_eq!(sources.compactions.truth_class(), TruthClass::Authoritative);
    assert_eq!(
        sources.compactions.provenance(),
        TelemetryProvenance::SessionAccounting
    );
    assert_eq!(
        sources.compaction_tokens_saved.truth_class(),
        TruthClass::Estimated
    );
    assert_eq!(
        sources.compaction_tokens_saved.provenance(),
        TelemetryProvenance::ContextMeterEstimate
    );
}

#[test]
fn snapshot_carries_owned_route_identity_and_explicit_authority_truth() {
    let snapshot = CurrentSessionSnapshot::from_input(CurrentSessionSnapshotInput {
        provider_route: route_receipt(),
        session_id: Some(SessionId::new("session-42").unwrap()),
        run_id: Some(crate::RunId::new("run-9").unwrap()),
        authority: authority(),
        context: anchored_context(),
        usage: SessionUsageInput::provider_reported(
            2,
            0,
            2,
            120,
            30,
            150,
            Some(20),
            SessionCostInput::unavailable(),
        ),
    });

    assert_eq!(
        snapshot.status.route.adapter.value(),
        Some(&ProviderAdapterId::AnthropicMessages)
    );
    assert_eq!(snapshot.status.route.generation.value(), Some(&7));
    assert_eq!(
        snapshot.status.route.wire_revision.value(),
        Some(&ProviderWireRevision::AnthropicMessages2023_06_01)
    );
    assert_eq!(
        snapshot.status.route.model.value().map(String::as_str),
        Some("claude-fixture")
    );
    assert_eq!(
        snapshot.status.session_id.value().map(String::as_str),
        Some("session-42")
    );
    assert_eq!(
        snapshot.status.run_id.value().map(String::as_str),
        Some("run-9")
    );
    assert_eq!(
        snapshot.status.authority.read_roots.truth_class(),
        TruthClass::Authoritative
    );
    assert_eq!(
        snapshot.status.authority.read_roots.provenance(),
        TelemetryProvenance::EffectiveAuthorityInput
    );
}

#[test]
fn context_snapshot_preserves_window_source_and_labels_arithmetic() {
    let snapshot = CurrentSessionSnapshot::from_input(CurrentSessionSnapshotInput {
        provider_route: route_receipt(),
        session_id: None,
        run_id: None,
        authority: authority(),
        context: anchored_context(),
        usage: SessionUsageInput::no_provider_usage(1, 1, SessionCostInput::unavailable()),
    });

    assert_eq!(
        snapshot.context.window_tokens.truth_class(),
        TruthClass::Authoritative
    );
    assert_eq!(
        snapshot.context.window_tokens.provenance(),
        TelemetryProvenance::ContextWindow(ContextWindowSource::GatewayProfile)
    );
    assert_eq!(
        snapshot.context.used_tokens.truth_class(),
        TruthClass::Estimated
    );
    assert_eq!(snapshot.context.available_tokens.value(), Some(&18_375));
    assert_eq!(
        snapshot.context.available_tokens.truth_class(),
        TruthClass::Derived
    );
    assert_eq!(
        snapshot.context.available_tokens.provenance(),
        TelemetryProvenance::ContextBudgetArithmetic
    );
}

#[test]
fn provider_reported_usage_keeps_coverage_and_does_not_invent_currency_cost() {
    let snapshot = CurrentSessionSnapshot::from_input(CurrentSessionSnapshotInput {
        provider_route: route_receipt(),
        session_id: None,
        run_id: None,
        authority: authority(),
        context: anchored_context(),
        usage: SessionUsageInput::provider_reported(
            3,
            1,
            3,
            400,
            50,
            450,
            None,
            SessionCostInput::unavailable(),
        ),
    });

    assert_eq!(snapshot.usage.prompt_tokens.value(), Some(&400));
    assert_eq!(
        snapshot.usage.prompt_tokens.truth_class(),
        TruthClass::ProviderReported
    );
    assert_eq!(snapshot.usage.responses_reporting_usage.value(), Some(&3));
    assert_eq!(snapshot.usage.responses_missing_usage.value(), Some(&1));
    assert_eq!(
        snapshot.usage.cached_prompt_tokens.truth_class(),
        TruthClass::Unavailable
    );
    assert_eq!(
        snapshot.usage.cached_prompt_tokens.provenance(),
        TelemetryProvenance::ProviderUsageNotItemized
    );
    assert_eq!(
        snapshot.usage.monetary_cost.truth_class(),
        TruthClass::Unavailable
    );
    assert_eq!(
        snapshot.usage.monetary_cost.provenance(),
        TelemetryProvenance::PortableCostUnavailable
    );
}

#[test]
fn run_usage_adapter_preserves_partial_coverage_and_explicit_cached_zero() {
    let all = ProviderUsageCoverageSnapshot {
        responses_reporting_usage: 1,
        responses_missing_usage: 2,
        cached_prompt_tokens: Some(0),
    };
    let run = RunUsageSnapshot {
        turns: 2,
        prompt_tokens: 17,
        completion_tokens: 3,
        total_tokens: 20,
        coverage: RunProviderUsageCoverageSnapshot {
            all,
            inference: all,
            compaction: ProviderUsageCoverageSnapshot::default(),
            model_catalog: ProviderUsageCoverageSnapshot::default(),
        },
    };

    let usage = UsageSnapshot::from(SessionUsageInput::from_run_usage(
        &run,
        SessionCostInput::unavailable(),
    ));

    assert_eq!(usage.responses_reporting_usage.value(), Some(&1));
    assert_eq!(usage.responses_missing_usage.value(), Some(&2));
    assert_eq!(usage.prompt_tokens.value(), Some(&17));
    assert_eq!(usage.cached_prompt_tokens.value(), Some(&0));
    assert_eq!(
        usage.cached_prompt_tokens.truth_class(),
        TruthClass::ProviderReported
    );
}

#[test]
fn omitted_provider_usage_is_unavailable_not_zero_in_memory_or_json() {
    let snapshot = CurrentSessionSnapshot::from_input(CurrentSessionSnapshotInput {
        provider_route: route_receipt(),
        session_id: None,
        run_id: None,
        authority: authority(),
        context: anchored_context(),
        usage: SessionUsageInput::no_provider_usage(2, 4, SessionCostInput::unavailable()),
    });

    assert_eq!(snapshot.usage.prompt_tokens.value(), None);
    assert_eq!(
        snapshot.usage.total_tokens.truth_class(),
        TruthClass::Unavailable
    );
    assert_eq!(
        snapshot.usage.total_tokens.provenance(),
        TelemetryProvenance::ProviderUsageNotReported
    );
    assert_eq!(snapshot.usage.responses_reporting_usage.value(), Some(&0));
    assert_eq!(snapshot.usage.responses_missing_usage.value(), Some(&4));

    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(json["usage"]["prompt_tokens"]["truth"], "unavailable");
    assert!(json["usage"]["prompt_tokens"].get("value").is_none());
    assert_eq!(json["usage"]["responses_missing_usage"]["value"], 4);
    assert!(json.get("credits").is_none());
    assert!(json.get("rate_limits").is_none());
    assert!(json.get("lifetime").is_none());
}

#[test]
fn explicit_provider_cost_remains_reported_and_is_never_derived() {
    let amount = CurrencyAmount {
        currency_code: "USD".to_string(),
        minor_units: 17,
        decimal_places: 2,
    };
    let snapshot = CurrentSessionSnapshot::from_input(CurrentSessionSnapshotInput {
        provider_route: route_receipt(),
        session_id: None,
        run_id: None,
        authority: authority(),
        context: anchored_context(),
        usage: SessionUsageInput::provider_reported(
            1,
            0,
            1,
            20,
            5,
            25,
            None,
            SessionCostInput::provider_reported(amount.clone()),
        ),
    });

    assert_eq!(snapshot.usage.monetary_cost.value(), Some(&amount));
    assert_eq!(
        snapshot.usage.monetary_cost.truth_class(),
        TruthClass::ProviderReported
    );
    assert_eq!(
        snapshot.usage.monetary_cost.provenance(),
        TelemetryProvenance::ExplicitCostObservation
    );
}
