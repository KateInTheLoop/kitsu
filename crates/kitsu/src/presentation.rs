//! Sanitized terminal notices and path labels.

/// Human-mode notice for an unrecoverable tool-result loss, gated to an
/// interactive styled terminal and nothing else.
///
/// This is deliberately NOT part of [`format_runtime_notice`]: all three
/// runtime-notice sinks share that formatter and two of them are not
/// terminals, so extending it would print this line on the piped and
/// `--plain` surfaces that must stay byte-identical. `stderr_styled()` is
/// `stderr_is_tty() && !plain_mode()`, which already excludes `--plain`,
/// `--screen-reader`, `NO_COLOR`, `KITSU_PLAIN`, `KITSU_SCREEN_READER` and
/// `TERM=dumb`; `is_tty()` adds the stdout half; the callers are already
/// inside their `!jsonl` branch.
///
/// One line per round naming the count, matching the event: the trimmer
/// aggregates within a round, so two bodies destroyed in one enforcement pass
/// are one loss of context, not two.
pub(crate) fn format_dropped_results_notice(
    event: &kitsu_core::AgentRuntimeEvent,
) -> Option<String> {
    let kitsu_core::AgentRuntimeEvent::ToolResultsDropped { round, dropped } = event else {
        return None;
    };
    if *dropped == 0 {
        return None;
    }
    if !(kitsu_render::stderr_styled() && kitsu_render::is_tty()) {
        return None;
    }
    Some(format!(
        "context budget dropped {dropped} tool result{} in round {round}; the provider never saw {}",
        if *dropped == 1 { "" } else { "s" },
        if *dropped == 1 { "it" } else { "them" },
    ))
}

/// Integration hook for human-mode runtime notices. The event log can call
/// this beside its JSONL serializer; results contain only host-owned bounded
/// facts, numeric status/timing fields, and no provider-controlled text.
///
/// The success path stays silent: a first-attempt success is the normal case
/// and narrating it is transport noise. Retries, failures, and a success that
/// needed more than one attempt still report (JSONL mode keeps every event).
pub(crate) fn format_runtime_notice(event: &kitsu_core::AgentRuntimeEvent) -> Option<String> {
    if let kitsu_core::AgentRuntimeEvent::StructuredFinalizationDeferred {
        repair_round,
        targets,
        ..
    } = event
    {
        return Some(format!(
            "exact output mismatch; keeping tools available in repair round {repair_round} for {} target{}",
            targets.len(),
            if targets.len() == 1 { "" } else { "s" }
        ));
    }
    let kitsu_core::AgentRuntimeEvent::ProviderAttempt {
        purpose,
        attempt,
        max_attempts,
        category,
        status,
        outcome,
        terminal,
        retry_delay_ms,
        retry_after_used,
        remaining_deadline_ms,
        turn_remaining_ms,
        ..
    } = event
    else {
        return None;
    };
    if matches!(category, kitsu_providers::ProviderAttemptCategory::Success) && *attempt <= 1 {
        return None;
    }
    let category = match category {
        kitsu_providers::ProviderAttemptCategory::Success => "success",
        kitsu_providers::ProviderAttemptCategory::RateLimited => "rate limited",
        kitsu_providers::ProviderAttemptCategory::Unavailable => "unavailable",
        kitsu_providers::ProviderAttemptCategory::Authentication => "authentication",
        kitsu_providers::ProviderAttemptCategory::ClientError => "client error",
        kitsu_providers::ProviderAttemptCategory::ServerError => "server error",
        kitsu_providers::ProviderAttemptCategory::UnexpectedStatus => "unexpected status",
        kitsu_providers::ProviderAttemptCategory::Timeout => "timeout",
        kitsu_providers::ProviderAttemptCategory::Connection => "connection",
        kitsu_providers::ProviderAttemptCategory::ResponseTooLarge => "response too large",
        kitsu_providers::ProviderAttemptCategory::InvalidResponse => "invalid response",
        kitsu_providers::ProviderAttemptCategory::Cancelled => "cancelled",
    };
    let outcome = match outcome {
        kitsu_providers::ProviderAttemptOutcome::ResponseAccepted => "response accepted",
        kitsu_providers::ProviderAttemptOutcome::BodyCompleted => "body completed",
        kitsu_providers::ProviderAttemptOutcome::BodyFailed => "body failed",
        kitsu_providers::ProviderAttemptOutcome::RetryScheduled => "retry scheduled",
        kitsu_providers::ProviderAttemptOutcome::Rejected => "request rejected",
        kitsu_providers::ProviderAttemptOutcome::TransportFailed => "transport failed",
        kitsu_providers::ProviderAttemptOutcome::DeadlineExceeded => "deadline exhausted",
        kitsu_providers::ProviderAttemptOutcome::Cancelled => "cancelled",
    };
    let status = status
        .map(|status| format!("; HTTP {status}"))
        .unwrap_or_default();
    let retry = retry_delay_ms
        .map(|delay| {
            format!(
                "; retry in {delay}ms{}",
                if *retry_after_used {
                    " (Retry-After)"
                } else {
                    ""
                }
            )
        })
        .unwrap_or_default();
    let purpose = match purpose {
        kitsu_core::ProviderRequestPurpose::Inference => "inference",
        kitsu_core::ProviderRequestPurpose::Compaction => "compaction",
        kitsu_core::ProviderRequestPurpose::ModelCatalog => "model catalog",
    };
    let turn_remaining = turn_remaining_ms
        .map(|remaining| format!("; {remaining}ms turn remaining"))
        .unwrap_or_default();
    Some(format!(
        "{purpose} provider attempt {attempt}/{max_attempts}: {category}{status}; {outcome}; terminal={terminal}{retry}; {remaining_deadline_ms}ms provider-request deadline remaining{turn_remaining}"
    ))
}

/// Replace the home-directory prefix with `~` for compact display in the banner.
pub(crate) fn terminal_inline(value: &str) -> String {
    kitsu_render::sanitize_terminal_text(value)
        .chars()
        .map(|ch| if matches!(ch, '\n' | '\t') { ' ' } else { ch })
        .collect()
}

pub(crate) fn shorten_home(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().to_string();
    if let Some(home) = kitsu_tools::host_home_dir() {
        let home = home.to_string_lossy();
        let home = home.trim_end_matches('/');
        if !home.is_empty() {
            // Require a path-segment boundary after the home prefix (exact match,
            // or followed by '/') so a sibling directory that merely SHARES the
            // home dir as a string prefix (e.g. HOME=/home/mmpg and cwd
            // /home/mmpg2/project) isn't corrupted into a bogus `~2/project`.
            if let Some(rest) = s.strip_prefix(home) {
                if rest.is_empty() {
                    return "~".to_string();
                }
                if let Some(rest) = rest.strip_prefix('/') {
                    return terminal_inline(&format!("~/{rest}"));
                }
            }
        }
    }
    terminal_inline(&s)
}
