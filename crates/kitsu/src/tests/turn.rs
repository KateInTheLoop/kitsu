use super::*;

#[test]
fn resumable_tail_keeps_clean_and_complete_transcripts() {
    // Ends on assistant prose: nothing to trim.
    let mut prose = vec![
        ChatMessage::user("q"),
        ChatMessage::assistant("done", Vec::new()),
    ];
    assert_eq!(truncate_to_resumable_tail(&mut prose), 0);
    assert_eq!(prose.len(), 2);

    // A fully answered tool round is a valid resume point: keep it intact.
    let mut complete = vec![
        ChatMessage::user("q"),
        ChatMessage::assistant("", vec![call("a")]),
        ChatMessage::tool("a", "result-a"),
    ];
    assert_eq!(truncate_to_resumable_tail(&mut complete), 0);
    assert_eq!(complete.len(), 3);
}

#[test]
fn resumable_tail_drops_dangling_and_partial_rounds() {
    // Deadline fired after the tool_calls message but before any result:
    // drop the unanswered assistant call so the tail is resumable.
    let mut dangling = vec![
        ChatMessage::user("q"),
        ChatMessage::assistant("", vec![call("a")]),
    ];
    assert_eq!(truncate_to_resumable_tail(&mut dangling), 1);
    assert_eq!(dangling.len(), 1);
    assert_eq!(dangling[0].role, kitsu_providers::Role::User);

    // Two calls requested, only one answered: drop the whole incomplete
    // round (the assistant message and its partial answer).
    let mut partial = vec![
        ChatMessage::user("q"),
        ChatMessage::assistant("", vec![call("a"), call("b")]),
        ChatMessage::tool("a", "result-a"),
    ];
    assert_eq!(truncate_to_resumable_tail(&mut partial), 2);
    assert_eq!(partial.len(), 1);
}

#[test]
fn resumable_tail_preserves_earlier_complete_round() {
    // First round complete, second round dangling: keep the first, drop the
    // second — a slow model that finished one round still keeps that work.
    let mut messages = vec![
        ChatMessage::user("q"),
        ChatMessage::assistant("", vec![call("a")]),
        ChatMessage::tool("a", "result-a"),
        ChatMessage::assistant("", vec![call("b")]),
    ];
    assert_eq!(truncate_to_resumable_tail(&mut messages), 1);
    assert_eq!(messages.len(), 3);
    // The surviving tail is the complete first round.
    assert_eq!(messages[2].role, kitsu_providers::Role::Tool);
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("a"));
}

#[test]
fn strict_turn_failure_commits_recovered_tool_errors() {
    // Recovered: an early failure, but the final tool round was clean.
    let recovered = kitsu_core::AgentTurnStatus {
        tool_errors: 2,
        final_round_tool_errors: 0,
        ..Default::default()
    };
    assert!(strict_turn_failure(&recovered, None, "recovered answer").is_none());

    // Unrecovered: the final tool round still failed.
    let unrecovered = kitsu_core::AgentTurnStatus {
        tool_errors: 1,
        final_round_tool_errors: 1,
        ..Default::default()
    };
    let failure = strict_turn_failure(&unrecovered, None, "could not finish").expect("must fail");
    assert_eq!(failure.kind, ScriptFailureKind::Tool);
    assert!(failure.issue.contains("1 tool call(s) failed"));

    // A policy denial always fails, even when a later round succeeded.
    let denied = kitsu_core::AgentTurnStatus {
        tool_errors: 1,
        policy_errors: 1,
        final_round_tool_errors: 0,
        ..Default::default()
    };
    let failure = strict_turn_failure(&denied, None, "unsupported answer").expect("must fail");
    assert_eq!(failure.kind, ScriptFailureKind::Policy);
    assert!(failure.issue.contains("denied by local policy"));

    let length_limited = kitsu_core::AgentTurnStatus {
        completion: kitsu_core::TurnCompletion::LengthLimited,
        ..Default::default()
    };
    let failure =
        strict_turn_failure(&length_limited, Some(12), "partial").expect("length limit must fail");
    assert_eq!(failure.kind, ScriptFailureKind::Provider);
    assert!(failure.issue.contains("hit --max-output-tokens 12"));
    assert!(failure.issue.contains("--max-output-tokens 24 or higher"));
    let implicit =
        strict_turn_failure(&length_limited, None, "partial").expect("gateway limit must fail");
    assert!(implicit.issue.contains("gateway's length limit"));
    assert!(implicit.issue.contains("--max-output-tokens <n>"));

    let failure = strict_turn_failure(&kitsu_core::AgentTurnStatus::default(), None, " \n\t")
        .expect("blank strict output must fail");
    assert_eq!(failure.kind, ScriptFailureKind::Provider);
    assert!(failure.issue.contains("no final assistant text"));
}
