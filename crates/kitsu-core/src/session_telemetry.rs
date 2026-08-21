//! Presentation-neutral current-session telemetry snapshots.
//!
//! This module labels facts that Kitsu already owns; it does not discover
//! facts, render terminal output, contact a provider, read configuration, or
//! persist history. In particular, missing provider usage and portable
//! currency cost remain unavailable rather than becoming plausible-looking
//! zeroes.

use kitsu_providers::{
    ProviderAdapterId, ProviderCapability, ProviderRouteReceipt, ProviderWireRevision,
};
use serde::{Deserialize, Serialize};

use crate::{RunId, SessionId};

pub const CURRENT_SESSION_SNAPSHOT_REVISION: u32 = 1;

/// The epistemic class of one snapshot value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruthClass {
    Authoritative,
    ProviderReported,
    Derived,
    Estimated,
    Unavailable,
}

/// Bounded sources from which current-session snapshot values may be built.
///
/// Keeping provenance closed prevents a renderer from smuggling a credential,
/// endpoint, response body, or other ambient string into a diagnostic deck.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextWindowSource {
    CommandLine,
    GatewayProfile,
    DiskCache,
    LiveProviderDiscovery,
    ConservativeDefault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryProvenance {
    ProviderRouteReceipt,
    RouteModelUnavailable,
    RuntimeIdentity,
    SessionIdentityUnavailable,
    RunIdentityUnavailable,
    EffectiveAuthorityInput,
    ContextWindow(ContextWindowSource),
    ContextBudgetArithmetic,
    ProviderAnchoredContextMeter,
    ContextMeterEstimate,
    ProviderUsageReports,
    ProviderUsageNotReported,
    ProviderUsageNotItemized,
    SessionAccounting,
    PortableCostUnavailable,
    ExplicitCostObservation,
}

/// A value plus exactly one truth class and one bounded provenance source.
/// `Unavailable` structurally has no value, so serializers cannot turn missing
/// provider data into zero by accident.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "truth", rename_all = "snake_case")]
pub enum TelemetryValue<T> {
    Authoritative {
        value: T,
        provenance: TelemetryProvenance,
    },
    ProviderReported {
        value: T,
        provenance: TelemetryProvenance,
    },
    Derived {
        value: T,
        provenance: TelemetryProvenance,
    },
    Estimated {
        value: T,
        provenance: TelemetryProvenance,
    },
    Unavailable {
        provenance: TelemetryProvenance,
    },
}

impl<T> TelemetryValue<T> {
    pub fn authoritative(value: T, provenance: TelemetryProvenance) -> Self {
        Self::Authoritative { value, provenance }
    }

    pub fn provider_reported(value: T, provenance: TelemetryProvenance) -> Self {
        Self::ProviderReported { value, provenance }
    }

    pub fn derived(value: T, provenance: TelemetryProvenance) -> Self {
        Self::Derived { value, provenance }
    }

    pub fn estimated(value: T, provenance: TelemetryProvenance) -> Self {
        Self::Estimated { value, provenance }
    }

    pub fn unavailable(provenance: TelemetryProvenance) -> Self {
        Self::Unavailable { provenance }
    }

    pub fn truth_class(&self) -> TruthClass {
        match self {
            Self::Authoritative { .. } => TruthClass::Authoritative,
            Self::ProviderReported { .. } => TruthClass::ProviderReported,
            Self::Derived { .. } => TruthClass::Derived,
            Self::Estimated { .. } => TruthClass::Estimated,
            Self::Unavailable { .. } => TruthClass::Unavailable,
        }
    }

    pub fn provenance(&self) -> TelemetryProvenance {
        match self {
            Self::Authoritative { provenance, .. }
            | Self::ProviderReported { provenance, .. }
            | Self::Derived { provenance, .. }
            | Self::Estimated { provenance, .. }
            | Self::Unavailable { provenance } => *provenance,
        }
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Authoritative { value, .. }
            | Self::ProviderReported { value, .. }
            | Self::Derived { value, .. }
            | Self::Estimated { value, .. } => Some(value),
            Self::Unavailable { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProviderRouteSnapshot {
    pub adapter: TelemetryValue<ProviderAdapterId>,
    pub profile: TelemetryValue<String>,
    pub generation: TelemetryValue<u64>,
    pub wire_revision: TelemetryValue<ProviderWireRevision>,
    pub model: TelemetryValue<String>,
    pub capabilities: TelemetryValue<Vec<ProviderCapability>>,
}

impl ActiveProviderRouteSnapshot {
    fn from_receipt(receipt: &ProviderRouteReceipt) -> Self {
        let route = receipt.route();
        let provenance = TelemetryProvenance::ProviderRouteReceipt;
        Self {
            adapter: TelemetryValue::authoritative(route.adapter(), provenance),
            profile: TelemetryValue::authoritative(
                route.profile().as_str().to_string(),
                provenance,
            ),
            generation: TelemetryValue::authoritative(route.generation().get(), provenance),
            wire_revision: TelemetryValue::authoritative(route.wire_revision(), provenance),
            model: receipt.model().map_or_else(
                || TelemetryValue::unavailable(TelemetryProvenance::RouteModelUnavailable),
                |model| TelemetryValue::authoritative(model.to_string(), provenance),
            ),
            capabilities: TelemetryValue::authoritative(
                route.capabilities().supported().collect(),
                provenance,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveAuthorityMode {
    Ask,
    Scoped,
    AutoAllow,
    Deny,
}

/// Explicit host input. Snapshot construction performs no permission lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveAuthorityInput {
    pub mode: EffectiveAuthorityMode,
    pub read_roots: u64,
    pub write_roots: u64,
    pub protected_roots: u64,
    pub exact_command_grants: u64,
    pub commands_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveAuthoritySnapshot {
    pub mode: TelemetryValue<EffectiveAuthorityMode>,
    pub read_roots: TelemetryValue<u64>,
    pub write_roots: TelemetryValue<u64>,
    pub protected_roots: TelemetryValue<u64>,
    pub exact_command_grants: TelemetryValue<u64>,
    pub commands_enabled: TelemetryValue<bool>,
}

impl From<EffectiveAuthorityInput> for EffectiveAuthoritySnapshot {
    fn from(input: EffectiveAuthorityInput) -> Self {
        let provenance = TelemetryProvenance::EffectiveAuthorityInput;
        Self {
            mode: TelemetryValue::authoritative(input.mode, provenance),
            read_roots: TelemetryValue::authoritative(input.read_roots, provenance),
            write_roots: TelemetryValue::authoritative(input.write_roots, provenance),
            protected_roots: TelemetryValue::authoritative(input.protected_roots, provenance),
            exact_command_grants: TelemetryValue::authoritative(
                input.exact_command_grants,
                provenance,
            ),
            commands_enabled: TelemetryValue::authoritative(input.commands_enabled, provenance),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSnapshotInput {
    pub window_tokens: TelemetryValue<u64>,
    pub effective_tokens: TelemetryValue<u64>,
    pub used_tokens: TelemetryValue<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub window_tokens: TelemetryValue<u64>,
    pub effective_tokens: TelemetryValue<u64>,
    pub used_tokens: TelemetryValue<u64>,
    pub available_tokens: TelemetryValue<u64>,
}

impl From<ContextSnapshotInput> for ContextSnapshot {
    fn from(input: ContextSnapshotInput) -> Self {
        let available_tokens = match (
            input.effective_tokens.value().copied(),
            input.used_tokens.value().copied(),
        ) {
            (Some(effective), Some(used)) => TelemetryValue::derived(
                effective.saturating_sub(used),
                TelemetryProvenance::ContextBudgetArithmetic,
            ),
            _ => TelemetryValue::unavailable(TelemetryProvenance::ContextBudgetArithmetic),
        };
        Self {
            window_tokens: input.window_tokens,
            effective_tokens: input.effective_tokens,
            used_tokens: input.used_tokens,
            available_tokens,
        }
    }
}

/// Fixed-point currency amount from an explicitly named upstream observation.
/// Snapshot construction never derives this from model names or token counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyAmount {
    pub currency_code: String,
    pub minor_units: u64,
    pub decimal_places: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCostInput(TelemetryValue<CurrencyAmount>);

impl SessionCostInput {
    pub fn unavailable() -> Self {
        Self(TelemetryValue::unavailable(
            TelemetryProvenance::PortableCostUnavailable,
        ))
    }

    pub fn explicit(value: TelemetryValue<CurrencyAmount>) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProviderUsageTotals {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    cached_prompt_tokens: Option<u64>,
}

/// Explicit current-process inputs. Coverage counts remain separate from token
/// values so a provider that omitted usage cannot be mistaken for zero spend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionUsageInput {
    turns: u64,
    responses_reporting_usage: u64,
    responses_missing_usage: u64,
    provider_usage: Option<ProviderUsageTotals>,
    cost: SessionCostInput,
}

impl SessionUsageInput {
    #[allow(clippy::too_many_arguments)]
    pub fn provider_reported(
        responses_reporting_usage: u64,
        responses_missing_usage: u64,
        turns: u64,
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
        cached_prompt_tokens: Option<u64>,
        cost: SessionCostInput,
    ) -> Self {
        Self {
            turns,
            responses_reporting_usage,
            responses_missing_usage,
            provider_usage: Some(ProviderUsageTotals {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cached_prompt_tokens,
            }),
            cost,
        }
    }

    pub fn no_provider_usage(
        turns: u64,
        responses_missing_usage: u64,
        cost: SessionCostInput,
    ) -> Self {
        Self {
            turns,
            responses_reporting_usage: 0,
            responses_missing_usage,
            provider_usage: None,
            cost,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub turns: TelemetryValue<u64>,
    pub responses_reporting_usage: TelemetryValue<u64>,
    pub responses_missing_usage: TelemetryValue<u64>,
    pub prompt_tokens: TelemetryValue<u64>,
    pub completion_tokens: TelemetryValue<u64>,
    pub total_tokens: TelemetryValue<u64>,
    pub cached_prompt_tokens: TelemetryValue<u64>,
    pub monetary_cost: TelemetryValue<CurrencyAmount>,
}

impl From<SessionUsageInput> for UsageSnapshot {
    fn from(input: SessionUsageInput) -> Self {
        let coverage = TelemetryProvenance::SessionAccounting;
        let (prompt_tokens, completion_tokens, total_tokens, cached_prompt_tokens) =
            input.provider_usage.map_or_else(
                || {
                    let missing = TelemetryProvenance::ProviderUsageNotReported;
                    (
                        TelemetryValue::unavailable(missing),
                        TelemetryValue::unavailable(missing),
                        TelemetryValue::unavailable(missing),
                        TelemetryValue::unavailable(TelemetryProvenance::ProviderUsageNotItemized),
                    )
                },
                |usage| {
                    let reported = TelemetryProvenance::ProviderUsageReports;
                    (
                        TelemetryValue::provider_reported(usage.prompt_tokens, reported),
                        TelemetryValue::provider_reported(usage.completion_tokens, reported),
                        TelemetryValue::provider_reported(usage.total_tokens, reported),
                        usage.cached_prompt_tokens.map_or_else(
                            || {
                                TelemetryValue::unavailable(
                                    TelemetryProvenance::ProviderUsageNotItemized,
                                )
                            },
                            |tokens| TelemetryValue::provider_reported(tokens, reported),
                        ),
                    )
                },
            );
        Self {
            turns: TelemetryValue::authoritative(input.turns, coverage),
            responses_reporting_usage: TelemetryValue::authoritative(
                input.responses_reporting_usage,
                coverage,
            ),
            responses_missing_usage: TelemetryValue::authoritative(
                input.responses_missing_usage,
                coverage,
            ),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_prompt_tokens,
            monetary_cost: input.cost.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub route: ActiveProviderRouteSnapshot,
    pub session_id: TelemetryValue<String>,
    pub run_id: TelemetryValue<String>,
    pub authority: EffectiveAuthoritySnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentSessionSnapshotInput {
    pub provider_route: ProviderRouteReceipt,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub authority: EffectiveAuthorityInput,
    pub context: ContextSnapshotInput,
    pub usage: SessionUsageInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentSessionSnapshot {
    pub schema_revision: u32,
    pub status: StatusSnapshot,
    pub context: ContextSnapshot,
    pub usage: UsageSnapshot,
}

impl CurrentSessionSnapshot {
    pub fn from_input(input: CurrentSessionSnapshotInput) -> Self {
        let status = StatusSnapshot {
            route: ActiveProviderRouteSnapshot::from_receipt(&input.provider_route),
            session_id: input.session_id.map_or_else(
                || TelemetryValue::unavailable(TelemetryProvenance::SessionIdentityUnavailable),
                |session_id| {
                    TelemetryValue::authoritative(
                        session_id.as_str().to_string(),
                        TelemetryProvenance::RuntimeIdentity,
                    )
                },
            ),
            run_id: input.run_id.map_or_else(
                || TelemetryValue::unavailable(TelemetryProvenance::RunIdentityUnavailable),
                |run_id| {
                    TelemetryValue::authoritative(
                        run_id.as_str().to_string(),
                        TelemetryProvenance::RuntimeIdentity,
                    )
                },
            ),
            authority: input.authority.into(),
        };
        Self {
            schema_revision: CURRENT_SESSION_SNAPSHOT_REVISION,
            status,
            context: input.context.into(),
            usage: input.usage.into(),
        }
    }
}
