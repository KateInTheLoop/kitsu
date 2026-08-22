use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

const MAX_PROFILE_BYTES: usize = 96;
const MAX_ENDPOINT_BYTES: usize = 2 * 1024;
const MAX_API_PATH_BYTES: usize = 1024;
const MAX_MODEL_BYTES: usize = 512;
pub(super) const MAX_OWNED_EFFECTS: usize = 8;
pub(super) const MAX_SETUP_STAGES: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SetupSelectionKind {
    Local,
    Hosted,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SetupAdapter {
    CompatibleChat,
    AnthropicMessages,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CredentialPlan {
    None,
    ManagedBearer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CatalogIntent {
    OfflineOnly,
    CatalogAfterConsent,
}

/// Untrusted proposal input deliberately has no `Debug` or serialization.
/// Endpoint strings are validated before they can enter an inspectable value.
pub(super) struct SetupRouteInput {
    profile: String,
    endpoint: String,
    chat_path: String,
    models_path: Option<String>,
    model: String,
    catalog: CatalogIntent,
}

impl SetupRouteInput {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        profile: impl Into<String>,
        endpoint: impl Into<String>,
        chat_path: impl Into<String>,
        models_path: Option<&str>,
        model: impl Into<String>,
        catalog: CatalogIntent,
    ) -> Self {
        Self {
            profile: profile.into(),
            endpoint: endpoint.into(),
            chat_path: chat_path.into(),
            models_path: models_path.map(str::to_string),
            model: model.into(),
            catalog,
        }
    }
}

/// Closed setup families. No variant can contain a credential value or an
/// executable/plugin hook.
pub(super) enum SetupSelection {
    Local {
        route: SetupRouteInput,
    },
    Hosted {
        route: SetupRouteInput,
    },
    Custom {
        route: SetupRouteInput,
        adapter: SetupAdapter,
        credential: CredentialPlan,
    },
}

/// Untrusted proposal input deliberately has no `Debug` or serialization.
pub(super) struct SetupProposalInput {
    target: PathBuf,
    selection: SetupSelection,
}

impl SetupProposalInput {
    pub(super) fn new(target: impl Into<PathBuf>, selection: SetupSelection) -> Self {
        Self {
            target: target.into(),
            selection,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PublicationMode {
    CreateNew,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct NoClobberConfigTarget {
    path: PathBuf,
}

impl NoClobberConfigTarget {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) const fn publication(&self) -> PublicationMode {
        PublicationMode::CreateNew
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EndpointViolation {
    Empty,
    Control,
    UserInfo,
    Query,
    Fragment,
    UnsupportedScheme,
    MissingAuthority,
    InvalidPort,
    TooLong,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SetupProposalError {
    InvalidTarget,
    InvalidProfile,
    InvalidEndpoint(EndpointViolation),
    LocalEndpointMustBeLoopbackHttp,
    InvalidApiPath,
    InvalidModel,
    CatalogPathRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SetupPreviewModelError {
    InvalidModel,
    NotExactlyNormalized,
    UnexpectedStage { actual: SetupStage },
}

/// Fully normalized, non-secret preview. Construction is lexical and performs
/// no DNS, filesystem, environment, config, credential, or provider lookup.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct NormalizedSetupPreview {
    selection: SetupSelectionKind,
    profile: String,
    endpoint: String,
    chat_path: String,
    models_path: Option<String>,
    model: String,
    adapter: SetupAdapter,
    credential: CredentialPlan,
    catalog: CatalogIntent,
    target: NoClobberConfigTarget,
}

impl NormalizedSetupPreview {
    pub(super) fn normalize(input: SetupProposalInput) -> Result<Self, SetupProposalError> {
        let target = normalize_target(input.target)?;
        let (selection, route, adapter, credential) = match input.selection {
            SetupSelection::Local { route } => (
                SetupSelectionKind::Local,
                route,
                SetupAdapter::CompatibleChat,
                CredentialPlan::None,
            ),
            SetupSelection::Hosted { route } => (
                SetupSelectionKind::Hosted,
                route,
                SetupAdapter::CompatibleChat,
                CredentialPlan::ManagedBearer,
            ),
            SetupSelection::Custom {
                route,
                adapter,
                credential,
            } => (SetupSelectionKind::Custom, route, adapter, credential),
        };
        let profile = normalize_profile(&route.profile)?;
        let endpoint = normalize_endpoint(&route.endpoint)?;
        if selection == SetupSelectionKind::Local && !is_loopback_http(&endpoint) {
            return Err(SetupProposalError::LocalEndpointMustBeLoopbackHttp);
        }
        let chat_path = normalize_api_path(&route.chat_path)?;
        let models_path = route
            .models_path
            .as_deref()
            .map(normalize_api_path)
            .transpose()?;
        if route.catalog == CatalogIntent::CatalogAfterConsent && models_path.is_none() {
            return Err(SetupProposalError::CatalogPathRequired);
        }
        let model = normalize_model(&route.model)?;
        Ok(Self {
            selection,
            profile,
            endpoint,
            chat_path,
            models_path,
            model,
            adapter,
            credential,
            catalog: route.catalog,
            target,
        })
    }

    pub(super) const fn selection(&self) -> SetupSelectionKind {
        self.selection
    }

    pub(super) fn profile(&self) -> &str {
        &self.profile
    }

    pub(super) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(super) fn chat_path(&self) -> &str {
        &self.chat_path
    }

    pub(super) fn models_path(&self) -> Option<&str> {
        self.models_path.as_deref()
    }

    pub(super) fn model(&self) -> &str {
        &self.model
    }

    pub(super) const fn adapter(&self) -> SetupAdapter {
        self.adapter
    }

    pub(super) const fn credential(&self) -> CredentialPlan {
        self.credential
    }

    pub(super) const fn catalog(&self) -> CatalogIntent {
        self.catalog
    }

    pub(super) const fn target(&self) -> &NoClobberConfigTarget {
        &self.target
    }
}

fn normalize_target(target: PathBuf) -> Result<NoClobberConfigTarget, SetupProposalError> {
    let mut normalized = PathBuf::new();
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => return Err(SetupProposalError::InvalidTarget),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.file_name().is_none() {
        return Err(SetupProposalError::InvalidTarget);
    }
    Ok(NoClobberConfigTarget { path: normalized })
}

fn normalize_profile(raw: &str) -> Result<String, SetupProposalError> {
    let value = raw.trim();
    if value.is_empty()
        || value.len() > MAX_PROFILE_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(SetupProposalError::InvalidProfile);
    }
    Ok(value.to_string())
}

fn normalize_model(raw: &str) -> Result<String, SetupProposalError> {
    let value = raw.trim();
    if value.is_empty() || value.len() > MAX_MODEL_BYTES || raw.chars().any(char::is_control) {
        return Err(SetupProposalError::InvalidModel);
    }
    Ok(value.to_string())
}

fn normalize_api_path(raw: &str) -> Result<String, SetupProposalError> {
    if raw.chars().any(|character| character.is_control()) {
        return Err(SetupProposalError::InvalidApiPath);
    }
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_API_PATH_BYTES
        || trimmed.contains(['?', '#'])
        || trimmed.chars().any(char::is_whitespace)
    {
        return Err(SetupProposalError::InvalidApiPath);
    }
    let mut normalized = String::with_capacity(trimmed.len().saturating_add(1));
    if !trimmed.starts_with('/') {
        normalized.push('/');
    }
    normalized.push_str(trimmed);
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

fn normalize_endpoint(raw: &str) -> Result<String, SetupProposalError> {
    if raw.chars().any(char::is_control) {
        return Err(SetupProposalError::InvalidEndpoint(
            EndpointViolation::Control,
        ));
    }
    let value = raw.trim();
    if value.is_empty() {
        return Err(SetupProposalError::InvalidEndpoint(
            EndpointViolation::Empty,
        ));
    }
    if value.len() > MAX_ENDPOINT_BYTES {
        return Err(SetupProposalError::InvalidEndpoint(
            EndpointViolation::TooLong,
        ));
    }
    if value.contains('?') {
        return Err(SetupProposalError::InvalidEndpoint(
            EndpointViolation::Query,
        ));
    }
    if value.contains('#') {
        return Err(SetupProposalError::InvalidEndpoint(
            EndpointViolation::Fragment,
        ));
    }
    if value.contains('@') {
        return Err(SetupProposalError::InvalidEndpoint(
            EndpointViolation::UserInfo,
        ));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(SetupProposalError::InvalidEndpoint(
            EndpointViolation::Control,
        ));
    }
    let (scheme, remainder) =
        value
            .split_once("://")
            .ok_or(SetupProposalError::InvalidEndpoint(
                EndpointViolation::UnsupportedScheme,
            ))?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err(SetupProposalError::InvalidEndpoint(
            EndpointViolation::UnsupportedScheme,
        ));
    }
    let authority_end = remainder.find('/').unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.contains('\\') {
        return Err(SetupProposalError::InvalidEndpoint(
            EndpointViolation::MissingAuthority,
        ));
    }
    validate_endpoint_authority(authority).map_err(SetupProposalError::InvalidEndpoint)?;
    let authority = authority.to_ascii_lowercase();
    let mut suffix = remainder[authority_end..].to_string();
    while suffix.ends_with('/') {
        suffix.pop();
    }
    Ok(format!("{scheme}://{authority}{suffix}"))
}

fn validate_endpoint_authority(authority: &str) -> Result<(), EndpointViolation> {
    let port = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some(closing) = bracketed.find(']') else {
            return Err(EndpointViolation::MissingAuthority);
        };
        if closing == 0 {
            return Err(EndpointViolation::MissingAuthority);
        }
        let suffix = &bracketed[closing + 1..];
        if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or(EndpointViolation::MissingAuthority)?,
            )
        }
    } else {
        if authority.contains(['[', ']']) {
            return Err(EndpointViolation::MissingAuthority);
        }
        match authority.rsplit_once(':') {
            Some((host, port)) => {
                if host.is_empty() || host.contains(':') {
                    return Err(EndpointViolation::MissingAuthority);
                }
                Some(port)
            }
            None => None,
        }
    };
    if let Some(port) = port {
        if port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().is_err()
        {
            return Err(EndpointViolation::InvalidPort);
        }
    }
    Ok(())
}

fn is_loopback_http(endpoint: &str) -> bool {
    let Some(remainder) = endpoint.strip_prefix("http://") else {
        return false;
    };
    let authority = remainder.split('/').next().unwrap_or_default();
    if let Some(rest) = authority.strip_prefix("[::1]") {
        return rest.is_empty()
            || rest.strip_prefix(':').is_some_and(|port| {
                !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
            });
    }
    let (host, port) = authority
        .rsplit_once(':')
        .map_or((authority, None), |(host, port)| (host, Some(port)));
    let valid_port = port.map_or(true, |port| {
        !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && port.parse::<u16>().is_ok()
    });
    valid_port && matches!(host, "localhost" | "127.0.0.1")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SetupStage {
    AwaitingConsent,
    Acquire,
    Validate,
    Publish,
    Rollback,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SetupFence {
    transaction_id: u64,
    ordinal: u8,
}

impl SetupFence {
    pub(super) fn new(transaction_id: u64, ordinal: u8) -> Result<Self, SetupTransitionError> {
        if transaction_id == 0 || ordinal == 0 || ordinal > MAX_SETUP_STAGES {
            return Err(SetupTransitionError::InvalidTransaction);
        }
        Ok(Self {
            transaction_id,
            ordinal,
        })
    }

    pub(super) const fn ordinal(self) -> u8 {
        self.ordinal
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum OwnedEffectKind {
    ManagedCredential,
    CatalogCache,
    ConfigFile,
}

/// A receipt exists only for an effect this transaction newly created and may
/// therefore undo. It is deliberately non-`Clone` and non-`Copy`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct OwnedEffectReceipt {
    transaction_id: u64,
    id: u64,
    kind: OwnedEffectKind,
}

impl OwnedEffectReceipt {
    pub(super) fn new(
        transaction_id: u64,
        id: u64,
        kind: OwnedEffectKind,
    ) -> Result<Self, SetupReceiptError> {
        if transaction_id == 0 {
            return Err(SetupReceiptError::InvalidTransaction);
        }
        if id == 0 {
            return Err(SetupReceiptError::InvalidOwnedEffect);
        }
        Ok(Self {
            transaction_id,
            id,
            kind,
        })
    }

    fn view(&self) -> OwnedEffectView {
        OwnedEffectView {
            id: self.id,
            kind: self.kind,
        }
    }

    pub(super) const fn transaction_id(&self) -> u64 {
        self.transaction_id
    }

    pub(super) const fn id(&self) -> u64 {
        self.id
    }

    pub(super) const fn kind(&self) -> OwnedEffectKind {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OwnedEffectView {
    id: u64,
    kind: OwnedEffectKind,
}

impl OwnedEffectView {
    pub(super) const fn id(&self) -> u64 {
        self.id
    }

    pub(super) const fn kind(&self) -> OwnedEffectKind {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SetupFailureKind {
    PermissionDenied,
    NetworkUnavailable,
    InvalidCredential,
    ValidationRejected,
    PublicationIo,
    Interrupted,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SetupReceiptError {
    InvalidTransaction,
    InvalidOwnedEffect,
    TooManyOwnedEffects,
    DuplicateOwnedEffect,
    InvalidPublicationEffect,
    PublicationEffectWrongTransaction,
}

fn validate_owned_effects(effects: &[OwnedEffectReceipt]) -> Result<(), SetupReceiptError> {
    if effects.len() > MAX_OWNED_EFFECTS {
        return Err(SetupReceiptError::TooManyOwnedEffects);
    }
    let mut ids = BTreeSet::new();
    if effects.iter().any(|effect| !ids.insert(effect.id)) {
        return Err(SetupReceiptError::DuplicateOwnedEffect);
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum AcquisitionDisposition {
    Acquired,
    Cancelled,
    Failed(SetupFailureKind),
}

/// Exact adapter result for one acquire directive. Non-`Clone` by design.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct AcquisitionReceipt {
    fence: SetupFence,
    disposition: AcquisitionDisposition,
    owned_effects: Vec<OwnedEffectReceipt>,
}

impl AcquisitionReceipt {
    pub(super) fn acquired(
        fence: SetupFence,
        owned_effects: Vec<OwnedEffectReceipt>,
    ) -> Result<Self, SetupReceiptError> {
        Self::new(fence, AcquisitionDisposition::Acquired, owned_effects)
    }

    pub(super) fn cancelled(
        fence: SetupFence,
        owned_effects: Vec<OwnedEffectReceipt>,
    ) -> Result<Self, SetupReceiptError> {
        Self::new(fence, AcquisitionDisposition::Cancelled, owned_effects)
    }

    pub(super) fn failed(
        fence: SetupFence,
        kind: SetupFailureKind,
        owned_effects: Vec<OwnedEffectReceipt>,
    ) -> Result<Self, SetupReceiptError> {
        Self::new(fence, AcquisitionDisposition::Failed(kind), owned_effects)
    }

    fn new(
        fence: SetupFence,
        disposition: AcquisitionDisposition,
        owned_effects: Vec<OwnedEffectReceipt>,
    ) -> Result<Self, SetupReceiptError> {
        validate_owned_effects(&owned_effects)?;
        Ok(Self {
            fence,
            disposition,
            owned_effects,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ValidationDisposition {
    Validated,
    Cancelled,
    Failed(SetupFailureKind),
}

/// Exact adapter result for one validation directive. Non-`Clone` by design.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ValidationReceipt {
    fence: SetupFence,
    disposition: ValidationDisposition,
}

impl ValidationReceipt {
    pub(super) const fn validated(fence: SetupFence) -> Self {
        Self {
            fence,
            disposition: ValidationDisposition::Validated,
        }
    }

    pub(super) const fn cancelled(fence: SetupFence) -> Self {
        Self {
            fence,
            disposition: ValidationDisposition::Cancelled,
        }
    }

    pub(super) const fn failed(fence: SetupFence, kind: SetupFailureKind) -> Self {
        Self {
            fence,
            disposition: ValidationDisposition::Failed(kind),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PublicationDisposition {
    Published,
    Collision,
    Cancelled,
    Failed(SetupFailureKind),
}

/// Exact adapter result for one create-new publication. Non-`Clone` by design.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PublicationReceipt {
    fence: SetupFence,
    disposition: PublicationDisposition,
    owned_effect: Option<OwnedEffectReceipt>,
}

impl PublicationReceipt {
    pub(super) fn published(
        fence: SetupFence,
        owned_effect: OwnedEffectReceipt,
    ) -> Result<Self, SetupReceiptError> {
        Self::with_optional_effect(fence, PublicationDisposition::Published, Some(owned_effect))
    }

    pub(super) const fn collision(fence: SetupFence) -> Self {
        Self {
            fence,
            disposition: PublicationDisposition::Collision,
            owned_effect: None,
        }
    }

    pub(super) fn cancelled(
        fence: SetupFence,
        owned_effect: Option<OwnedEffectReceipt>,
    ) -> Result<Self, SetupReceiptError> {
        Self::with_optional_effect(fence, PublicationDisposition::Cancelled, owned_effect)
    }

    pub(super) fn failed(
        fence: SetupFence,
        kind: SetupFailureKind,
        owned_effect: Option<OwnedEffectReceipt>,
    ) -> Result<Self, SetupReceiptError> {
        Self::with_optional_effect(fence, PublicationDisposition::Failed(kind), owned_effect)
    }

    fn with_optional_effect(
        fence: SetupFence,
        disposition: PublicationDisposition,
        owned_effect: Option<OwnedEffectReceipt>,
    ) -> Result<Self, SetupReceiptError> {
        if let Some(effect) = owned_effect.as_ref() {
            if effect.kind != OwnedEffectKind::ConfigFile {
                return Err(SetupReceiptError::InvalidPublicationEffect);
            }
            if effect.transaction_id != fence.transaction_id {
                return Err(SetupReceiptError::PublicationEffectWrongTransaction);
            }
        }
        Ok(Self {
            fence,
            disposition,
            owned_effect,
        })
    }
}

/// Exact adapter result for one bounded rollback directive. Non-`Clone` by
/// design; failed IDs identify effects that remain owned by the transaction.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct RollbackReceipt {
    fence: SetupFence,
    failed_effect_ids: Vec<u64>,
}

impl RollbackReceipt {
    pub(super) fn new(
        fence: SetupFence,
        failed_effect_ids: Vec<u64>,
    ) -> Result<Self, SetupReceiptError> {
        if failed_effect_ids.len() > MAX_OWNED_EFFECTS {
            return Err(SetupReceiptError::TooManyOwnedEffects);
        }
        let mut ids = BTreeSet::new();
        if failed_effect_ids.iter().any(|id| !ids.insert(*id)) {
            return Err(SetupReceiptError::DuplicateOwnedEffect);
        }
        Ok(Self {
            fence,
            failed_effect_ids,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SetupTransitionError {
    InvalidTransaction,
    WrongTransaction,
    OwnedEffectWrongTransaction,
    FenceMismatch,
    UnexpectedStage {
        expected: SetupStage,
        received: SetupStage,
    },
    CancellationReceiptRequired {
        at: SetupStage,
    },
    UnknownRollbackEffect,
    DuplicateOwnedEffect,
    TooManyOwnedEffects,
    InvalidPublicationEffect,
    AlreadyTerminal,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SetupTerminalCause {
    Published,
    Cancelled {
        at: SetupStage,
    },
    Collision,
    Failed {
        at: SetupStage,
        kind: SetupFailureKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RollbackTruth {
    NotNeeded,
    Complete { effects: u8 },
    Incomplete { rolled_back: u8, remaining: u8 },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SetupTerminal {
    cause: SetupTerminalCause,
    rollback: RollbackTruth,
    retained_effects: Vec<OwnedEffectReceipt>,
}

impl SetupTerminal {
    pub(super) const fn cause(&self) -> &SetupTerminalCause {
        &self.cause
    }

    pub(super) const fn rollback(&self) -> RollbackTruth {
        self.rollback
    }

    pub(super) fn retained_effects(&self) -> Vec<OwnedEffectView> {
        self.retained_effects
            .iter()
            .map(OwnedEffectReceipt::view)
            .collect()
    }

    /// Transfers the terminal truth and every still-owned effect to the
    /// adapter's recovery owner. Published effects are adopted, not residue,
    /// and therefore never appear in this collection.
    pub(super) fn into_parts(self) -> (SetupTerminalCause, RollbackTruth, Vec<OwnedEffectReceipt>) {
        (self.cause, self.rollback, self.retained_effects)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValidationScope {
    Offline,
    AcquiredCatalog,
}

#[derive(Debug)]
pub(super) enum SetupDirective<'a> {
    Preview {
        preview: &'a NormalizedSetupPreview,
    },
    Acquire {
        fence: SetupFence,
        credential: CredentialPlan,
        catalog: CatalogIntent,
    },
    Validate {
        fence: SetupFence,
        scope: ValidationScope,
    },
    Publish {
        fence: SetupFence,
        target: &'a NoClobberConfigTarget,
        mode: PublicationMode,
    },
    Rollback {
        fence: SetupFence,
        effects: Vec<OwnedEffectView>,
    },
    Terminal {
        outcome: &'a SetupTerminal,
    },
}

impl SetupDirective<'_> {
    pub(super) const fn stage(&self) -> SetupStage {
        match self {
            Self::Preview { .. } => SetupStage::AwaitingConsent,
            Self::Acquire { .. } => SetupStage::Acquire,
            Self::Validate { .. } => SetupStage::Validate,
            Self::Publish { .. } => SetupStage::Publish,
            Self::Rollback { .. } => SetupStage::Rollback,
            Self::Terminal { .. } => SetupStage::Terminal,
        }
    }

    pub(super) const fn permits_effects(&self) -> bool {
        matches!(
            self,
            Self::Acquire { .. }
                | Self::Validate { .. }
                | Self::Publish { .. }
                | Self::Rollback { .. }
        )
    }
}

#[derive(Debug)]
enum TransactionState {
    AwaitingConsent,
    Acquire {
        fence: SetupFence,
    },
    Validate {
        fence: SetupFence,
        owned_effects: Vec<OwnedEffectReceipt>,
    },
    Publish {
        fence: SetupFence,
        owned_effects: Vec<OwnedEffectReceipt>,
    },
    Rollback {
        fence: SetupFence,
        primary: Option<SetupTerminalCause>,
        owned_effects: Vec<OwnedEffectReceipt>,
    },
    Terminal(SetupTerminal),
}

impl TransactionState {
    const fn stage(&self) -> SetupStage {
        match self {
            Self::AwaitingConsent => SetupStage::AwaitingConsent,
            Self::Acquire { .. } => SetupStage::Acquire,
            Self::Validate { .. } => SetupStage::Validate,
            Self::Publish { .. } => SetupStage::Publish,
            Self::Rollback { .. } => SetupStage::Rollback,
            Self::Terminal(_) => SetupStage::Terminal,
        }
    }
}

/// Sole mutable owner of a proposed setup transaction. It is deliberately
/// non-`Clone`: one receipt can advance exactly one owner.
#[derive(Debug)]
pub(super) struct GuidedSetupTransaction {
    transaction_id: u64,
    preview: NormalizedSetupPreview,
    state: TransactionState,
    completed_stages: u8,
}

/// Failed consuming extraction keeps the sole transaction owner available to
/// its caller. It is deliberately non-`Clone` and non-`Copy`.
#[derive(Debug)]
pub(super) struct SetupTerminalTransferError {
    stage: SetupStage,
    transaction: Box<GuidedSetupTransaction>,
}

impl SetupTerminalTransferError {
    pub(super) const fn stage(&self) -> SetupStage {
        self.stage
    }

    pub(super) fn into_transaction(self) -> GuidedSetupTransaction {
        *self.transaction
    }
}

impl GuidedSetupTransaction {
    pub(super) fn new(
        transaction_id: u64,
        preview: NormalizedSetupPreview,
    ) -> Result<Self, SetupTransitionError> {
        if transaction_id == 0 {
            return Err(SetupTransitionError::InvalidTransaction);
        }
        Ok(Self {
            transaction_id,
            preview,
            state: TransactionState::AwaitingConsent,
            completed_stages: 0,
        })
    }

    pub(super) fn directive(&self) -> SetupDirective<'_> {
        match &self.state {
            TransactionState::AwaitingConsent => SetupDirective::Preview {
                preview: &self.preview,
            },
            TransactionState::Acquire { fence } => SetupDirective::Acquire {
                fence: *fence,
                credential: self.preview.credential,
                catalog: self.preview.catalog,
            },
            TransactionState::Validate { fence, .. } => SetupDirective::Validate {
                fence: *fence,
                scope: if self.preview.catalog == CatalogIntent::CatalogAfterConsent {
                    ValidationScope::AcquiredCatalog
                } else {
                    ValidationScope::Offline
                },
            },
            TransactionState::Publish { fence, .. } => SetupDirective::Publish {
                fence: *fence,
                target: &self.preview.target,
                mode: PublicationMode::CreateNew,
            },
            TransactionState::Rollback {
                fence,
                owned_effects,
                ..
            } => SetupDirective::Rollback {
                fence: *fence,
                effects: owned_effects
                    .iter()
                    .rev()
                    .map(OwnedEffectReceipt::view)
                    .collect(),
            },
            TransactionState::Terminal(outcome) => SetupDirective::Terminal { outcome },
        }
    }

    pub(super) fn terminal(&self) -> Option<&SetupTerminal> {
        match &self.state {
            TransactionState::Terminal(terminal) => Some(terminal),
            _ => None,
        }
    }

    pub(super) fn into_terminal(self) -> Result<SetupTerminal, SetupTerminalTransferError> {
        let stage = self.state.stage();
        if stage != SetupStage::Terminal {
            return Err(SetupTerminalTransferError {
                stage,
                transaction: Box::new(self),
            });
        }
        let Self {
            state: TransactionState::Terminal(terminal),
            ..
        } = self
        else {
            unreachable!("terminal stage always carries terminal ownership")
        };
        Ok(terminal)
    }

    pub(super) const fn completed_stages(&self) -> u8 {
        self.completed_stages
    }

    pub(super) fn replace_preview_model_exact(
        &mut self,
        model: &str,
    ) -> Result<(), SetupPreviewModelError> {
        let normalized =
            normalize_model(model).map_err(|_| SetupPreviewModelError::InvalidModel)?;
        if normalized != model {
            return Err(SetupPreviewModelError::NotExactlyNormalized);
        }
        let actual = self.state.stage();
        if actual != SetupStage::AwaitingConsent {
            return Err(SetupPreviewModelError::UnexpectedStage { actual });
        }
        self.preview.model = normalized;
        Ok(())
    }

    pub(super) fn consent(&mut self) -> Result<(), SetupTransitionError> {
        match self.state.stage() {
            SetupStage::Terminal => return Err(SetupTransitionError::AlreadyTerminal),
            SetupStage::AwaitingConsent => {}
            expected => {
                return Err(SetupTransitionError::UnexpectedStage {
                    expected,
                    received: SetupStage::AwaitingConsent,
                });
            }
        }
        let fence = self.advance_fence();
        self.state = TransactionState::Acquire { fence };
        Ok(())
    }

    pub(super) fn cancel(&mut self) -> Result<(), SetupTransitionError> {
        let stage = self.state.stage();
        match &self.state {
            TransactionState::Terminal(_) => return Err(SetupTransitionError::AlreadyTerminal),
            TransactionState::AwaitingConsent => {}
            TransactionState::Acquire { .. }
            | TransactionState::Validate { .. }
            | TransactionState::Publish { .. }
            | TransactionState::Rollback { .. } => {
                return Err(SetupTransitionError::CancellationReceiptRequired { at: stage });
            }
        }
        self.state = TransactionState::Terminal(SetupTerminal {
            cause: SetupTerminalCause::Cancelled { at: stage },
            rollback: RollbackTruth::NotNeeded,
            retained_effects: Vec::new(),
        });
        Ok(())
    }

    pub(super) fn accept_acquisition(
        &mut self,
        receipt: AcquisitionReceipt,
    ) -> Result<(), SetupTransitionError> {
        self.require_fence(SetupStage::Acquire, receipt.fence)?;
        if receipt
            .owned_effects
            .iter()
            .any(|effect| effect.transaction_id != self.transaction_id)
        {
            return Err(SetupTransitionError::OwnedEffectWrongTransaction);
        }
        let ordinal = self.advance_ordinal();
        match receipt.disposition {
            AcquisitionDisposition::Acquired => {
                self.state = TransactionState::Validate {
                    fence: self.fence(ordinal),
                    owned_effects: receipt.owned_effects,
                };
            }
            AcquisitionDisposition::Cancelled => self.stop_at_ordinal(
                SetupTerminalCause::Cancelled {
                    at: SetupStage::Acquire,
                },
                receipt.owned_effects,
                ordinal,
            ),
            AcquisitionDisposition::Failed(kind) => self.stop_at_ordinal(
                SetupTerminalCause::Failed {
                    at: SetupStage::Acquire,
                    kind,
                },
                receipt.owned_effects,
                ordinal,
            ),
        }
        Ok(())
    }

    pub(super) fn accept_validation(
        &mut self,
        receipt: ValidationReceipt,
    ) -> Result<(), SetupTransitionError> {
        self.require_fence(SetupStage::Validate, receipt.fence)?;
        let owned_effects = match &mut self.state {
            TransactionState::Validate { owned_effects, .. } => std::mem::take(owned_effects),
            _ => unreachable!("fence validation proved the state"),
        };
        let ordinal = self.advance_ordinal();
        match receipt.disposition {
            ValidationDisposition::Validated => {
                self.state = TransactionState::Publish {
                    fence: self.fence(ordinal),
                    owned_effects,
                };
            }
            ValidationDisposition::Cancelled => self.stop_at_ordinal(
                SetupTerminalCause::Cancelled {
                    at: SetupStage::Validate,
                },
                owned_effects,
                ordinal,
            ),
            ValidationDisposition::Failed(kind) => self.stop_at_ordinal(
                SetupTerminalCause::Failed {
                    at: SetupStage::Validate,
                    kind,
                },
                owned_effects,
                ordinal,
            ),
        }
        Ok(())
    }

    pub(super) fn accept_publication(
        &mut self,
        receipt: PublicationReceipt,
    ) -> Result<(), SetupTransitionError> {
        self.require_fence(SetupStage::Publish, receipt.fence)?;
        if let Some(effect) = receipt.owned_effect.as_ref() {
            if effect.transaction_id != self.transaction_id {
                return Err(SetupTransitionError::OwnedEffectWrongTransaction);
            }
            if effect.kind != OwnedEffectKind::ConfigFile {
                return Err(SetupTransitionError::InvalidPublicationEffect);
            }
            let TransactionState::Publish { owned_effects, .. } = &self.state else {
                unreachable!("fence validation proved the state")
            };
            if owned_effects.iter().any(|owned| owned.id == effect.id) {
                return Err(SetupTransitionError::DuplicateOwnedEffect);
            }
            if owned_effects.len() == MAX_OWNED_EFFECTS {
                return Err(SetupTransitionError::TooManyOwnedEffects);
            }
        }
        let owned_effects = match &mut self.state {
            TransactionState::Publish { owned_effects, .. } => std::mem::take(owned_effects),
            _ => unreachable!("fence validation proved the state"),
        };
        let mut owned_effects = owned_effects;
        if let Some(effect) = receipt.owned_effect {
            owned_effects.push(effect);
        }
        let ordinal = self.advance_ordinal();
        match receipt.disposition {
            PublicationDisposition::Published => {
                self.state = TransactionState::Terminal(SetupTerminal {
                    cause: SetupTerminalCause::Published,
                    rollback: RollbackTruth::NotNeeded,
                    retained_effects: Vec::new(),
                });
            }
            PublicationDisposition::Collision => {
                self.stop_at_ordinal(SetupTerminalCause::Collision, owned_effects, ordinal)
            }
            PublicationDisposition::Cancelled => self.stop_at_ordinal(
                SetupTerminalCause::Cancelled {
                    at: SetupStage::Publish,
                },
                owned_effects,
                ordinal,
            ),
            PublicationDisposition::Failed(kind) => self.stop_at_ordinal(
                SetupTerminalCause::Failed {
                    at: SetupStage::Publish,
                    kind,
                },
                owned_effects,
                ordinal,
            ),
        }
        Ok(())
    }

    pub(super) fn accept_rollback(
        &mut self,
        receipt: RollbackReceipt,
    ) -> Result<(), SetupTransitionError> {
        self.require_fence(SetupStage::Rollback, receipt.fence)?;
        let known_ids = match &self.state {
            TransactionState::Rollback { owned_effects, .. } => owned_effects
                .iter()
                .map(|effect| effect.id)
                .collect::<BTreeSet<_>>(),
            _ => unreachable!("fence validation proved the state"),
        };
        if receipt
            .failed_effect_ids
            .iter()
            .any(|id| !known_ids.contains(id))
        {
            return Err(SetupTransitionError::UnknownRollbackEffect);
        }
        let failed_ids = receipt
            .failed_effect_ids
            .into_iter()
            .collect::<BTreeSet<_>>();
        let (primary, owned_effects) = match &mut self.state {
            TransactionState::Rollback {
                primary,
                owned_effects,
                ..
            } => (
                primary
                    .take()
                    .expect("rollback always retains its primary cause"),
                std::mem::take(owned_effects),
            ),
            _ => unreachable!("fence validation proved the state"),
        };
        let total = owned_effects.len();
        let retained_effects = owned_effects
            .into_iter()
            .filter(|effect| failed_ids.contains(&effect.id))
            .collect::<Vec<_>>();
        let remaining = retained_effects.len();
        self.advance_ordinal();
        self.state = TransactionState::Terminal(SetupTerminal {
            cause: primary,
            rollback: if remaining == 0 {
                RollbackTruth::Complete {
                    effects: u8::try_from(total).unwrap_or(u8::MAX),
                }
            } else {
                RollbackTruth::Incomplete {
                    rolled_back: u8::try_from(total.saturating_sub(remaining)).unwrap_or(u8::MAX),
                    remaining: u8::try_from(remaining).unwrap_or(u8::MAX),
                }
            },
            retained_effects,
        });
        Ok(())
    }

    fn require_fence(
        &self,
        received_stage: SetupStage,
        received_fence: SetupFence,
    ) -> Result<(), SetupTransitionError> {
        let expected_stage = self.state.stage();
        if expected_stage == SetupStage::Terminal {
            return Err(SetupTransitionError::AlreadyTerminal);
        }
        if expected_stage != received_stage {
            return Err(SetupTransitionError::UnexpectedStage {
                expected: expected_stage,
                received: received_stage,
            });
        }
        if received_fence.transaction_id != self.transaction_id {
            return Err(SetupTransitionError::WrongTransaction);
        }
        let expected_fence = match &self.state {
            TransactionState::Acquire { fence }
            | TransactionState::Validate { fence, .. }
            | TransactionState::Publish { fence, .. }
            | TransactionState::Rollback { fence, .. } => *fence,
            TransactionState::AwaitingConsent | TransactionState::Terminal(_) => {
                unreachable!("effect stage always carries a fence")
            }
        };
        if received_fence != expected_fence {
            return Err(SetupTransitionError::FenceMismatch);
        }
        Ok(())
    }

    fn stop_at_ordinal(
        &mut self,
        primary: SetupTerminalCause,
        owned_effects: Vec<OwnedEffectReceipt>,
        ordinal: u8,
    ) {
        if owned_effects.is_empty() {
            self.state = TransactionState::Terminal(SetupTerminal {
                cause: primary,
                rollback: RollbackTruth::NotNeeded,
                retained_effects: Vec::new(),
            });
        } else {
            self.state = TransactionState::Rollback {
                fence: self.fence(ordinal),
                primary: Some(primary),
                owned_effects,
            };
        }
    }

    fn advance_fence(&mut self) -> SetupFence {
        let ordinal = self.advance_ordinal();
        self.fence(ordinal)
    }

    fn advance_ordinal(&mut self) -> u8 {
        debug_assert!(self.completed_stages < MAX_SETUP_STAGES);
        self.completed_stages = self
            .completed_stages
            .saturating_add(1)
            .min(MAX_SETUP_STAGES);
        self.completed_stages
    }

    const fn fence(&self, ordinal: u8) -> SetupFence {
        SetupFence {
            transaction_id: self.transaction_id,
            ordinal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn route(endpoint: &str, catalog: CatalogIntent) -> SetupRouteInput {
        SetupRouteInput::new(
            "  example  ",
            endpoint,
            "v1/chat/completions/",
            Some("v1/models/"),
            "  model-v1  ",
            catalog,
        )
    }

    fn proposal(selection: SetupSelection) -> SetupProposalInput {
        SetupProposalInput::new(PathBuf::from("./kitsu.toml"), selection)
    }

    fn local_proposal() -> SetupProposalInput {
        proposal(SetupSelection::Local {
            route: route("  http://localhost:11434/  ", CatalogIntent::OfflineOnly),
        })
    }

    fn hosted_proposal() -> SetupProposalInput {
        proposal(SetupSelection::Hosted {
            route: route(
                "  https://api.example.test/  ",
                CatalogIntent::CatalogAfterConsent,
            ),
        })
    }

    fn custom_proposal() -> SetupProposalInput {
        proposal(SetupSelection::Custom {
            route: route(
                " https://custom.example.test/base/ ",
                CatalogIntent::CatalogAfterConsent,
            ),
            adapter: SetupAdapter::AnthropicMessages,
            credential: CredentialPlan::ManagedBearer,
        })
    }

    fn transaction(id: u64, input: SetupProposalInput) -> GuidedSetupTransaction {
        GuidedSetupTransaction::new(id, NormalizedSetupPreview::normalize(input).unwrap()).unwrap()
    }

    fn current_fence(transaction: &GuidedSetupTransaction, expected: SetupStage) -> SetupFence {
        let directive = transaction.directive();
        assert_eq!(directive.stage(), expected);
        match directive {
            SetupDirective::Acquire { fence, .. }
            | SetupDirective::Validate { fence, .. }
            | SetupDirective::Publish { fence, .. }
            | SetupDirective::Rollback { fence, .. } => fence,
            SetupDirective::Preview { .. } | SetupDirective::Terminal { .. } => {
                panic!("stage {expected:?} has no effect fence")
            }
        }
    }

    fn owned(transaction_id: u64, effect_id: u64, kind: OwnedEffectKind) -> OwnedEffectReceipt {
        OwnedEffectReceipt::new(transaction_id, effect_id, kind).unwrap()
    }

    fn advance_to_validation(
        transaction: &mut GuidedSetupTransaction,
        owned_effects: Vec<OwnedEffectReceipt>,
    ) {
        transaction.consent().unwrap();
        let fence = current_fence(transaction, SetupStage::Acquire);
        transaction
            .accept_acquisition(AcquisitionReceipt::acquired(fence, owned_effects).unwrap())
            .unwrap();
        assert_eq!(transaction.directive().stage(), SetupStage::Validate);
    }

    fn advance_to_publication(
        transaction: &mut GuidedSetupTransaction,
        owned_effects: Vec<OwnedEffectReceipt>,
    ) {
        advance_to_validation(transaction, owned_effects);
        let fence = current_fence(transaction, SetupStage::Validate);
        transaction
            .accept_validation(ValidationReceipt::validated(fence))
            .unwrap();
        assert_eq!(transaction.directive().stage(), SetupStage::Publish);
    }

    fn finish_rollback(transaction: &mut GuidedSetupTransaction, failed_effect_ids: Vec<u64>) {
        let fence = current_fence(transaction, SetupStage::Rollback);
        transaction
            .accept_rollback(RollbackReceipt::new(fence, failed_effect_ids).unwrap())
            .unwrap();
    }

    #[test]
    fn local_hosted_and_custom_proposals_normalize_without_effects() {
        let cases = [
            (
                local_proposal(),
                SetupSelectionKind::Local,
                "http://localhost:11434",
                SetupAdapter::CompatibleChat,
                CredentialPlan::None,
                CatalogIntent::OfflineOnly,
            ),
            (
                hosted_proposal(),
                SetupSelectionKind::Hosted,
                "https://api.example.test",
                SetupAdapter::CompatibleChat,
                CredentialPlan::ManagedBearer,
                CatalogIntent::CatalogAfterConsent,
            ),
            (
                custom_proposal(),
                SetupSelectionKind::Custom,
                "https://custom.example.test/base",
                SetupAdapter::AnthropicMessages,
                CredentialPlan::ManagedBearer,
                CatalogIntent::CatalogAfterConsent,
            ),
        ];

        for (input, kind, endpoint, adapter, credential, catalog) in cases {
            let preview = NormalizedSetupPreview::normalize(input).unwrap();
            assert_eq!(preview.selection(), kind);
            assert_eq!(preview.profile(), "example");
            assert_eq!(preview.endpoint(), endpoint);
            assert_eq!(preview.chat_path(), "/v1/chat/completions");
            assert_eq!(preview.models_path(), Some("/v1/models"));
            assert_eq!(preview.model(), "model-v1");
            assert_eq!(preview.adapter(), adapter);
            assert_eq!(preview.credential(), credential);
            assert_eq!(preview.catalog(), catalog);
            assert_eq!(preview.target().path(), PathBuf::from("kitsu.toml"));
            assert_eq!(preview.target().publication(), PublicationMode::CreateNew);

            let transaction = GuidedSetupTransaction::new(7, preview).unwrap();
            let directive = transaction.directive();
            assert_eq!(directive.stage(), SetupStage::AwaitingConsent);
            assert!(!directive.permits_effects());
            assert!(matches!(directive, SetupDirective::Preview { .. }));
        }
    }

    #[test]
    fn preview_model_replacement_is_exact_preconsent_and_rejections_are_zero_mutation() {
        let mut transaction = transaction(8, local_proposal());
        assert_eq!(
            transaction.replace_preview_model_exact("  selected/model  "),
            Err(SetupPreviewModelError::NotExactlyNormalized),
        );
        match transaction.directive() {
            SetupDirective::Preview { preview } => assert_eq!(preview.model(), "model-v1"),
            other => panic!("expected unchanged preview, got {other:?}"),
        }
        assert_eq!(
            transaction.replace_preview_model_exact("bad\nmodel"),
            Err(SetupPreviewModelError::InvalidModel),
        );
        match transaction.directive() {
            SetupDirective::Preview { preview } => assert_eq!(preview.model(), "model-v1"),
            other => panic!("expected unchanged preview, got {other:?}"),
        }

        transaction
            .replace_preview_model_exact("selected/model")
            .unwrap();
        match transaction.directive() {
            SetupDirective::Preview { preview } => assert_eq!(preview.model(), "selected/model"),
            other => panic!("expected replaced preview, got {other:?}"),
        }

        transaction.consent().unwrap();
        assert_eq!(
            transaction.replace_preview_model_exact("later/model"),
            Err(SetupPreviewModelError::UnexpectedStage {
                actual: SetupStage::Acquire,
            }),
        );
        assert_eq!(transaction.directive().stage(), SetupStage::Acquire);
    }

    #[test]
    fn normalization_rejects_secret_bearing_or_ambient_endpoint_shapes_offline() {
        let invalid = [
            (
                "https://user:credential@example.test",
                EndpointViolation::UserInfo,
            ),
            (
                "https://example.test?token=credential",
                EndpointViolation::Query,
            ),
            (
                "https://example.test/#credential",
                EndpointViolation::Fragment,
            ),
            ("file:///tmp/provider", EndpointViolation::UnsupportedScheme),
            ("https://example.test/\nnext", EndpointViolation::Control),
        ];
        for (endpoint, violation) in invalid {
            let input = proposal(SetupSelection::Custom {
                route: route(endpoint, CatalogIntent::OfflineOnly),
                adapter: SetupAdapter::CompatibleChat,
                credential: CredentialPlan::None,
            });
            let error = NormalizedSetupPreview::normalize(input).unwrap_err();
            assert_eq!(
                error,
                SetupProposalError::InvalidEndpoint(violation),
                "{endpoint}"
            );
            let debug = format!("{error:?}");
            assert!(!debug.contains("credential"), "secret leaked: {debug}");
        }

        let non_loopback = proposal(SetupSelection::Local {
            route: route("http://example.test:11434", CatalogIntent::OfflineOnly),
        });
        assert_eq!(
            NormalizedSetupPreview::normalize(non_loopback).unwrap_err(),
            SetupProposalError::LocalEndpointMustBeLoopbackHttp
        );
        let local_https = proposal(SetupSelection::Local {
            route: route("https://localhost:11434", CatalogIntent::OfflineOnly),
        });
        assert_eq!(
            NormalizedSetupPreview::normalize(local_https).unwrap_err(),
            SetupProposalError::LocalEndpointMustBeLoopbackHttp
        );
    }

    #[test]
    fn normalization_rejects_malformed_or_out_of_range_ports_offline() {
        for endpoint in [
            "https://example.test:not-a-port",
            "https://example.test:65536",
            "https://example.test:",
        ] {
            let input = proposal(SetupSelection::Custom {
                route: route(endpoint, CatalogIntent::OfflineOnly),
                adapter: SetupAdapter::CompatibleChat,
                credential: CredentialPlan::None,
            });
            assert_eq!(
                NormalizedSetupPreview::normalize(input).unwrap_err(),
                SetupProposalError::InvalidEndpoint(EndpointViolation::InvalidPort),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn invalid_proposals_are_typed_before_a_transaction_or_effect_exists() {
        let empty_profile = SetupRouteInput::new(
            "   ",
            "https://example.test",
            "/v1/chat/completions",
            None,
            "model",
            CatalogIntent::OfflineOnly,
        );
        assert_eq!(
            NormalizedSetupPreview::normalize(proposal(SetupSelection::Hosted {
                route: empty_profile,
            }))
            .unwrap_err(),
            SetupProposalError::InvalidProfile
        );

        let no_catalog_path = SetupRouteInput::new(
            "example",
            "https://example.test",
            "/v1/chat/completions",
            None,
            "model",
            CatalogIntent::CatalogAfterConsent,
        );
        assert_eq!(
            NormalizedSetupPreview::normalize(proposal(SetupSelection::Hosted {
                route: no_catalog_path,
            }))
            .unwrap_err(),
            SetupProposalError::CatalogPathRequired
        );

        assert_eq!(
            NormalizedSetupPreview::normalize(SetupProposalInput::new(
                PathBuf::from("../kitsu.toml"),
                SetupSelection::Local {
                    route: route("http://127.0.0.1:11434", CatalogIntent::OfflineOnly),
                },
            ))
            .unwrap_err(),
            SetupProposalError::InvalidTarget
        );
        assert_eq!(
            GuidedSetupTransaction::new(
                0,
                NormalizedSetupPreview::normalize(local_proposal()).unwrap()
            )
            .unwrap_err(),
            SetupTransitionError::InvalidTransaction
        );
    }

    #[test]
    fn consent_is_the_first_possible_effect_boundary_and_decline_is_clean() {
        let mut transaction = transaction(11, hosted_proposal());
        assert!(matches!(
            transaction.directive(),
            SetupDirective::Preview { .. }
        ));
        assert!(!transaction.directive().permits_effects());

        transaction.cancel().unwrap();
        let terminal = transaction.terminal().expect("typed cancellation");
        assert_eq!(
            terminal.cause(),
            &SetupTerminalCause::Cancelled {
                at: SetupStage::AwaitingConsent
            }
        );
        assert_eq!(terminal.rollback(), RollbackTruth::NotNeeded);
        assert!(terminal.retained_effects().is_empty());
        assert!(matches!(
            transaction.directive(),
            SetupDirective::Terminal { .. }
        ));
    }

    #[test]
    fn cancellation_after_consent_requires_the_exact_stage_receipt() {
        let mut acquiring = transaction(13, hosted_proposal());
        acquiring.consent().unwrap();
        assert_eq!(
            acquiring.cancel(),
            Err(SetupTransitionError::CancellationReceiptRequired {
                at: SetupStage::Acquire,
            })
        );
        assert_eq!(acquiring.directive().stage(), SetupStage::Acquire);
        let fence = current_fence(&acquiring, SetupStage::Acquire);
        acquiring
            .accept_acquisition(
                AcquisitionReceipt::cancelled(
                    fence,
                    vec![owned(13, 1, OwnedEffectKind::ManagedCredential)],
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(acquiring.directive().stage(), SetupStage::Rollback);

        let mut validating = transaction(15, hosted_proposal());
        advance_to_validation(
            &mut validating,
            vec![owned(15, 2, OwnedEffectKind::ManagedCredential)],
        );
        assert_eq!(
            validating.cancel(),
            Err(SetupTransitionError::CancellationReceiptRequired {
                at: SetupStage::Validate,
            })
        );
        assert_eq!(validating.directive().stage(), SetupStage::Validate);
        let fence = current_fence(&validating, SetupStage::Validate);
        validating
            .accept_validation(ValidationReceipt::cancelled(fence))
            .unwrap();
        assert_eq!(validating.directive().stage(), SetupStage::Rollback);

        let mut publishing = transaction(17, custom_proposal());
        advance_to_publication(
            &mut publishing,
            vec![owned(17, 3, OwnedEffectKind::CatalogCache)],
        );
        assert_eq!(
            publishing.cancel(),
            Err(SetupTransitionError::CancellationReceiptRequired {
                at: SetupStage::Publish,
            })
        );
        assert_eq!(publishing.directive().stage(), SetupStage::Publish);
        let fence = current_fence(&publishing, SetupStage::Publish);
        publishing
            .accept_publication(PublicationReceipt::cancelled(fence, None).unwrap())
            .unwrap();
        assert_eq!(publishing.directive().stage(), SetupStage::Rollback);
    }

    #[test]
    fn exact_acquire_validate_publish_receipts_are_required_for_success() {
        let mut transaction = transaction(19, hosted_proposal());
        transaction.consent().unwrap();
        let acquire = transaction.directive();
        assert!(acquire.permits_effects());
        assert!(matches!(
            acquire,
            SetupDirective::Acquire {
                credential: CredentialPlan::ManagedBearer,
                catalog: CatalogIntent::CatalogAfterConsent,
                ..
            }
        ));
        let acquire_fence = current_fence(&transaction, SetupStage::Acquire);
        transaction
            .accept_acquisition(
                AcquisitionReceipt::acquired(
                    acquire_fence,
                    vec![owned(19, 1, OwnedEffectKind::ManagedCredential)],
                )
                .unwrap(),
            )
            .unwrap();

        let validation_fence = current_fence(&transaction, SetupStage::Validate);
        transaction
            .accept_validation(ValidationReceipt::validated(validation_fence))
            .unwrap();
        let publication = transaction.directive();
        assert!(matches!(
            publication,
            SetupDirective::Publish {
                mode: PublicationMode::CreateNew,
                ..
            }
        ));
        assert!(transaction.terminal().is_none());

        let publish_fence = current_fence(&transaction, SetupStage::Publish);
        transaction
            .accept_publication(
                PublicationReceipt::published(
                    publish_fence,
                    owned(19, 2, OwnedEffectKind::ConfigFile),
                )
                .unwrap(),
            )
            .unwrap();
        let terminal = transaction.terminal().expect("published terminal");
        assert_eq!(terminal.cause(), &SetupTerminalCause::Published);
        assert_eq!(terminal.rollback(), RollbackTruth::NotNeeded);
        assert!(terminal.retained_effects().is_empty());
    }

    #[test]
    fn partial_acquire_cancellation_rolls_back_only_owned_effects_in_reverse_order() {
        let mut transaction = transaction(23, hosted_proposal());
        transaction.consent().unwrap();
        let fence = current_fence(&transaction, SetupStage::Acquire);
        transaction
            .accept_acquisition(
                AcquisitionReceipt::cancelled(
                    fence,
                    vec![
                        owned(23, 1, OwnedEffectKind::CatalogCache),
                        owned(23, 2, OwnedEffectKind::ManagedCredential),
                    ],
                )
                .unwrap(),
            )
            .unwrap();

        match transaction.directive() {
            SetupDirective::Rollback { effects, .. } => {
                assert_eq!(
                    effects.iter().map(OwnedEffectView::id).collect::<Vec<_>>(),
                    vec![2, 1]
                );
            }
            other => panic!("expected rollback, got {other:?}"),
        }
        finish_rollback(&mut transaction, Vec::new());
        let terminal = transaction.terminal().unwrap();
        assert_eq!(
            terminal.cause(),
            &SetupTerminalCause::Cancelled {
                at: SetupStage::Acquire
            }
        );
        assert_eq!(terminal.rollback(), RollbackTruth::Complete { effects: 2 });
        assert!(terminal.retained_effects().is_empty());
    }

    #[test]
    fn validation_network_failure_and_publication_collision_preserve_primary_truth() {
        let mut failed = transaction(29, hosted_proposal());
        advance_to_validation(
            &mut failed,
            vec![owned(29, 3, OwnedEffectKind::ManagedCredential)],
        );
        let validation_fence = current_fence(&failed, SetupStage::Validate);
        failed
            .accept_validation(ValidationReceipt::failed(
                validation_fence,
                SetupFailureKind::NetworkUnavailable,
            ))
            .unwrap();
        finish_rollback(&mut failed, Vec::new());
        assert_eq!(
            failed.terminal().unwrap().cause(),
            &SetupTerminalCause::Failed {
                at: SetupStage::Validate,
                kind: SetupFailureKind::NetworkUnavailable,
            }
        );

        let mut collision = transaction(31, custom_proposal());
        advance_to_publication(
            &mut collision,
            vec![owned(31, 4, OwnedEffectKind::ManagedCredential)],
        );
        let publish_fence = current_fence(&collision, SetupStage::Publish);
        collision
            .accept_publication(PublicationReceipt::collision(publish_fence))
            .unwrap();
        finish_rollback(&mut collision, Vec::new());
        assert_eq!(
            collision.terminal().unwrap().cause(),
            &SetupTerminalCause::Collision
        );
    }

    #[test]
    fn publication_receipt_combinations_are_closed_and_config_file_only() {
        let fence = SetupFence::new(53, 3).unwrap();
        assert_eq!(
            PublicationReceipt::published(fence, owned(53, 1, OwnedEffectKind::ManagedCredential),)
                .unwrap_err(),
            SetupReceiptError::InvalidPublicationEffect
        );
        assert_eq!(
            PublicationReceipt::cancelled(
                fence,
                Some(owned(53, 2, OwnedEffectKind::CatalogCache)),
            )
            .unwrap_err(),
            SetupReceiptError::InvalidPublicationEffect
        );
        assert_eq!(
            PublicationReceipt::failed(
                fence,
                SetupFailureKind::PublicationIo,
                Some(owned(999, 3, OwnedEffectKind::ConfigFile)),
            )
            .unwrap_err(),
            SetupReceiptError::PublicationEffectWrongTransaction
        );

        PublicationReceipt::published(fence, owned(53, 4, OwnedEffectKind::ConfigFile))
            .expect("published requires one exact config-file effect");
        PublicationReceipt::collision(fence);
        PublicationReceipt::cancelled(fence, None)
            .expect("pre-install cancellation carries no config effect");
        PublicationReceipt::failed(
            fence,
            SetupFailureKind::Interrupted,
            Some(owned(53, 5, OwnedEffectKind::ConfigFile)),
        )
        .expect("post-install failure may carry one config effect");
    }

    #[test]
    fn publication_effect_is_fenced_merged_once_and_rolled_back_first() {
        let mut transaction = transaction(59, custom_proposal());
        advance_to_publication(
            &mut transaction,
            vec![owned(59, 1, OwnedEffectKind::ManagedCredential)],
        );
        let publish_fence = current_fence(&transaction, SetupStage::Publish);

        let wrong_fence = SetupFence::new(59, publish_fence.ordinal() + 1).unwrap();
        let wrong_receipt = PublicationReceipt::failed(
            wrong_fence,
            SetupFailureKind::PublicationIo,
            Some(owned(59, 2, OwnedEffectKind::ConfigFile)),
        )
        .unwrap();
        assert_eq!(
            transaction.accept_publication(wrong_receipt),
            Err(SetupTransitionError::FenceMismatch)
        );
        assert_eq!(transaction.directive().stage(), SetupStage::Publish);

        let duplicate = PublicationReceipt::failed(
            publish_fence,
            SetupFailureKind::PublicationIo,
            Some(owned(59, 1, OwnedEffectKind::ConfigFile)),
        )
        .unwrap();
        assert_eq!(
            transaction.accept_publication(duplicate),
            Err(SetupTransitionError::DuplicateOwnedEffect)
        );
        assert_eq!(transaction.directive().stage(), SetupStage::Publish);

        transaction
            .accept_publication(
                PublicationReceipt::failed(
                    publish_fence,
                    SetupFailureKind::PublicationIo,
                    Some(owned(59, 2, OwnedEffectKind::ConfigFile)),
                )
                .unwrap(),
            )
            .unwrap();
        match transaction.directive() {
            SetupDirective::Rollback { effects, .. } => {
                assert_eq!(
                    effects
                        .iter()
                        .map(|effect| (effect.id(), effect.kind()))
                        .collect::<Vec<_>>(),
                    vec![
                        (2, OwnedEffectKind::ConfigFile),
                        (1, OwnedEffectKind::ManagedCredential),
                    ]
                );
            }
            other => panic!("expected rollback after partial publication, got {other:?}"),
        }
    }

    #[test]
    fn interrupted_publication_and_incomplete_rollback_retain_exact_residue() {
        let mut transaction = transaction(37, custom_proposal());
        advance_to_publication(
            &mut transaction,
            vec![
                owned(37, 5, OwnedEffectKind::CatalogCache),
                owned(37, 6, OwnedEffectKind::ManagedCredential),
            ],
        );
        let publish_fence = current_fence(&transaction, SetupStage::Publish);
        transaction
            .accept_publication(
                PublicationReceipt::failed(publish_fence, SetupFailureKind::Interrupted, None)
                    .unwrap(),
            )
            .unwrap();
        finish_rollback(&mut transaction, vec![6]);

        let terminal = transaction.terminal().unwrap();
        assert_eq!(
            terminal.cause(),
            &SetupTerminalCause::Failed {
                at: SetupStage::Publish,
                kind: SetupFailureKind::Interrupted,
            }
        );
        assert_eq!(
            terminal.rollback(),
            RollbackTruth::Incomplete {
                rolled_back: 1,
                remaining: 1,
            }
        );
        assert_eq!(
            terminal
                .retained_effects()
                .iter()
                .map(OwnedEffectView::id)
                .collect::<Vec<_>>(),
            vec![6]
        );
    }

    #[test]
    fn terminal_ownership_and_rollback_residue_transfer_to_a_recovery_owner() {
        let awaiting = transaction(39, local_proposal());
        let transfer_error = awaiting.into_terminal().unwrap_err();
        assert_eq!(transfer_error.stage(), SetupStage::AwaitingConsent);
        let awaiting = transfer_error.into_transaction();
        assert_eq!(awaiting.directive().stage(), SetupStage::AwaitingConsent);

        let mut incomplete = transaction(39, custom_proposal());
        advance_to_publication(
            &mut incomplete,
            vec![
                owned(39, 7, OwnedEffectKind::CatalogCache),
                owned(39, 8, OwnedEffectKind::ManagedCredential),
            ],
        );
        let publish_fence = current_fence(&incomplete, SetupStage::Publish);
        incomplete
            .accept_publication(
                PublicationReceipt::failed(publish_fence, SetupFailureKind::Interrupted, None)
                    .unwrap(),
            )
            .unwrap();
        finish_rollback(&mut incomplete, vec![8]);

        let terminal = incomplete.into_terminal().unwrap();
        let (cause, rollback, retained_effects) = terminal.into_parts();
        assert_eq!(
            cause,
            SetupTerminalCause::Failed {
                at: SetupStage::Publish,
                kind: SetupFailureKind::Interrupted,
            }
        );
        assert_eq!(
            rollback,
            RollbackTruth::Incomplete {
                rolled_back: 1,
                remaining: 1,
            }
        );
        assert_eq!(
            retained_effects
                .iter()
                .map(OwnedEffectReceipt::view)
                .map(|effect| effect.id())
                .collect::<Vec<_>>(),
            vec![8]
        );

        let mut published = transaction(45, hosted_proposal());
        advance_to_publication(
            &mut published,
            vec![owned(45, 9, OwnedEffectKind::ManagedCredential)],
        );
        let publish_fence = current_fence(&published, SetupStage::Publish);
        published
            .accept_publication(
                PublicationReceipt::published(
                    publish_fence,
                    owned(45, 10, OwnedEffectKind::ConfigFile),
                )
                .unwrap(),
            )
            .unwrap();
        let (cause, rollback, retained_effects) = published.into_terminal().unwrap().into_parts();
        assert_eq!(cause, SetupTerminalCause::Published);
        assert_eq!(rollback, RollbackTruth::NotNeeded);
        assert!(retained_effects.is_empty());
    }

    #[test]
    fn wrong_transaction_duplicate_and_out_of_order_receipts_never_advance() {
        let mut transaction = transaction(41, hosted_proposal());
        transaction.consent().unwrap();
        let acquire_fence = current_fence(&transaction, SetupStage::Acquire);
        let wrong_fence = SetupFence::new(999, acquire_fence.ordinal()).unwrap();
        assert_eq!(
            transaction
                .accept_acquisition(AcquisitionReceipt::acquired(wrong_fence, Vec::new()).unwrap()),
            Err(SetupTransitionError::WrongTransaction)
        );
        assert_eq!(transaction.directive().stage(), SetupStage::Acquire);

        assert_eq!(
            transaction.accept_acquisition(
                AcquisitionReceipt::acquired(
                    acquire_fence,
                    vec![owned(999, 1, OwnedEffectKind::ManagedCredential)],
                )
                .unwrap(),
            ),
            Err(SetupTransitionError::OwnedEffectWrongTransaction)
        );
        assert_eq!(transaction.directive().stage(), SetupStage::Acquire);

        transaction
            .accept_acquisition(AcquisitionReceipt::acquired(acquire_fence, Vec::new()).unwrap())
            .unwrap();
        assert_eq!(transaction.directive().stage(), SetupStage::Validate);
        assert_eq!(
            transaction.accept_acquisition(
                AcquisitionReceipt::acquired(acquire_fence, Vec::new()).unwrap()
            ),
            Err(SetupTransitionError::UnexpectedStage {
                expected: SetupStage::Validate,
                received: SetupStage::Acquire,
            })
        );
        assert_eq!(transaction.directive().stage(), SetupStage::Validate);
    }

    #[test]
    fn owned_effect_caps_duplicates_and_unknown_rollback_failures_are_typed() {
        let fence = SetupFence::new(43, 1).unwrap();
        let too_many = (1..=MAX_OWNED_EFFECTS as u64 + 1)
            .map(|id| owned(43, id, OwnedEffectKind::CatalogCache))
            .collect();
        assert_eq!(
            AcquisitionReceipt::acquired(fence, too_many).unwrap_err(),
            SetupReceiptError::TooManyOwnedEffects
        );
        assert_eq!(
            AcquisitionReceipt::acquired(
                fence,
                vec![
                    owned(43, 1, OwnedEffectKind::CatalogCache),
                    owned(43, 1, OwnedEffectKind::ManagedCredential),
                ],
            )
            .unwrap_err(),
            SetupReceiptError::DuplicateOwnedEffect
        );

        let mut transaction = transaction(43, hosted_proposal());
        advance_to_validation(
            &mut transaction,
            vec![owned(43, 1, OwnedEffectKind::ManagedCredential)],
        );
        let validation_fence = current_fence(&transaction, SetupStage::Validate);
        transaction
            .accept_validation(ValidationReceipt::failed(
                validation_fence,
                SetupFailureKind::ValidationRejected,
            ))
            .unwrap();
        let rollback_fence = current_fence(&transaction, SetupStage::Rollback);
        assert_eq!(
            transaction.accept_rollback(RollbackReceipt::new(rollback_fence, vec![999]).unwrap()),
            Err(SetupTransitionError::UnknownRollbackEffect)
        );
        assert_eq!(transaction.directive().stage(), SetupStage::Rollback);
    }

    #[test]
    fn terminal_state_is_stable_and_rejects_every_late_transition() {
        let mut transaction = transaction(47, local_proposal());
        advance_to_publication(&mut transaction, Vec::new());
        let publish_fence = current_fence(&transaction, SetupStage::Publish);
        transaction
            .accept_publication(
                PublicationReceipt::published(
                    publish_fence,
                    owned(47, 1, OwnedEffectKind::ConfigFile),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(transaction.directive().stage(), SetupStage::Terminal);
        assert_eq!(
            transaction.cancel(),
            Err(SetupTransitionError::AlreadyTerminal)
        );
        assert_eq!(
            transaction.consent(),
            Err(SetupTransitionError::AlreadyTerminal)
        );
        assert_eq!(transaction.directive().stage(), SetupStage::Terminal);
        assert_eq!(
            transaction.terminal().unwrap().cause(),
            &SetupTerminalCause::Published
        );
        assert!(transaction.completed_stages() <= MAX_SETUP_STAGES);
    }
}
