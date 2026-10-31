use super::*;

/// **The report one dispatch produces, in its author's own words.**
///
/// Three outcomes and three authors: the registry refuses over capacity
/// and its own `AgentStartError` is the line, the parser refuses a
/// malformed line and its own message is the line, and a started agent is
/// named by its id. A second wording of either refusal would be a second
/// source of truth about what the key just did -- the shape
/// `agent_view_steer_report` and `agent_view_rename` both avoid.
///
/// The capacity string is **derived** from `AgentStartError`, not spelled:
/// it is the registry's sentence and the view only carries it.
#[cfg(unix)]
#[test]
fn the_dispatch_report_carries_the_registrys_own_refusal_and_names_the_ceiling() {
    let (started, _) = recording_dispatcher(Ok(7));
    let report = agent_view_dispatch(&started, "measure the paste latency");
    assert_eq!(
        report.line, "started background agent #7",
        "a started dispatch is reported by the id the user can now act on"
    );
    assert!(report.changed, "and it changed the registry");

    for error in [
        subagent::AgentStartError {
            kind: subagent::AgentCapacityKind::Running,
            current: 8,
            limit: 8,
        },
        subagent::AgentStartError {
            kind: subagent::AgentCapacityKind::Records,
            current: 128,
            limit: 128,
        },
    ] {
        let (refusing, _) = recording_dispatcher(Err(error.to_string()));
        let report = agent_view_dispatch(&refusing, "one more agent");
        assert_eq!(
            report.line,
            error.to_string(),
            "the registry's own sentence is the line, and it names the \
                 ceiling that refused: {error}"
        );
        assert!(
            !report.changed,
            "and a refused dispatch changed nothing: {}",
            report.line
        );
        assert!(
            report.line.contains(&error.limit.to_string()),
            "A6 names both ceilings, so the limit is in the line the user \
                 reads: {}",
            report.line
        );
    }

    // A malformed line never reaches the dispatcher at all: the parse is
    // the view's and refusing it costs no gateway call.
    let (never, seen) = recording_dispatcher(Ok(9));
    let report = agent_view_dispatch(&never, "@");
    assert_eq!(
        report.line,
        kitsu_tui::agents_view::parse_dispatch("@")
            .expect_err("`@` with no profile is not a dispatch")
            .message(),
        "the parser's own message is the line"
    );
    assert!(!report.changed);
    assert!(
        seen.lock().expect("the recording dispatcher").is_empty(),
        "and nothing was dispatched"
    );
    // POSITIVE CONTROL: the same dispatcher really does get called for a
    // line the parser accepts, so the emptiness above is the refusal and
    // not a dispatcher that had stopped being called.
    let _ = agent_view_dispatch(&never, "@reviewer review the diff");
    assert_eq!(
        *seen.lock().expect("the recording dispatcher"),
        vec![("review the diff".to_string(), Some("reviewer".to_string()))],
        "POSITIVE CONTROL: an accepted line reaches the dispatcher, \
             profile and task apart"
    );
}
