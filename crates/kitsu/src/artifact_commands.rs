//! Command-artifact inspection and reclamation.

use kitsu_core::ArtifactStoragePolicy;

use crate::artifacts;
use crate::chat::state::SessionInfo;
use crate::command_artifact;
use crate::presentation::shorten_home;
use crate::presentation::terminal_inline;
use crate::setup::artifact_policy_receipt;
use crate::setup::ArtifactPolicySource;

/// On-disk totals for generations this process does not own. `/artifacts`
/// reporting "0 retained" in a fresh session while gigabytes sit on disk is the
/// lie this closes.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CrossRunArtifacts {
    pub(crate) generations: usize,
    pub(crate) bytes: u64,
}

pub(crate) fn cross_run_artifacts(session: &SessionInfo) -> CrossRunArtifacts {
    let own = session.offload_dir.borrow().clone();
    let Ok(generations) = artifacts::scan(&session.state_dir) else {
        return CrossRunArtifacts::default();
    };
    let mut report = CrossRunArtifacts::default();
    for generation in generations {
        if own.as_deref().is_some_and(|own| own == generation.path) {
            continue;
        }
        report.generations += 1;
        report.bytes += generation.bytes;
    }
    report
}

pub(crate) fn command_artifact_payload(
    sink: &command_artifact::CommandArtifactSink,
    cross_run: CrossRunArtifacts,
) -> serde_json::Value {
    let quota = sink.quota();
    let usage = sink.usage();
    let artifacts = sink
        .list()
        .into_iter()
        .map(|artifact| {
            serde_json::json!({
                "id": artifact.id,
                "path": artifact.reference,
                "artifact_bytes": artifact.artifact_bytes,
                "captured_bytes": artifact.captured_bytes,
                "truncated": artifact.truncated,
                "limit_bytes": artifact.limit_bytes,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "kind": "command_artifacts",
        "quota": {
            "max_count": quota.max_count,
            "max_total_bytes": quota.max_total_bytes,
            "overflow": format!("{:?}", quota.overflow).to_lowercase(),
        },
        "usage": {
            "retained_count": usage.retained_count,
            "retained_bytes": usage.retained_bytes,
            "in_flight_count": usage.in_flight_count,
            "reserved_bytes": usage.reserved_bytes,
            "available_count": usage.available_count(),
            "available_bytes": usage.available_bytes(),
        },
        "artifacts": artifacts,
        "cross_run": {
            "generation_count": cross_run.generations,
            "bytes": cross_run.bytes,
        },
    })
}

pub(crate) fn format_command_artifacts(
    sink: &command_artifact::CommandArtifactSink,
    cross_run: CrossRunArtifacts,
) -> String {
    let usage = sink.usage();
    let mut output = format!(
        "command artifacts: {} retained / {} in flight | {} retained bytes / {} reserved bytes | {} slots and {} bytes available",
        usage.retained_count,
        usage.in_flight_count,
        usage.retained_bytes,
        usage.reserved_bytes,
        usage.available_count(),
        usage.available_bytes(),
    );
    for artifact in sink.list() {
        output.push_str(&format!(
            "\n  #{}  {} ({} bytes; {} captured{}; cap {})",
            artifact.id,
            shorten_home(&artifact.reference),
            artifact.artifact_bytes,
            artifact.captured_bytes,
            if artifact.truncated {
                ", truncated"
            } else {
                ""
            },
            artifact.limit_bytes,
        ));
    }
    if usage.retained_count == 0 {
        output.push_str("\n  none");
    } else {
        output.push_str("\n  prune with /artifacts prune <id|all>");
    }
    if cross_run.generations > 0 {
        output.push_str(&format!(
            "\n  other runs:   {} generation(s), {} retained on disk - `kitsu artifacts list` (host-only)",
            cross_run.generations,
            artifacts::human_bytes(cross_run.bytes),
        ));
    }
    output
}

pub(crate) fn handle_command_artifacts(
    sink: Option<&command_artifact::CommandArtifactSink>,
    cross_run: CrossRunArtifacts,
    policy: ArtifactStoragePolicy,
    policy_source: ArtifactPolicySource,
    arg: &str,
) -> Result<serde_json::Value, ()> {
    if policy == ArtifactStoragePolicy::InlineOnly {
        let action = arg.split_whitespace().next().unwrap_or("list");
        if !matches!(action, "" | "list") || arg.split_whitespace().nth(1).is_some() {
            eprintln!(
                "automatic artifact storage is disabled; existing artifacts are unchanged — use `kitsu artifacts` for explicit host-only management"
            );
            return Err(());
        }
        eprintln!(
            "command artifacts: automatic capture disabled ({}); bounded previews only; existing artifacts unchanged\n  other runs: {} generation(s), {} retained on disk — inspect with `kitsu artifacts list`",
            policy_source.as_str(),
            cross_run.generations,
            artifacts::human_bytes(cross_run.bytes),
        );
        return Ok(serde_json::json!({
            "kind": "command_artifacts",
            "artifacts": [],
            "cross_run": {
                "generation_count": cross_run.generations,
                "bytes": cross_run.bytes,
            },
            "artifact_policy": artifact_policy_receipt(policy, policy_source, false),
        }));
    }
    let Some(sink) = sink else {
        eprintln!("no artifacts — enable command tool to capture artifact generations");
        return Err(());
    };
    let mut parts = arg.split_whitespace();
    match parts.next().unwrap_or("list") {
        "list" if parts.next().is_none() => {
            eprintln!("{}", format_command_artifacts(sink, cross_run));
            Ok(command_artifact_payload(sink, cross_run))
        }
        "prune" => {
            let Some(target) = parts.next() else {
                eprintln!("usage: /artifacts prune <id|all>");
                return Err(());
            };
            if parts.next().is_some() {
                eprintln!("usage: /artifacts prune <id|all>");
                return Err(());
            }
            let removed = if target == "all" {
                sink.prune_all()
            } else {
                let Ok(id) = target.parse::<u64>() else {
                    eprintln!("usage: /artifacts prune <id|all>");
                    return Err(());
                };
                sink.prune(id)
                    .map(|artifact| artifact.into_iter().collect())
            };
            match removed {
                Ok(removed) => {
                    eprintln!("~ pruned {} command artifact(s)", removed.len());
                    let mut payload = command_artifact_payload(sink, cross_run);
                    payload["action"] = serde_json::Value::String("prune".to_string());
                    payload["removed"] =
                        serde_json::json!(removed.iter().map(|a| a.id).collect::<Vec<_>>());
                    Ok(payload)
                }
                Err(error) => {
                    eprintln!(
                        "cannot prune command artifacts: {}",
                        terminal_inline(&error.to_string())
                    );
                    Err(())
                }
            }
        }
        _ => {
            eprintln!("usage: /artifacts [list|prune <id|all>]");
            Err(())
        }
    }
}
