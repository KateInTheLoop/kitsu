use std::path::Path;

use serde_json::json;

mod semantic_harness {
    use std::collections::HashSet;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use kitsu_tools::builtins::fs::{
        find_symbol_tool, grep_tool, outline_file_tool, read_file_tool, read_manifest_tool,
    };
    use kitsu_tools::{HandlerFn, Permissions};
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use thiserror::Error;

    const VECTOR_SCHEMA_V1: &str = "kitsu.semantic-evidence.v1";
    const ARTIFACT_SCHEMA_V1: &str = "kitsu.semantic-evidence.artifact.v1";
    const BASELINE_ADAPTER_V1: &str = "current-read-only-tools.v1";
    const CORPUS_PLACEHOLDER: &str = "$CORPUS";
    const MAX_VECTOR_INPUT_BYTES: usize = 64 * 1024;
    const MAX_VECTORS: usize = 32;
    const MAX_VECTOR_ID_BYTES: usize = 96;
    const MAX_PROMPT_BYTES: usize = 4 * 1024;
    const MAX_CALLS_PER_VECTOR: usize = 16;
    const MAX_TOTAL_CALLS: usize = 256;
    const MAX_SERIALIZED_ARGS_BYTES: usize = 4 * 1024;
    const MAX_RUBRIC_NEEDLES_PER_CLASS: usize = 64;
    const MAX_RUBRIC_NEEDLE_BYTES: usize = 512;
    const MAX_PROMPT_TOKENS: usize = 4 * 1024;
    const MAX_TOOL_BYTES: usize = 64 * 1024;
    const MAX_ROUNDS: usize = 8;
    const MAX_LATENCY_MILLIS: u64 = 10_000;

    #[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub(super) enum TaskKindV1 {
        Locate,
        Explain,
        Change,
    }

    #[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum MetricKindV1 {
        Success,
        RelevanceBasisPoints,
        Rounds,
        PromptTokens,
        ToolBytes,
        LatencyDeadline,
    }

    const REQUIRED_METRICS_V1: [MetricKindV1; 6] = [
        MetricKindV1::Success,
        MetricKindV1::RelevanceBasisPoints,
        MetricKindV1::Rounds,
        MetricKindV1::PromptTokens,
        MetricKindV1::ToolBytes,
        MetricKindV1::LatencyDeadline,
    ];

    #[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum ReadOnlyToolV1 {
        FindSymbol,
        OutlineFile,
        FsReadFile,
        RepoGrep,
        ReadManifest,
    }

    impl ReadOnlyToolV1 {
        fn handler(self, permissions: Arc<Permissions>) -> HandlerFn {
            match self {
                Self::FindSymbol => find_symbol_tool(permissions).1,
                Self::OutlineFile => outline_file_tool(permissions).1,
                Self::FsReadFile => read_file_tool(permissions).1,
                Self::RepoGrep => grep_tool(permissions).1,
                Self::ReadManifest => read_manifest_tool(permissions).1,
            }
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct BudgetV1 {
        prompt_tokens: usize,
        pub(super) tool_bytes: usize,
        rounds: usize,
        latency_millis: u64,
    }

    impl BudgetV1 {
        pub(super) fn is_bounded(&self) -> bool {
            (1..=MAX_PROMPT_TOKENS).contains(&self.prompt_tokens)
                && (1..=MAX_TOOL_BYTES).contains(&self.tool_bytes)
                && (1..=MAX_ROUNDS).contains(&self.rounds)
                && (1..=MAX_LATENCY_MILLIS).contains(&self.latency_millis)
        }
    }

    #[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub(super) struct EvidenceRubricV1 {
        required: Vec<String>,
        pub(super) forbidden: Vec<String>,
    }

    impl EvidenceRubricV1 {
        pub(super) fn new<R, F>(required: Vec<R>, forbidden: Vec<F>) -> Self
        where
            R: Into<String>,
            F: Into<String>,
        {
            Self {
                required: required.into_iter().map(Into::into).collect(),
                forbidden: forbidden.into_iter().map(Into::into).collect(),
            }
        }

        pub(super) fn evaluate(&self, evidence: &str) -> RubricResultV1 {
            let missing: Vec<String> = self
                .required
                .iter()
                .filter(|needle| !evidence.contains(needle.as_str()))
                .cloned()
                .collect();
            let forbidden: Vec<String> = self
                .forbidden
                .iter()
                .filter(|needle| evidence.contains(needle.as_str()))
                .cloned()
                .collect();
            let satisfied = self.required.len().saturating_sub(missing.len());
            let relevance_basis_points = if self.required.is_empty() {
                10_000
            } else {
                u16::try_from(satisfied.saturating_mul(10_000) / self.required.len())
                    .expect("basis-point score is bounded")
            };
            if missing.is_empty() && forbidden.is_empty() {
                RubricResultV1::passed(relevance_basis_points)
            } else {
                RubricResultV1::Failed {
                    relevance_basis_points,
                    missing,
                    forbidden,
                }
            }
        }

        pub(super) fn require_for_test(&mut self, needle: &str) {
            self.required.push(needle.to_string());
        }

        fn is_closed(&self) -> bool {
            !self.required.is_empty()
                && self
                    .required
                    .iter()
                    .chain(&self.forbidden)
                    .all(|needle| !needle.is_empty())
        }

        fn is_resource_bounded(&self) -> bool {
            self.required.len() <= MAX_RUBRIC_NEEDLES_PER_CLASS
                && self.forbidden.len() <= MAX_RUBRIC_NEEDLES_PER_CLASS
                && self
                    .required
                    .iter()
                    .chain(&self.forbidden)
                    .all(|needle| needle.len() <= MAX_RUBRIC_NEEDLE_BYTES)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum RubricResultV1 {
        Passed {
            relevance_basis_points: u16,
        },
        Failed {
            relevance_basis_points: u16,
            missing: Vec<String>,
            forbidden: Vec<String>,
        },
    }

    impl RubricResultV1 {
        pub(super) fn passed(relevance_basis_points: u16) -> Self {
            Self::Passed {
                relevance_basis_points,
            }
        }

        pub(super) fn failed(
            relevance_basis_points: u16,
            missing: Vec<&str>,
            forbidden: Vec<&str>,
        ) -> Self {
            Self::Failed {
                relevance_basis_points,
                missing: missing.into_iter().map(Into::into).collect(),
                forbidden: forbidden.into_iter().map(Into::into).collect(),
            }
        }

        fn relevance_basis_points(&self) -> u16 {
            match self {
                Self::Passed {
                    relevance_basis_points,
                }
                | Self::Failed {
                    relevance_basis_points,
                    ..
                } => *relevance_basis_points,
            }
        }

        fn has_missing(&self) -> bool {
            matches!(self, Self::Failed { missing, .. } if !missing.is_empty())
        }

        fn has_forbidden(&self) -> bool {
            matches!(self, Self::Failed { forbidden, .. } if !forbidden.is_empty())
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct ToolCallV1 {
        round: usize,
        tool: ReadOnlyToolV1,
        args: Value,
    }

    impl ToolCallV1 {
        pub(super) fn is_read_only(&self) -> bool {
            matches!(
                self.tool,
                ReadOnlyToolV1::FindSymbol
                    | ReadOnlyToolV1::OutlineFile
                    | ReadOnlyToolV1::FsReadFile
                    | ReadOnlyToolV1::RepoGrep
                    | ReadOnlyToolV1::ReadManifest
            )
        }

        fn is_closed(&self) -> bool {
            self.round > 0
                && self.args.is_object()
                && self
                    .args
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| {
                        path == CORPUS_PLACEHOLDER
                            || path.starts_with(&format!("{CORPUS_PLACEHOLDER}/"))
                    })
        }

        fn is_resource_bounded(&self) -> bool {
            serde_json::to_vec(&self.args)
                .is_ok_and(|encoded| encoded.len() <= MAX_SERIALIZED_ARGS_BYTES)
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct BenchmarkVectorV1 {
        id: String,
        task: TaskKindV1,
        prompt: String,
        budget: BudgetV1,
        calls: Vec<ToolCallV1>,
        rubric: EvidenceRubricV1,
    }

    impl BenchmarkVectorV1 {
        pub(super) fn task(&self) -> TaskKindV1 {
            self.task
        }

        pub(super) fn budget(&self) -> &BudgetV1 {
            &self.budget
        }

        pub(super) fn budget_mut_for_test(&mut self) -> &mut BudgetV1 {
            &mut self.budget
        }

        pub(super) fn calls(&self) -> &[ToolCallV1] {
            &self.calls
        }

        pub(super) fn rubric_mut_for_test(&mut self) -> &mut EvidenceRubricV1 {
            &mut self.rubric
        }

        fn is_closed(&self) -> bool {
            let id_is_closed = !self.id.is_empty()
                && self.id.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                });
            id_is_closed
                && !self.prompt.trim().is_empty()
                && self.budget.is_bounded()
                && !self.calls.is_empty()
                && self.calls.iter().all(|call| {
                    call.is_closed() && call.is_read_only() && call.round <= self.budget.rounds
                })
                && self.rubric.is_closed()
        }

        fn is_resource_bounded(&self) -> bool {
            self.id.len() <= MAX_VECTOR_ID_BYTES
                && self.prompt.len() <= MAX_PROMPT_BYTES
                && self.calls.len() <= MAX_CALLS_PER_VECTOR
                && self.calls.iter().all(ToolCallV1::is_resource_bounded)
                && self.rubric.is_resource_bounded()
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct BenchmarkSuiteV1 {
        schema: String,
        metric_contract: Vec<MetricKindV1>,
        vectors: Vec<BenchmarkVectorV1>,
    }

    impl BenchmarkSuiteV1 {
        pub(super) fn parse_and_validate(input: &str) -> Result<Self, ContractErrorV1> {
            if input.len() > MAX_VECTOR_INPUT_BYTES {
                return Err(ContractErrorV1::ResourceLimitExceeded);
            }
            let suite: Self =
                serde_json::from_str(input).map_err(|_| ContractErrorV1::MalformedVector)?;
            if suite.schema != VECTOR_SCHEMA_V1 {
                return Err(ContractErrorV1::UnsupportedSchema);
            }
            if suite.metric_contract != REQUIRED_METRICS_V1 {
                return Err(ContractErrorV1::MetricContractMismatch);
            }
            if suite.vectors.len() > MAX_VECTORS
                || suite
                    .vectors
                    .iter()
                    .map(|vector| vector.calls.len())
                    .sum::<usize>()
                    > MAX_TOTAL_CALLS
                || suite
                    .vectors
                    .iter()
                    .any(|vector| !vector.is_resource_bounded())
            {
                return Err(ContractErrorV1::ResourceLimitExceeded);
            }
            if suite.vectors.is_empty() || suite.vectors.iter().any(|vector| !vector.is_closed()) {
                return Err(ContractErrorV1::MalformedVector);
            }
            let mut ids = HashSet::with_capacity(suite.vectors.len());
            if suite
                .vectors
                .iter()
                .any(|vector| !ids.insert(vector.id.as_str()))
            {
                return Err(ContractErrorV1::DuplicateVectorId);
            }
            Ok(suite)
        }

        pub(super) fn vectors(&self) -> &[BenchmarkVectorV1] {
            &self.vectors
        }

        pub(super) fn vectors_mut_for_test(&mut self) -> &mut [BenchmarkVectorV1] {
            &mut self.vectors
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum ContractErrorV1 {
        UnsupportedSchema,
        MalformedVector,
        MetricContractMismatch,
        DuplicateVectorId,
        ResourceLimitExceeded,
    }

    #[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub(super) enum EvidenceScopeV1 {
        LocateReadOnlyEvidence,
    }

    #[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum LatencyClassV1 {
        WithinDeadline,
        TimedOut,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    struct MetricObservationV1 {
        rounds: usize,
        prompt_tokens: usize,
        tool_bytes: usize,
        latency_deadline: LatencyClassV1,
    }

    #[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub(super) enum FailureReasonV1 {
        PromptTokenBudgetExceeded,
        ToolByteBudgetExceeded,
        RoundBudgetExceeded,
        LatencyDeadlineExceeded,
        MissingEvidence,
        ForbiddenEvidence,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    #[serde(tag = "status", rename_all = "snake_case")]
    pub(super) enum EvaluationOutcomeV1 {
        Passed,
        Failed { reasons: Vec<FailureReasonV1> },
    }

    #[derive(Debug, Clone, Serialize, PartialEq)]
    pub(super) struct ArtifactCallV1 {
        round: usize,
        tool: ReadOnlyToolV1,
        args: Value,
        output: String,
    }

    impl ArtifactCallV1 {
        pub(super) fn is_read_only(&self) -> bool {
            matches!(
                self.tool,
                ReadOnlyToolV1::FindSymbol
                    | ReadOnlyToolV1::OutlineFile
                    | ReadOnlyToolV1::FsReadFile
                    | ReadOnlyToolV1::RepoGrep
                    | ReadOnlyToolV1::ReadManifest
            )
        }

        pub(super) fn output_bytes(&self) -> usize {
            self.output.len()
        }
    }

    #[derive(Debug, Clone, Serialize, PartialEq)]
    pub(super) struct BenchmarkArtifactV1 {
        schema: &'static str,
        vector_id: String,
        task: TaskKindV1,
        evaluation_scope: EvidenceScopeV1,
        adapter: &'static str,
        evidence_outcome: EvaluationOutcomeV1,
        relevance_basis_points: u16,
        metrics: MetricObservationV1,
        calls: Vec<ArtifactCallV1>,
    }

    impl BenchmarkArtifactV1 {
        pub(super) fn task(&self) -> TaskKindV1 {
            self.task
        }

        pub(super) fn outcome(&self) -> &EvaluationOutcomeV1 {
            &self.evidence_outcome
        }

        pub(super) fn scope(&self) -> EvidenceScopeV1 {
            self.evaluation_scope
        }

        pub(super) fn calls(&self) -> &[ArtifactCallV1] {
            &self.calls
        }

        pub(super) fn observed_tool_bytes(&self) -> usize {
            self.metrics.tool_bytes
        }
    }

    #[derive(Debug, Error, PartialEq, Eq)]
    pub(super) enum ArtifactErrorV1 {
        #[error("benchmark evidence contains an unnormalized absolute path")]
        UnnormalizedAbsolutePath,
    }

    #[derive(Debug, Error)]
    pub(super) enum HarnessErrorV1 {
        #[error("invalid corpus root")]
        InvalidCorpusRoot,
        #[error("artifact normalization failed: {0}")]
        Artifact(#[from] ArtifactErrorV1),
        #[error("read-only tool failed: {0}")]
        Tool(String),
        #[error("artifact serialization failed: {0}")]
        Serialization(#[from] serde_json::Error),
    }

    pub(super) fn estimated_tokens_v1(bytes: &[u8]) -> usize {
        bytes.len().div_ceil(4)
    }

    pub(super) fn normalize_evidence_v1(
        corpus_root: &Path,
        evidence: &str,
    ) -> Result<String, ArtifactErrorV1> {
        let root = corpus_root
            .to_str()
            .ok_or(ArtifactErrorV1::UnnormalizedAbsolutePath)?;
        let normalized = evidence.replace(root, CORPUS_PLACEHOLDER);
        if normalized.contains("/tmp/")
            || normalized.contains("/private/tmp/")
            || normalized.contains("\\Temp\\")
        {
            return Err(ArtifactErrorV1::UnnormalizedAbsolutePath);
        }
        Ok(normalized)
    }

    fn replace_corpus_placeholder(value: &mut Value, corpus_root: &str) {
        match value {
            Value::String(text) => {
                *text = text.replace(CORPUS_PLACEHOLDER, corpus_root);
            }
            Value::Array(values) => {
                for value in values {
                    replace_corpus_placeholder(value, corpus_root);
                }
            }
            Value::Object(values) => {
                for value in values.values_mut() {
                    replace_corpus_placeholder(value, corpus_root);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    pub(super) async fn run_current_tools_baseline_records_v1(
        suite: &BenchmarkSuiteV1,
        corpus_root: &Path,
    ) -> Result<Vec<BenchmarkArtifactV1>, HarnessErrorV1> {
        let corpus_root_text = corpus_root
            .to_str()
            .ok_or(HarnessErrorV1::InvalidCorpusRoot)?;
        let permissions = Arc::new(
            Permissions::for_roots(&[corpus_root_text.to_string()], &[])
                .map_err(|error| HarnessErrorV1::Tool(error.to_string()))?,
        );
        let mut artifacts = Vec::with_capacity(suite.vectors.len());

        for vector in &suite.vectors {
            let prompt_tokens = estimated_tokens_v1(vector.prompt.as_bytes());
            let mut rounds = 0_usize;
            let mut latency_deadline = LatencyClassV1::WithinDeadline;
            let mut calls = Vec::with_capacity(vector.calls.len());
            let mut evidence = String::new();
            let mut tool_bytes = 0_usize;
            let mut retained_tool_bytes = 0_usize;

            if prompt_tokens <= vector.budget.prompt_tokens {
                for call in &vector.calls {
                    rounds = rounds.max(call.round);
                    if rounds > vector.budget.rounds {
                        break;
                    }
                    let mut args = call.args.clone();
                    replace_corpus_placeholder(&mut args, corpus_root_text);
                    let handler = call.tool.handler(permissions.clone());
                    let deadline = Duration::from_millis(vector.budget.latency_millis);
                    let raw_output = match tokio::time::timeout(
                        deadline,
                        handler(serde_json::to_string(&args)?),
                    )
                    .await
                    {
                        Ok(Ok(output)) => output,
                        Ok(Err(error)) => return Err(HarnessErrorV1::Tool(error.to_string())),
                        Err(_) => {
                            latency_deadline = LatencyClassV1::TimedOut;
                            "(tool call exceeded latency deadline)".to_string()
                        }
                    };
                    let output = normalize_evidence_v1(corpus_root, &raw_output)?;
                    tool_bytes = tool_bytes.saturating_add(output.len());
                    let remaining = vector.budget.tool_bytes.saturating_sub(retained_tool_bytes);
                    let mut retained_output = output;
                    if retained_output.len() > remaining {
                        let mut end = remaining;
                        while !retained_output.is_char_boundary(end) {
                            end = end.saturating_sub(1);
                        }
                        retained_output.truncate(end);
                    }
                    retained_tool_bytes = retained_tool_bytes.saturating_add(retained_output.len());
                    if !evidence.is_empty() {
                        evidence.push('\n');
                    }
                    evidence.push_str(&retained_output);
                    calls.push(ArtifactCallV1 {
                        round: call.round,
                        tool: call.tool,
                        args: call.args.clone(),
                        output: retained_output,
                    });
                    if tool_bytes > vector.budget.tool_bytes
                        || latency_deadline == LatencyClassV1::TimedOut
                    {
                        break;
                    }
                }
            }

            let rubric = vector.rubric.evaluate(&evidence);
            let mut budget_reasons = Vec::new();
            if prompt_tokens > vector.budget.prompt_tokens {
                budget_reasons.push(FailureReasonV1::PromptTokenBudgetExceeded);
            }
            if tool_bytes > vector.budget.tool_bytes {
                budget_reasons.push(FailureReasonV1::ToolByteBudgetExceeded);
            }
            if rounds > vector.budget.rounds {
                budget_reasons.push(FailureReasonV1::RoundBudgetExceeded);
            }
            if latency_deadline == LatencyClassV1::TimedOut {
                budget_reasons.push(FailureReasonV1::LatencyDeadlineExceeded);
            }
            let evidence_outcome = if budget_reasons.is_empty() {
                let mut rubric_reasons = Vec::new();
                if rubric.has_missing() {
                    rubric_reasons.push(FailureReasonV1::MissingEvidence);
                }
                if rubric.has_forbidden() {
                    rubric_reasons.push(FailureReasonV1::ForbiddenEvidence);
                }
                if rubric_reasons.is_empty() {
                    EvaluationOutcomeV1::Passed
                } else {
                    EvaluationOutcomeV1::Failed {
                        reasons: rubric_reasons,
                    }
                }
            } else {
                EvaluationOutcomeV1::Failed {
                    reasons: budget_reasons,
                }
            };

            artifacts.push(BenchmarkArtifactV1 {
                schema: ARTIFACT_SCHEMA_V1,
                vector_id: vector.id.clone(),
                task: vector.task,
                evaluation_scope: EvidenceScopeV1::LocateReadOnlyEvidence,
                adapter: BASELINE_ADAPTER_V1,
                evidence_outcome,
                relevance_basis_points: rubric.relevance_basis_points(),
                metrics: MetricObservationV1 {
                    rounds,
                    prompt_tokens,
                    tool_bytes,
                    latency_deadline,
                },
                calls,
            });
        }
        Ok(artifacts)
    }

    pub(super) async fn run_current_tools_baseline_v1(
        suite: &BenchmarkSuiteV1,
        corpus_root: &Path,
    ) -> Result<Vec<u8>, HarnessErrorV1> {
        let artifacts = run_current_tools_baseline_records_v1(suite, corpus_root).await?;
        let mut jsonl = Vec::new();
        for artifact in artifacts {
            serde_json::to_writer(&mut jsonl, &artifact)?;
            jsonl.push(b'\n');
        }
        Ok(jsonl)
    }
}

use semantic_harness::*;

const VECTOR_JSON: &str = include_str!("fixtures/semantic_evidence/vectors/baseline-v1.json");
const EXPECTED_JSONL: &str = include_str!("fixtures/semantic_evidence/vectors/baseline-v1.jsonl");

const CORPUS: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        include_str!("fixtures/semantic_evidence/corpus/Cargo.toml"),
    ),
    (
        "package.json",
        include_str!("fixtures/semantic_evidence/corpus/package.json"),
    ),
    (
        "src/slug.rs",
        include_str!("fixtures/semantic_evidence/corpus/src/slug.rs"),
    ),
    (
        "src/lib.rs",
        include_str!("fixtures/semantic_evidence/corpus/src/lib.rs"),
    ),
    (
        "service/invoice.py",
        include_str!("fixtures/semantic_evidence/corpus/service/invoice.py"),
    ),
    (
        "service/report.py",
        include_str!("fixtures/semantic_evidence/corpus/service/report.py"),
    ),
    (
        "web/retry.ts",
        include_str!("fixtures/semantic_evidence/corpus/web/retry.ts"),
    ),
    (
        "generated/retry_snapshot.ts",
        include_str!("fixtures/semantic_evidence/corpus/generated/retry_snapshot.ts"),
    ),
];

fn materialize_corpus(root: &Path) {
    for (relative, content) in CORPUS {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("fixture has a parent")).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

#[test]
fn vectors_are_closed_versioned_unique_and_budgeted() {
    let suite = BenchmarkSuiteV1::parse_and_validate(VECTOR_JSON).unwrap();
    assert_eq!(suite.vectors().len(), 3);
    assert_eq!(suite.vectors()[0].task(), TaskKindV1::Locate);
    assert_eq!(suite.vectors()[1].task(), TaskKindV1::Explain);
    assert_eq!(suite.vectors()[2].task(), TaskKindV1::Change);
    assert!(suite
        .vectors()
        .iter()
        .all(|vector| vector.budget().is_bounded()));
    assert!(suite
        .vectors()
        .iter()
        .flat_map(|vector| vector.calls())
        .all(|call| call.is_read_only()));
}

#[test]
fn unknown_schema_task_metric_and_duplicate_ids_fail_closed() {
    let mut unknown_schema: serde_json::Value = serde_json::from_str(VECTOR_JSON).unwrap();
    unknown_schema["schema"] = json!("kitsu.semantic-evidence.v2");
    assert_eq!(
        BenchmarkSuiteV1::parse_and_validate(&unknown_schema.to_string()).unwrap_err(),
        ContractErrorV1::UnsupportedSchema
    );

    let mut unknown_task: serde_json::Value = serde_json::from_str(VECTOR_JSON).unwrap();
    unknown_task["vectors"][0]["task"] = json!("summarize");
    assert_eq!(
        BenchmarkSuiteV1::parse_and_validate(&unknown_task.to_string()).unwrap_err(),
        ContractErrorV1::MalformedVector
    );

    let mut unknown_metric: serde_json::Value = serde_json::from_str(VECTOR_JSON).unwrap();
    unknown_metric["metric_contract"][0] = json!("wall_clock_guess");
    assert_eq!(
        BenchmarkSuiteV1::parse_and_validate(&unknown_metric.to_string()).unwrap_err(),
        ContractErrorV1::MalformedVector
    );

    let mut duplicate: serde_json::Value = serde_json::from_str(VECTOR_JSON).unwrap();
    duplicate["vectors"][1]["id"] = duplicate["vectors"][0]["id"].clone();
    assert_eq!(
        BenchmarkSuiteV1::parse_and_validate(&duplicate.to_string()).unwrap_err(),
        ContractErrorV1::DuplicateVectorId
    );
}

#[test]
fn parser_rejects_every_oversized_resource_dimension() {
    assert_eq!(
        BenchmarkSuiteV1::parse_and_validate(&" ".repeat(64 * 1024 + 1)).unwrap_err(),
        ContractErrorV1::ResourceLimitExceeded
    );

    let base: serde_json::Value = serde_json::from_str(VECTOR_JSON).unwrap();
    let mut oversized = Vec::new();

    let mut too_many_vectors = base.clone();
    too_many_vectors["vectors"] = serde_json::Value::Array(vec![base["vectors"][0].clone(); 33]);
    oversized.push(too_many_vectors);

    let mut long_id = base.clone();
    long_id["vectors"][0]["id"] = json!("x".repeat(97));
    oversized.push(long_id);

    let mut long_prompt = base.clone();
    long_prompt["vectors"][0]["prompt"] = json!("x".repeat(4 * 1024 + 1));
    oversized.push(long_prompt);

    let mut too_many_calls = base.clone();
    too_many_calls["vectors"][0]["calls"] =
        serde_json::Value::Array(vec![base["vectors"][0]["calls"][0].clone(); 17]);
    oversized.push(too_many_calls);

    let mut large_args = base.clone();
    large_args["vectors"][0]["calls"][0]["args"]["padding"] = json!("x".repeat(4 * 1024));
    oversized.push(large_args);

    let mut too_many_needles = base.clone();
    too_many_needles["vectors"][0]["rubric"]["required"] = serde_json::Value::Array(
        (0..65)
            .map(|index| json!(format!("needle-{index}")))
            .collect(),
    );
    oversized.push(too_many_needles);

    let mut long_needle = base;
    long_needle["vectors"][0]["rubric"]["forbidden"][0] = json!("x".repeat(513));
    oversized.push(long_needle);

    for candidate in oversized {
        assert_eq!(
            BenchmarkSuiteV1::parse_and_validate(&candidate.to_string()).unwrap_err(),
            ContractErrorV1::ResourceLimitExceeded
        );
    }
}

#[test]
fn token_estimate_and_relevance_rubric_are_exact() {
    assert_eq!(estimated_tokens_v1(b""), 0);
    assert_eq!(estimated_tokens_v1(b"a"), 1);
    assert_eq!(estimated_tokens_v1(b"abcd"), 1);
    assert_eq!(estimated_tokens_v1(b"abcde"), 2);

    let rubric = EvidenceRubricV1::new(vec!["alpha", "beta"], vec!["secret"]);
    assert_eq!(
        rubric.evaluate("alpha and beta"),
        RubricResultV1::passed(10_000)
    );
    assert_eq!(
        rubric.evaluate("alpha only"),
        RubricResultV1::failed(5_000, vec!["beta"], vec![])
    );
    assert_eq!(
        rubric.evaluate("alpha beta secret"),
        RubricResultV1::failed(10_000, vec![], vec!["secret"])
    );
}

#[test]
fn random_absolute_paths_are_normalized_or_refused() {
    assert_eq!(
        normalize_evidence_v1(
            Path::new("/tmp/random-corpus-a"),
            "/tmp/random-corpus-a/src/lib.rs:4"
        )
        .unwrap(),
        "$CORPUS/src/lib.rs:4"
    );
    assert_eq!(
        normalize_evidence_v1(
            Path::new("/tmp/random-corpus-a"),
            "/tmp/random-corpus-b/src/lib.rs:4"
        )
        .unwrap_err(),
        ArtifactErrorV1::UnnormalizedAbsolutePath
    );
}

#[tokio::test]
async fn current_read_only_tools_emit_the_exact_reproducible_baseline() {
    let suite = BenchmarkSuiteV1::parse_and_validate(VECTOR_JSON).unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    materialize_corpus(first.path());
    materialize_corpus(second.path());

    let first_artifact = run_current_tools_baseline_v1(&suite, first.path())
        .await
        .unwrap();
    let second_artifact = run_current_tools_baseline_v1(&suite, second.path())
        .await
        .unwrap();

    assert_eq!(first_artifact, second_artifact);
    let first_artifact = String::from_utf8(first_artifact).unwrap();
    assert_eq!(first_artifact, EXPECTED_JSONL);
    assert!(!first_artifact.contains("/tmp/"));
}

#[tokio::test]
async fn byte_overflow_and_forbidden_evidence_fail_without_claiming_success() {
    let mut suite = BenchmarkSuiteV1::parse_and_validate(VECTOR_JSON).unwrap();
    suite.vectors_mut_for_test()[2]
        .budget_mut_for_test()
        .tool_bytes = 1;
    let mutated_rubric = suite.vectors_mut_for_test()[1].rubric_mut_for_test();
    mutated_rubric.require_for_test("evidence-that-is-not-present");
    mutated_rubric.forbidden.push("ROUND_HALF_UP".to_string());

    let corpus = tempfile::tempdir().unwrap();
    materialize_corpus(corpus.path());
    let artifacts = run_current_tools_baseline_records_v1(&suite, corpus.path())
        .await
        .unwrap();

    assert_eq!(
        artifacts[2].outcome(),
        &EvaluationOutcomeV1::Failed {
            reasons: vec![FailureReasonV1::ToolByteBudgetExceeded]
        }
    );
    assert_eq!(
        artifacts[1].outcome(),
        &EvaluationOutcomeV1::Failed {
            reasons: vec![
                FailureReasonV1::MissingEvidence,
                FailureReasonV1::ForbiddenEvidence,
            ]
        }
    );
    assert_eq!(artifacts[2].task(), TaskKindV1::Change);
    assert_eq!(
        artifacts[2].scope(),
        EvidenceScopeV1::LocateReadOnlyEvidence
    );
    assert_eq!(artifacts[2].calls().len(), 1);
    assert!(artifacts[2].calls().iter().all(|call| call.is_read_only()));
    assert!(artifacts[2].observed_tool_bytes() > 1);
    assert!(
        artifacts[2]
            .calls()
            .iter()
            .map(ArtifactCallV1::output_bytes)
            .sum::<usize>()
            <= 1
    );
}
