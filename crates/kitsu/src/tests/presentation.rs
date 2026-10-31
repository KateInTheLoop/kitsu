use super::*;

#[test]
fn human_provider_attempt_notice_is_safe_and_complete() {
    let event = kitsu_core::AgentRuntimeEvent::ProviderAttempt {
        round: 2,
        purpose: kitsu_core::ProviderRequestPurpose::Inference,
        route_generation: None,
        attempt: 1,
        max_attempts: 3,
        category: kitsu_providers::ProviderAttemptCategory::RateLimited,
        status: Some(429),
        outcome: kitsu_providers::ProviderAttemptOutcome::RetryScheduled,
        terminal: false,
        retry_delay_ms: Some(750),
        retry_after_used: true,
        deadline_scope: kitsu_core::ProviderDeadlineScope::ProviderRequest,
        remaining_deadline_ms: 12_500,
        turn_remaining_ms: Some(9_500),
    };
    let notice = format_runtime_notice(&event).unwrap();
    assert!(notice.contains("attempt 1/3"));
    assert!(notice.contains("rate limited; HTTP 429"));
    assert!(notice.contains("retry in 750ms (Retry-After)"));
    assert!(notice.contains("12500ms provider-request deadline remaining"));
    assert!(notice.contains("9500ms turn remaining"));
}

#[test]
fn first_attempt_success_telemetry_is_silent_but_retried_success_reports() {
    let success = |attempt: u32| kitsu_core::AgentRuntimeEvent::ProviderAttempt {
        round: 1,
        purpose: kitsu_core::ProviderRequestPurpose::Inference,
        route_generation: None,
        attempt,
        max_attempts: 3,
        category: kitsu_providers::ProviderAttemptCategory::Success,
        status: Some(200),
        outcome: kitsu_providers::ProviderAttemptOutcome::BodyCompleted,
        terminal: true,
        retry_delay_ms: None,
        retry_after_used: false,
        deadline_scope: kitsu_core::ProviderDeadlineScope::ProviderRequest,
        remaining_deadline_ms: 120_000,
        turn_remaining_ms: None,
    };
    // The normal case — one attempt, success — is transport noise and
    // must never narrate after a good turn.
    assert_eq!(format_runtime_notice(&success(1)), None);
    // A success that needed retries closes the visible retry story.
    let retried = format_runtime_notice(&success(2)).unwrap();
    assert!(retried.contains("attempt 2/3: success"));
}

#[test]
fn human_structured_repair_notice_is_content_free_and_actionable() {
    let event = kitsu_core::AgentRuntimeEvent::StructuredFinalizationDeferred {
        round: 2,
        repair_round: 3,
        finalization_round_reserved: true,
        cause: "required_write_sha256_mismatch".to_string(),
        targets: vec![kitsu_core::StructuredFinalizationRepairTarget {
            path: "/private/work/result.json".to_string(),
            expected_sha256: "a".repeat(64),
            actual_sha256: "b".repeat(64),
        }],
        repair_instruction_disclosed: true,
        write_authority_granted: false,
    };

    let notice = format_runtime_notice(&event).unwrap();
    assert_eq!(
        notice,
        "exact output mismatch; keeping tools available in repair round 3 for 1 target"
    );
    assert!(!notice.contains("/private"));
    assert!(!notice.contains(&"a".repeat(64)));
    assert!(!notice.contains(&"b".repeat(64)));
}

/// Regression: a sibling directory that merely SHARES the home dir as a
/// string prefix (no path-segment boundary) must NOT be corrupted by
/// `strip_prefix` — it must be left as the full, unshortened path.
#[test]
#[serial]
fn shorten_home_requires_a_path_segment_boundary() {
    let _g_home = EnvVarGuard::set("HOME", "/home/mmpg");
    assert_eq!(
        shorten_home(std::path::Path::new("/home/mmpg2/project")),
        "/home/mmpg2/project",
        "a sibling dir sharing HOME as a bare string prefix must not become ~2/project"
    );
}

#[test]
#[serial]
fn shorten_home_shortens_a_real_subdirectory() {
    let _g_home = EnvVarGuard::set("HOME", "/home/mmpg");
    assert_eq!(
        shorten_home(std::path::Path::new("/home/mmpg/project")),
        "~/project"
    );
}

#[test]
#[serial]
fn shorten_home_shortens_the_home_dir_itself() {
    let _g_home = EnvVarGuard::set("HOME", "/home/mmpg");
    assert_eq!(shorten_home(std::path::Path::new("/home/mmpg")), "~");
}
