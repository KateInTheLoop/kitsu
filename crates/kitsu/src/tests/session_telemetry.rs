use super::*;

#[test]
fn current_session_authority_counts_minimal_roots_and_protected_union_once() {
    let authority = current_session_authority_input(
        Mode::Scoped,
        vec!["/work".into(), "/work/src".into(), "/other".into()],
        vec!["/work".into(), "/work/out".into(), "/work".into()],
        vec!["/private".into(), "/private/config".into()],
        vec!["/private".into(), "/state".into(), "/state/cache".into()],
        2,
        true,
    );

    assert_eq!(authority.mode, kitsu_core::EffectiveAuthorityMode::Scoped);
    assert_eq!(authority.read_roots, 2);
    assert_eq!(authority.write_roots, 1);
    assert_eq!(authority.protected_roots, 2);
    assert_eq!(authority.exact_command_grants, 2);
    assert!(authority.commands_enabled);
}

#[test]
fn current_session_context_preserves_default_anchor_and_shortfall_truth() {
    let default = current_session_context_input(
        None,
        ContextWindowSource::ConservativeDefault,
        Some(2_048),
        1_000,
        false,
        None,
    );
    assert_eq!(default.window_tokens.value(), Some(&DEFAULT_CONTEXT_WINDOW));
    assert_eq!(
        default.window_tokens.truth_class(),
        kitsu_core::TruthClass::Estimated
    );
    assert_eq!(
        default.used_tokens.provenance(),
        kitsu_core::TelemetryProvenance::ContextMeterEstimate
    );

    let clamped = current_session_context_input(
        Some(32_768),
        ContextWindowSource::LiveDiscovery,
        Some(2_048),
        12_000,
        true,
        Some(kitsu_core::ContextWindowEvidence {
            reported_prompt_tokens: 8_192,
            estimated_sent_tokens: 20_000,
            resolved_window: 32_768,
            warned: true,
            shortfall_count: 2,
        }),
    );
    assert_eq!(clamped.window_tokens.value(), Some(&32_768));
    assert_eq!(
        clamped.window_tokens.truth_class(),
        kitsu_core::TruthClass::Authoritative
    );
    assert_eq!(
        clamped.used_tokens.provenance(),
        kitsu_core::TelemetryProvenance::ProviderAnchoredContextMeter
    );
    assert_eq!(
        clamped.effective_tokens.value(),
        Some(&ContextBudget::new(8_192, Some(2_048)).effective)
    );
    assert_eq!(
        clamped.effective_tokens.truth_class(),
        kitsu_core::TruthClass::Derived
    );
}

#[test]
fn current_session_observation_preserves_mixed_coverage_and_cached_presence() {
    let authority = current_session_authority_input(
        Mode::Deny,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        0,
        false,
    );
    let context = current_session_context_input(
        Some(16_384),
        ContextWindowSource::Profile,
        Some(1_024),
        400,
        false,
        None,
    );
    let coverage = |cached_prompt_tokens| kitsu_core::RunUsageSnapshot {
        turns: 2,
        prompt_tokens: 120,
        completion_tokens: 30,
        total_tokens: 150,
        coverage: kitsu_core::RunProviderUsageCoverageSnapshot {
            all: kitsu_core::ProviderUsageCoverageSnapshot {
                responses_reporting_usage: 2,
                responses_missing_usage: 1,
                cached_prompt_tokens,
            },
            ..Default::default()
        },
    };

    let absent = current_session_snapshot_from_observation(
        telemetry_route_receipt_fixture(),
        Some("bad session id".to_string()),
        run_spec().run_id().clone(),
        authority,
        context.clone(),
        coverage(None),
    );
    assert_eq!(absent.status.session_id.value(), None);
    assert_eq!(absent.usage.responses_reporting_usage.value(), Some(&2));
    assert_eq!(absent.usage.responses_missing_usage.value(), Some(&1));
    assert_eq!(absent.usage.cached_prompt_tokens.value(), None);

    let supplied_zero = current_session_snapshot_from_observation(
        telemetry_route_receipt_fixture(),
        Some("session-42".to_string()),
        run_spec().run_id().clone(),
        authority,
        context,
        coverage(Some(0)),
    );
    assert_eq!(
        supplied_zero.status.session_id.value(),
        Some(&"session-42".to_string())
    );
    assert_eq!(supplied_zero.usage.cached_prompt_tokens.value(), Some(&0));
    assert_eq!(
        supplied_zero.usage.monetary_cost.truth_class(),
        kitsu_core::TruthClass::Unavailable
    );
}

#[test]
fn command_grant_formatter_explains_executable_identity() {
    let root = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let policy = kitsu_tools::CommandPolicy::new(
        &[root.path().to_string_lossy().to_string()],
        kitsu_tools::CommandMode::Deny,
    )
    .unwrap();
    policy
        .grant_command(&[executable.to_string_lossy().into_owned()], root.path())
        .unwrap();
    let grant = policy.session_grants().pop().unwrap();

    let rendered = format_command_grant_identity(&grant);
    assert!(rendered.contains("resolution invocation path"));
    assert!(rendered.contains("observed "));
    assert!(rendered.contains("fingerprint bound to "));
    assert!(rendered.contains("revalidated before execution"));
    assert!(rendered.contains("immutable handle no"));
    assert!(!rendered.contains('\u{1b}'));
}
