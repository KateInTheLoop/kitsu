//! Provider-neutral route identity and capability contract.
//!
//! These values describe an immutable compiled route generation without
//! containing an endpoint, credential, request, response, or provider config
//! object. Composition will publish them in later E04 cards; this module only
//! establishes validated values and pure pre-send admission.

use std::collections::BTreeSet;
use std::num::{NonZeroU32, NonZeroU64};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

pub const PROVIDER_ROUTE_CONTRACT_REVISION: u32 = 1;

/// Closed IDs reserved for compiled Kitsu adapter implementations. The
/// presence of a variant is not adapter promotion: composition may publish it
/// only after the matching implementation passes conformance. A config/profile
/// label cannot invent a new adapter or silently select a different wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderAdapterId {
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
}

/// Exact protocol revision spoken by one compiled adapter generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderWireRevision {
    #[serde(rename = "openai_chat_completions_v1")]
    OpenAiChatCompletionsV1,
    #[serde(rename = "anthropic_messages_2023_06_01")]
    AnthropicMessages2023_06_01,
}

/// Opaque, versioned profile identity. It is deliberately not a user-visible
/// gateway name: later composition derives the 128-bit lowercase hex payload
/// from the config namespace plus profile identity so same-label profiles in
/// different configs cannot collide.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ProviderProfileId(String);

impl ProviderProfileId {
    pub const PREFIX: &'static str = "profile-v1-";
    pub const HEX_BYTES: usize = 32;
    pub const MAX_BYTES: usize = Self::PREFIX.len() + Self::HEX_BYTES;

    pub fn new(value: impl Into<String>) -> Result<Self, ProviderRouteContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ProviderRouteContractError::EmptyProfileId);
        }
        if value.len() > Self::MAX_BYTES {
            return Err(ProviderRouteContractError::ProfileIdTooLong {
                actual_bytes: value.len(),
                max_bytes: Self::MAX_BYTES,
            });
        }
        if let Some((index, byte)) = value
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_'))
        {
            return Err(ProviderRouteContractError::InvalidProfileIdCharacter { index, byte });
        }
        let digest = value
            .strip_prefix(Self::PREFIX)
            .ok_or(ProviderRouteContractError::InvalidProfileIdFormat)?;
        if digest.len() != Self::HEX_BYTES
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ProviderRouteContractError::InvalidProfileIdFormat);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ProviderProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// Monotonic identity of one published route snapshot. Zero is never a live
/// generation, preventing omitted/defaulted state from looking current.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderRouteGeneration(NonZeroU64);

impl ProviderRouteGeneration {
    pub fn new(value: u64) -> Result<Self, ProviderRouteContractError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(ProviderRouteContractError::ZeroGeneration)
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Provider/model behavior that composition has positively admitted for one
/// immutable route. Absence means unsupported; model names never imply support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapability {
    Streaming,
    Tools,
    ImageInput,
    StructuredOutput,
    Usage,
    ModelCatalog,
}

/// Explicit capability snapshot with its own nonzero revision. A deterministic
/// set avoids display-order and duplicate ambiguity in receipts and caches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCapabilities {
    revision: NonZeroU32,
    supported: BTreeSet<ProviderCapability>,
}

impl ProviderCapabilities {
    pub fn new<I>(revision: u32, supported: I) -> Result<Self, ProviderRouteContractError>
    where
        I: IntoIterator<Item = ProviderCapability>,
    {
        let revision =
            NonZeroU32::new(revision).ok_or(ProviderRouteContractError::ZeroCapabilityRevision)?;
        let mut admitted = BTreeSet::new();
        for capability in supported {
            if !admitted.insert(capability) {
                return Err(ProviderRouteContractError::DuplicateCapability { capability });
            }
        }
        Ok(Self {
            revision,
            supported: admitted,
        })
    }

    pub fn revision(&self) -> u32 {
        self.revision.get()
    }

    pub fn supports(&self, capability: ProviderCapability) -> bool {
        self.supported.contains(&capability)
    }

    pub fn supported(&self) -> impl Iterator<Item = ProviderCapability> + '_ {
        self.supported.iter().copied()
    }
}

impl<'de> Deserialize<'de> for ProviderCapabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawCapabilities {
            revision: u32,
            supported: Vec<ProviderCapability>,
        }

        let raw = RawCapabilities::deserialize(deserializer)?;
        Self::new(raw.revision, raw.supported).map_err(D::Error::custom)
    }
}

/// Secret-free identity of one immutable provider route snapshot. Endpoint,
/// auth and model values will be bound behind this identity by E04-S03 rather
/// than serialized here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRouteIdentity {
    contract_revision: u32,
    adapter: ProviderAdapterId,
    profile: ProviderProfileId,
    generation: ProviderRouteGeneration,
    wire_revision: ProviderWireRevision,
    capabilities: ProviderCapabilities,
}

impl ProviderRouteIdentity {
    pub fn new(
        adapter: ProviderAdapterId,
        profile: ProviderProfileId,
        generation: ProviderRouteGeneration,
        wire_revision: ProviderWireRevision,
        capabilities: ProviderCapabilities,
    ) -> Result<Self, ProviderRouteContractError> {
        if !adapter_supports_wire(adapter, wire_revision) {
            return Err(ProviderRouteContractError::AdapterWireMismatch {
                adapter,
                wire_revision,
            });
        }
        Ok(Self {
            contract_revision: PROVIDER_ROUTE_CONTRACT_REVISION,
            adapter,
            profile,
            generation,
            wire_revision,
            capabilities,
        })
    }

    pub fn contract_revision(&self) -> u32 {
        self.contract_revision
    }

    pub fn adapter(&self) -> ProviderAdapterId {
        self.adapter
    }

    pub fn profile(&self) -> &ProviderProfileId {
        &self.profile
    }

    pub fn generation(&self) -> ProviderRouteGeneration {
        self.generation
    }

    pub fn wire_revision(&self) -> ProviderWireRevision {
        self.wire_revision
    }

    pub fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// Pure admission used before a physical send. Generation mismatch wins
    /// over feature checks so an obsolete route can never be made usable by
    /// its old capability set.
    pub fn validate_for_send(
        &self,
        current_generation: ProviderRouteGeneration,
        required: &[ProviderCapability],
    ) -> Result<(), ProviderRouteContractError> {
        if self.generation != current_generation {
            return Err(ProviderRouteContractError::StaleGeneration {
                bound: self.generation,
                current: current_generation,
            });
        }
        for capability in required {
            if !self.capabilities.supports(*capability) {
                return Err(ProviderRouteContractError::UnsupportedCapability {
                    capability: *capability,
                });
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ProviderRouteIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawRouteIdentity {
            contract_revision: u32,
            adapter: ProviderAdapterId,
            profile: ProviderProfileId,
            generation: ProviderRouteGeneration,
            wire_revision: ProviderWireRevision,
            capabilities: ProviderCapabilities,
        }

        let raw = RawRouteIdentity::deserialize(deserializer)?;
        if raw.contract_revision != PROVIDER_ROUTE_CONTRACT_REVISION {
            return Err(D::Error::custom(
                ProviderRouteContractError::UnsupportedContractRevision {
                    found: raw.contract_revision,
                    supported: PROVIDER_ROUTE_CONTRACT_REVISION,
                },
            ));
        }
        Self::new(
            raw.adapter,
            raw.profile,
            raw.generation,
            raw.wire_revision,
            raw.capabilities,
        )
        .map_err(D::Error::custom)
    }
}

fn adapter_supports_wire(adapter: ProviderAdapterId, wire: ProviderWireRevision) -> bool {
    matches!(
        (adapter, wire),
        (
            ProviderAdapterId::OpenAiCompatible,
            ProviderWireRevision::OpenAiChatCompletionsV1
        ) | (
            ProviderAdapterId::AnthropicMessages,
            ProviderWireRevision::AnthropicMessages2023_06_01
        )
    )
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProviderRouteContractError {
    #[error("provider profile identity must not be empty")]
    EmptyProfileId,
    #[error("provider profile identity is {actual_bytes} bytes; maximum is {max_bytes}")]
    ProfileIdTooLong {
        actual_bytes: usize,
        max_bytes: usize,
    },
    #[error("provider profile identity has an invalid byte {byte} at offset {index}")]
    InvalidProfileIdCharacter { index: usize, byte: u8 },
    #[error("provider profile identity must be profile-v1- plus 32 lowercase hex digits")]
    InvalidProfileIdFormat,
    #[error("provider route generation must be nonzero")]
    ZeroGeneration,
    #[error("provider capability revision must be nonzero")]
    ZeroCapabilityRevision,
    #[error("provider capability {capability:?} is duplicated")]
    DuplicateCapability { capability: ProviderCapability },
    #[error("provider route contract revision {found} is unsupported; supported revision is {supported}")]
    UnsupportedContractRevision { found: u32, supported: u32 },
    #[error("provider adapter {adapter:?} cannot speak wire revision {wire_revision:?}")]
    AdapterWireMismatch {
        adapter: ProviderAdapterId,
        wire_revision: ProviderWireRevision,
    },
    #[error("provider route generation {bound:?} is stale; current generation is {current:?}")]
    StaleGeneration {
        bound: ProviderRouteGeneration,
        current: ProviderRouteGeneration,
    },
    #[error("provider route does not support {capability:?}")]
    UnsupportedCapability { capability: ProviderCapability },
}
