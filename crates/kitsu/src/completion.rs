//! Host-owned final-answer sinks and required-write postconditions.

use crate::cli::Cli;
use kitsu_tools::Permissions;

use crate::cli_error::CliError;
use crate::cli_runtime::emit_jsonl;
use crate::presentation::terminal_inline;
use crate::session;
use crate::skills;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequiredWriteFingerprint {
    pub(crate) bytes: usize,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequiredWriteContract {
    pub(crate) requested: String,
    pub(crate) resolved: std::path::PathBuf,
    pub(crate) initial: Option<RequiredWriteFingerprint>,
    pub(crate) expected_sha256: Option<String>,
}

pub(crate) struct RequiredWriteEvaluation {
    pub(crate) satisfied: bool,
    pub(crate) results: Vec<serde_json::Value>,
    pub(crate) failure: Option<CliError>,
}

pub(crate) fn required_write_fingerprint(
    path: &std::path::Path,
    perms: &Permissions,
) -> Result<Option<RequiredWriteFingerprint>, String> {
    let Some(bytes) =
        kitsu_tools::builtins::fs::read_verified_write_target(path, perms.max_write_bytes())
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    perms.check_write_size(bytes.len()).map_err(|error| {
        format!(
            "required-write target '{}' cannot be verified: {error}",
            path.display()
        )
    })?;
    Ok(Some(RequiredWriteFingerprint {
        bytes: bytes.len(),
        sha256: skills::content_sha256_hex(&bytes),
    }))
}

pub(crate) fn required_write_fingerprint_bytes(bytes: &[u8]) -> RequiredWriteFingerprint {
    RequiredWriteFingerprint {
        bytes: bytes.len(),
        sha256: skills::content_sha256_hex(bytes),
    }
}

pub(crate) async fn prepare_required_writes(
    requested_paths: &[String],
    requested_sha256: &[String],
    perms: &Permissions,
) -> Result<Vec<RequiredWriteContract>, CliError> {
    let mut requested_contracts = requested_paths
        .iter()
        .map(|requested| (requested.clone(), None, "--require-write"))
        .collect::<Vec<_>>();
    for encoded in requested_sha256 {
        let (requested, sha256) = parse_required_write_sha256(encoded)?;
        requested_contracts.push((requested, Some(sha256), "--require-write-sha256"));
    }

    let mut contracts: Vec<RequiredWriteContract> = Vec::with_capacity(requested_contracts.len());
    for (requested, expected_sha256, option) in requested_contracts {
        let resolved = perms
            .check_write(std::path::Path::new(&requested))
            .await
            .map_err(|error| {
                CliError::user(format!(
                    "{option} target '{}' has no active model filesystem write authority: {error}; add --allow-write or --allow-read-write for its parent",
                    requested
                ))
            })?;
        if let Some(existing) = contracts
            .iter_mut()
            .find(|contract| contract.resolved == resolved)
        {
            if let Some(expected_sha256) = expected_sha256 {
                match existing.expected_sha256.as_deref() {
                    Some(existing_sha256) if existing_sha256 != expected_sha256 => {
                        return Err(CliError::user(format!(
                            "conflicting --require-write-sha256 expectations for '{}': {existing_sha256} and {expected_sha256}",
                            resolved.display()
                        )));
                    }
                    Some(_) => {}
                    None => {
                        if existing
                            .initial
                            .as_ref()
                            .is_some_and(|initial| initial.sha256 == expected_sha256)
                        {
                            return Err(CliError::user(format!(
                                "--require-write-sha256 target '{}' already has the required SHA-256 {expected_sha256} at launch; the required-write contract also requires a final content change, so this expectation cannot be satisfied",
                                requested
                            )));
                        }
                        existing.expected_sha256 = Some(expected_sha256);
                    }
                }
            }
            continue;
        }
        let initial = required_write_fingerprint(&resolved, perms).map_err(CliError::user)?;
        if initial.is_some() {
            perms.check_read(&resolved).await.map_err(|error| {
                CliError::user(format!(
                    "{option} target '{}' already exists but has no active model filesystem read authority: {error}; existing targets need --allow-read-write for their parent (or matching --allow-path in addition to write authority), while --allow-write alone is only valid for creating a new file",
                    requested
                ))
            })?;
        }
        if let (Some(initial), Some(expected_sha256)) = (&initial, &expected_sha256) {
            if initial.sha256 == *expected_sha256 {
                return Err(CliError::user(format!(
                    "--require-write-sha256 target '{}' already has the required SHA-256 {expected_sha256} at launch; the required-write contract also requires a final content change, so this expectation cannot be satisfied",
                    requested
                )));
            }
        }
        contracts.push(RequiredWriteContract {
            requested,
            resolved,
            initial,
            expected_sha256,
        });
    }
    Ok(contracts)
}

pub(crate) fn parse_required_write_sha256(encoded: &str) -> Result<(String, String), CliError> {
    let Some((path, sha256)) = encoded.rsplit_once('=') else {
        return Err(CliError::user(
            "--require-write-sha256 expects PATH=SHA256 (PATH may contain '=')".to_string(),
        ));
    };
    if path.is_empty() {
        return Err(CliError::user(
            "--require-write-sha256 expects a non-empty PATH before the final '='".to_string(),
        ));
    }
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CliError::user(
            "--require-write-sha256 expects exactly 64 hexadecimal SHA-256 characters after the final '='"
                .to_string(),
        ));
    }
    Ok((path.to_string(), sha256.to_ascii_lowercase()))
}

pub(crate) fn required_write_targets(contracts: &[RequiredWriteContract]) -> Vec<String> {
    contracts
        .iter()
        .map(|contract| contract.resolved.to_string_lossy().into_owned())
        .collect()
}

pub(crate) fn required_write_expectations(
    contracts: &[RequiredWriteContract],
) -> Vec<serde_json::Value> {
    contracts
        .iter()
        .filter_map(|contract| {
            contract.expected_sha256.as_ref().map(|sha256| {
                serde_json::json!({
                    "path": contract.resolved.to_string_lossy(),
                    "sha256": sha256,
                })
            })
        })
        .collect()
}

pub(crate) fn required_write_system_instruction(
    contracts: &[RequiredWriteContract],
) -> Option<String> {
    (!contracts.is_empty()).then(|| {
        let targets = serde_json::to_string(&required_write_targets(contracts))
            .expect("required-write path list serializes");
        let expectations = required_write_expectations(contracts);
        let exact = if expectations.is_empty() {
            String::new()
        } else {
            format!(
                " Exact raw-byte SHA-256 expectations are host-verified for this JSON array: {}.",
                serde_json::to_string(&expectations)
                    .expect("required-write SHA-256 expectations serialize")
            )
        };
        format!(
            "\n\n---\nHost-enforced completion postcondition: before your final response, use the filesystem tools to make a real content change to every path in this JSON array: {targets}.{exact} The paths already have explicit write authority; this instruction grants no additional authority. A final response is not successful unless Kitsu verifies both the model-tool write and a final state different from process launch, plus every declared exact SHA-256 expectation."
        )
    })
}

pub(crate) fn emit_required_write_armed(
    cli: &Cli,
    contracts: &[RequiredWriteContract],
) -> Result<(), CliError> {
    if contracts.is_empty() {
        return Ok(());
    }
    let targets = required_write_targets(contracts);
    let expectations = required_write_expectations(contracts);
    if cli.jsonl {
        emit_jsonl(
            "completion_postcondition",
            serde_json::json!({
                "phase": "armed",
                "condition": "required_write",
                "status": "pending",
                "targets": targets,
                "expectations": expectations,
                "results": [],
                "verification": "verified_model_fs_write_and_final_state_change",
                "model_disclosed": true,
                "write_authority_granted": false,
            }),
        )?;
    } else {
        eprintln!(
            "~ completion postcondition armed: {} required model-written file(s)",
            targets.len()
        );
    }
    Ok(())
}

/// Return a bounded, host-owned repair instruction only when a model write is
/// already journaled and its descriptor-pinned current bytes satisfy every
/// ordinary mutation invariant except an exact SHA-256 expectation. Missing
/// writes and unverifiable state remain the terminal postcondition's job; this
/// gate must never broaden authority or turn a run-level contract into an
/// eager per-turn failure.
pub(crate) fn required_write_structured_repair(
    contracts: &[RequiredWriteContract],
    journal: &kitsu_tools::builtins::fs::WriteJournal,
) -> Option<kitsu_core::StructuredFinalizationRepair> {
    let mut targets = Vec::new();
    for contract in contracts {
        let Some(expected_sha256) = contract.expected_sha256.as_ref() else {
            continue;
        };
        let Ok(Some((verification, Some(final_value)))) = journal
            .verify_written_path_with_current_content(&contract.resolved, |current| {
                current.map(required_write_fingerprint_bytes)
            })
        else {
            continue;
        };
        let final_state_changed = match &contract.initial {
            None => true,
            Some(initial) => initial != &final_value,
        };
        if verification.net_content_changed
            && verification.current_matches_latest_write
            && final_state_changed
            && final_value.sha256 != *expected_sha256
        {
            targets.push(kitsu_core::StructuredFinalizationRepairTarget {
                path: contract.resolved.to_string_lossy().into_owned(),
                expected_sha256: expected_sha256.clone(),
                actual_sha256: final_value.sha256,
            });
        }
    }
    if targets.is_empty() {
        return None;
    }
    targets.sort_by(|left, right| left.path.cmp(&right.path));
    let disclosed_targets =
        serde_json::to_string(&targets).expect("structured-finalization repair targets serialize");
    Some(kitsu_core::StructuredFinalizationRepair {
        cause: "required_write_sha256_mismatch".to_string(),
        instruction: format!(
            "Kitsu verified that the latest filesystem write does not yet match the required raw-byte SHA-256. Keep working with the available filesystem tools before answering. Correct these targets exactly: {disclosed_targets}. Do not give a final answer until every digest matches. This instruction grants no additional authority."
        ),
        targets,
    })
}

pub(crate) fn evaluate_required_writes(
    contracts: &[RequiredWriteContract],
    perms: &Permissions,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
) -> RequiredWriteEvaluation {
    if contracts.is_empty() {
        return RequiredWriteEvaluation {
            satisfied: true,
            results: Vec::new(),
            failure: None,
        };
    }
    let mut results = Vec::with_capacity(contracts.len());
    let mut failures = Vec::new();
    for contract in contracts {
        let journal_verification = journal
            .verify_written_path_with_current_content(&contract.resolved, |current| {
                current.map(required_write_fingerprint_bytes)
            });
        let (
            journal_recorded,
            journal_net_change,
            current_matches,
            journal_error,
            final_value,
            final_error,
        ) = match journal_verification {
            Ok(Some((verification, final_value))) => (
                true,
                verification.net_content_changed,
                verification.current_matches_latest_write,
                None,
                final_value,
                None,
            ),
            Ok(None) => match required_write_fingerprint(&contract.resolved, perms) {
                Ok(value) => (false, false, false, None, value, None),
                Err(error) => (false, false, false, None, None, Some(error)),
            },
            Err(error) => (false, false, false, Some(error.to_string()), None, None),
        };
        let final_state_changed = match (&contract.initial, &final_value) {
            (None, Some(_)) => true,
            (Some(initial), Some(final_value)) => initial != final_value,
            _ => false,
        };
        let expected_sha256_matches = contract.expected_sha256.as_ref().map(|expected| {
            final_value
                .as_ref()
                .is_some_and(|final_value| final_value.sha256 == *expected)
        });
        let satisfied = journal_recorded
            && journal_net_change
            && current_matches
            && final_state_changed
            && final_error.is_none()
            && expected_sha256_matches != Some(false);
        let reason = if satisfied {
            None
        } else if let Some(error) = journal_error.as_deref() {
            Some(error.to_string())
        } else if let Some(error) = final_error.as_deref() {
            Some(error.to_string())
        } else if !journal_recorded {
            Some("no verified model filesystem write was recorded".to_string())
        } else if !journal_net_change || !final_state_changed {
            Some("final file contents do not differ from process launch".to_string())
        } else if !current_matches {
            Some("current file no longer matches the latest verified model write".to_string())
        } else if expected_sha256_matches == Some(false) {
            Some(format!(
                "final raw-byte SHA-256 {} does not match required {}",
                final_value
                    .as_ref()
                    .map_or("missing", |value| value.sha256.as_str()),
                contract
                    .expected_sha256
                    .as_deref()
                    .expect("mismatch requires an exact expectation")
            ))
        } else {
            Some("required write could not be verified".to_string())
        };
        if let Some(reason) = reason.as_ref() {
            failures.push(format!("{} ({reason})", contract.requested));
        }
        results.push(serde_json::json!({
            "path": contract.resolved.to_string_lossy(),
            "requested_path": contract.requested,
            "satisfied": satisfied,
            "journal_recorded": journal_recorded,
            "journal_net_content_changed": journal_net_change,
            "current_matches_latest_write": current_matches,
            "final_state_changed": final_state_changed,
            "initial_bytes": contract.initial.as_ref().map(|value| value.bytes),
            "initial_sha256": contract.initial.as_ref().map(|value| value.sha256.as_str()),
            "final_bytes": final_value.as_ref().map(|value| value.bytes),
            "final_sha256": final_value.as_ref().map(|value| value.sha256.as_str()),
            "expected_sha256": contract.expected_sha256.as_deref(),
            "expected_sha256_matches": expected_sha256_matches,
            "reason": reason,
        }));
    }
    let satisfied = failures.is_empty();
    let failure = (!satisfied).then(|| {
        CliError::user(format!(
            "completion postcondition failed: required write contract was not satisfied for {}",
            failures.join(", ")
        ))
    });
    RequiredWriteEvaluation {
        satisfied,
        results,
        failure,
    }
}

pub(crate) fn emit_required_write_evaluation(
    cli: &Cli,
    contracts: &[RequiredWriteContract],
    evaluation: &RequiredWriteEvaluation,
) -> Result<(), CliError> {
    if contracts.is_empty() {
        return Ok(());
    }
    if cli.jsonl {
        let expectations = required_write_expectations(contracts);
        emit_jsonl(
            "completion_postcondition",
            serde_json::json!({
                "phase": "evaluated",
                "condition": "required_write",
                "status": if evaluation.satisfied { "satisfied" } else { "failed" },
                "targets": required_write_targets(contracts),
                "expectations": expectations,
                "results": evaluation.results,
                "verification": "verified_model_fs_write_and_final_state_change",
                "model_disclosed": true,
                "write_authority_granted": false,
            }),
        )?;
    } else if evaluation.satisfied {
        eprintln!(
            "~ completion postcondition satisfied: {} required model-written file(s) verified",
            contracts.len()
        );
    } else if let Some(error) = evaluation.failure.as_ref() {
        eprintln!("~ {}", terminal_inline(&error.to_string()));
    }
    Ok(())
}

pub(crate) fn emit_required_write_error(cli: &Cli, error: &CliError) -> Result<(), CliError> {
    if cli.jsonl {
        emit_jsonl(
            "error",
            serde_json::json!({
                "kind": "postcondition",
                "condition": "required_write",
                "message": error.to_string(),
                "exit_code": error.exit_code(),
            }),
        )?;
    }
    Ok(())
}

pub(crate) fn reject_host_file_command_authority(
    path: &std::path::Path,
    option: &str,
    purpose: &str,
    writable_roots: &[std::path::PathBuf],
    readonly_runtime_roots: &[std::path::PathBuf],
) -> Result<(), CliError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        CliError::user(format!(
            "{option} {purpose} '{}' has no filename",
            path.display()
        ))
    })?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        CliError::user(format!(
            "cannot resolve {option} parent '{}': {error}",
            parent.display()
        ))
    })?;
    let canonical_target = canonical_parent.join(name);

    for (roots, label) in [
        (writable_roots, "writable command root"),
        (readonly_runtime_roots, "read-only command runtime root"),
    ] {
        if let Some(root) = roots.iter().find(|root| canonical_target.starts_with(root)) {
            return Err(CliError::user(format!(
                "{option} {purpose} '{}' is visible inside {label} '{}'; choose a separate host-owned path outside command authority",
                path.display(),
                root.display()
            )));
        }
    }

    #[cfg(unix)]
    if let Ok(metadata) = std::fs::symlink_metadata(&canonical_target) {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() > 1 {
            return Err(CliError::user(format!(
                "{option} {purpose} '{}' has multiple hard links while command tools are enabled; choose a single-link host-owned file",
                path.display()
            )));
        }
    }
    Ok(())
}

pub(crate) fn reject_host_output_command_authority(
    cli: &Cli,
    writable_roots: &[std::path::PathBuf],
    readonly_runtime_roots: &[std::path::PathBuf],
) -> Result<(), CliError> {
    if let Some(destination) = cli.output_last_message.as_deref() {
        reject_host_file_command_authority(
            destination,
            "--output-last-message",
            "destination",
            writable_roots,
            readonly_runtime_roots,
        )?;
    }
    if let Some(schema) = cli.output_schema.as_deref() {
        reject_host_file_command_authority(
            schema,
            "--output-schema",
            "file",
            writable_roots,
            readonly_runtime_roots,
        )?;
    }
    Ok(())
}

pub(crate) fn write_output_last_message(
    cli: &Cli,
    turn_id: &str,
    text: &str,
) -> Result<(), CliError> {
    let Some(destination) = cli.output_last_message.as_deref() else {
        return Ok(());
    };
    if let Err(error) = session::atomic_write_output_file(destination, text.as_bytes()) {
        let installed = error.installed;
        return Err(CliError::user(format!(
            "failed to write --output-last-message '{}': {error}{}",
            destination.display(),
            if installed {
                "; the exact message was installed, but durability could not be confirmed"
            } else {
                "; the destination was left unchanged"
            }
        )));
    }
    if cli.jsonl {
        emit_jsonl(
            "output_last_message",
            serde_json::json!({
                "turn_id": turn_id,
                "destination": destination
                    .to_str()
                    .expect("JSONL output destination validated as UTF-8"),
                "bytes": text.len(),
                "sha256": skills::content_sha256_hex(text.as_bytes()),
                "atomic": true,
                "assistant_committed": true,
                "content_included": false,
                "model_write_authority": false,
            }),
        )?;
    } else {
        eprintln!(
            "~ saved last committed assistant message to {} ({} bytes, atomic)",
            terminal_inline(&destination.display().to_string()),
            text.len(),
        );
    }
    Ok(())
}

pub(crate) fn emit_output_last_message_error(
    cli: &Cli,
    turn_id: &str,
    error: &CliError,
) -> Result<(), CliError> {
    if cli.jsonl {
        emit_jsonl(
            "error",
            serde_json::json!({
                "turn_id": turn_id,
                "kind": "output_last_message",
                "message": error.to_string(),
                "exit_code": error.exit_code(),
            }),
        )?;
    }
    Ok(())
}
