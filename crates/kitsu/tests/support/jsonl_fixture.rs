//! Narrow `kitsu.chat.v1` evidence inspector for real CLI carrier tests.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlOutcomeClass {
    Succeeded,
    PartiallySucceeded,
    UserFault,
    ProviderFault,
    ToolFault,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlAssistantDisposition {
    Committed,
    Discarded,
    Absent,
}

#[derive(Debug, PartialEq, Eq)]
pub struct JsonlFacts {
    pub run_id: String,
    pub class: JsonlOutcomeClass,
    pub assistant: JsonlAssistantDisposition,
    pub terminal_count: usize,
    pub exit_code: i32,
    pub provider_requests: usize,
    pub provider_attempts: usize,
    pub tool_started: usize,
    pub tool_finished: usize,
}

/// Inspect the existing JSONL projection without treating it as the production
/// owner of semantic outcome. Missing/unstable identity, schema drift, or an
/// absent summary is a fixture error rather than inferred state.
pub fn inspect_jsonl(events: &[serde_json::Value], exit_code: i32) -> Result<JsonlFacts, String> {
    let first = events
        .first()
        .ok_or_else(|| "JSONL carrier emitted no records".to_string())?;
    let run_id = first["run_id"]
        .as_str()
        .filter(|run_id| !run_id.is_empty())
        .ok_or_else(|| "first JSONL record has no non-empty run_id".to_string())?;
    for (index, event) in events.iter().enumerate() {
        if event["schema"] != "kitsu.chat.v1" {
            return Err(format!("record {index} has the wrong schema"));
        }
        if event["run_id"] != run_id {
            return Err(format!("record {index} changed or stripped run_id"));
        }
    }

    let summaries = events
        .iter()
        .filter(|event| event["type"] == "final_summary")
        .collect::<Vec<_>>();
    if summaries.len() != 1 {
        return Err(format!(
            "expected one final_summary, observed {}",
            summaries.len()
        ));
    }
    let summary = summaries[0];
    if events.last() != Some(summary) {
        return Err("records appeared after final_summary".to_string());
    }
    let class = match summary["status"].as_str() {
        Some("ok") => JsonlOutcomeClass::Succeeded,
        Some("partial") => JsonlOutcomeClass::PartiallySucceeded,
        Some("cancelled") => JsonlOutcomeClass::Cancelled,
        Some("error") if summary["provider_failures"].as_u64().unwrap_or(0) > 0 => {
            JsonlOutcomeClass::ProviderFault
        }
        Some("error")
            if summary["tool_failures"].as_u64().unwrap_or(0) > 0
                || summary["policy_failures"].as_u64().unwrap_or(0) > 0 =>
        {
            JsonlOutcomeClass::ToolFault
        }
        Some("error") => JsonlOutcomeClass::UserFault,
        Some(status) => return Err(format!("unknown final_summary status '{status}'")),
        None => return Err("final_summary status is missing".to_string()),
    };

    let dispositions = events
        .iter()
        .filter(|event| event["type"] == "assistant_disposition")
        .collect::<Vec<_>>();
    let assistant = match dispositions
        .last()
        .and_then(|event| event["status"].as_str())
    {
        Some("committed" | "committed_partial") => JsonlAssistantDisposition::Committed,
        Some("discarded") => JsonlAssistantDisposition::Discarded,
        Some(status) => return Err(format!("unknown assistant disposition '{status}'")),
        None => JsonlAssistantDisposition::Absent,
    };

    Ok(JsonlFacts {
        run_id: run_id.to_string(),
        class,
        assistant,
        terminal_count: summaries.len(),
        exit_code,
        provider_requests: events
            .iter()
            .filter(|event| event["type"] == "provider_request")
            .count(),
        provider_attempts: events
            .iter()
            .filter(|event| event["type"] == "provider_attempt")
            .count(),
        tool_started: events
            .iter()
            .filter(|event| event["type"] == "tool_started")
            .count(),
        tool_finished: events
            .iter()
            .filter(|event| event["type"] == "tool_finished")
            .count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: &str, run_id: &str, payload: serde_json::Value) -> serde_json::Value {
        let mut object = payload.as_object().cloned().unwrap_or_default();
        object.insert("schema".to_string(), serde_json::json!("kitsu.chat.v1"));
        object.insert("type".to_string(), serde_json::json!(kind));
        object.insert("run_id".to_string(), serde_json::json!(run_id));
        serde_json::Value::Object(object)
    }

    #[test]
    fn inspector_rejects_stripped_or_cross_run_identity() {
        let stripped = vec![
            record("error", "run-a", serde_json::json!({})),
            record(
                "final_summary",
                "run-b",
                serde_json::json!({"status": "error", "user_failures": 1}),
            ),
        ];

        assert!(inspect_jsonl(&stripped, 1)
            .expect_err("cross-run summary must fail")
            .contains("changed or stripped run_id"));
    }

    #[test]
    fn inspector_keeps_accepted_partial_distinct_from_success() {
        let events = vec![record(
            "final_summary",
            "run-partial",
            serde_json::json!({"status": "partial"}),
        )];

        let facts = inspect_jsonl(&events, 0).unwrap();
        assert_eq!(facts.class, JsonlOutcomeClass::PartiallySucceeded);
        assert_eq!(facts.assistant, JsonlAssistantDisposition::Absent);
        assert_eq!(facts.terminal_count, 1);
    }

    #[test]
    fn inspector_rejects_duplicate_or_nonterminal_summary() {
        let summary = || {
            record(
                "final_summary",
                "run-terminal",
                serde_json::json!({"status": "ok"}),
            )
        };
        let duplicate = vec![summary(), summary()];
        assert!(inspect_jsonl(&duplicate, 0)
            .expect_err("duplicate summary must fail")
            .contains("expected one final_summary"));

        let followed = vec![
            summary(),
            record("late_work", "run-terminal", serde_json::json!({})),
        ];
        assert_eq!(
            inspect_jsonl(&followed, 0).unwrap_err(),
            "records appeared after final_summary"
        );
    }
}
