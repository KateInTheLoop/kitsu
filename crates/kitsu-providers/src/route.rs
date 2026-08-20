//! Provider-neutral route identity and capability contract.
//!
//! These values describe an immutable compiled route generation without
//! containing an endpoint, credential, request, response, or provider config
//! object. Composition will publish them in later E04 cards; this module only
//! establishes validated values and pure pre-send admission.

use std::collections::BTreeSet;
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ring::digest::{Context, SHA256};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::error::ProviderError;
use crate::telemetry::ProviderTelemetrySink;
use crate::types::ChatRequest;
use crate::{ChatStream, Provider};

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
/// gateway name: composition derives the 128-bit lowercase hex payload from
/// the config namespace plus profile identity so same-label profiles in
/// different configs do not share identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ProviderProfileId(String);

impl ProviderProfileId {
    pub const PREFIX: &'static str = "profile-v1-";
    pub const HEX_BYTES: usize = 32;
    pub const MAX_BYTES: usize = Self::PREFIX.len() + Self::HEX_BYTES;
    pub const MAX_NAMESPACE_BYTES: usize = 64 * 1024;
    pub const MAX_PROFILE_NAME_BYTES: usize = 1024;

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

    /// Derive a collision-resistant opaque identity from a canonical config
    /// namespace and the gateway key within that config. Endpoint, model,
    /// flavor, capability, and credential values are deliberately not inputs:
    /// those can change while the logical profile identity stays stable and a
    /// later route generation records the replacement.
    pub fn derive_v1(
        namespace: impl AsRef<[u8]>,
        profile_name: impl AsRef<[u8]>,
    ) -> Result<Self, ProviderRouteContractError> {
        let namespace = namespace.as_ref();
        let profile_name = profile_name.as_ref();
        if namespace.is_empty() {
            return Err(ProviderRouteContractError::EmptyProfileNamespace);
        }
        if namespace.len() > Self::MAX_NAMESPACE_BYTES {
            return Err(ProviderRouteContractError::ProfileNamespaceTooLong {
                actual_bytes: namespace.len(),
                max_bytes: Self::MAX_NAMESPACE_BYTES,
            });
        }
        if profile_name.is_empty() {
            return Err(ProviderRouteContractError::EmptyProfileName);
        }
        if profile_name.len() > Self::MAX_PROFILE_NAME_BYTES {
            return Err(ProviderRouteContractError::ProfileNameTooLong {
                actual_bytes: profile_name.len(),
                max_bytes: Self::MAX_PROFILE_NAME_BYTES,
            });
        }

        // Length framing makes the two inputs unambiguous. SHA-256 is truncated
        // to 128 bits to match the v1 serialized profile shape while retaining
        // collision resistance appropriate for a local identity namespace.
        let mut digest = Context::new(&SHA256);
        digest.update(b"kitsu.provider-profile.v1\0");
        digest.update(&(namespace.len() as u64).to_be_bytes());
        digest.update(namespace);
        digest.update(&(profile_name.len() as u64).to_be_bytes());
        digest.update(profile_name);
        let digest = digest.finish();
        let mut encoded = String::with_capacity(Self::MAX_BYTES);
        encoded.push_str(Self::PREFIX);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in &digest.as_ref()[..16] {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Self::new(encoded)
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

/// Secret-free origin of the credential frozen into one route generation.
/// This is safe for diagnostics; it never contains a variable name, header,
/// token, endpoint, or credential fingerprint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthSource {
    None,
    StaticConfiguration,
    Environment,
    ManagedCredential,
}

/// Whether a separately bounded model-catalog runtime belongs to a route.
/// The value must agree exactly with the declaration's `ModelCatalog`
/// capability before the route can be published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderCatalogPolicy {
    Unavailable,
    Configured,
}

impl ProviderCatalogPolicy {
    pub fn is_configured(self) -> bool {
        matches!(self, Self::Configured)
    }
}

/// Identifies one timeout inside an immutable route policy without copying a
/// duration or any route configuration into validation errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderRouteTimeout {
    Inference,
    Catalog,
    AdvisoryDiscovery,
}

/// Retry, catalog, and time-budget choices frozen alongside both physical
/// runtimes. Keeping this provider-neutral lets every compiled adapter publish
/// the same policy facts without importing CLI/config types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderRoutePolicy {
    inference_retries: u32,
    catalog_retries: u32,
    inference_timeout: Duration,
    catalog_timeout: Duration,
    advisory_discovery_timeout: Duration,
    catalog: ProviderCatalogPolicy,
}

impl ProviderRoutePolicy {
    pub fn new(
        inference_retries: u32,
        catalog_retries: u32,
        inference_timeout: Duration,
        catalog_timeout: Duration,
        advisory_discovery_timeout: Duration,
        catalog: ProviderCatalogPolicy,
    ) -> Result<Self, ProviderRouteContractError> {
        for (kind, timeout) in [
            (ProviderRouteTimeout::Inference, inference_timeout),
            (ProviderRouteTimeout::Catalog, catalog_timeout),
            (
                ProviderRouteTimeout::AdvisoryDiscovery,
                advisory_discovery_timeout,
            ),
        ] {
            if timeout.is_zero() {
                return Err(ProviderRouteContractError::ZeroRouteTimeout { kind });
            }
        }
        Ok(Self {
            inference_retries,
            catalog_retries,
            inference_timeout,
            catalog_timeout,
            advisory_discovery_timeout,
            catalog,
        })
    }

    pub fn inference_retries(self) -> u32 {
        self.inference_retries
    }

    pub fn catalog_retries(self) -> u32 {
        self.catalog_retries
    }

    pub fn inference_timeout(self) -> Duration {
        self.inference_timeout
    }

    pub fn catalog_timeout(self) -> Duration {
        self.catalog_timeout
    }

    pub fn advisory_discovery_timeout(self) -> Duration {
        self.advisory_discovery_timeout
    }

    pub fn catalog(self) -> ProviderCatalogPolicy {
        self.catalog
    }
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

/// A validated route declaration before a generation is published. Current
/// composition can state which compiled adapter/wire/capabilities it will use
/// without falsely claiming that all physical sends are generation-bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRouteDeclaration {
    adapter: ProviderAdapterId,
    profile: ProviderProfileId,
    wire_revision: ProviderWireRevision,
    capabilities: ProviderCapabilities,
}

impl ProviderRouteDeclaration {
    pub fn new(
        adapter: ProviderAdapterId,
        profile: ProviderProfileId,
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
            adapter,
            profile,
            wire_revision,
            capabilities,
        })
    }

    /// Truthful declaration for today's compiled compatible-Chat adapter.
    /// `Usage` means the decoder can normalize reported usage, not that every
    /// compatible endpoint promises to report it.
    pub fn compatible_chat_v1(profile: ProviderProfileId, model_catalog: bool) -> Self {
        let mut supported = vec![
            ProviderCapability::Streaming,
            ProviderCapability::Tools,
            ProviderCapability::StructuredOutput,
            ProviderCapability::Usage,
        ];
        if model_catalog {
            supported.push(ProviderCapability::ModelCatalog);
        }
        let capabilities = ProviderCapabilities::new(1, supported)
            .expect("the compiled compatible-Chat capability set is valid");
        Self::new(
            ProviderAdapterId::OpenAiCompatible,
            profile,
            ProviderWireRevision::OpenAiChatCompletionsV1,
            capabilities,
        )
        .expect("the compiled compatible-Chat adapter/wire pair is valid")
    }

    /// Truthful declaration for a management generation that can only read
    /// the compatible adapter's model catalog. The adapter/wire identify the
    /// compiled implementation; capabilities describe the narrower admitted
    /// operation surface.
    pub fn compatible_catalog_v1(profile: ProviderProfileId) -> Self {
        let capabilities = ProviderCapabilities::new(1, [ProviderCapability::ModelCatalog])
            .expect("the compiled compatible catalog capability set is valid");
        Self::new(
            ProviderAdapterId::OpenAiCompatible,
            profile,
            ProviderWireRevision::OpenAiChatCompletionsV1,
            capabilities,
        )
        .expect("the compiled compatible catalog adapter/wire pair is valid")
    }

    /// Truthful declaration for the compiled Anthropic Messages 2023-06-01
    /// adapter. Structured output, image input, and model catalog remain
    /// absent because the native implementation does not expose them.
    pub fn native_messages_v1(profile: ProviderProfileId) -> Self {
        let capabilities = ProviderCapabilities::new(
            1,
            [
                ProviderCapability::Streaming,
                ProviderCapability::Tools,
                ProviderCapability::Usage,
            ],
        )
        .expect("the compiled native Messages capability set is valid");
        Self::new(
            ProviderAdapterId::AnthropicMessages,
            profile,
            ProviderWireRevision::AnthropicMessages2023_06_01,
            capabilities,
        )
        .expect("the compiled native Messages adapter/wire pair is valid")
    }

    pub fn adapter(&self) -> ProviderAdapterId {
        self.adapter
    }

    pub fn profile(&self) -> &ProviderProfileId {
        &self.profile
    }

    pub fn wire_revision(&self) -> ProviderWireRevision {
        self.wire_revision
    }

    pub fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
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
        let declaration =
            ProviderRouteDeclaration::new(adapter, profile, wire_revision, capabilities)?;
        Ok(Self::from_declaration(declaration, generation))
    }

    pub fn from_declaration(
        declaration: ProviderRouteDeclaration,
        generation: ProviderRouteGeneration,
    ) -> Self {
        Self {
            contract_revision: PROVIDER_ROUTE_CONTRACT_REVISION,
            adapter: declaration.adapter,
            profile: declaration.profile,
            generation,
            wire_revision: declaration.wire_revision,
            capabilities: declaration.capabilities,
        }
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

/// One immutable runtime generation. The provider-specific runtime owns its
/// endpoint, paths, frozen auth material, caches, and transport configuration;
/// this provider-neutral wrapper owns the corresponding identity, model,
/// source class, and policy as one inseparable value.
///
/// This runtime value is intentionally neither `Debug` nor serializable: only
/// its secret-free identity and explicit receipt fields may cross diagnostic
/// boundaries.
pub struct ProviderRouteSnapshot<P> {
    identity: ProviderRouteIdentity,
    model: Option<String>,
    auth_source: ProviderAuthSource,
    policy: ProviderRoutePolicy,
    inference_runtime: Option<Arc<P>>,
    catalog_runtime: Arc<P>,
}

impl<P> ProviderRouteSnapshot<P> {
    pub const MAX_MODEL_ID_BYTES: usize = 4 * 1024;

    /// Construct the first published generation for one logical profile.
    /// Later replacement cards will allocate higher generations under a
    /// publication owner; callers cannot choose or reuse a generation here.
    pub fn initial(
        declaration: ProviderRouteDeclaration,
        model: impl Into<String>,
        auth_source: ProviderAuthSource,
        policy: ProviderRoutePolicy,
        inference_runtime: Arc<P>,
        catalog_runtime: Arc<P>,
    ) -> Result<Self, ProviderRouteContractError> {
        let generation = ProviderRouteGeneration::new(1)
            .expect("the initial provider route generation is nonzero");
        Self::with_generation(
            declaration,
            generation,
            model,
            auth_source,
            policy,
            inference_runtime,
            catalog_runtime,
        )
    }

    /// Construct an initial generation that can perform catalog management
    /// but has no selected model or inference runtime. This distinct
    /// constructor prevents catalog commands from using placeholder models.
    pub fn initial_catalog_only(
        declaration: ProviderRouteDeclaration,
        auth_source: ProviderAuthSource,
        policy: ProviderRoutePolicy,
        catalog_runtime: Arc<P>,
    ) -> Result<Self, ProviderRouteContractError> {
        let generation = ProviderRouteGeneration::new(1)
            .expect("the initial provider route generation is nonzero");
        Self::catalog_only_with_generation(
            declaration,
            generation,
            auth_source,
            policy,
            catalog_runtime,
        )
    }

    /// Construct, but do not publish, the exact successor of a catalog-only
    /// generation. This retains the same stale-writer and no-skip rules as a
    /// model-bound replacement without introducing an inference binding.
    pub fn successor_catalog_only(
        previous: &ProviderRouteIdentity,
        declaration: ProviderRouteDeclaration,
        auth_source: ProviderAuthSource,
        policy: ProviderRoutePolicy,
        catalog_runtime: Arc<P>,
    ) -> Result<Self, ProviderRouteContractError> {
        if declaration.profile() != previous.profile() {
            return Err(ProviderRouteContractError::ReplacementProfileMismatch);
        }
        let next = previous
            .generation()
            .get()
            .checked_add(1)
            .ok_or(ProviderRouteContractError::GenerationExhausted)?;
        let generation = ProviderRouteGeneration::new(next)
            .expect("incrementing a nonzero generation remains nonzero");
        Self::catalog_only_with_generation(
            declaration,
            generation,
            auth_source,
            policy,
            catalog_runtime,
        )
    }

    fn catalog_only_with_generation(
        declaration: ProviderRouteDeclaration,
        generation: ProviderRouteGeneration,
        auth_source: ProviderAuthSource,
        policy: ProviderRoutePolicy,
        catalog_runtime: Arc<P>,
    ) -> Result<Self, ProviderRouteContractError> {
        Self::validate_catalog_contract(&declaration, policy)?;
        if !declaration
            .capabilities()
            .supports(ProviderCapability::ModelCatalog)
        {
            return Err(ProviderRouteContractError::UnsupportedCapability {
                capability: ProviderCapability::ModelCatalog,
            });
        }
        if let Some(capability) = declaration
            .capabilities()
            .supported()
            .find(|capability| *capability != ProviderCapability::ModelCatalog)
        {
            return Err(ProviderRouteContractError::CatalogOnlyCapabilityMismatch { capability });
        }
        Ok(Self {
            identity: ProviderRouteIdentity::from_declaration(declaration, generation),
            model: None,
            auth_source,
            policy,
            inference_runtime: None,
            catalog_runtime,
        })
    }

    /// Construct the exact next generation for the same logical profile. This
    /// does not publish it; [`ProviderRouteCurrent::replace`] performs the
    /// expected-generation check and atomic swap.
    pub fn successor(
        previous: &ProviderRouteIdentity,
        declaration: ProviderRouteDeclaration,
        model: impl Into<String>,
        auth_source: ProviderAuthSource,
        policy: ProviderRoutePolicy,
        inference_runtime: Arc<P>,
        catalog_runtime: Arc<P>,
    ) -> Result<Self, ProviderRouteContractError> {
        if declaration.profile() != previous.profile() {
            return Err(ProviderRouteContractError::ReplacementProfileMismatch);
        }
        let next = previous
            .generation()
            .get()
            .checked_add(1)
            .ok_or(ProviderRouteContractError::GenerationExhausted)?;
        let generation = ProviderRouteGeneration::new(next)
            .expect("incrementing a nonzero generation remains nonzero");
        Self::with_generation(
            declaration,
            generation,
            model,
            auth_source,
            policy,
            inference_runtime,
            catalog_runtime,
        )
    }

    fn with_generation(
        declaration: ProviderRouteDeclaration,
        generation: ProviderRouteGeneration,
        model: impl Into<String>,
        auth_source: ProviderAuthSource,
        policy: ProviderRoutePolicy,
        inference_runtime: Arc<P>,
        catalog_runtime: Arc<P>,
    ) -> Result<Self, ProviderRouteContractError> {
        let model = Self::validated_model(model)?;
        Self::validate_catalog_contract(&declaration, policy)?;
        Ok(Self {
            identity: ProviderRouteIdentity::from_declaration(declaration, generation),
            model: Some(model),
            auth_source,
            policy,
            inference_runtime: Some(inference_runtime),
            catalog_runtime,
        })
    }

    fn validate_catalog_contract(
        declaration: &ProviderRouteDeclaration,
        policy: ProviderRoutePolicy,
    ) -> Result<(), ProviderRouteContractError> {
        let declared_catalog = declaration
            .capabilities()
            .supports(ProviderCapability::ModelCatalog);
        let configured_catalog = policy.catalog().is_configured();
        if declared_catalog != configured_catalog {
            return Err(ProviderRouteContractError::CatalogCapabilityMismatch {
                declared: declared_catalog,
                configured: configured_catalog,
            });
        }
        Ok(())
    }

    fn validated_model(model: impl Into<String>) -> Result<String, ProviderRouteContractError> {
        let model = model.into();
        if model.is_empty() {
            return Err(ProviderRouteContractError::EmptyModelId);
        }
        if model.len() > Self::MAX_MODEL_ID_BYTES {
            return Err(ProviderRouteContractError::ModelIdTooLong {
                actual_bytes: model.len(),
                max_bytes: Self::MAX_MODEL_ID_BYTES,
            });
        }
        Ok(model)
    }

    fn successor_model(
        &self,
        model: impl Into<String>,
    ) -> Result<Self, ProviderRouteContractError> {
        let inference_runtime = self
            .inference_runtime
            .clone()
            .ok_or(ProviderRouteContractError::InferenceUnavailable)?;
        let declaration = ProviderRouteDeclaration::new(
            self.identity.adapter(),
            self.identity.profile().clone(),
            self.identity.wire_revision(),
            self.identity.capabilities().clone(),
        )?;
        Self::successor(
            &self.identity,
            declaration,
            model,
            self.auth_source,
            self.policy,
            inference_runtime,
            self.catalog_runtime.clone(),
        )
    }

    pub fn identity(&self) -> &ProviderRouteIdentity {
        &self.identity
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn auth_source(&self) -> ProviderAuthSource {
        self.auth_source
    }

    pub fn policy(&self) -> ProviderRoutePolicy {
        self.policy
    }

    pub fn inference_runtime(&self) -> Option<&Arc<P>> {
        self.inference_runtime.as_ref()
    }

    pub fn catalog_runtime(&self) -> &Arc<P> {
        &self.catalog_runtime
    }
}

/// Atomic owner of the current snapshot for one process-local profile. A
/// handle binds one snapshot at creation; replacement changes what future
/// handles observe without mutating already admitted provider state.
pub struct ProviderRouteCurrent<P> {
    current: Arc<RwLock<Arc<ProviderRouteSnapshot<P>>>>,
}

impl<P> Clone for ProviderRouteCurrent<P> {
    fn clone(&self) -> Self {
        Self {
            current: self.current.clone(),
        }
    }
}

impl<P> ProviderRouteCurrent<P> {
    pub fn new(initial: ProviderRouteSnapshot<P>) -> Self {
        Self {
            current: Arc::new(RwLock::new(Arc::new(initial))),
        }
    }

    /// Bind future logical requests to the snapshot that is current now.
    pub fn bind(&self) -> Result<ProviderRouteHandle<P>, ProviderRouteContractError> {
        let bound = self
            .current
            .read()
            .map_err(|_| ProviderRouteContractError::CurrentRouteUnavailable)?
            .clone();
        let model = bound
            .model()
            .ok_or(ProviderRouteContractError::InferenceUnavailable)?
            .to_string();
        Ok(ProviderRouteHandle {
            bound,
            model,
            current: self.current.clone(),
        })
    }

    /// Bind a scoped model to the current route generation without changing
    /// the process default. This is used by explicit subagent profiles: model
    /// choice is immutable for that logical operation while endpoint, auth,
    /// wire, caches, and retry policy remain generation-bound.
    pub fn bind_model(
        &self,
        model: impl Into<String>,
    ) -> Result<ProviderRouteHandle<P>, ProviderRouteContractError> {
        let model = ProviderRouteSnapshot::<P>::validated_model(model)?;
        let bound = self
            .current
            .read()
            .map_err(|_| ProviderRouteContractError::CurrentRouteUnavailable)?
            .clone();
        if bound.inference_runtime().is_none() {
            return Err(ProviderRouteContractError::InferenceUnavailable);
        }
        Ok(ProviderRouteHandle {
            bound,
            model,
            current: self.current.clone(),
        })
    }

    /// Bind a catalog operation without inventing a selected model. Catalog
    /// capability remains an admission-time check so optional catalogs keep
    /// their graceful `None` contract.
    pub fn bind_catalog(&self) -> Result<ProviderCatalogHandle<P>, ProviderRouteContractError> {
        let bound = self
            .current
            .read()
            .map_err(|_| ProviderRouteContractError::CurrentRouteUnavailable)?
            .clone();
        Ok(ProviderCatalogHandle {
            bound,
            current: self.current.clone(),
        })
    }

    /// Publish exactly one successor if `expected` is still current. Any
    /// validation or lock failure leaves the old snapshot installed.
    pub fn replace(
        &self,
        expected: ProviderRouteGeneration,
        replacement: ProviderRouteSnapshot<P>,
    ) -> Result<(), ProviderRouteContractError> {
        let mut current = self
            .current
            .write()
            .map_err(|_| ProviderRouteContractError::CurrentRouteUnavailable)?;
        let actual = current.identity().generation();
        if actual != expected {
            return Err(ProviderRouteContractError::StaleGeneration {
                bound: expected,
                current: actual,
            });
        }
        if replacement.identity().profile() != current.identity().profile() {
            return Err(ProviderRouteContractError::ReplacementProfileMismatch);
        }
        let required = actual
            .get()
            .checked_add(1)
            .ok_or(ProviderRouteContractError::GenerationExhausted)?;
        if replacement.identity().generation().get() != required {
            return Err(ProviderRouteContractError::NonSuccessorGeneration {
                current: actual,
                proposed: replacement.identity().generation(),
            });
        }
        *current = Arc::new(replacement);
        Ok(())
    }

    /// Atomically replace only the process-default model. An unchanged model
    /// is a no-op; a changed model publishes exactly one successor. Validation
    /// and expected-generation failure leave the current snapshot untouched.
    pub fn replace_model(
        &self,
        expected: ProviderRouteGeneration,
        model: impl Into<String>,
    ) -> Result<ProviderRouteHandle<P>, ProviderRouteContractError> {
        let model = ProviderRouteSnapshot::<P>::validated_model(model)?;
        let mut current = self
            .current
            .write()
            .map_err(|_| ProviderRouteContractError::CurrentRouteUnavailable)?;
        let actual = current.identity().generation();
        if actual != expected {
            return Err(ProviderRouteContractError::StaleGeneration {
                bound: expected,
                current: actual,
            });
        }
        if current.inference_runtime().is_none() {
            return Err(ProviderRouteContractError::InferenceUnavailable);
        }
        if current.model() == Some(model.as_str()) {
            return Ok(ProviderRouteHandle {
                bound: current.clone(),
                model,
                current: self.current.clone(),
            });
        }
        let replacement = Arc::new(current.successor_model(model.clone())?);
        *current = replacement.clone();
        Ok(ProviderRouteHandle {
            bound: replacement,
            model,
            current: self.current.clone(),
        })
    }
}

/// One request-facing binding to an immutable route snapshot. Admission reads
/// the current identity once and returns an owned runtime `Arc`; after that an
/// already admitted request may drain through its bound retry policy even if a
/// replacement is published concurrently.
pub struct ProviderRouteHandle<P> {
    bound: Arc<ProviderRouteSnapshot<P>>,
    model: String,
    current: Arc<RwLock<Arc<ProviderRouteSnapshot<P>>>>,
}

impl<P> Clone for ProviderRouteHandle<P> {
    fn clone(&self) -> Self {
        Self {
            bound: self.bound.clone(),
            model: self.model.clone(),
            current: self.current.clone(),
        }
    }
}

impl<P> ProviderRouteHandle<P> {
    pub fn identity(&self) -> &ProviderRouteIdentity {
        self.bound.identity()
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn auth_source(&self) -> ProviderAuthSource {
        self.bound.auth_source()
    }

    pub fn policy(&self) -> ProviderRoutePolicy {
        self.bound.policy()
    }

    fn validate(&self, required: &[ProviderCapability]) -> Result<(), ProviderRouteContractError> {
        let current_generation = self
            .current
            .read()
            .map_err(|_| ProviderRouteContractError::CurrentRouteUnavailable)?
            .identity()
            .generation();
        self.bound
            .identity()
            .validate_for_send(current_generation, required)
    }

    pub(crate) fn admit_inference_runtime(
        &self,
        required: &[ProviderCapability],
    ) -> Result<Arc<P>, ProviderRouteContractError> {
        self.validate(required)?;
        self.bound
            .inference_runtime()
            .cloned()
            .ok_or(ProviderRouteContractError::InferenceUnavailable)
    }

    /// Preserve the provider port's optional-catalog contract: a current route
    /// without catalog capability reports absence without entering a runtime.
    /// Stale generations still fail before that graceful `None` result.
    pub(crate) fn admit_optional_catalog_runtime(
        &self,
    ) -> Result<Option<Arc<P>>, ProviderRouteContractError> {
        self.validate(&[])?;
        if !self
            .bound
            .identity()
            .capabilities()
            .supports(ProviderCapability::ModelCatalog)
        {
            return Ok(None);
        }
        Ok(Some(self.bound.catalog_runtime().clone()))
    }

    /// Use the inference runtime for an optional auxiliary capability while
    /// retaining the same current-generation and graceful-absence semantics.
    pub(crate) fn admit_optional_inference_runtime(
        &self,
        capability: ProviderCapability,
    ) -> Result<Option<Arc<P>>, ProviderRouteContractError> {
        self.validate(&[])?;
        if !self.bound.identity().capabilities().supports(capability) {
            return Ok(None);
        }
        self.bound
            .inference_runtime()
            .cloned()
            .map(Some)
            .ok_or(ProviderRouteContractError::InferenceUnavailable)
    }
}

/// One catalog-facing binding to an immutable route snapshot. It deliberately
/// has no `Provider` implementation, model accessor, or chat method: catalog
/// management cannot acquire inference authority by type confusion.
pub struct ProviderCatalogHandle<P> {
    bound: Arc<ProviderRouteSnapshot<P>>,
    current: Arc<RwLock<Arc<ProviderRouteSnapshot<P>>>>,
}

impl<P> Clone for ProviderCatalogHandle<P> {
    fn clone(&self) -> Self {
        Self {
            bound: self.bound.clone(),
            current: self.current.clone(),
        }
    }
}

impl<P> ProviderCatalogHandle<P> {
    pub fn identity(&self) -> &ProviderRouteIdentity {
        self.bound.identity()
    }

    pub fn auth_source(&self) -> ProviderAuthSource {
        self.bound.auth_source()
    }

    pub fn policy(&self) -> ProviderRoutePolicy {
        self.bound.policy()
    }

    fn validate(&self) -> Result<(), ProviderRouteContractError> {
        let current_generation = self
            .current
            .read()
            .map_err(|_| ProviderRouteContractError::CurrentRouteUnavailable)?
            .identity()
            .generation();
        self.bound
            .identity()
            .validate_for_send(current_generation, &[])
    }

    pub(crate) fn admit_optional_catalog_runtime(
        &self,
    ) -> Result<Option<Arc<P>>, ProviderRouteContractError> {
        self.validate()?;
        if !self
            .bound
            .identity()
            .capabilities()
            .supports(ProviderCapability::ModelCatalog)
        {
            return Ok(None);
        }
        Ok(Some(self.bound.catalog_runtime().clone()))
    }
}

impl<P> ProviderCatalogHandle<P>
where
    P: Provider,
{
    pub async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        let Some(runtime) = self.admit_optional_catalog_runtime()? else {
            return Ok(None);
        };
        runtime.list_models().await
    }

    pub async fn list_models_with_telemetry(
        &self,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        let Some(runtime) = self.admit_optional_catalog_runtime()? else {
            return Ok(None);
        };
        let telemetry =
            telemetry.map(|sink| sink.with_route_generation(self.bound.identity().generation()));
        runtime.list_models_with_telemetry(telemetry).await
    }
}

impl<P> Provider for ProviderRouteHandle<P>
where
    P: Provider,
{
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        let runtime = self.admit_inference_runtime(&[])?;
        runtime.healthcheck().await
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        let required = required_chat_capabilities(&req);
        let runtime = self.admit_inference_runtime(&required)?;
        if req.model != self.model {
            return Err(ProviderRouteContractError::RequestModelMismatch.into());
        }
        runtime.chat_stream(req).await
    }

    async fn chat_stream_with_telemetry(
        &self,
        req: ChatRequest,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<ChatStream, ProviderError> {
        let required = required_chat_capabilities(&req);
        let runtime = self.admit_inference_runtime(&required)?;
        if req.model != self.model {
            return Err(ProviderRouteContractError::RequestModelMismatch.into());
        }
        let telemetry =
            telemetry.map(|sink| sink.with_route_generation(self.bound.identity().generation()));
        runtime.chat_stream_with_telemetry(req, telemetry).await
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        let Some(runtime) = self.admit_optional_catalog_runtime()? else {
            return Ok(None);
        };
        runtime.list_models().await
    }

    async fn list_models_with_telemetry(
        &self,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        let Some(runtime) = self.admit_optional_catalog_runtime()? else {
            return Ok(None);
        };
        let telemetry =
            telemetry.map(|sink| sink.with_route_generation(self.bound.identity().generation()));
        runtime.list_models_with_telemetry(telemetry).await
    }
}

/// Convenience port for one-shot callers. Multi-round logical work should
/// bind a [`ProviderRouteHandle`] once and retain it for the whole operation;
/// this implementation deliberately binds only for the duration of one port
/// call.
impl<P> Provider for ProviderRouteCurrent<P>
where
    P: Provider,
{
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        self.bind()?.healthcheck().await
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        self.bind_model(req.model.clone())?.chat_stream(req).await
    }

    async fn chat_stream_with_telemetry(
        &self,
        req: ChatRequest,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<ChatStream, ProviderError> {
        self.bind_model(req.model.clone())?
            .chat_stream_with_telemetry(req, telemetry)
            .await
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        self.bind_catalog()?.list_models().await
    }

    async fn list_models_with_telemetry(
        &self,
        telemetry: Option<ProviderTelemetrySink>,
    ) -> Result<Option<Vec<String>>, ProviderError> {
        self.bind_catalog()?
            .list_models_with_telemetry(telemetry)
            .await
    }
}

fn required_chat_capabilities(req: &ChatRequest) -> Vec<ProviderCapability> {
    let mut required = Vec::with_capacity(3);
    if req.stream {
        required.push(ProviderCapability::Streaming);
    }
    if req.tools.as_ref().is_some_and(|tools| !tools.is_empty()) {
        required.push(ProviderCapability::Tools);
    }
    if req.response_format.is_some() {
        required.push(ProviderCapability::StructuredOutput);
    }
    required
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
    #[error("provider profile namespace must not be empty")]
    EmptyProfileNamespace,
    #[error("provider profile namespace is {actual_bytes} bytes; maximum is {max_bytes}")]
    ProfileNamespaceTooLong {
        actual_bytes: usize,
        max_bytes: usize,
    },
    #[error("provider profile name must not be empty")]
    EmptyProfileName,
    #[error("provider profile name is {actual_bytes} bytes; maximum is {max_bytes}")]
    ProfileNameTooLong {
        actual_bytes: usize,
        max_bytes: usize,
    },
    #[error("provider model identity must not be empty")]
    EmptyModelId,
    #[error("provider model identity is {actual_bytes} bytes; maximum is {max_bytes}")]
    ModelIdTooLong {
        actual_bytes: usize,
        max_bytes: usize,
    },
    #[error("provider model-catalog path must not be empty")]
    EmptyCatalogPath,
    #[error("provider route generation must be nonzero")]
    ZeroGeneration,
    #[error("provider route generation space is exhausted")]
    GenerationExhausted,
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
    #[error("provider route {kind:?} timeout must be nonzero")]
    ZeroRouteTimeout { kind: ProviderRouteTimeout },
    #[error(
        "provider model-catalog capability ({declared}) does not match configured policy ({configured})"
    )]
    CatalogCapabilityMismatch { declared: bool, configured: bool },
    #[error("catalog-only provider route must not declare {capability:?}")]
    CatalogOnlyCapabilityMismatch { capability: ProviderCapability },
    #[error("provider route replacement must keep the same logical profile")]
    ReplacementProfileMismatch,
    #[error(
        "provider route replacement generation {proposed:?} is not the successor of {current:?}"
    )]
    NonSuccessorGeneration {
        current: ProviderRouteGeneration,
        proposed: ProviderRouteGeneration,
    },
    #[error("current provider route is unavailable")]
    CurrentRouteUnavailable,
    #[error("provider route has no inference binding")]
    InferenceUnavailable,
    #[error("provider request model does not match its immutable route binding")]
    RequestModelMismatch,
}
