use std::cmp::Ordering;
use std::collections::BTreeMap;

pub(super) const MODEL_SELECTION_PROPOSAL_REVISION: u16 = 1;
pub(super) const MAX_RAW_CATALOG_ROWS: usize = 1_024;
pub(super) const MAX_MODEL_ID_BYTES: usize = 512;
pub(super) const MAX_CATALOG_BYTES: usize = 256 * 1_024;
pub(super) const MAX_NORMALIZED_MODELS: usize = 1_024;
pub(super) const MAX_FILTER_BYTES: usize = 128;
pub(super) const MIN_TERMINAL_COLUMNS: usize = 8;
pub(super) const MAX_TERMINAL_COLUMNS: usize = 512;
pub(super) const MAX_PROJECTED_ROWS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CatalogUnavailableReason {
    NotSupported,
    NotObserved,
    FetchFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CatalogAvailability {
    Available,
    Offline,
    Unavailable(CatalogUnavailableReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CatalogSource {
    Live,
    Cache,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CatalogFreshness {
    Current,
    Stale,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CatalogProvenance {
    ProviderObservation,
    RetainedSnapshot,
    None,
}

/// Closed catalog truth supplied with one immutable observation. The fields are
/// kept separate for projection, while normalization admits only this matrix:
///
/// - available + live + current + provider observation;
/// - available + cache + current/stale + retained snapshot;
/// - offline + cache + current/stale + retained snapshot;
/// - offline + none + unavailable + no provenance; or
/// - unavailable(reason) + none + unavailable + no provenance.
///
/// In particular, offline cannot claim a live observation and unavailable
/// cannot carry catalog rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CatalogTruth {
    availability: CatalogAvailability,
    source: CatalogSource,
    freshness: CatalogFreshness,
    provenance: CatalogProvenance,
}

impl CatalogTruth {
    pub(super) const fn new(
        availability: CatalogAvailability,
        source: CatalogSource,
        freshness: CatalogFreshness,
        provenance: CatalogProvenance,
    ) -> Self {
        Self {
            availability,
            source,
            freshness,
            provenance,
        }
    }

    pub(super) const fn availability(self) -> CatalogAvailability {
        self.availability
    }

    pub(super) const fn source(self) -> CatalogSource {
        self.source
    }

    pub(super) const fn freshness(self) -> CatalogFreshness {
        self.freshness
    }

    pub(super) const fn provenance(self) -> CatalogProvenance {
        self.provenance
    }

    const fn is_valid(self) -> bool {
        matches!(
            (
                self.availability,
                self.source,
                self.freshness,
                self.provenance,
            ),
            (
                CatalogAvailability::Available,
                CatalogSource::Live,
                CatalogFreshness::Current,
                CatalogProvenance::ProviderObservation,
            ) | (
                CatalogAvailability::Available,
                CatalogSource::Cache,
                CatalogFreshness::Current | CatalogFreshness::Stale,
                CatalogProvenance::RetainedSnapshot,
            ) | (
                CatalogAvailability::Offline,
                CatalogSource::Cache,
                CatalogFreshness::Current | CatalogFreshness::Stale,
                CatalogProvenance::RetainedSnapshot,
            ) | (
                CatalogAvailability::Offline,
                CatalogSource::None,
                CatalogFreshness::Unavailable,
                CatalogProvenance::None,
            ) | (
                CatalogAvailability::Unavailable(_),
                CatalogSource::None,
                CatalogFreshness::Unavailable,
                CatalogProvenance::None,
            )
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CatalogMembership {
    Present,
    Absent,
    Unobserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectionBasis {
    Current,
    ConfiguredDefault,
    CatalogFirst,
    Explicit,
}

/// Errors are closed and contain no provider-controlled model/filter text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ModelPickerError {
    ContradictoryCatalogTruth,
    CatalogRowsWithoutSource,
    TooManyCatalogRows,
    CatalogBytesExceeded,
    TooManySelectableModels,
    InvalidModelId,
    InvalidFilter,
    TerminalWidthOutOfRange,
    ProjectionRowsOutOfRange,
    NoSelectableModel,
    UnknownModel,
}

/// Untrusted catalog input deliberately has no `Debug` or serialization.
/// Construction stops consuming rows at the first resource violation.
pub(super) struct CatalogSnapshotInput {
    truth: CatalogTruth,
    catalog_ids: Vec<String>,
    current_model: Option<String>,
    default_model: Option<String>,
    preflight_error: Option<ModelPickerError>,
}

impl CatalogSnapshotInput {
    pub(super) fn new<I, S>(
        truth: CatalogTruth,
        catalog_ids: I,
        current_model: Option<&str>,
        default_model: Option<&str>,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut ids = Vec::new();
        let mut total_bytes = 0usize;
        let mut preflight_error = None;
        for raw in catalog_ids {
            if ids.len() == MAX_RAW_CATALOG_ROWS {
                preflight_error = Some(ModelPickerError::TooManyCatalogRows);
                break;
            }
            let raw = raw.into();
            let Some(next_total) = total_bytes.checked_add(raw.len()) else {
                preflight_error = Some(ModelPickerError::CatalogBytesExceeded);
                break;
            };
            if next_total > MAX_CATALOG_BYTES {
                preflight_error = Some(ModelPickerError::CatalogBytesExceeded);
                break;
            }
            if normalize_model_id(&raw).is_err() {
                preflight_error = Some(ModelPickerError::InvalidModelId);
                break;
            }
            total_bytes = next_total;
            ids.push(raw);
        }

        let mut current_model_owned = None;
        let mut default_model_owned = None;
        if preflight_error.is_none() {
            for (configured, destination) in [
                (current_model, &mut current_model_owned),
                (default_model, &mut default_model_owned),
            ]
            .into_iter()
            .filter_map(|(configured, destination)| {
                configured.map(|configured| (configured, destination))
            }) {
                let Some(next_total) = total_bytes.checked_add(configured.len()) else {
                    preflight_error = Some(ModelPickerError::CatalogBytesExceeded);
                    break;
                };
                if next_total > MAX_CATALOG_BYTES {
                    preflight_error = Some(ModelPickerError::CatalogBytesExceeded);
                    break;
                }
                if normalize_model_id(configured).is_err() {
                    preflight_error = Some(ModelPickerError::InvalidModelId);
                    break;
                }
                total_bytes = next_total;
                *destination = Some(configured.to_string());
            }
        }

        Self {
            truth,
            catalog_ids: ids,
            current_model: current_model_owned,
            default_model: default_model_owned,
            preflight_error,
        }
    }

    pub(super) fn normalize(self) -> Result<ModelPickerSnapshot, ModelPickerError> {
        if !self.truth.is_valid() {
            return Err(ModelPickerError::ContradictoryCatalogTruth);
        }
        if let Some(error) = self.preflight_error {
            return Err(error);
        }
        if self.truth.source == CatalogSource::None && !self.catalog_ids.is_empty() {
            return Err(ModelPickerError::CatalogRowsWithoutSource);
        }

        let mut candidates = BTreeMap::<String, CandidateFacts>::new();
        for raw in self.catalog_ids {
            let id = normalize_model_id(&raw)?;
            candidates.entry(id).or_default().in_catalog = true;
        }
        let current_model = self
            .current_model
            .as_deref()
            .map(normalize_model_id)
            .transpose()?;
        if let Some(current) = &current_model {
            candidates.entry(current.clone()).or_default().is_current = true;
        }
        let default_model = self
            .default_model
            .as_deref()
            .map(normalize_model_id)
            .transpose()?;
        if let Some(default) = &default_model {
            candidates.entry(default.clone()).or_default().is_default = true;
        }
        if candidates.len() > MAX_NORMALIZED_MODELS {
            return Err(ModelPickerError::TooManySelectableModels);
        }

        let candidates = candidates
            .into_iter()
            .map(|(id, facts)| ModelCandidate {
                id,
                in_catalog: facts.in_catalog,
                is_current: facts.is_current,
                is_default: facts.is_default,
            })
            .collect();
        Ok(ModelPickerSnapshot {
            truth: self.truth,
            candidates,
            current_model,
            default_model,
        })
    }
}

#[derive(Default)]
struct CandidateFacts {
    in_catalog: bool,
    is_current: bool,
    is_default: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct ModelCandidate {
    id: String,
    in_catalog: bool,
    is_current: bool,
    is_default: bool,
}

/// One normalized, immutable picker snapshot. It owns no runtime handle and
/// exposes no mutation or publication operation.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ModelPickerSnapshot {
    truth: CatalogTruth,
    candidates: Vec<ModelCandidate>,
    current_model: Option<String>,
    default_model: Option<String>,
}

impl ModelPickerSnapshot {
    pub(super) const fn truth(&self) -> CatalogTruth {
        self.truth
    }

    pub(super) fn model_count(&self) -> usize {
        self.candidates.len()
    }

    pub(super) fn current_model(&self) -> Option<&str> {
        self.current_model.as_deref()
    }

    pub(super) fn default_model(&self) -> Option<&str> {
        self.default_model.as_deref()
    }

    pub(super) fn project(&self, query: &PickerQuery) -> ModelPickerView {
        let mut ranked = self
            .candidates
            .iter()
            .filter_map(|candidate| {
                let match_rank = model_match_rank(&candidate.id, &query.filter);
                (match_rank.is_some() || candidate.is_current).then_some((candidate, match_rank))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| compare_ranked(*left, *right));

        let match_count = self
            .candidates
            .iter()
            .filter(|candidate| model_match_rank(&candidate.id, &query.filter).is_some())
            .count();
        let included_count = ranked.len();
        let mut display_was_truncated = false;
        let rows = ranked
            .into_iter()
            .take(query.max_rows)
            .map(|(candidate, match_rank)| {
                let display_id = truncate_identifier(&candidate.id, query.terminal_columns);
                display_was_truncated |= display_id.len() != candidate.id.len();
                ModelPickerRow {
                    model_id: candidate.id.clone(),
                    display_id,
                    is_current: candidate.is_current,
                    is_default: candidate.is_default,
                    pinned_outside_filter: candidate.is_current && match_rank.is_none(),
                    catalog_membership: self.catalog_membership(candidate),
                }
            })
            .collect::<Vec<_>>();
        let omitted_count = included_count.saturating_sub(rows.len());
        ModelPickerView {
            truth: self.truth,
            rows,
            candidate_count: self.candidates.len(),
            match_count,
            omitted_count,
            truncated: omitted_count > 0 || display_was_truncated,
        }
    }

    pub(super) fn stage_preferred(&self) -> Result<ModelSelectionProposal, ModelPickerError> {
        let (candidate, basis) = if let Some(candidate) = self
            .candidates
            .iter()
            .find(|candidate| candidate.is_current)
        {
            (candidate, SelectionBasis::Current)
        } else if let Some(candidate) = self
            .candidates
            .iter()
            .find(|candidate| candidate.is_default)
        {
            (candidate, SelectionBasis::ConfiguredDefault)
        } else {
            let candidate = self
                .candidates
                .iter()
                .min_by(|left, right| compare_identifier(&left.id, &right.id))
                .ok_or(ModelPickerError::NoSelectableModel)?;
            (candidate, SelectionBasis::CatalogFirst)
        };
        Ok(self.proposal(candidate, basis))
    }

    pub(super) fn stage_exact(
        &self,
        input: ModelSelectionInput,
    ) -> Result<ModelSelectionProposal, ModelPickerError> {
        let id = normalize_model_id(&input.model_id)?;
        let candidate = self
            .candidates
            .iter()
            .find(|candidate| candidate.id == id)
            .ok_or(ModelPickerError::UnknownModel)?;
        Ok(self.proposal(candidate, SelectionBasis::Explicit))
    }

    fn proposal(
        &self,
        candidate: &ModelCandidate,
        basis: SelectionBasis,
    ) -> ModelSelectionProposal {
        ModelSelectionProposal {
            model_id: candidate.id.clone(),
            basis,
            was_current: candidate.is_current,
            was_default: candidate.is_default,
            catalog_membership: self.catalog_membership(candidate),
            catalog_truth: self.truth,
        }
    }

    fn catalog_membership(&self, candidate: &ModelCandidate) -> CatalogMembership {
        if candidate.in_catalog {
            CatalogMembership::Present
        } else if self.truth.availability == CatalogAvailability::Available
            && self.truth.freshness == CatalogFreshness::Current
        {
            CatalogMembership::Absent
        } else {
            CatalogMembership::Unobserved
        }
    }
}

/// Untrusted exact-selection input deliberately has no `Debug` or
/// serialization. It cannot carry an action, executable, or configuration.
pub(super) struct ModelSelectionInput {
    model_id: String,
}

impl ModelSelectionInput {
    pub(super) fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ModelSelectionProposal {
    model_id: String,
    basis: SelectionBasis,
    was_current: bool,
    was_default: bool,
    catalog_membership: CatalogMembership,
    catalog_truth: CatalogTruth,
}

impl ModelSelectionProposal {
    pub(super) const fn revision(&self) -> u16 {
        MODEL_SELECTION_PROPOSAL_REVISION
    }

    pub(super) fn model_id(&self) -> &str {
        &self.model_id
    }

    pub(super) const fn basis(&self) -> SelectionBasis {
        self.basis
    }

    pub(super) const fn was_current(&self) -> bool {
        self.was_current
    }

    pub(super) const fn was_default(&self) -> bool {
        self.was_default
    }

    pub(super) const fn catalog_membership(&self) -> CatalogMembership {
        self.catalog_membership
    }

    pub(super) const fn catalog_truth(&self) -> CatalogTruth {
        self.catalog_truth
    }
}

#[derive(PartialEq, Eq)]
pub(super) struct PickerQuery {
    filter: String,
    terminal_columns: usize,
    max_rows: usize,
}

impl PickerQuery {
    pub(super) fn new(
        filter: impl AsRef<str>,
        terminal_columns: usize,
        max_rows: usize,
    ) -> Result<Self, ModelPickerError> {
        if !(MIN_TERMINAL_COLUMNS..=MAX_TERMINAL_COLUMNS).contains(&terminal_columns) {
            return Err(ModelPickerError::TerminalWidthOutOfRange);
        }
        if !(1..=MAX_PROJECTED_ROWS).contains(&max_rows) {
            return Err(ModelPickerError::ProjectionRowsOutOfRange);
        }
        let filter = normalize_filter(filter.as_ref())?;
        Ok(Self {
            filter,
            terminal_columns,
            max_rows,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ModelPickerRow {
    model_id: String,
    display_id: String,
    is_current: bool,
    is_default: bool,
    pinned_outside_filter: bool,
    catalog_membership: CatalogMembership,
}

impl ModelPickerRow {
    pub(super) fn model_id(&self) -> &str {
        &self.model_id
    }

    pub(super) fn display_id(&self) -> &str {
        &self.display_id
    }

    pub(super) const fn is_current(&self) -> bool {
        self.is_current
    }

    pub(super) const fn is_default(&self) -> bool {
        self.is_default
    }

    pub(super) const fn is_pinned_outside_filter(&self) -> bool {
        self.pinned_outside_filter
    }

    pub(super) const fn catalog_membership(&self) -> CatalogMembership {
        self.catalog_membership
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ModelPickerView {
    truth: CatalogTruth,
    rows: Vec<ModelPickerRow>,
    candidate_count: usize,
    match_count: usize,
    omitted_count: usize,
    truncated: bool,
}

impl ModelPickerView {
    pub(super) const fn truth(&self) -> CatalogTruth {
        self.truth
    }

    pub(super) fn rows(&self) -> &[ModelPickerRow] {
        &self.rows
    }

    pub(super) const fn candidate_count(&self) -> usize {
        self.candidate_count
    }

    pub(super) const fn match_count(&self) -> usize {
        self.match_count
    }

    pub(super) const fn omitted_count(&self) -> usize {
        self.omitted_count
    }

    pub(super) const fn is_truncated(&self) -> bool {
        self.truncated
    }
}

fn normalize_model_id(raw: &str) -> Result<String, ModelPickerError> {
    let value = raw.trim();
    if value.is_empty()
        || value.len() > MAX_MODEL_ID_BYTES
        || value.contains("://")
        || !value.bytes().all(is_model_id_byte)
        || resembles_credential(value)
    {
        return Err(ModelPickerError::InvalidModelId);
    }
    Ok(value.to_string())
}

fn normalize_filter(raw: &str) -> Result<String, ModelPickerError> {
    let value = raw.trim();
    if value.len() > MAX_FILTER_BYTES
        || (!value.is_empty()
            && (value.contains("://")
                || !value.bytes().all(is_model_id_byte)
                || resembles_credential(value)))
    {
        return Err(ModelPickerError::InvalidFilter);
    }
    Ok(value.to_ascii_lowercase())
}

const fn is_model_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':' | b'+' | b'@')
}

fn resembles_credential(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let long_prefixed = [
        "sk-",
        "hf_",
        "ghp_",
        "gho_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
    ];
    lower.starts_with("bearer-")
        || lower.starts_with("bearer_")
        || (value.len() >= 24 && long_prefixed.iter().any(|prefix| lower.starts_with(prefix)))
}

fn model_match_rank(model_id: &str, filter: &str) -> Option<u8> {
    if filter.is_empty() {
        return Some(3);
    }
    let lower = model_id.to_ascii_lowercase();
    if lower == filter {
        Some(0)
    } else if lower.starts_with(filter) {
        Some(1)
    } else if lower
        .rsplit_once('/')
        .is_some_and(|(_, basename)| basename.starts_with(filter))
    {
        Some(2)
    } else if lower.contains(filter) {
        Some(3)
    } else {
        None
    }
}

fn compare_ranked(
    left: (&ModelCandidate, Option<u8>),
    right: (&ModelCandidate, Option<u8>),
) -> Ordering {
    right
        .0
        .is_current
        .cmp(&left.0.is_current)
        .then_with(|| left.1.unwrap_or(u8::MAX).cmp(&right.1.unwrap_or(u8::MAX)))
        .then_with(|| right.0.is_default.cmp(&left.0.is_default))
        .then_with(|| compare_identifier(&left.0.id, &right.0.id))
}

fn compare_identifier(left: &str, right: &str) -> Ordering {
    left.to_ascii_lowercase()
        .cmp(&right.to_ascii_lowercase())
        .then_with(|| left.cmp(right))
}

fn truncate_identifier(model_id: &str, columns: usize) -> String {
    if model_id.len() <= columns {
        return model_id.to_string();
    }
    let mut display = String::with_capacity(columns + '…'.len_utf8());
    display.extend(model_id.bytes().take(columns - 1).map(char::from));
    display.push('…');
    display
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_input(
        ids: &[&str],
        current: Option<&str>,
        default: Option<&str>,
    ) -> CatalogSnapshotInput {
        CatalogSnapshotInput::new(
            CatalogTruth::new(
                CatalogAvailability::Available,
                CatalogSource::Live,
                CatalogFreshness::Current,
                CatalogProvenance::ProviderObservation,
            ),
            ids.iter().copied(),
            current,
            default,
        )
    }

    fn cached_truth(
        availability: CatalogAvailability,
        freshness: CatalogFreshness,
    ) -> CatalogTruth {
        CatalogTruth::new(
            availability,
            CatalogSource::Cache,
            freshness,
            CatalogProvenance::RetainedSnapshot,
        )
    }

    fn query(filter: &str, columns: usize, rows: usize) -> PickerQuery {
        PickerQuery::new(filter, columns, rows).unwrap()
    }

    #[test]
    fn truth_matrix_names_available_offline_stale_and_unavailable_without_free_form_detail() {
        let valid = [
            CatalogTruth::new(
                CatalogAvailability::Available,
                CatalogSource::Live,
                CatalogFreshness::Current,
                CatalogProvenance::ProviderObservation,
            ),
            cached_truth(CatalogAvailability::Available, CatalogFreshness::Current),
            cached_truth(CatalogAvailability::Available, CatalogFreshness::Stale),
            cached_truth(CatalogAvailability::Offline, CatalogFreshness::Current),
            cached_truth(CatalogAvailability::Offline, CatalogFreshness::Stale),
            CatalogTruth::new(
                CatalogAvailability::Offline,
                CatalogSource::None,
                CatalogFreshness::Unavailable,
                CatalogProvenance::None,
            ),
            CatalogTruth::new(
                CatalogAvailability::Unavailable(CatalogUnavailableReason::NotSupported),
                CatalogSource::None,
                CatalogFreshness::Unavailable,
                CatalogProvenance::None,
            ),
        ];
        for truth in valid {
            CatalogSnapshotInput::new(truth, std::iter::empty::<&str>(), Some("configured"), None)
                .normalize()
                .unwrap();
        }

        let invalid = [
            CatalogTruth::new(
                CatalogAvailability::Available,
                CatalogSource::None,
                CatalogFreshness::Unavailable,
                CatalogProvenance::None,
            ),
            CatalogTruth::new(
                CatalogAvailability::Unavailable(CatalogUnavailableReason::NotObserved),
                CatalogSource::Live,
                CatalogFreshness::Current,
                CatalogProvenance::ProviderObservation,
            ),
            CatalogTruth::new(
                CatalogAvailability::Available,
                CatalogSource::Cache,
                CatalogFreshness::Stale,
                CatalogProvenance::ProviderObservation,
            ),
            CatalogTruth::new(
                CatalogAvailability::Offline,
                CatalogSource::Live,
                CatalogFreshness::Current,
                CatalogProvenance::ProviderObservation,
            ),
        ];
        for truth in invalid {
            assert_eq!(
                CatalogSnapshotInput::new(truth, ["model"], None, None)
                    .normalize()
                    .unwrap_err(),
                ModelPickerError::ContradictoryCatalogTruth,
            );
        }
    }

    #[test]
    fn unavailable_truth_rejects_catalog_rows_but_keeps_configured_choices_offline() {
        let unavailable = CatalogTruth::new(
            CatalogAvailability::Unavailable(CatalogUnavailableReason::NotSupported),
            CatalogSource::None,
            CatalogFreshness::Unavailable,
            CatalogProvenance::None,
        );
        assert_eq!(
            CatalogSnapshotInput::new(unavailable, ["invented"], None, None)
                .normalize()
                .unwrap_err(),
            ModelPickerError::CatalogRowsWithoutSource,
        );

        let snapshot = CatalogSnapshotInput::new(
            unavailable,
            std::iter::empty::<&str>(),
            Some("current-model"),
            Some("default-model"),
        )
        .normalize()
        .unwrap();
        assert_eq!(snapshot.truth(), unavailable);
        assert_eq!(snapshot.model_count(), 2);
        assert_eq!(
            snapshot.stage_preferred().unwrap().model_id(),
            "current-model"
        );

        let offline = CatalogSnapshotInput::new(
            cached_truth(CatalogAvailability::Offline, CatalogFreshness::Stale),
            ["cached-b", "cached-a"],
            None,
            Some("cached-b"),
        )
        .normalize()
        .unwrap();
        assert_eq!(offline.truth().availability(), CatalogAvailability::Offline);
        assert_eq!(offline.truth().freshness(), CatalogFreshness::Stale);
        assert_eq!(offline.stage_preferred().unwrap().model_id(), "cached-b");
    }

    #[test]
    fn model_ids_normalize_dedupe_and_union_current_default_without_inference() {
        let snapshot = live_input(
            &["  zoo/model  ", "alpha/model", "zoo/model", "Beta/model"],
            Some(" configured/current "),
            Some("alpha/model"),
        )
        .normalize()
        .unwrap();

        assert_eq!(snapshot.model_count(), 4);
        assert_eq!(snapshot.current_model(), Some("configured/current"));
        assert_eq!(snapshot.default_model(), Some("alpha/model"));
        let view = snapshot.project(&query("", 80, 16));
        let ids: Vec<_> = view.rows().iter().map(ModelPickerRow::model_id).collect();
        assert_eq!(
            ids,
            [
                "configured/current",
                "alpha/model",
                "Beta/model",
                "zoo/model"
            ]
        );
        assert!(view.rows()[0].is_current());
        assert!(!view.rows()[0].is_default());
        assert!(view.rows()[1].is_default());
        assert_eq!(
            view.rows()[1].catalog_membership(),
            CatalogMembership::Present
        );
        assert_eq!(
            view.rows()[0].catalog_membership(),
            CatalogMembership::Absent
        );
    }

    #[test]
    fn filter_ranking_is_deterministic_and_current_remains_pinned() {
        let snapshot = live_input(
            &[
                "vendor/beta-plus",
                "beta",
                "vendor/alpha-beta",
                "alphabeta",
                "beta-v2",
            ],
            Some("current/unmatched"),
            Some("beta-v2"),
        )
        .normalize()
        .unwrap();

        let first = snapshot.project(&query("beta", 80, 16));
        let second = snapshot.project(&query("BETA", 80, 16));
        let expected = [
            "current/unmatched",
            "beta",
            "beta-v2",
            "vendor/beta-plus",
            "alphabeta",
            "vendor/alpha-beta",
        ];
        let first_ids: Vec<_> = first.rows().iter().map(ModelPickerRow::model_id).collect();
        let second_ids: Vec<_> = second.rows().iter().map(ModelPickerRow::model_id).collect();
        assert_eq!(first_ids, expected);
        assert_eq!(second_ids, expected);
        assert!(first.rows()[0].is_pinned_outside_filter());
        assert_eq!(first.match_count(), 5);
        assert_eq!(first.candidate_count(), 6);
        assert_eq!(first.omitted_count(), 0);
    }

    #[test]
    fn preferred_and_exact_selection_return_staged_proposals_only() {
        let snapshot = live_input(
            &["catalog-b", "catalog-a"],
            None,
            Some("configured-default"),
        )
        .normalize()
        .unwrap();

        let preferred = snapshot.stage_preferred().unwrap();
        assert_eq!(preferred.revision(), MODEL_SELECTION_PROPOSAL_REVISION);
        assert_eq!(preferred.model_id(), "configured-default");
        assert_eq!(preferred.basis(), SelectionBasis::ConfiguredDefault);
        assert_eq!(preferred.catalog_membership(), CatalogMembership::Absent);
        assert_eq!(preferred.catalog_truth(), snapshot.truth());

        let explicit = snapshot
            .stage_exact(ModelSelectionInput::new(" catalog-b "))
            .unwrap();
        assert_eq!(explicit.model_id(), "catalog-b");
        assert_eq!(explicit.basis(), SelectionBasis::Explicit);
        assert_eq!(explicit.catalog_membership(), CatalogMembership::Present);
        assert!(!explicit.was_current());
        assert!(!explicit.was_default());

        assert_eq!(
            snapshot
                .stage_exact(ModelSelectionInput::new("unknown"))
                .unwrap_err(),
            ModelPickerError::UnknownModel,
        );
    }

    #[test]
    fn empty_preferred_selection_is_truthful() {
        let snapshot = CatalogSnapshotInput::new(
            CatalogTruth::new(
                CatalogAvailability::Unavailable(CatalogUnavailableReason::NotObserved),
                CatalogSource::None,
                CatalogFreshness::Unavailable,
                CatalogProvenance::None,
            ),
            std::iter::empty::<&str>(),
            None,
            None,
        )
        .normalize()
        .unwrap();
        assert_eq!(
            snapshot.stage_preferred().unwrap_err(),
            ModelPickerError::NoSelectableModel,
        );
        let view = snapshot.project(&query("", 20, 4));
        assert!(view.rows().is_empty());
        assert_eq!(view.truth(), snapshot.truth());
    }

    #[test]
    fn catalog_membership_is_unobserved_when_catalog_truth_cannot_prove_absence() {
        let unavailable = CatalogSnapshotInput::new(
            CatalogTruth::new(
                CatalogAvailability::Unavailable(CatalogUnavailableReason::FetchFailed),
                CatalogSource::None,
                CatalogFreshness::Unavailable,
                CatalogProvenance::None,
            ),
            std::iter::empty::<&str>(),
            Some("configured-current"),
            None,
        )
        .normalize()
        .unwrap();
        let proposal = unavailable.stage_preferred().unwrap();
        assert_eq!(proposal.catalog_membership(), CatalogMembership::Unobserved);
        assert_eq!(proposal.catalog_truth(), unavailable.truth());

        let offline = CatalogSnapshotInput::new(
            cached_truth(CatalogAvailability::Offline, CatalogFreshness::Stale),
            ["cached-model"],
            Some("configured-current"),
            None,
        )
        .normalize()
        .unwrap();
        let rows = offline.project(&query("", 80, 8));
        assert_eq!(
            rows.rows()[0].catalog_membership(),
            CatalogMembership::Unobserved
        );
        assert_eq!(
            rows.rows()[1].catalog_membership(),
            CatalogMembership::Present
        );
        assert_eq!(rows.truth().source(), CatalogSource::Cache);
        assert_eq!(rows.truth().freshness(), CatalogFreshness::Stale);

        let stale_available = CatalogSnapshotInput::new(
            cached_truth(CatalogAvailability::Available, CatalogFreshness::Stale),
            ["retained-model"],
            None,
            Some("configured-default"),
        )
        .normalize()
        .unwrap();
        assert_eq!(
            stale_available
                .stage_preferred()
                .unwrap()
                .catalog_membership(),
            CatalogMembership::Unobserved
        );
    }

    #[test]
    fn narrow_projection_is_bounded_without_losing_exact_selection_identity() {
        let snapshot = live_input(
            &["vendor/a-very-long-model-identifier", "tiny"],
            Some("vendor/a-very-long-model-identifier"),
            Some("tiny"),
        )
        .normalize()
        .unwrap();
        let view = snapshot.project(&query("", MIN_TERMINAL_COLUMNS, 1));
        assert_eq!(view.rows().len(), 1);
        let row = &view.rows()[0];
        assert_eq!(row.model_id(), "vendor/a-very-long-model-identifier");
        assert_eq!(row.display_id(), "vendor/…");
        assert!(row.display_id().chars().count() <= MIN_TERMINAL_COLUMNS);
        assert_eq!(view.omitted_count(), 1);
        assert!(view.is_truncated());

        let proposal = snapshot
            .stage_exact(ModelSelectionInput::new(row.model_id()))
            .unwrap();
        assert_eq!(proposal.model_id(), row.model_id());
    }

    #[test]
    fn raw_and_normalized_resource_limits_fail_before_projection() {
        let too_many = (0..=MAX_RAW_CATALOG_ROWS).map(|index| format!("model-{index}"));
        assert_eq!(
            CatalogSnapshotInput::new(
                CatalogTruth::new(
                    CatalogAvailability::Available,
                    CatalogSource::Live,
                    CatalogFreshness::Current,
                    CatalogProvenance::ProviderObservation,
                ),
                too_many,
                None,
                None,
            )
            .normalize()
            .unwrap_err(),
            ModelPickerError::TooManyCatalogRows,
        );

        let long = "m".repeat(MAX_MODEL_ID_BYTES + 1);
        assert_eq!(
            live_input(&[&long], None, None).normalize().unwrap_err(),
            ModelPickerError::InvalidModelId,
        );

        let each = "m".repeat((MAX_CATALOG_BYTES / MAX_RAW_CATALOG_ROWS) + 1);
        let total_overflow = (0..MAX_RAW_CATALOG_ROWS).map(|index| format!("{each}{index}"));
        assert_eq!(
            CatalogSnapshotInput::new(
                CatalogTruth::new(
                    CatalogAvailability::Available,
                    CatalogSource::Live,
                    CatalogFreshness::Current,
                    CatalogProvenance::ProviderObservation,
                ),
                total_overflow,
                None,
                None,
            )
            .normalize()
            .unwrap_err(),
            ModelPickerError::CatalogBytesExceeded,
        );

        let at_candidate_cap = (0..MAX_NORMALIZED_MODELS).map(|index| format!("m{index:04}"));
        assert_eq!(
            CatalogSnapshotInput::new(
                CatalogTruth::new(
                    CatalogAvailability::Available,
                    CatalogSource::Live,
                    CatalogFreshness::Current,
                    CatalogProvenance::ProviderObservation,
                ),
                at_candidate_cap,
                Some("one-extra-current"),
                None,
            )
            .normalize()
            .unwrap_err(),
            ModelPickerError::TooManySelectableModels,
        );

        let base = live_input(&["model"], None, None).normalize().unwrap();
        assert_eq!(
            PickerQuery::new("x".repeat(MAX_FILTER_BYTES + 1), 80, 12)
                .err()
                .unwrap(),
            ModelPickerError::InvalidFilter,
        );
        assert_eq!(
            PickerQuery::new("", MIN_TERMINAL_COLUMNS - 1, 12)
                .err()
                .unwrap(),
            ModelPickerError::TerminalWidthOutOfRange,
        );
        assert_eq!(
            PickerQuery::new("", MAX_TERMINAL_COLUMNS + 1, 12)
                .err()
                .unwrap(),
            ModelPickerError::TerminalWidthOutOfRange,
        );
        assert_eq!(
            PickerQuery::new("", 80, 0).err().unwrap(),
            ModelPickerError::ProjectionRowsOutOfRange,
        );
        assert_eq!(
            PickerQuery::new("", 80, MAX_PROJECTED_ROWS + 1)
                .err()
                .unwrap(),
            ModelPickerError::ProjectionRowsOutOfRange,
        );
        assert_eq!(base.model_count(), 1);
    }

    #[test]
    fn secret_shaped_and_unsafe_ids_are_rejected_with_non_echoing_errors() {
        let unsafe_values = [
            "sk-abcdefghijklmnopqrstuvwxyz123456",
            "hf_abcdefghijklmnopqrstuvwxyz123456",
            "Bearer-token-value",
            "model?api_key=value",
            "model#token=value",
            "model name",
            "model\nsecret",
            "model=value",
            "https://user@models.example/model",
        ];
        for value in unsafe_values {
            let error = live_input(&[value], None, None).normalize().unwrap_err();
            assert_eq!(error, ModelPickerError::InvalidModelId);
            let debug = format!("{error:?}");
            assert!(!debug.contains(value), "raw value leaked: {debug}");
        }

        let query_error = PickerQuery::new("sk-abcdefghijklmnopqrstuvwxyz123456", 80, 12)
            .err()
            .unwrap();
        assert_eq!(query_error, ModelPickerError::InvalidFilter);
        assert!(!format!("{query_error:?}").contains("abcdefghijklmnopqrstuvwxyz"));

        let real = live_input(&["org/model:tag"], None, None)
            .normalize()
            .unwrap();
        assert_eq!(real.stage_preferred().unwrap().model_id(), "org/model:tag");
    }
}
