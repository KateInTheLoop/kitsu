use std::path::Path;

use serde_json::json;

mod semantic_harness {
    use std::collections::{BTreeMap, BTreeSet, HashSet};
    use std::path::{Component, Path};
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
    const AUTHORITY_VECTOR_SCHEMA_V1: &str = "kitsu.semantic-evidence.authority-staleness.v1";
    const ARTIFACT_SCHEMA_V1: &str = "kitsu.semantic-evidence.artifact.v1";
    const BASELINE_ADAPTER_V1: &str = "current-read-only-tools.v1";
    const SELECTOR_ADAPTER_V1: &str = "bounded-structural-selector.v1";
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
    const MAX_SELECTOR_TERMS: usize = 12;
    const MAX_SELECTOR_FILES: usize = 4;
    const MAX_SELECTOR_HITS: usize = 64;
    const MAX_SELECTOR_OUTPUT_BYTES: usize = 4 * 1024;

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
        SemanticSelector,
    }

    impl ReadOnlyToolV1 {
        fn handler(self, permissions: Arc<Permissions>) -> HandlerFn {
            match self {
                Self::FindSymbol => find_symbol_tool(permissions).1,
                Self::OutlineFile => outline_file_tool(permissions).1,
                Self::FsReadFile => read_file_tool(permissions).1,
                Self::RepoGrep => grep_tool(permissions).1,
                Self::ReadManifest => read_manifest_tool(permissions).1,
                Self::SemanticSelector => {
                    unreachable!("the test-only selector is never registered as a tool")
                }
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

        pub(super) fn prompt(&self) -> &str {
            &self.prompt
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

    #[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub(super) enum HostileCandidateKindV1 {
        DeniedRoot,
        Private,
        Symlink,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct HostileCandidateV1 {
        pub(super) id: String,
        pub(super) kind: HostileCandidateKindV1,
        pub(super) fixture: String,
        pub(super) mount: String,
        pub(super) canary: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct SourceChangeV1 {
        pub(super) path: String,
        pub(super) replacement_fixture: String,
        pub(super) before: String,
        pub(super) after: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct AuthorityStalenessVectorV1 {
        schema: String,
        pub(super) prompt: String,
        pub(super) hostile_candidates: Vec<HostileCandidateV1>,
        pub(super) source_change: SourceChangeV1,
    }

    impl AuthorityStalenessVectorV1 {
        pub(super) fn parse_and_validate(input: &str) -> Result<Self, ContractErrorV1> {
            if input.len() > MAX_VECTOR_INPUT_BYTES {
                return Err(ContractErrorV1::ResourceLimitExceeded);
            }
            let vector: Self =
                serde_json::from_str(input).map_err(|_| ContractErrorV1::MalformedVector)?;
            if vector.schema != AUTHORITY_VECTOR_SCHEMA_V1 {
                return Err(ContractErrorV1::UnsupportedSchema);
            }
            let expected_candidates = [
                (
                    "denied_root",
                    HostileCandidateKindV1::DeniedRoot,
                    "hostile/denied-root.ts",
                    "denied/denied-root.ts",
                    "DENIED_ROOT_CANARY",
                ),
                (
                    "private",
                    HostileCandidateKindV1::Private,
                    "hostile/private.ts",
                    "private/private.ts",
                    "PRIVATE_CANARY",
                ),
                (
                    "symlink",
                    HostileCandidateKindV1::Symlink,
                    "hostile/symlink-target.ts",
                    "web/symlink-retry.ts",
                    "SYMLINK_CANARY",
                ),
            ];
            let valid_path = |path: &str| valid_relative_observation_path_v1(path);
            let candidates_are_closed = vector.hostile_candidates.len()
                == expected_candidates.len()
                && vector
                    .hostile_candidates
                    .iter()
                    .zip(expected_candidates)
                    .all(|(candidate, expected)| {
                        candidate.id == expected.0
                            && candidate.kind == expected.1
                            && candidate.fixture == expected.2
                            && candidate.mount == expected.3
                            && candidate.canary == expected.4
                            && candidate.id.bytes().all(|byte| {
                                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                            })
                            && valid_path(&candidate.fixture)
                            && valid_path(&candidate.mount)
                            && candidate.fixture.len() <= MAX_RUBRIC_NEEDLE_BYTES
                            && candidate.mount.len() <= MAX_RUBRIC_NEEDLE_BYTES
                            && candidate.canary.bytes().all(|byte| {
                                byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                            })
                            && candidate.canary.len() <= MAX_RUBRIC_NEEDLE_BYTES
                    });
            let mut ids = HashSet::new();
            let unique_ids = vector
                .hostile_candidates
                .iter()
                .all(|candidate| ids.insert(candidate.id.as_str()));
            if vector.prompt.trim().is_empty()
                || vector.prompt.len() > MAX_PROMPT_BYTES
                || !candidates_are_closed
                || !unique_ids
                || !valid_path(&vector.source_change.path)
                || !valid_path(&vector.source_change.replacement_fixture)
                || vector.source_change.before.is_empty()
                || vector.source_change.after.is_empty()
                || vector.source_change.before == vector.source_change.after
                || vector.source_change.before.len() > MAX_RUBRIC_NEEDLE_BYTES
                || vector.source_change.after.len() > MAX_RUBRIC_NEEDLE_BYTES
                || vector.source_change.path != "web/retry.ts"
                || vector.source_change.replacement_fixture != "hostile/authorized-after.ts"
                || vector.source_change.before != "MAX_RETRY_ATTEMPTS = 3"
                || vector.source_change.after != "MAX_RETRY_ATTEMPTS = 5"
            {
                return Err(ContractErrorV1::MalformedVector);
            }
            Ok(vector)
        }
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
                    | ReadOnlyToolV1::SemanticSelector
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
        #[error("structural selector failed: {0}")]
        Selector(String),
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum StructuralRoleV1 {
        Definition,
        Reference,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct StructuralHitV1 {
        role: StructuralRoleV1,
        path: String,
        line: usize,
    }

    #[derive(Debug, Default)]
    struct RankedFileV1 {
        score: usize,
        definitions: usize,
        references: usize,
    }

    fn identifier_words_v1(input: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current = String::new();
        for character in input.chars() {
            if character.is_ascii_alphanumeric() || character == '_' {
                current.push(character);
            } else if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        }
        if !current.is_empty() {
            words.push(current);
        }
        words
    }

    fn is_selector_stop_word_v1(word: &str) -> bool {
        matches!(
            word,
            "a" | "an"
                | "and"
                | "are"
                | "authoritative"
                | "called"
                | "change"
                | "definition"
                | "defined"
                | "evidence"
                | "explain"
                | "for"
                | "from"
                | "how"
                | "identify"
                | "in"
                | "is"
                | "it"
                | "limit"
                | "locate"
                | "mirrors"
                | "needed"
                | "of"
                | "production"
                | "the"
                | "to"
                | "touching"
                | "use"
                | "where"
                | "without"
        )
    }

    fn prompt_terms_v1(prompt: &str) -> Vec<String> {
        let mut seen = BTreeSet::new();
        identifier_words_v1(prompt)
            .into_iter()
            .filter(|word| word.len() >= 3)
            .filter(|word| {
                let lower = word.to_ascii_lowercase();
                !is_selector_stop_word_v1(&lower) && seen.insert(lower)
            })
            .take(MAX_SELECTOR_TERMS)
            .collect()
    }

    fn valid_relative_observation_path_v1(path: &str) -> bool {
        !path.is_empty()
            && !Path::new(path).is_absolute()
            && Path::new(path)
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
    }

    fn parse_structural_hits_v1(raw: &str) -> Result<Vec<StructuralHitV1>, HarnessErrorV1> {
        let mut role = None;
        let mut hits = Vec::new();
        for line in raw.lines() {
            match line {
                "definitions:" => {
                    role = Some(StructuralRoleV1::Definition);
                    continue;
                }
                "references:" => {
                    role = Some(StructuralRoleV1::Reference);
                    continue;
                }
                _ => {}
            }
            let Some(hit_role) = role else {
                continue;
            };
            let Some(candidate) = line.strip_prefix("  ") else {
                continue;
            };
            if candidate.starts_with('[') {
                continue;
            }
            let mut fields = candidate.splitn(3, ':');
            let (Some(path), Some(line), Some(_text)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Ok(line) = line.parse::<usize>() else {
                continue;
            };
            if !valid_relative_observation_path_v1(path) {
                return Err(HarnessErrorV1::Selector(
                    "tool observation contained an unsafe path".to_string(),
                ));
            }
            hits.push(StructuralHitV1 {
                role: hit_role,
                path: path.to_string(),
                line,
            });
            if hits.len() > MAX_SELECTOR_HITS {
                return Err(HarnessErrorV1::Selector(
                    "symbol graph exceeded its hit bound".to_string(),
                ));
            }
        }
        Ok(hits)
    }

    fn path_is_generated_or_build_output_v1(path: &str) -> bool {
        Path::new(path).components().any(|component| {
            let Component::Normal(name) = component else {
                return true;
            };
            let lower = name.to_string_lossy().to_ascii_lowercase();
            matches!(
                lower.as_str(),
                "generated" | "target" | "node_modules" | "dist" | "build" | "vendor"
            ) || lower.contains(".generated.")
        })
    }

    fn rank_structural_files_v1(hits: &[StructuralHitV1]) -> Vec<String> {
        let mut graph = BTreeMap::<String, RankedFileV1>::new();
        for hit in hits {
            if path_is_generated_or_build_output_v1(&hit.path) {
                continue;
            }
            let node = graph.entry(hit.path.clone()).or_default();
            match hit.role {
                StructuralRoleV1::Definition => {
                    node.definitions += 1;
                    node.score += 100;
                }
                StructuralRoleV1::Reference => {
                    node.references += 1;
                    node.score += 60;
                }
            }
        }
        for node in graph.values_mut() {
            if node.definitions > 0 && node.references > 0 {
                node.score += 20;
            }
        }
        let mut ranked: Vec<(String, usize)> = graph
            .into_iter()
            .map(|(path, node)| (path, node.score))
            .collect();
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        ranked
            .into_iter()
            .take(MAX_SELECTOR_FILES)
            .map(|(path, _)| path)
            .collect()
    }

    async fn call_selector_observer_v1(
        tool: ReadOnlyToolV1,
        permissions: Arc<Permissions>,
        args: Value,
    ) -> Result<String, HarnessErrorV1> {
        tool.handler(permissions)(serde_json::to_string(&args)?)
            .await
            .map_err(|error| HarnessErrorV1::Tool(error.to_string()))
    }

    fn choose_identifier_from_search_v1(raw: &str, term: &str) -> Option<String> {
        let term = term.to_ascii_lowercase();
        let mut counts = BTreeMap::<String, usize>::new();
        for word in identifier_words_v1(raw) {
            let lower = word.to_ascii_lowercase();
            if lower != term
                && lower.contains(&term)
                && word.len() <= 128
                && word
                    .bytes()
                    .any(|byte| byte == b'_' || byte.is_ascii_uppercase())
            {
                *counts.entry(word).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by(|left, right| {
                let score = |(word, count): &(String, usize)| {
                    count * 100
                        + word.bytes().filter(|byte| *byte == b'_').count() * 20
                        + word.bytes().filter(u8::is_ascii_uppercase).count()
                };
                score(left)
                    .cmp(&score(right))
                    .then_with(|| right.0.cmp(&left.0))
            })
            .map(|(word, _)| word)
    }

    async fn discover_symbol_v1(
        prompt: &str,
        corpus_root: &str,
        permissions: Arc<Permissions>,
    ) -> Result<(String, Vec<StructuralHitV1>), HarnessErrorV1> {
        let terms = prompt_terms_v1(prompt);
        for candidate in terms.iter().filter(|term| term.contains('_')) {
            let raw = call_selector_observer_v1(
                ReadOnlyToolV1::FindSymbol,
                permissions.clone(),
                serde_json::json!({
                    "symbol": candidate,
                    "path": corpus_root,
                    "max_results": MAX_SELECTOR_HITS,
                }),
            )
            .await?;
            let hits = parse_structural_hits_v1(&raw)?;
            if !hits.is_empty() {
                return Ok((candidate.clone(), hits));
            }
        }

        for term in terms.iter().filter(|term| !term.contains('_')) {
            let raw = call_selector_observer_v1(
                ReadOnlyToolV1::RepoGrep,
                permissions.clone(),
                serde_json::json!({
                    "pattern": format!("(?i){}", regex::escape(term)),
                    "path": corpus_root,
                    "max_count": MAX_SELECTOR_HITS,
                }),
            )
            .await?;
            let Some(candidate) = choose_identifier_from_search_v1(&raw, term) else {
                continue;
            };
            let symbol_raw = call_selector_observer_v1(
                ReadOnlyToolV1::FindSymbol,
                permissions.clone(),
                serde_json::json!({
                    "symbol": candidate,
                    "path": corpus_root,
                    "max_results": MAX_SELECTOR_HITS,
                }),
            )
            .await?;
            let hits = parse_structural_hits_v1(&symbol_raw)?;
            if !hits.is_empty() {
                return Ok((candidate, hits));
            }
        }
        Err(HarnessErrorV1::Selector(
            "no prompt-derived symbol was observed".to_string(),
        ))
    }

    fn selector_manifest_name_v1(task: TaskKindV1, selected: &[String]) -> Option<&'static str> {
        if task != TaskKindV1::Change {
            return None;
        }
        if selected
            .iter()
            .any(|path| path.ends_with(".ts") || path.ends_with(".js"))
        {
            Some("package.json")
        } else if selected.iter().any(|path| path.ends_with(".rs")) {
            Some("Cargo.toml")
        } else if selected.iter().any(|path| path.ends_with(".py")) {
            Some("pyproject.toml")
        } else {
            None
        }
    }

    pub(super) async fn select_structural_evidence_v1(
        task: TaskKindV1,
        prompt: &str,
        corpus_root: &Path,
    ) -> Result<String, HarnessErrorV1> {
        if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            return Err(HarnessErrorV1::Selector(
                "prompt is empty or exceeds the selector bound".to_string(),
            ));
        }
        let corpus_root_text = corpus_root
            .to_str()
            .ok_or(HarnessErrorV1::InvalidCorpusRoot)?;
        let permissions = Arc::new(
            Permissions::for_roots(&[corpus_root_text.to_string()], &[])
                .map_err(|error| HarnessErrorV1::Tool(error.to_string()))?,
        );
        select_structural_evidence_with_policy_hook_v1(
            task,
            prompt,
            corpus_root,
            permissions,
            |_| Ok(()),
        )
        .await
    }

    pub(super) async fn select_structural_evidence_with_policy_hook_v1<F>(
        task: TaskKindV1,
        prompt: &str,
        corpus_root: &Path,
        permissions: Arc<Permissions>,
        after_discovery: F,
    ) -> Result<String, HarnessErrorV1>
    where
        F: FnOnce(&Permissions) -> Result<(), HarnessErrorV1>,
    {
        if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            return Err(HarnessErrorV1::Selector(
                "prompt is empty or exceeds the selector bound".to_string(),
            ));
        }
        let corpus_root_text = corpus_root
            .to_str()
            .ok_or(HarnessErrorV1::InvalidCorpusRoot)?;
        let (symbol, hits) =
            discover_symbol_v1(prompt, corpus_root_text, permissions.clone()).await?;
        let selected = rank_structural_files_v1(&hits);
        if selected.is_empty() {
            return Err(HarnessErrorV1::Selector(
                "all observed symbol files were excluded".to_string(),
            ));
        }
        let selected_set: BTreeSet<&str> = selected.iter().map(String::as_str).collect();
        let selected_hits: Vec<&StructuralHitV1> = hits
            .iter()
            .filter(|hit| selected_set.contains(hit.path.as_str()))
            .collect();
        // The ranked graph is candidate metadata, never evidence. Tests place
        // source and authority changes here; every output line below must come
        // from a new permission-gated read under the resulting state.
        after_discovery(&permissions)?;
        let definitions = selected_hits
            .iter()
            .filter(|hit| hit.role == StructuralRoleV1::Definition)
            .count();
        let references = selected_hits
            .iter()
            .filter(|hit| hit.role == StructuralRoleV1::Reference)
            .count();
        let mut evidence_lines = BTreeMap::<(String, usize), String>::new();
        for path in &selected {
            let path_hits: Vec<&StructuralHitV1> = selected_hits
                .iter()
                .copied()
                .filter(|hit| hit.path == *path)
                .collect();
            let absolute = corpus_root.join(path);
            let absolute = absolute.to_str().ok_or(HarnessErrorV1::InvalidCorpusRoot)?;
            let mut requested_windows: Vec<(usize, usize)> = path_hits
                .iter()
                .map(|hit| match hit.role {
                    StructuralRoleV1::Definition => (
                        hit.line.saturating_sub(3).max(1),
                        hit.line.saturating_add(3),
                    ),
                    StructuralRoleV1::Reference => (hit.line, hit.line),
                })
                .collect();
            requested_windows.sort_unstable();
            let mut windows = Vec::<(usize, usize)>::new();
            for (start, end) in requested_windows {
                if let Some((_, previous_end)) = windows.last_mut() {
                    if start <= previous_end.saturating_add(1) {
                        *previous_end = (*previous_end).max(end);
                        continue;
                    }
                }
                windows.push((start, end));
            }

            let mut current_lines = BTreeMap::<usize, String>::new();
            for (start, end) in windows {
                let snippet = call_selector_observer_v1(
                    ReadOnlyToolV1::FsReadFile,
                    permissions.clone(),
                    serde_json::json!({
                        "path": absolute,
                        "start_line": start,
                        "end_line": end,
                    }),
                )
                .await?;
                for line in snippet.lines() {
                    let Some((number, text)) = line.split_once('\t') else {
                        continue;
                    };
                    let Ok(line_number) = number.parse::<usize>() else {
                        continue;
                    };
                    if (start..=end).contains(&line_number) {
                        let text = text.trim();
                        if !text.is_empty() {
                            current_lines
                                .entry(line_number)
                                .or_insert_with(|| text.to_string());
                        }
                    }
                }
            }
            let every_hit_is_current = path_hits.iter().all(|hit| {
                current_lines.get(&hit.line).is_some_and(|text| {
                    identifier_words_v1(text).iter().any(|word| word == &symbol)
                })
            });
            if !every_hit_is_current {
                return Err(HarnessErrorV1::Selector(format!(
                    "selected structural evidence changed before reading '{path}'"
                )));
            }
            evidence_lines.extend(
                current_lines
                    .into_iter()
                    .map(|(line, text)| ((path.clone(), line), text)),
            );
        }

        let mut output = format!(
            "structural_selector> \"{symbol}\" — {definitions} definition(s), {references} reference(s)\n"
        );
        for ((path, line), text) in evidence_lines {
            output.push_str(&format!("{path}:{line}: {text}\n"));
        }
        if let Some(manifest) = selector_manifest_name_v1(task, &selected) {
            let path = corpus_root.join(manifest);
            let path = path.to_str().ok_or(HarnessErrorV1::InvalidCorpusRoot)?;
            let manifest_output = call_selector_observer_v1(
                ReadOnlyToolV1::ReadManifest,
                permissions,
                serde_json::json!({"path": path}),
            )
            .await?;
            if let Some(header) = manifest_output.lines().next() {
                output.push_str(header);
                output.push('\n');
            }
        }
        let output = normalize_evidence_v1(corpus_root, &output)?;
        if output.len() > MAX_SELECTOR_OUTPUT_BYTES {
            return Err(HarnessErrorV1::Selector(
                "rendered evidence exceeded the selector output bound".to_string(),
            ));
        }
        Ok(output)
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

    pub(super) async fn run_structural_selector_records_v1(
        suite: &BenchmarkSuiteV1,
        corpus_root: &Path,
    ) -> Result<Vec<BenchmarkArtifactV1>, HarnessErrorV1> {
        let mut artifacts = Vec::with_capacity(suite.vectors.len());
        for vector in &suite.vectors {
            let prompt_tokens = estimated_tokens_v1(vector.prompt.as_bytes());
            let mut rounds = 0;
            let mut latency_deadline = LatencyClassV1::WithinDeadline;
            let mut calls = Vec::new();
            let mut tool_bytes = 0;
            let mut evidence = String::new();
            if prompt_tokens <= vector.budget.prompt_tokens {
                rounds = 1;
                let deadline = Duration::from_millis(vector.budget.latency_millis);
                let raw_output = match tokio::time::timeout(
                    deadline,
                    select_structural_evidence_v1(vector.task, &vector.prompt, corpus_root),
                )
                .await
                {
                    Ok(result) => result?,
                    Err(_) => {
                        latency_deadline = LatencyClassV1::TimedOut;
                        "(structural selector exceeded latency deadline)".to_string()
                    }
                };
                tool_bytes = raw_output.len();
                let mut retained_output = raw_output;
                if retained_output.len() > vector.budget.tool_bytes {
                    let mut end = vector.budget.tool_bytes;
                    while !retained_output.is_char_boundary(end) {
                        end = end.saturating_sub(1);
                    }
                    retained_output.truncate(end);
                }
                evidence.push_str(&retained_output);
                calls.push(ArtifactCallV1 {
                    round: 1,
                    tool: ReadOnlyToolV1::SemanticSelector,
                    args: serde_json::json!({
                        "path": CORPUS_PLACEHOLDER,
                        "prompt": vector.prompt,
                        "task": vector.task,
                    }),
                    output: retained_output,
                });
            }

            let rubric = vector.rubric.evaluate(&evidence);
            let mut reasons = Vec::new();
            if prompt_tokens > vector.budget.prompt_tokens {
                reasons.push(FailureReasonV1::PromptTokenBudgetExceeded);
            }
            if tool_bytes > vector.budget.tool_bytes {
                reasons.push(FailureReasonV1::ToolByteBudgetExceeded);
            }
            if rounds > vector.budget.rounds {
                reasons.push(FailureReasonV1::RoundBudgetExceeded);
            }
            if latency_deadline == LatencyClassV1::TimedOut {
                reasons.push(FailureReasonV1::LatencyDeadlineExceeded);
            }
            if reasons.is_empty() {
                if rubric.has_missing() {
                    reasons.push(FailureReasonV1::MissingEvidence);
                }
                if rubric.has_forbidden() {
                    reasons.push(FailureReasonV1::ForbiddenEvidence);
                }
            }
            let evidence_outcome = if reasons.is_empty() {
                EvaluationOutcomeV1::Passed
            } else {
                EvaluationOutcomeV1::Failed { reasons }
            };
            artifacts.push(BenchmarkArtifactV1 {
                schema: ARTIFACT_SCHEMA_V1,
                vector_id: vector.id.clone(),
                task: vector.task,
                evaluation_scope: EvidenceScopeV1::LocateReadOnlyEvidence,
                adapter: SELECTOR_ADAPTER_V1,
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

    pub(super) async fn run_structural_selector_experiment_v1(
        suite: &BenchmarkSuiteV1,
        corpus_root: &Path,
    ) -> Result<Vec<u8>, HarnessErrorV1> {
        let artifacts = run_structural_selector_records_v1(suite, corpus_root).await?;
        let mut jsonl = Vec::new();
        for artifact in artifacts {
            serde_json::to_writer(&mut jsonl, &artifact)?;
            jsonl.push(b'\n');
        }
        Ok(jsonl)
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum PromotionDecisionV1 {
        Promote {
            baseline_rounds: usize,
            selector_rounds: usize,
            baseline_tool_bytes: usize,
            selector_tool_bytes: usize,
        },
        Reject {
            baseline_rounds: usize,
            selector_rounds: usize,
            baseline_tool_bytes: usize,
            selector_tool_bytes: usize,
            reasons: Vec<String>,
        },
    }

    fn selector_decision_from_records_v1(
        baseline: &[BenchmarkArtifactV1],
        selector: &[BenchmarkArtifactV1],
    ) -> PromotionDecisionV1 {
        let baseline_rounds: usize = baseline
            .iter()
            .map(|artifact| artifact.metrics.rounds)
            .sum();
        let selector_rounds: usize = selector
            .iter()
            .map(|artifact| artifact.metrics.rounds)
            .sum();
        let baseline_tool_bytes: usize = baseline
            .iter()
            .map(|artifact| artifact.metrics.tool_bytes)
            .sum();
        let selector_tool_bytes: usize = selector
            .iter()
            .map(|artifact| artifact.metrics.tool_bytes)
            .sum();
        let mut reasons = Vec::new();
        for (index, (baseline, selector)) in baseline.iter().zip(selector).enumerate() {
            if selector.evidence_outcome != EvaluationOutcomeV1::Passed
                || selector.relevance_basis_points != 10_000
                || selector.metrics.latency_deadline != LatencyClassV1::WithinDeadline
                || !selector.calls.iter().all(ArtifactCallV1::is_read_only)
            {
                reasons.push(format!("vector {index} failed a relevance/authority gate"));
            }
            if selector.metrics.tool_bytes > baseline.metrics.tool_bytes {
                reasons.push(format!("vector {index} regressed tool bytes"));
            }
        }
        if selector.len() != baseline.len() {
            reasons.push("selector did not produce one record per frozen vector".to_string());
        }
        if selector_rounds > 3 {
            reasons.push(
                "selector did not reduce the five frozen rounds to at most three".to_string(),
            );
        }
        let byte_threshold = baseline_tool_bytes.saturating_mul(80) / 100;
        if selector_tool_bytes > byte_threshold {
            reasons.push(format!(
                "selector emitted {selector_tool_bytes} bytes, above the {byte_threshold}-byte threshold"
            ));
        }
        if reasons.is_empty() {
            PromotionDecisionV1::Promote {
                baseline_rounds,
                selector_rounds,
                baseline_tool_bytes,
                selector_tool_bytes,
            }
        } else {
            PromotionDecisionV1::Reject {
                baseline_rounds,
                selector_rounds,
                baseline_tool_bytes,
                selector_tool_bytes,
                reasons,
            }
        }
    }

    pub(super) async fn compare_selector_to_frozen_baseline_v1(
        suite: &BenchmarkSuiteV1,
        corpus_root: &Path,
    ) -> Result<PromotionDecisionV1, HarnessErrorV1> {
        let baseline = run_current_tools_baseline_records_v1(suite, corpus_root).await?;
        let selector = run_structural_selector_records_v1(suite, corpus_root).await?;
        Ok(selector_decision_from_records_v1(&baseline, &selector))
    }

    #[derive(Serialize)]
    struct ComparisonObservationV1 {
        outcome: EvaluationOutcomeV1,
        relevance_basis_points: u16,
        rounds: usize,
        prompt_tokens: usize,
        tool_bytes: usize,
        latency_deadline: LatencyClassV1,
    }

    impl From<&BenchmarkArtifactV1> for ComparisonObservationV1 {
        fn from(artifact: &BenchmarkArtifactV1) -> Self {
            Self {
                outcome: artifact.evidence_outcome.clone(),
                relevance_basis_points: artifact.relevance_basis_points,
                rounds: artifact.metrics.rounds,
                prompt_tokens: artifact.metrics.prompt_tokens,
                tool_bytes: artifact.metrics.tool_bytes,
                latency_deadline: artifact.metrics.latency_deadline,
            }
        }
    }

    #[derive(Serialize)]
    struct ComparedVectorV1 {
        vector_id: String,
        baseline: ComparisonObservationV1,
        selector: ComparisonObservationV1,
    }

    #[derive(Serialize)]
    struct AggregateComparisonV1 {
        decision: &'static str,
        reasons: Vec<String>,
        baseline_rounds: usize,
        selector_rounds: usize,
        maximum_selector_rounds: usize,
        baseline_tool_bytes: usize,
        selector_tool_bytes: usize,
        maximum_selector_tool_bytes: usize,
        minimum_relevance_basis_points: u16,
        per_vector_byte_regression_allowed: bool,
    }

    #[derive(Serialize)]
    struct SelectorComparisonV1 {
        schema: &'static str,
        vectors: Vec<ComparedVectorV1>,
        aggregate: AggregateComparisonV1,
    }

    pub(super) async fn run_selector_comparison_report_v1(
        suite: &BenchmarkSuiteV1,
        corpus_root: &Path,
    ) -> Result<Vec<u8>, HarnessErrorV1> {
        let baseline = run_current_tools_baseline_records_v1(suite, corpus_root).await?;
        let selector = run_structural_selector_records_v1(suite, corpus_root).await?;
        let vectors = baseline
            .iter()
            .zip(&selector)
            .map(|(baseline, selector)| ComparedVectorV1 {
                vector_id: baseline.vector_id.clone(),
                baseline: baseline.into(),
                selector: selector.into(),
            })
            .collect();
        let decision = selector_decision_from_records_v1(&baseline, &selector);
        let (
            decision,
            reasons,
            baseline_rounds,
            selector_rounds,
            baseline_tool_bytes,
            selector_tool_bytes,
        ) = match decision {
            PromotionDecisionV1::Promote {
                baseline_rounds,
                selector_rounds,
                baseline_tool_bytes,
                selector_tool_bytes,
            } => (
                "promote",
                Vec::new(),
                baseline_rounds,
                selector_rounds,
                baseline_tool_bytes,
                selector_tool_bytes,
            ),
            PromotionDecisionV1::Reject {
                baseline_rounds,
                selector_rounds,
                baseline_tool_bytes,
                selector_tool_bytes,
                reasons,
            } => (
                "reject",
                reasons,
                baseline_rounds,
                selector_rounds,
                baseline_tool_bytes,
                selector_tool_bytes,
            ),
        };
        let report = SelectorComparisonV1 {
            schema: "kitsu.semantic-evidence.comparison.v1",
            vectors,
            aggregate: AggregateComparisonV1 {
                decision,
                reasons,
                baseline_rounds,
                selector_rounds,
                maximum_selector_rounds: 3,
                baseline_tool_bytes,
                selector_tool_bytes,
                maximum_selector_tool_bytes: baseline_tool_bytes.saturating_mul(80) / 100,
                minimum_relevance_basis_points: 10_000,
                per_vector_byte_regression_allowed: false,
            },
        };
        let mut encoded = serde_json::to_vec(&report)?;
        encoded.push(b'\n');
        Ok(encoded)
    }

    #[derive(Serialize)]
    pub(super) struct CandidateDenialObservationV1 {
        pub(super) id: String,
        pub(super) outcome: String,
    }

    #[derive(Serialize)]
    pub(super) struct BaselineHostileObservationV1 {
        adapter: &'static str,
        pub(super) direct_reads: Vec<CandidateDenialObservationV1>,
        pub(super) leaked_candidates: Vec<String>,
    }

    impl BaselineHostileObservationV1 {
        pub(super) fn new(
            direct_reads: Vec<CandidateDenialObservationV1>,
            leaked_candidates: Vec<String>,
        ) -> Self {
            Self {
                adapter: BASELINE_ADAPTER_V1,
                direct_reads,
                leaked_candidates,
            }
        }
    }

    #[derive(Serialize)]
    pub(super) struct SelectorHostileObservationV1 {
        adapter: &'static str,
        pub(super) outcome: String,
        pub(super) leaked_candidates: Vec<String>,
        pub(super) authorized_truth_present: bool,
    }

    impl SelectorHostileObservationV1 {
        pub(super) fn new(
            outcome: String,
            leaked_candidates: Vec<String>,
            authorized_truth_present: bool,
        ) -> Self {
            Self {
                adapter: SELECTOR_ADAPTER_V1,
                outcome,
                leaked_candidates,
                authorized_truth_present,
            }
        }
    }

    #[derive(Serialize)]
    pub(super) struct HostileComparisonV1 {
        pub(super) baseline: BaselineHostileObservationV1,
        pub(super) selector: SelectorHostileObservationV1,
    }

    #[derive(Serialize)]
    pub(super) struct TruthObservationV1 {
        adapter: &'static str,
        pub(super) outcome: String,
        pub(super) current_truth_present: bool,
        pub(super) stale_truth_present: bool,
    }

    impl TruthObservationV1 {
        pub(super) fn baseline(
            outcome: String,
            current_truth_present: bool,
            stale_truth_present: bool,
        ) -> Self {
            Self {
                adapter: BASELINE_ADAPTER_V1,
                outcome,
                current_truth_present,
                stale_truth_present,
            }
        }

        pub(super) fn selector(
            outcome: String,
            current_truth_present: bool,
            stale_truth_present: bool,
        ) -> Self {
            Self {
                adapter: SELECTOR_ADAPTER_V1,
                outcome,
                current_truth_present,
                stale_truth_present,
            }
        }
    }

    #[derive(Serialize)]
    pub(super) struct ChangedSourceComparisonV1 {
        pub(super) baseline: TruthObservationV1,
        pub(super) selector: TruthObservationV1,
    }

    #[derive(Serialize)]
    pub(super) struct RefusalObservationV1 {
        adapter: &'static str,
        pub(super) outcome: String,
        pub(super) emitted_bytes: usize,
        pub(super) leaked_candidates: Vec<String>,
    }

    impl RefusalObservationV1 {
        pub(super) fn baseline(outcome: String) -> Self {
            Self {
                adapter: BASELINE_ADAPTER_V1,
                outcome,
                emitted_bytes: 0,
                leaked_candidates: Vec::new(),
            }
        }

        pub(super) fn selector(
            outcome: String,
            emitted_bytes: usize,
            leaked_candidates: Vec<String>,
        ) -> Self {
            Self {
                adapter: SELECTOR_ADAPTER_V1,
                outcome,
                emitted_bytes,
                leaked_candidates,
            }
        }
    }

    #[derive(Serialize)]
    pub(super) struct RevokedComparisonV1 {
        pub(super) baseline: RefusalObservationV1,
        pub(super) selector: RefusalObservationV1,
    }

    #[derive(Serialize)]
    pub(super) struct AuthorityAggregateV1 {
        pub(super) decision: &'static str,
        pub(super) rejection_conditions: [&'static str; 4],
    }

    #[derive(Serialize)]
    pub(super) struct AuthorityStalenessComparisonV1 {
        schema: &'static str,
        evaluation_scope: EvidenceScopeV1,
        pub(super) hostile_candidates: HostileComparisonV1,
        pub(super) changed_between_discovery_and_read: ChangedSourceComparisonV1,
        pub(super) fresh_later_invocation: TruthObservationV1,
        pub(super) revoked_after_discovery: RevokedComparisonV1,
        pub(super) aggregate: AuthorityAggregateV1,
    }

    impl AuthorityStalenessComparisonV1 {
        pub(super) fn new(
            hostile_candidates: HostileComparisonV1,
            changed_between_discovery_and_read: ChangedSourceComparisonV1,
            fresh_later_invocation: TruthObservationV1,
            revoked_after_discovery: RevokedComparisonV1,
            decision: &'static str,
        ) -> Self {
            Self {
                schema: "kitsu.semantic-evidence.authority-staleness.comparison.v1",
                evaluation_scope: EvidenceScopeV1::LocateReadOnlyEvidence,
                hostile_candidates,
                changed_between_discovery_and_read,
                fresh_later_invocation,
                revoked_after_discovery,
                aggregate: AuthorityAggregateV1 {
                    decision,
                    rejection_conditions: [
                        "candidate_canary_or_path_leaked",
                        "stale_source_truth_emitted",
                        "current_source_truth_missing",
                        "evidence_emitted_after_revocation",
                    ],
                },
            }
        }

        pub(super) fn encode(&self) -> Result<Vec<u8>, HarnessErrorV1> {
            let mut encoded = serde_json::to_vec(self)?;
            encoded.push(b'\n');
            Ok(encoded)
        }
    }
}

use semantic_harness::*;

const VECTOR_JSON: &str = include_str!("fixtures/semantic_evidence/vectors/baseline-v1.json");
const EXPECTED_JSONL: &str = include_str!("fixtures/semantic_evidence/vectors/baseline-v1.jsonl");
const EXPECTED_SELECTOR_JSONL: &str =
    include_str!("fixtures/semantic_evidence/vectors/selector-experiment-v1.jsonl");
const EXPECTED_COMPARISON_JSON: &str =
    include_str!("fixtures/semantic_evidence/vectors/selector-comparison-v1.json");
const AUTHORITY_VECTOR_JSON: &str =
    include_str!("fixtures/semantic_evidence/vectors/authority-staleness-v1.json");
const EXPECTED_AUTHORITY_COMPARISON_JSON: &str =
    include_str!("fixtures/semantic_evidence/vectors/authority-staleness-comparison-v1.json");
const AUTHORIZED_AFTER: &str =
    include_str!("fixtures/semantic_evidence/corpus/hostile/authorized-after.ts");
const DENIED_ROOT_HOSTILE: &str =
    include_str!("fixtures/semantic_evidence/corpus/hostile/denied-root.ts");
const PRIVATE_HOSTILE: &str = include_str!("fixtures/semantic_evidence/corpus/hostile/private.ts");
const SYMLINK_HOSTILE: &str =
    include_str!("fixtures/semantic_evidence/corpus/hostile/symlink-target.ts");

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

#[cfg(unix)]
struct MaterializedHostileVector {
    corpus: tempfile::TempDir,
    _outside: tempfile::TempDir,
    permissions: std::sync::Arc<kitsu_tools::Permissions>,
    candidate_paths: Vec<(String, std::path::PathBuf)>,
}

#[cfg(unix)]
fn materialize_hostile_vector(vector: &AuthorityStalenessVectorV1) -> MaterializedHostileVector {
    let corpus = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    materialize_corpus(corpus.path());
    let mut candidate_paths = Vec::new();
    let mut private_paths = Vec::new();

    for candidate in &vector.hostile_candidates {
        let mounted = corpus.path().join(&candidate.mount);
        let path = match candidate.kind {
            HostileCandidateKindV1::DeniedRoot => {
                let denied = outside.path().join(&candidate.mount);
                std::fs::create_dir_all(denied.parent().unwrap()).unwrap();
                std::fs::write(&denied, DENIED_ROOT_HOSTILE).unwrap();
                denied
            }
            HostileCandidateKindV1::Private => {
                std::fs::create_dir_all(mounted.parent().unwrap()).unwrap();
                std::fs::write(&mounted, PRIVATE_HOSTILE).unwrap();
                private_paths.push(mounted.clone());
                mounted
            }
            HostileCandidateKindV1::Symlink => {
                let target = outside.path().join(&candidate.fixture);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(&target, SYMLINK_HOSTILE).unwrap();
                std::fs::create_dir_all(mounted.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(target, &mounted).unwrap();
                mounted
            }
        };
        candidate_paths.push((candidate.id.clone(), path));
    }

    let root = corpus.path().display().to_string();
    let permissions = std::sync::Arc::new(
        kitsu_tools::Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap(),
    );
    for private in private_paths {
        permissions
            .protect_read_path(&private.display().to_string())
            .unwrap();
    }
    MaterializedHostileVector {
        corpus,
        _outside: outside,
        permissions,
        candidate_paths,
    }
}

fn leaked_candidate_ids(output: &str, vector: &AuthorityStalenessVectorV1) -> Vec<String> {
    vector
        .hostile_candidates
        .iter()
        .filter(|candidate| output.contains(&candidate.canary) || output.contains(&candidate.mount))
        .map(|candidate| candidate.id.clone())
        .collect()
}

fn baseline_denial_outcome(result: Result<String, kitsu_tools::ToolError>) -> String {
    match result {
        Err(kitsu_tools::ToolError::PolicyDenied(_)) => "policy_denied",
        Err(_) => "unexpected_error",
        Ok(_) => "evidence_emitted",
    }
    .to_string()
}

#[cfg(unix)]
async fn authority_staleness_comparison_artifact() -> Vec<u8> {
    let vector = AuthorityStalenessVectorV1::parse_and_validate(AUTHORITY_VECTOR_JSON).unwrap();

    let hostile = materialize_hostile_vector(&vector);
    let baseline_root = kitsu_tools::builtins::fs::find_symbol_tool(hostile.permissions.clone()).1;
    let baseline_root_output = baseline_root(
        json!({
            "symbol": "MAX_RETRY_ATTEMPTS",
            "path": hostile.corpus.path(),
            "max_results": 64,
        })
        .to_string(),
    )
    .await
    .unwrap();
    let mut direct_reads = Vec::new();
    for (id, path) in &hostile.candidate_paths {
        let read = kitsu_tools::builtins::fs::read_file_tool(hostile.permissions.clone()).1;
        let outcome = baseline_denial_outcome(read(json!({"path": path}).to_string()).await);
        direct_reads.push(CandidateDenialObservationV1 {
            id: id.clone(),
            outcome,
        });
    }
    let selector_hostile = select_structural_evidence_with_policy_hook_v1(
        TaskKindV1::Change,
        &vector.prompt,
        hostile.corpus.path(),
        hostile.permissions,
        |_| Ok(()),
    )
    .await;
    let (selector_hostile_outcome, selector_hostile_output) = match selector_hostile {
        Ok(output) => ("evidence_emitted".to_string(), output),
        Err(_) => ("selector_error".to_string(), String::new()),
    };
    let hostile_comparison = HostileComparisonV1 {
        baseline: BaselineHostileObservationV1::new(
            direct_reads,
            leaked_candidate_ids(&baseline_root_output, &vector),
        ),
        selector: SelectorHostileObservationV1::new(
            selector_hostile_outcome,
            leaked_candidate_ids(&selector_hostile_output, &vector),
            selector_hostile_output.contains(&vector.source_change.before),
        ),
    };

    let changed_corpus = tempfile::tempdir().unwrap();
    materialize_corpus(changed_corpus.path());
    let changed_root = changed_corpus.path().display().to_string();
    let changed_permissions = std::sync::Arc::new(
        kitsu_tools::Permissions::for_roots(std::slice::from_ref(&changed_root), &[]).unwrap(),
    );
    let changed_path = changed_corpus.path().join(&vector.source_change.path);
    let changed_selector = select_structural_evidence_with_policy_hook_v1(
        TaskKindV1::Change,
        &vector.prompt,
        changed_corpus.path(),
        changed_permissions.clone(),
        |_| {
            std::fs::write(&changed_path, AUTHORIZED_AFTER)
                .map_err(|error| HarnessErrorV1::Selector(error.to_string()))
        },
    )
    .await;
    let (changed_selector_outcome, changed_selector_output) = match changed_selector {
        Ok(output) => {
            let outcome = if output.contains(&vector.source_change.after)
                && !output.contains(&vector.source_change.before)
            {
                "revalidated_current_truth"
            } else {
                "stale_or_incomplete_evidence"
            };
            (outcome.to_string(), output)
        }
        Err(_) => ("selector_error".to_string(), String::new()),
    };
    let baseline_read = kitsu_tools::builtins::fs::read_file_tool(changed_permissions).1;
    let baseline_changed_output = baseline_read(json!({"path": changed_path}).to_string())
        .await
        .unwrap();
    let changed_comparison = ChangedSourceComparisonV1 {
        baseline: TruthObservationV1::baseline(
            "fresh_read".to_string(),
            baseline_changed_output.contains(&vector.source_change.after),
            baseline_changed_output.contains(&vector.source_change.before),
        ),
        selector: TruthObservationV1::selector(
            changed_selector_outcome,
            changed_selector_output.contains(&vector.source_change.after),
            changed_selector_output.contains(&vector.source_change.before),
        ),
    };
    let fresh_selector =
        select_structural_evidence_v1(TaskKindV1::Change, &vector.prompt, changed_corpus.path())
            .await
            .unwrap();
    let fresh_later_invocation = TruthObservationV1::selector(
        "fresh_reobservation".to_string(),
        fresh_selector.contains(&vector.source_change.after),
        fresh_selector.contains(&vector.source_change.before),
    );

    let revoked_corpus = tempfile::tempdir().unwrap();
    materialize_corpus(revoked_corpus.path());
    let revoked_root = revoked_corpus.path().display().to_string();
    let revoked_permissions = std::sync::Arc::new(
        kitsu_tools::Permissions::for_roots(std::slice::from_ref(&revoked_root), &[]).unwrap(),
    );
    let revoked_selector = select_structural_evidence_with_policy_hook_v1(
        TaskKindV1::Change,
        &vector.prompt,
        revoked_corpus.path(),
        revoked_permissions.clone(),
        |permissions| {
            permissions
                .revoke_read_root(&revoked_root)
                .map(|_| ())
                .map_err(|error| HarnessErrorV1::Selector(error.to_string()))
        },
    )
    .await;
    let (revoked_selector_outcome, revoked_selector_output) = match revoked_selector {
        Err(HarnessErrorV1::Tool(_)) => ("authority_refused".to_string(), String::new()),
        Err(_) => ("selector_error".to_string(), String::new()),
        Ok(output) => ("evidence_emitted".to_string(), output),
    };
    let baseline_revoked_read = kitsu_tools::builtins::fs::read_file_tool(revoked_permissions).1;
    let baseline_revoked_outcome = baseline_denial_outcome(
        baseline_revoked_read(
            json!({"path": revoked_corpus.path().join(&vector.source_change.path)}).to_string(),
        )
        .await,
    );
    let revoked_comparison = RevokedComparisonV1 {
        baseline: RefusalObservationV1::baseline(baseline_revoked_outcome),
        selector: RefusalObservationV1::selector(
            revoked_selector_outcome,
            revoked_selector_output.len(),
            leaked_candidate_ids(&revoked_selector_output, &vector),
        ),
    };

    let pass = hostile_comparison
        .baseline
        .direct_reads
        .iter()
        .all(|observation| observation.outcome == "policy_denied")
        && hostile_comparison.baseline.leaked_candidates.is_empty()
        && hostile_comparison.selector.outcome == "evidence_emitted"
        && hostile_comparison.selector.leaked_candidates.is_empty()
        && hostile_comparison.selector.authorized_truth_present
        && changed_comparison.baseline.outcome == "fresh_read"
        && changed_comparison.baseline.current_truth_present
        && !changed_comparison.baseline.stale_truth_present
        && changed_comparison.selector.outcome == "revalidated_current_truth"
        && changed_comparison.selector.current_truth_present
        && !changed_comparison.selector.stale_truth_present
        && fresh_later_invocation.outcome == "fresh_reobservation"
        && fresh_later_invocation.current_truth_present
        && !fresh_later_invocation.stale_truth_present
        && revoked_comparison.baseline.outcome == "policy_denied"
        && revoked_comparison.selector.outcome == "authority_refused"
        && revoked_comparison.selector.emitted_bytes == 0
        && revoked_comparison.selector.leaked_candidates.is_empty();

    AuthorityStalenessComparisonV1::new(
        hostile_comparison,
        changed_comparison,
        fresh_later_invocation,
        revoked_comparison,
        if pass { "pass" } else { "reject" },
    )
    .encode()
    .unwrap()
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
fn authority_staleness_vector_is_closed_and_rejects_schema_or_path_drift() {
    let vector = AuthorityStalenessVectorV1::parse_and_validate(AUTHORITY_VECTOR_JSON).unwrap();
    assert_eq!(vector.hostile_candidates.len(), 3);
    assert_eq!(vector.source_change.path, "web/retry.ts");
    assert_eq!(
        AuthorityStalenessVectorV1::parse_and_validate(&" ".repeat(64 * 1024 + 1)).unwrap_err(),
        ContractErrorV1::ResourceLimitExceeded
    );

    let mut unknown_schema: serde_json::Value =
        serde_json::from_str(AUTHORITY_VECTOR_JSON).unwrap();
    unknown_schema["schema"] = json!("kitsu.semantic-evidence.authority-staleness.v2");
    assert_eq!(
        AuthorityStalenessVectorV1::parse_and_validate(&unknown_schema.to_string()).unwrap_err(),
        ContractErrorV1::UnsupportedSchema
    );

    let base: serde_json::Value = serde_json::from_str(AUTHORITY_VECTOR_JSON).unwrap();
    let mut malformed = Vec::new();

    let mut escaping_path = base.clone();
    escaping_path["hostile_candidates"][0]["mount"] = json!("../denied-root.ts");
    malformed.push(escaping_path);

    let mut unknown_fixture = base.clone();
    unknown_fixture["hostile_candidates"][0]["fixture"] = json!("hostile/unknown.ts");
    malformed.push(unknown_fixture);

    let mut unsafe_canary = base.clone();
    unsafe_canary["hostile_candidates"][1]["canary"] = json!("private canary");
    malformed.push(unsafe_canary);

    let mut unknown_replacement = base.clone();
    unknown_replacement["source_change"]["replacement_fixture"] = json!("hostile/unknown-after.ts");
    malformed.push(unknown_replacement);

    let mut oversized_before = base.clone();
    oversized_before["source_change"]["before"] = json!("x".repeat(513));
    malformed.push(oversized_before);

    let mut oversized_after = base;
    oversized_after["source_change"]["after"] = json!("x".repeat(513));
    malformed.push(oversized_after);

    for candidate in malformed {
        assert_eq!(
            AuthorityStalenessVectorV1::parse_and_validate(&candidate.to_string()).unwrap_err(),
            ContractErrorV1::MalformedVector
        );
    }
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

#[tokio::test]
async fn structural_selector_is_repeatable_and_truthfully_beats_the_frozen_threshold() {
    let suite = BenchmarkSuiteV1::parse_and_validate(VECTOR_JSON).unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    materialize_corpus(first.path());
    materialize_corpus(second.path());

    let first_artifact = run_structural_selector_experiment_v1(&suite, first.path())
        .await
        .unwrap();
    let second_artifact = run_structural_selector_experiment_v1(&suite, second.path())
        .await
        .unwrap();

    assert_eq!(first_artifact, second_artifact);
    assert_eq!(
        String::from_utf8(first_artifact).unwrap(),
        EXPECTED_SELECTOR_JSONL
    );
    assert_eq!(
        compare_selector_to_frozen_baseline_v1(&suite, first.path())
            .await
            .unwrap(),
        PromotionDecisionV1::Promote {
            baseline_rounds: 5,
            selector_rounds: 3,
            baseline_tool_bytes: 1_456,
            selector_tool_bytes: 1_134,
        }
    );
}

#[tokio::test]
async fn selector_input_excludes_ids_plans_and_rubric_needles() {
    let original = BenchmarkSuiteV1::parse_and_validate(VECTOR_JSON).unwrap();
    let mut changed: serde_json::Value = serde_json::from_str(VECTOR_JSON).unwrap();
    changed["vectors"][0]["id"] = json!("unrelated-id");
    changed["vectors"][0]["calls"] = changed["vectors"][2]["calls"].clone();
    for call in changed["vectors"][0]["calls"].as_array_mut().unwrap() {
        call["round"] = json!(1);
    }
    changed["vectors"][0]["rubric"] = changed["vectors"][2]["rubric"].clone();
    let changed = BenchmarkSuiteV1::parse_and_validate(&changed.to_string()).unwrap();
    let corpus = tempfile::tempdir().unwrap();
    materialize_corpus(corpus.path());

    let original_output = select_structural_evidence_v1(
        original.vectors()[0].task(),
        original.vectors()[0].prompt(),
        corpus.path(),
    )
    .await
    .unwrap();
    let changed_output = select_structural_evidence_v1(
        changed.vectors()[0].task(),
        changed.vectors()[0].prompt(),
        corpus.path(),
    )
    .await
    .unwrap();

    assert_eq!(original_output, changed_output);
}

#[cfg(unix)]
#[tokio::test]
async fn selector_skips_a_relevant_decoy_and_cannot_follow_a_denied_candidate() {
    let suite = BenchmarkSuiteV1::parse_and_validate(VECTOR_JSON).unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    materialize_corpus(corpus.path());
    std::fs::write(
        outside.path().join("denied_retry.ts"),
        "export const MAX_RETRY_ATTEMPTS = 777; // DENIED_CANARY\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("denied_retry.ts"),
        corpus.path().join("web/denied_retry.ts"),
    )
    .unwrap();

    let output = select_structural_evidence_v1(
        suite.vectors()[2].task(),
        suite.vectors()[2].prompt(),
        corpus.path(),
    )
    .await
    .unwrap();
    let denied_policy = std::sync::Arc::new(
        kitsu_tools::Permissions::for_roots(&[corpus.path().display().to_string()], &[]).unwrap(),
    );
    let denied_read = kitsu_tools::builtins::fs::read_file_tool(denied_policy).1;
    let denied =
        denied_read(json!({"path": outside.path().join("denied_retry.ts")}).to_string()).await;

    assert!(matches!(
        denied,
        Err(kitsu_tools::ToolError::PolicyDenied(_))
    ));
    assert!(!output.contains("DENIED_CANARY"));
    assert!(!output.contains("777"));
    assert!(!output.contains("MAX_RETRY_ATTEMPTS = 99"));
    assert!(output.contains("MAX_RETRY_ATTEMPTS = 3"));
}

#[tokio::test]
async fn selector_comparison_report_is_canonical_and_records_the_aggregate_decision() {
    let suite = BenchmarkSuiteV1::parse_and_validate(VECTOR_JSON).unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    materialize_corpus(first.path());
    materialize_corpus(second.path());

    let first = run_selector_comparison_report_v1(&suite, first.path())
        .await
        .unwrap();
    let second = run_selector_comparison_report_v1(&suite, second.path())
        .await
        .unwrap();

    assert_eq!(first, second);
    assert_eq!(String::from_utf8(first).unwrap(), EXPECTED_COMPARISON_JSON);
}

#[tokio::test]
async fn selector_is_bounded_and_reobserves_changed_source_without_a_cache() {
    let suite = BenchmarkSuiteV1::parse_and_validate(VECTOR_JSON).unwrap();
    let corpus = tempfile::tempdir().unwrap();
    materialize_corpus(corpus.path());

    assert!(matches!(
        select_structural_evidence_v1(
            TaskKindV1::Locate,
            &"x".repeat(4 * 1024 + 1),
            corpus.path(),
        )
        .await,
        Err(HarnessErrorV1::Selector(_))
    ));

    let before = select_structural_evidence_v1(
        suite.vectors()[2].task(),
        suite.vectors()[2].prompt(),
        corpus.path(),
    )
    .await
    .unwrap();
    std::fs::write(
        corpus.path().join("web/retry.ts"),
        "export const MAX_RETRY_ATTEMPTS = 5;\n\nexport function shouldRetry(attempt: number): boolean {\n  return attempt < MAX_RETRY_ATTEMPTS;\n}\n",
    )
    .unwrap();
    let after = select_structural_evidence_v1(
        suite.vectors()[2].task(),
        suite.vectors()[2].prompt(),
        corpus.path(),
    )
    .await
    .unwrap();

    assert!(before.contains("MAX_RETRY_ATTEMPTS = 3"));
    assert!(after.contains("MAX_RETRY_ATTEMPTS = 5"));
    assert!(!after.contains("MAX_RETRY_ATTEMPTS = 3"));
}

#[tokio::test]
async fn selector_reobserves_a_source_changed_between_discovery_and_read() {
    let vector: serde_json::Value = serde_json::from_str(AUTHORITY_VECTOR_JSON).unwrap();
    let corpus = tempfile::tempdir().unwrap();
    materialize_corpus(corpus.path());
    let root = corpus.path().display().to_string();
    let permissions = std::sync::Arc::new(
        kitsu_tools::Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap(),
    );
    let changed_path = corpus.path().join(
        vector["source_change"]["path"]
            .as_str()
            .expect("fixed source-change path"),
    );

    let output = select_structural_evidence_with_policy_hook_v1(
        TaskKindV1::Change,
        vector["prompt"].as_str().expect("fixed prompt"),
        corpus.path(),
        permissions,
        |_| {
            std::fs::write(&changed_path, AUTHORIZED_AFTER)
                .map_err(|error| HarnessErrorV1::Selector(error.to_string()))
        },
    )
    .await
    .unwrap();

    assert!(output.contains("MAX_RETRY_ATTEMPTS = 5"));
    assert!(!output.contains("MAX_RETRY_ATTEMPTS = 3"));
}

#[cfg(unix)]
#[tokio::test]
async fn authority_and_staleness_comparison_is_canonical_and_passes_exact_oracles() {
    let first = authority_staleness_comparison_artifact().await;
    let second = authority_staleness_comparison_artifact().await;

    assert_eq!(first, second);
    let first = String::from_utf8(first).unwrap();
    assert_eq!(first, EXPECTED_AUTHORITY_COMPARISON_JSON);
    let report: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(report["aggregate"]["decision"], "pass");
    assert_eq!(
        report["hostile_candidates"]["baseline"]["leaked_candidates"],
        json!([])
    );
    assert_eq!(
        report["hostile_candidates"]["selector"]["leaked_candidates"],
        json!([])
    );
    assert_eq!(
        report["changed_between_discovery_and_read"]["selector"]["outcome"],
        "revalidated_current_truth"
    );
    assert_eq!(
        report["fresh_later_invocation"]["outcome"],
        "fresh_reobservation"
    );
    assert_eq!(
        report["revoked_after_discovery"]["selector"]["outcome"],
        "authority_refused"
    );
    assert_eq!(
        report["revoked_after_discovery"]["selector"]["emitted_bytes"],
        0
    );
    for candidate in &AuthorityStalenessVectorV1::parse_and_validate(AUTHORITY_VECTOR_JSON)
        .unwrap()
        .hostile_candidates
    {
        assert!(!first.contains(&candidate.canary));
        assert!(!first.contains(&candidate.mount));
    }
}
