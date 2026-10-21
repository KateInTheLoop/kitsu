//! Private local/offline setup coordinator.
//!
//! Product activation, UI, prompting, credentials, catalogs, providers, and
//! model selection remain deliberately outside this module.

use super::model_picker::{ModelNamespaceBinding, ModelSelectionProposal, SelectionBasis};
use super::onboarding::{
    AcquisitionReceipt, CatalogIntent, CredentialPlan, GuidedSetupTransaction,
    NormalizedSetupPreview, PublicationMode, PublicationReceipt, RollbackReceipt, RollbackTruth,
    SetupAdapter, SetupDirective, SetupFailureKind, SetupPreviewModelError, SetupReceiptError,
    SetupSelectionKind, SetupStage, SetupTerminalCause, SetupTransitionError, ValidationReceipt,
    ValidationScope,
};
use super::publication::{
    publish_create_new, ConfigLedgerError, ConfigLedgerRegistrationError,
    ConfigPublicationFailureKind, ConfigPublicationLedger, ConfigPublicationOutcome,
    ConfigPublicationRecovery, ConfigRecoveryTransferError, ConfigResidueKind,
    ConfigResidueRegistrationError, ConfigRollbackResult, RegisteredConfigPublicationResidue,
    RenderedConfigBytes, UninstalledStageCleanupId,
};
use std::fmt;

const CONFIG_EFFECT_ID: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocalSetupDecision {
    Decline,
    Consent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocalSetupCoordinatorError {
    InvalidTransaction,
    TargetMustBeAbsolute,
    UnsupportedPlan,
    IncompatibleModelNamespace,
}

/// One private owner binds the pure transaction and concrete publication
/// ledger to the same transaction ID. It has no ambient path, provider,
/// credential, catalog, activation, or presentation dependency.
pub(super) struct LocalSetupCoordinator {
    binding: ModelNamespaceBinding,
    transaction: GuidedSetupTransaction,
    publication: ConfigPublicationLedger,
}

impl fmt::Debug for LocalSetupCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalSetupCoordinator")
            .field("stage", &self.transaction.directive().stage())
            .field("publication", &self.publication)
            .finish_non_exhaustive()
    }
}

impl LocalSetupCoordinator {
    pub(super) fn new(
        transaction_id: u64,
        preview: NormalizedSetupPreview,
        binding: ModelNamespaceBinding,
    ) -> Result<Self, LocalSetupCoordinatorError> {
        if transaction_id == 0 {
            return Err(LocalSetupCoordinatorError::InvalidTransaction);
        }
        // A relative preview was normalized lexically, but publication would
        // otherwise bind it to whatever current directory happened to exist
        // later. Refuse it before consent instead of granting ambient meaning.
        if !preview.target().path().is_absolute() {
            return Err(LocalSetupCoordinatorError::TargetMustBeAbsolute);
        }
        if preview.selection() != SetupSelectionKind::Local
            || preview.adapter() != SetupAdapter::CompatibleChat
            || preview.credential() != CredentialPlan::None
            || preview.catalog() != CatalogIntent::OfflineOnly
            || preview.target().publication() != PublicationMode::CreateNew
        {
            return Err(LocalSetupCoordinatorError::UnsupportedPlan);
        }
        if !binding.is_compatible_chat_v1() {
            return Err(LocalSetupCoordinatorError::IncompatibleModelNamespace);
        }
        let transaction = GuidedSetupTransaction::new(transaction_id, preview)
            .map_err(|_| LocalSetupCoordinatorError::InvalidTransaction)?;
        let publication = ConfigPublicationLedger::new(transaction_id)
            .map_err(|_| LocalSetupCoordinatorError::InvalidTransaction)?;
        Ok(Self {
            binding,
            transaction,
            publication,
        })
    }

    pub(super) const fn binding(&self) -> &ModelNamespaceBinding {
        &self.binding
    }

    pub(super) fn preview(&self) -> &NormalizedSetupPreview {
        match self.transaction.directive() {
            SetupDirective::Preview { preview } => preview,
            _ => unreachable!("a coordinator exposes its preview only before it is consumed"),
        }
    }

    pub(super) fn finish(
        self,
        decision: LocalSetupDecision,
    ) -> Result<LocalSetupCompletion, LocalSetupExecutionError> {
        let Self {
            binding: _,
            mut transaction,
            mut publication,
        } = self;

        if decision == LocalSetupDecision::Decline {
            if let Err(error) = transaction.cancel() {
                return Err(LocalSetupExecutionError::coordinator(
                    LocalSetupExecutionFailure::Transition(error),
                    transaction,
                    publication,
                ));
            }
            return finish_terminal(transaction, publication, None, None);
        }

        let rendered = match transaction.directive() {
            SetupDirective::Preview { preview } => match RenderedConfigBytes::render(preview) {
                Ok(rendered) => rendered,
                Err(error) => {
                    return Err(LocalSetupExecutionError::coordinator(
                        LocalSetupExecutionFailure::Render(error),
                        transaction,
                        publication,
                    ));
                }
            },
            directive => {
                return Err(LocalSetupExecutionError::coordinator(
                    LocalSetupExecutionFailure::UnexpectedDirective {
                        expected: SetupStage::AwaitingConsent,
                        actual: directive.stage(),
                    },
                    transaction,
                    publication,
                ));
            }
        };
        if let Err(error) = transaction.consent() {
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::Transition(error),
                transaction,
                publication,
            ));
        }

        let acquisition_fence = match transaction.directive() {
            SetupDirective::Acquire {
                fence,
                credential: CredentialPlan::None,
                catalog: CatalogIntent::OfflineOnly,
            } => fence,
            directive => {
                return Err(LocalSetupExecutionError::coordinator(
                    LocalSetupExecutionFailure::UnexpectedDirective {
                        expected: SetupStage::Acquire,
                        actual: directive.stage(),
                    },
                    transaction,
                    publication,
                ));
            }
        };
        let acquisition = match AcquisitionReceipt::acquired(acquisition_fence, Vec::new()) {
            Ok(receipt) => receipt,
            Err(error) => {
                return Err(LocalSetupExecutionError::coordinator(
                    LocalSetupExecutionFailure::Receipt(error),
                    transaction,
                    publication,
                ));
            }
        };
        if let Err(error) = transaction.accept_acquisition(acquisition) {
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::Transition(error),
                transaction,
                publication,
            ));
        }

        let validation_fence = match transaction.directive() {
            SetupDirective::Validate {
                fence,
                scope: ValidationScope::Offline,
            } => fence,
            directive => {
                return Err(LocalSetupExecutionError::coordinator(
                    LocalSetupExecutionFailure::UnexpectedDirective {
                        expected: SetupStage::Validate,
                        actual: directive.stage(),
                    },
                    transaction,
                    publication,
                ));
            }
        };
        if let Err(error) =
            transaction.accept_validation(ValidationReceipt::validated(validation_fence))
        {
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::Transition(error),
                transaction,
                publication,
            ));
        }

        let (publication_fence, outcome) = match transaction.directive() {
            SetupDirective::Publish {
                fence,
                target,
                mode: PublicationMode::CreateNew,
            } => (fence, publish_create_new(target, &rendered)),
            directive => {
                return Err(LocalSetupExecutionError::coordinator(
                    LocalSetupExecutionFailure::UnexpectedDirective {
                        expected: SetupStage::Publish,
                        actual: directive.stage(),
                    },
                    transaction,
                    publication,
                ));
            }
        };

        match outcome {
            ConfigPublicationOutcome::Created(receipt) => {
                let logical = match publication.register_created(CONFIG_EFFECT_ID, receipt) {
                    Ok(logical) => logical,
                    Err(rejected) => {
                        return Err(LocalSetupExecutionError::created_registration(
                            transaction,
                            publication,
                            rejected,
                        ));
                    }
                };
                let receipt = match PublicationReceipt::published(publication_fence, logical) {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        return Err(LocalSetupExecutionError::coordinator(
                            LocalSetupExecutionFailure::Receipt(error),
                            transaction,
                            publication,
                        ));
                    }
                };
                if let Err(error) = transaction.accept_publication(receipt) {
                    return Err(LocalSetupExecutionError::coordinator(
                        LocalSetupExecutionFailure::Transition(error),
                        transaction,
                        publication,
                    ));
                }
                // Published is adoption, not cleanup residue. Dropping the
                // proof owner closes its file descriptor without deleting the
                // caller-approved config.
                finish_terminal(transaction, publication, None, None)
            }
            ConfigPublicationOutcome::Collision => {
                if let Err(error) =
                    transaction.accept_publication(PublicationReceipt::collision(publication_fence))
                {
                    return Err(LocalSetupExecutionError::coordinator(
                        LocalSetupExecutionFailure::Transition(error),
                        transaction,
                        publication,
                    ));
                }
                finish_terminal(transaction, publication, None, None)
            }
            ConfigPublicationOutcome::Failed {
                kind,
                residue: None,
            } => {
                let receipt = match PublicationReceipt::failed(
                    publication_fence,
                    SetupFailureKind::PublicationIo,
                    None,
                ) {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        return Err(LocalSetupExecutionError::coordinator(
                            LocalSetupExecutionFailure::Receipt(error),
                            transaction,
                            publication,
                        ));
                    }
                };
                if let Err(error) = transaction.accept_publication(receipt) {
                    return Err(LocalSetupExecutionError::coordinator(
                        LocalSetupExecutionFailure::Transition(error),
                        transaction,
                        publication,
                    ));
                }
                finish_terminal(transaction, publication, Some(kind), None)
            }
            ConfigPublicationOutcome::Failed {
                kind,
                residue: Some(residue),
            } => {
                let registered = match publication.register_residue(CONFIG_EFFECT_ID, residue) {
                    Ok(registered) => registered,
                    Err(rejected) => {
                        return Err(LocalSetupExecutionError::residue_registration(
                            transaction,
                            publication,
                            rejected,
                        ));
                    }
                };
                match registered {
                    RegisteredConfigPublicationResidue::OwnedEffect(logical) => {
                        finish_failed_owned_publication(
                            transaction,
                            publication,
                            publication_fence,
                            kind,
                            logical,
                        )
                    }
                    RegisteredConfigPublicationResidue::UninstalledStage(cleanup_id) => {
                        finish_failed_stage_publication(
                            transaction,
                            publication,
                            publication_fence,
                            kind,
                            cleanup_id,
                        )
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectedLocalSetupAdmissionFailure {
    SelectionMustBeExplicit,
    ModelNamespaceMismatch,
    PreviewModelRejected(SetupPreviewModelError),
}

#[derive(Debug)]
pub(super) struct SelectedLocalSetupAdmissionError {
    kind: SelectedLocalSetupAdmissionFailure,
    owners: Box<SelectedLocalSetupAdmissionOwners>,
}

#[derive(Debug)]
struct SelectedLocalSetupAdmissionOwners {
    coordinator: LocalSetupCoordinator,
    proposal: ModelSelectionProposal,
}

impl SelectedLocalSetupAdmissionError {
    pub(super) const fn kind(&self) -> SelectedLocalSetupAdmissionFailure {
        self.kind
    }

    pub(super) fn into_parts(self) -> (LocalSetupCoordinator, ModelSelectionProposal) {
        let SelectedLocalSetupAdmissionOwners {
            coordinator,
            proposal,
        } = *self.owners;
        (coordinator, proposal)
    }
}

/// Sole owner of a selected local setup. The exact picker proposal remains
/// attached across publication, rollback, recovery, and execution failures.
pub(super) struct SelectedLocalSetupCoordinator {
    coordinator: LocalSetupCoordinator,
    proposal: ModelSelectionProposal,
}

impl SelectedLocalSetupCoordinator {
    pub(super) fn new(
        mut coordinator: LocalSetupCoordinator,
        proposal: ModelSelectionProposal,
    ) -> Result<Self, SelectedLocalSetupAdmissionError> {
        if proposal.basis() != SelectionBasis::Explicit {
            return Err(SelectedLocalSetupAdmissionError {
                kind: SelectedLocalSetupAdmissionFailure::SelectionMustBeExplicit,
                owners: Box::new(SelectedLocalSetupAdmissionOwners {
                    coordinator,
                    proposal,
                }),
            });
        }
        if proposal.binding() != coordinator.binding() {
            return Err(SelectedLocalSetupAdmissionError {
                kind: SelectedLocalSetupAdmissionFailure::ModelNamespaceMismatch,
                owners: Box::new(SelectedLocalSetupAdmissionOwners {
                    coordinator,
                    proposal,
                }),
            });
        }
        if let Err(error) = coordinator
            .transaction
            .replace_preview_model_exact(proposal.model_id())
        {
            return Err(SelectedLocalSetupAdmissionError {
                kind: SelectedLocalSetupAdmissionFailure::PreviewModelRejected(error),
                owners: Box::new(SelectedLocalSetupAdmissionOwners {
                    coordinator,
                    proposal,
                }),
            });
        }
        Ok(Self {
            coordinator,
            proposal,
        })
    }

    pub(super) fn preview(&self) -> &NormalizedSetupPreview {
        self.coordinator.preview()
    }

    pub(super) const fn proposal(&self) -> &ModelSelectionProposal {
        &self.proposal
    }

    pub(super) fn finish(
        self,
        decision: LocalSetupDecision,
    ) -> Result<SelectedLocalSetupCompletion, SelectedLocalSetupExecutionError> {
        let Self {
            coordinator,
            proposal,
        } = self;
        match coordinator.finish(decision) {
            Ok(completion) => Ok(SelectedLocalSetupCompletion {
                completion,
                proposal,
            }),
            Err(error) => Err(SelectedLocalSetupExecutionError { error, proposal }),
        }
    }
}

pub(super) struct SelectedLocalSetupCompletion {
    completion: LocalSetupCompletion,
    proposal: ModelSelectionProposal,
}

impl SelectedLocalSetupCompletion {
    pub(super) const fn proposal(&self) -> &ModelSelectionProposal {
        &self.proposal
    }

    pub(super) const fn cause(&self) -> &SetupTerminalCause {
        self.completion.cause()
    }

    pub(super) const fn rollback(&self) -> RollbackTruth {
        self.completion.rollback()
    }

    pub(super) const fn publication_failure(&self) -> Option<ConfigPublicationFailureKind> {
        self.completion.publication_failure()
    }

    pub(super) const fn recovery_kind(&self) -> Option<ConfigResidueKind> {
        self.completion.recovery_kind()
    }

    pub(super) fn retry_recovery(
        &mut self,
    ) -> Result<LocalSetupRecoveryStatus, LocalSetupRecoveryError> {
        self.completion.retry_recovery()
    }
}

#[derive(Debug)]
pub(super) struct SelectedLocalSetupExecutionError {
    error: LocalSetupExecutionError,
    proposal: ModelSelectionProposal,
}

impl SelectedLocalSetupExecutionError {
    pub(super) const fn kind(&self) -> &LocalSetupExecutionFailure {
        self.error.kind()
    }

    pub(super) const fn proposal(&self) -> &ModelSelectionProposal {
        &self.proposal
    }

    pub(super) fn into_parts(self) -> (LocalSetupExecutionError, ModelSelectionProposal) {
        (self.error, self.proposal)
    }
}

fn finish_failed_owned_publication(
    mut transaction: GuidedSetupTransaction,
    mut publication: ConfigPublicationLedger,
    fence: super::onboarding::SetupFence,
    failure: ConfigPublicationFailureKind,
    logical: super::onboarding::OwnedEffectReceipt,
) -> Result<LocalSetupCompletion, LocalSetupExecutionError> {
    let receipt =
        match PublicationReceipt::failed(fence, SetupFailureKind::PublicationIo, Some(logical)) {
            Ok(receipt) => receipt,
            Err(error) => {
                return Err(LocalSetupExecutionError::coordinator(
                    LocalSetupExecutionFailure::Receipt(error),
                    transaction,
                    publication,
                ));
            }
        };
    if let Err(error) = transaction.accept_publication(receipt) {
        return Err(LocalSetupExecutionError::coordinator(
            LocalSetupExecutionFailure::Transition(error),
            transaction,
            publication,
        ));
    }
    let rollback_fence = match transaction.directive() {
        SetupDirective::Rollback { fence, effects }
            if effects.len() == 1
                && effects[0].id() == CONFIG_EFFECT_ID
                && effects[0].kind() == super::onboarding::OwnedEffectKind::ConfigFile =>
        {
            fence
        }
        directive => {
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::UnexpectedDirective {
                    expected: SetupStage::Rollback,
                    actual: directive.stage(),
                },
                transaction,
                publication,
            ));
        }
    };
    let rollback = match publication.rollback(CONFIG_EFFECT_ID) {
        Ok(rollback) => rollback,
        Err(error) => {
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::Ledger(error),
                transaction,
                publication,
            ));
        }
    };
    let (failed_ids, retained_kind) = match rollback {
        ConfigRollbackResult::Removed => (Vec::new(), None),
        ConfigRollbackResult::Incomplete(kind) => (vec![CONFIG_EFFECT_ID], Some(kind)),
    };
    let receipt = match RollbackReceipt::new(rollback_fence, failed_ids) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::Receipt(error),
                transaction,
                publication,
            ));
        }
    };
    if let Err(error) = transaction.accept_rollback(receipt) {
        return Err(LocalSetupExecutionError::coordinator(
            LocalSetupExecutionFailure::Transition(error),
            transaction,
            publication,
        ));
    }
    finish_terminal(transaction, publication, Some(failure), retained_kind)
}

fn finish_failed_stage_publication(
    mut transaction: GuidedSetupTransaction,
    mut publication: ConfigPublicationLedger,
    fence: super::onboarding::SetupFence,
    failure: ConfigPublicationFailureKind,
    cleanup_id: UninstalledStageCleanupId,
) -> Result<LocalSetupCompletion, LocalSetupExecutionError> {
    let cleanup = match publication.rollback_uninstalled_stage(cleanup_id) {
        Ok(cleanup) => cleanup,
        Err(error) => {
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::Ledger(error),
                transaction,
                publication,
            ));
        }
    };
    let receipt = match PublicationReceipt::failed(fence, SetupFailureKind::PublicationIo, None) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::Receipt(error),
                transaction,
                publication,
            ));
        }
    };
    if let Err(error) = transaction.accept_publication(receipt) {
        return Err(LocalSetupExecutionError::coordinator(
            LocalSetupExecutionFailure::Transition(error),
            transaction,
            publication,
        ));
    }
    let terminal = match transaction.into_terminal() {
        Ok(terminal) => terminal,
        Err(error) => {
            let stage = error.stage();
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::UnexpectedDirective {
                    expected: SetupStage::Terminal,
                    actual: stage,
                },
                error.into_transaction(),
                publication,
            ));
        }
    };
    let (cause, rollback, retained) = terminal.into_parts();
    if !retained.is_empty() {
        return Err(LocalSetupExecutionError::coordinator_parts(
            LocalSetupExecutionFailure::RecoveryInvariant,
            cause,
            rollback,
            publication,
        ));
    }
    let recovery = match cleanup {
        ConfigRollbackResult::Removed => None,
        ConfigRollbackResult::Incomplete(kind) => Some(LocalSetupRecovery::UninstalledStage {
            publication,
            cleanup_id,
            kind,
        }),
    };
    Ok(LocalSetupCompletion {
        cause,
        rollback,
        publication_failure: Some(failure),
        recovery,
    })
}

fn finish_terminal(
    transaction: GuidedSetupTransaction,
    publication: ConfigPublicationLedger,
    publication_failure: Option<ConfigPublicationFailureKind>,
    retained_kind: Option<ConfigResidueKind>,
) -> Result<LocalSetupCompletion, LocalSetupExecutionError> {
    let terminal = match transaction.into_terminal() {
        Ok(terminal) => terminal,
        Err(error) => {
            let stage = error.stage();
            return Err(LocalSetupExecutionError::coordinator(
                LocalSetupExecutionFailure::UnexpectedDirective {
                    expected: SetupStage::Terminal,
                    actual: stage,
                },
                error.into_transaction(),
                publication,
            ));
        }
    };
    let (cause, rollback, retained) = terminal.into_parts();
    if retained.is_empty() {
        return Ok(LocalSetupCompletion {
            cause,
            rollback,
            publication_failure,
            recovery: None,
        });
    }
    match publication.into_recovery(retained) {
        Ok(owner) => {
            let effect_ids = owner.effect_ids();
            if effect_ids.as_slice() != [CONFIG_EFFECT_ID] {
                return Err(LocalSetupExecutionError::recovery_owner(
                    LocalSetupExecutionFailure::RecoveryInvariant,
                    cause,
                    rollback,
                    owner,
                ));
            }
            let effect_id = CONFIG_EFFECT_ID;
            let Some(kind) = retained_kind else {
                return Err(LocalSetupExecutionError::recovery_owner(
                    LocalSetupExecutionFailure::RecoveryInvariant,
                    cause,
                    rollback,
                    owner,
                ));
            };
            Ok(LocalSetupCompletion {
                cause,
                rollback,
                publication_failure,
                recovery: Some(LocalSetupRecovery::Installed {
                    publication: owner,
                    effect_id,
                    kind,
                }),
            })
        }
        Err(rejected) => Err(LocalSetupExecutionError::recovery_transfer(
            cause, rollback, rejected,
        )),
    }
}

pub(super) struct LocalSetupCompletion {
    cause: SetupTerminalCause,
    rollback: RollbackTruth,
    publication_failure: Option<ConfigPublicationFailureKind>,
    recovery: Option<LocalSetupRecovery>,
}

impl LocalSetupCompletion {
    pub(super) const fn cause(&self) -> &SetupTerminalCause {
        &self.cause
    }

    pub(super) const fn rollback(&self) -> RollbackTruth {
        self.rollback
    }

    pub(super) const fn publication_failure(&self) -> Option<ConfigPublicationFailureKind> {
        self.publication_failure
    }

    pub(super) const fn recovery_kind(&self) -> Option<ConfigResidueKind> {
        match &self.recovery {
            Some(LocalSetupRecovery::Installed { kind, .. })
            | Some(LocalSetupRecovery::UninstalledStage { kind, .. }) => Some(*kind),
            None => None,
        }
    }

    pub(super) fn retry_recovery(
        &mut self,
    ) -> Result<LocalSetupRecoveryStatus, LocalSetupRecoveryError> {
        let Some(recovery) = self.recovery.take() else {
            return Err(LocalSetupRecoveryError::NoRecovery);
        };
        match recovery {
            LocalSetupRecovery::Installed {
                mut publication,
                effect_id,
                kind,
            } => match publication.rollback(effect_id) {
                Ok(ConfigRollbackResult::Removed) => Ok(LocalSetupRecoveryStatus::Complete),
                Ok(ConfigRollbackResult::Incomplete(next)) => {
                    self.recovery = Some(LocalSetupRecovery::Installed {
                        publication,
                        effect_id,
                        kind: next,
                    });
                    Ok(LocalSetupRecoveryStatus::Incomplete(next))
                }
                Err(error) => {
                    self.recovery = Some(LocalSetupRecovery::Installed {
                        publication,
                        effect_id,
                        kind,
                    });
                    Err(LocalSetupRecoveryError::Ledger(error))
                }
            },
            LocalSetupRecovery::UninstalledStage {
                mut publication,
                cleanup_id,
                kind,
            } => match publication.rollback_uninstalled_stage(cleanup_id) {
                Ok(ConfigRollbackResult::Removed) => Ok(LocalSetupRecoveryStatus::Complete),
                Ok(ConfigRollbackResult::Incomplete(next)) => {
                    self.recovery = Some(LocalSetupRecovery::UninstalledStage {
                        publication,
                        cleanup_id,
                        kind: next,
                    });
                    Ok(LocalSetupRecoveryStatus::Incomplete(next))
                }
                Err(error) => {
                    self.recovery = Some(LocalSetupRecovery::UninstalledStage {
                        publication,
                        cleanup_id,
                        kind,
                    });
                    Err(LocalSetupRecoveryError::Ledger(error))
                }
            },
        }
    }
}

enum LocalSetupRecovery {
    Installed {
        publication: ConfigPublicationRecovery,
        effect_id: u64,
        kind: ConfigResidueKind,
    },
    UninstalledStage {
        publication: ConfigPublicationLedger,
        cleanup_id: UninstalledStageCleanupId,
        kind: ConfigResidueKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocalSetupRecoveryStatus {
    Complete,
    Incomplete(ConfigResidueKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocalSetupRecoveryError {
    NoRecovery,
    Ledger(ConfigLedgerError),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LocalSetupExecutionFailure {
    Render(super::publication::ConfigRenderError),
    Receipt(SetupReceiptError),
    Transition(SetupTransitionError),
    Ledger(ConfigLedgerError),
    UnexpectedDirective {
        expected: SetupStage,
        actual: SetupStage,
    },
    Registration,
    RecoveryInvariant,
    RecoveryTransfer(ConfigLedgerError),
}

pub(super) struct LocalSetupExecutionError {
    kind: LocalSetupExecutionFailure,
    owner: Box<LocalSetupExecutionOwner>,
}

enum LocalSetupExecutionOwner {
    Coordinator {
        transaction: Box<GuidedSetupTransaction>,
        publication: Box<ConfigPublicationLedger>,
    },
    CoordinatorParts {
        cause: SetupTerminalCause,
        rollback: RollbackTruth,
        publication: Box<ConfigPublicationLedger>,
    },
    CreatedRegistration {
        transaction: Box<GuidedSetupTransaction>,
        publication: Box<ConfigPublicationLedger>,
        rejected: ConfigLedgerRegistrationError,
    },
    ResidueRegistration {
        transaction: Box<GuidedSetupTransaction>,
        publication: Box<ConfigPublicationLedger>,
        rejected: ConfigResidueRegistrationError,
    },
    RecoveryOwner {
        cause: SetupTerminalCause,
        rollback: RollbackTruth,
        publication: ConfigPublicationRecovery,
    },
    RecoveryTransfer {
        cause: SetupTerminalCause,
        rollback: RollbackTruth,
        rejected: ConfigRecoveryTransferError,
    },
}

impl LocalSetupExecutionError {
    pub(super) const fn kind(&self) -> &LocalSetupExecutionFailure {
        &self.kind
    }

    fn coordinator(
        kind: LocalSetupExecutionFailure,
        transaction: GuidedSetupTransaction,
        publication: ConfigPublicationLedger,
    ) -> Self {
        Self {
            kind,
            owner: Box::new(LocalSetupExecutionOwner::Coordinator {
                transaction: Box::new(transaction),
                publication: Box::new(publication),
            }),
        }
    }

    fn created_registration(
        transaction: GuidedSetupTransaction,
        publication: ConfigPublicationLedger,
        rejected: ConfigLedgerRegistrationError,
    ) -> Self {
        Self {
            kind: LocalSetupExecutionFailure::Registration,
            owner: Box::new(LocalSetupExecutionOwner::CreatedRegistration {
                transaction: Box::new(transaction),
                publication: Box::new(publication),
                rejected,
            }),
        }
    }

    fn coordinator_parts(
        kind: LocalSetupExecutionFailure,
        cause: SetupTerminalCause,
        rollback: RollbackTruth,
        publication: ConfigPublicationLedger,
    ) -> Self {
        Self {
            kind,
            owner: Box::new(LocalSetupExecutionOwner::CoordinatorParts {
                cause,
                rollback,
                publication: Box::new(publication),
            }),
        }
    }

    fn residue_registration(
        transaction: GuidedSetupTransaction,
        publication: ConfigPublicationLedger,
        rejected: ConfigResidueRegistrationError,
    ) -> Self {
        Self {
            kind: LocalSetupExecutionFailure::Registration,
            owner: Box::new(LocalSetupExecutionOwner::ResidueRegistration {
                transaction: Box::new(transaction),
                publication: Box::new(publication),
                rejected,
            }),
        }
    }

    fn recovery_owner(
        kind: LocalSetupExecutionFailure,
        cause: SetupTerminalCause,
        rollback: RollbackTruth,
        publication: ConfigPublicationRecovery,
    ) -> Self {
        Self {
            kind,
            owner: Box::new(LocalSetupExecutionOwner::RecoveryOwner {
                cause,
                rollback,
                publication,
            }),
        }
    }

    fn recovery_transfer(
        cause: SetupTerminalCause,
        rollback: RollbackTruth,
        rejected: ConfigRecoveryTransferError,
    ) -> Self {
        Self {
            kind: LocalSetupExecutionFailure::RecoveryTransfer(rejected.error()),
            owner: Box::new(LocalSetupExecutionOwner::RecoveryTransfer {
                cause,
                rollback,
                rejected,
            }),
        }
    }
}

impl fmt::Debug for LocalSetupExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let owner = match self.owner.as_ref() {
            LocalSetupExecutionOwner::Coordinator { .. } => "coordinator",
            LocalSetupExecutionOwner::CoordinatorParts { .. } => "coordinator-parts",
            LocalSetupExecutionOwner::CreatedRegistration { .. } => "created-registration",
            LocalSetupExecutionOwner::ResidueRegistration { .. } => "residue-registration",
            LocalSetupExecutionOwner::RecoveryOwner { .. } => "recovery",
            LocalSetupExecutionOwner::RecoveryTransfer { .. } => "recovery-transfer",
        };
        formatter
            .debug_struct("LocalSetupExecutionError")
            .field("kind", &self.kind)
            .field("retained_owner", &owner)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model_picker::{
        CatalogAvailability, CatalogFreshness, CatalogMembership, CatalogProvenance,
        CatalogSnapshotInput, CatalogSource, CatalogTruth, ModelNamespaceBinding,
        ModelPickerSnapshot, ModelSelectionInput, ModelSelectionProposal, SelectionBasis,
        MODEL_SELECTION_PROPOSAL_REVISION,
    };
    use crate::config::onboarding::{
        CatalogIntent, CredentialPlan, NormalizedSetupPreview, RollbackTruth, SetupAdapter,
        SetupProposalInput, SetupRouteInput, SetupSelection, SetupStage, SetupTerminalCause,
    };
    #[cfg(target_os = "linux")]
    use crate::config::publication::{
        inject_next_rollback_fault, inject_publication_faults, ConfigPublicationFailureKind,
        ConfigResidueKind, PublicationFaultPoint, RollbackFaultPoint,
    };
    use kitsu_providers::route::{ProviderProfileId, ProviderRouteDeclaration};
    use std::path::{Path, PathBuf};

    fn profile(name: &[u8]) -> ProviderProfileId {
        ProviderProfileId::derive_v1(b"selected-local-setup", name).unwrap()
    }

    fn compatible_binding(name: &[u8]) -> ModelNamespaceBinding {
        ModelNamespaceBinding::from_declaration(&ProviderRouteDeclaration::compatible_chat_v1(
            profile(name),
            true,
        ))
    }

    fn native_messages_binding(name: &[u8]) -> ModelNamespaceBinding {
        ModelNamespaceBinding::from_declaration(&ProviderRouteDeclaration::native_messages_v1(
            profile(name),
        ))
    }

    fn selection_truth() -> CatalogTruth {
        CatalogTruth::new(
            CatalogAvailability::Offline,
            CatalogSource::Cache,
            CatalogFreshness::Stale,
            CatalogProvenance::RetainedSnapshot,
        )
    }

    fn selection_snapshot(binding: ModelNamespaceBinding) -> ModelPickerSnapshot {
        CatalogSnapshotInput::new(
            binding,
            selection_truth(),
            ["fallback/model", "selected/model"],
            Some("selected/model"),
            Some("fallback/model"),
        )
        .normalize()
        .unwrap()
    }

    fn explicit_proposal(binding: ModelNamespaceBinding) -> ModelSelectionProposal {
        selection_snapshot(binding)
            .stage_exact(ModelSelectionInput::new("  selected/model  "))
            .unwrap()
    }

    fn assert_complete_proposal_truth(
        proposal: &ModelSelectionProposal,
        binding: &ModelNamespaceBinding,
        basis: SelectionBasis,
    ) {
        assert_eq!(proposal.binding(), binding);
        assert_eq!(proposal.revision(), MODEL_SELECTION_PROPOSAL_REVISION);
        assert_eq!(proposal.model_id(), "selected/model");
        assert_eq!(proposal.basis(), basis);
        assert!(proposal.was_current());
        assert!(!proposal.was_default());
        assert_eq!(proposal.catalog_membership(), CatalogMembership::Present);
        assert_eq!(proposal.catalog_truth(), selection_truth());
    }

    fn local_preview(target: impl Into<PathBuf>) -> NormalizedSetupPreview {
        NormalizedSetupPreview::normalize(SetupProposalInput::new(
            target,
            SetupSelection::Local {
                route: SetupRouteInput::new(
                    "local",
                    "http://127.0.0.1:11434",
                    "/v1/chat/completions",
                    None,
                    "model-v1",
                    CatalogIntent::OfflineOnly,
                ),
            },
        ))
        .unwrap()
    }

    fn hosted_preview(target: impl Into<PathBuf>) -> NormalizedSetupPreview {
        NormalizedSetupPreview::normalize(SetupProposalInput::new(
            target,
            SetupSelection::Hosted {
                route: SetupRouteInput::new(
                    "hosted",
                    "https://api.example.test",
                    "/v1/chat/completions",
                    Some("/v1/models"),
                    "model-v1",
                    CatalogIntent::CatalogAfterConsent,
                ),
            },
        ))
        .unwrap()
    }

    fn run_local(transaction_id: u64, target: &Path) -> LocalSetupCompletion {
        LocalSetupCoordinator::new(
            transaction_id,
            local_preview(target),
            compatible_binding(b"base-path"),
        )
        .unwrap()
        .finish(LocalSetupDecision::Consent)
        .unwrap()
    }

    fn selected_local(
        transaction_id: u64,
        target: &Path,
        binding: ModelNamespaceBinding,
    ) -> SelectedLocalSetupCoordinator {
        let coordinator =
            LocalSetupCoordinator::new(transaction_id, local_preview(target), binding.clone())
                .unwrap();
        SelectedLocalSetupCoordinator::new(coordinator, explicit_proposal(binding)).unwrap()
    }

    #[test]
    fn admission_is_absolute_local_credential_free_and_offline_before_effects() {
        let relative = PathBuf::from(format!(
            ".kitsu-local-setup-relative-{}.toml",
            std::process::id()
        ));
        assert!(!relative.exists());
        assert_eq!(
            LocalSetupCoordinator::new(
                3,
                local_preview(&relative),
                compatible_binding(b"relative"),
            )
            .unwrap_err(),
            LocalSetupCoordinatorError::TargetMustBeAbsolute
        );
        assert!(!relative.exists());

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        assert_eq!(
            LocalSetupCoordinator::new(5, hosted_preview(&target), compatible_binding(b"hosted"),)
                .unwrap_err(),
            LocalSetupCoordinatorError::UnsupportedPlan
        );
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn explicit_decline_is_zero_effect() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let completion =
            LocalSetupCoordinator::new(7, local_preview(&target), compatible_binding(b"decline"))
                .unwrap()
                .finish(LocalSetupDecision::Decline)
                .unwrap();

        assert_eq!(
            completion.cause(),
            &SetupTerminalCause::Cancelled {
                at: SetupStage::AwaitingConsent,
            }
        );
        assert_eq!(completion.rollback(), RollbackTruth::NotNeeded);
        assert_eq!(completion.publication_failure(), None);
        assert_eq!(completion.recovery_kind(), None);
        assert!(!target.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn consent_consumes_only_offline_directives_and_publishes_exact_config() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let completion = run_local(11, &target);

        assert_eq!(completion.cause(), &SetupTerminalCause::Published);
        assert_eq!(completion.rollback(), RollbackTruth::NotNeeded);
        assert_eq!(completion.publication_failure(), None);
        assert_eq!(completion.recovery_kind(), None);
        let text = std::fs::read_to_string(&target).unwrap();
        let config: crate::config::Config = toml::from_str(&text).unwrap();
        assert_eq!(config.default_gateway.as_deref(), Some("local"));
        let gateway = &config.gateways["local"];
        assert_eq!(
            gateway.adapter,
            crate::config::GatewayAdapter::CompatibleChat
        );
        assert_eq!(gateway.auth.method, "none");
        assert!(gateway.auth.token.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn collision_preserves_preexisting_config_byte_for_byte() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let original = b"preexisting = true\n";
        std::fs::write(&target, original).unwrap();

        let completion = run_local(13, &target);

        assert_eq!(completion.cause(), &SetupTerminalCause::Collision);
        assert_eq!(completion.rollback(), RollbackTruth::NotNeeded);
        assert_eq!(completion.recovery_kind(), None);
        assert_eq!(std::fs::read(&target).unwrap(), original);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_parent_fails_without_creating_it() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("missing");
        let target = parent.join("kitsu.toml");

        let completion = run_local(17, &target);

        assert_eq!(
            completion.cause(),
            &SetupTerminalCause::Failed {
                at: SetupStage::Publish,
                kind: crate::config::onboarding::SetupFailureKind::PublicationIo,
            }
        );
        assert_eq!(
            completion.publication_failure(),
            Some(ConfigPublicationFailureKind::ParentMissing)
        );
        assert_eq!(completion.rollback(), RollbackTruth::NotNeeded);
        assert_eq!(completion.recovery_kind(), None);
        assert!(!parent.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uninstalled_stage_residue_uses_its_nonlogical_cleanup_channel() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let _publication_fault_scope = inject_publication_faults(&[
            PublicationFaultPoint::Write,
            PublicationFaultPoint::UninstalledStageCleanup,
        ]);

        let completion = run_local(19, &target);

        assert_eq!(
            completion.publication_failure(),
            Some(ConfigPublicationFailureKind::Write)
        );
        assert_eq!(completion.rollback(), RollbackTruth::NotNeeded);
        assert_eq!(completion.recovery_kind(), None);
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn incomplete_stage_cleanup_returns_its_distinct_recovery_owner() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let _publication_fault_scope = inject_publication_faults(&[
            PublicationFaultPoint::Write,
            PublicationFaultPoint::UninstalledStageCleanup,
        ]);
        let _rollback_fault_scope = inject_next_rollback_fault(RollbackFaultPoint::DirectorySync);

        let mut completion = run_local(21, &target);

        assert_eq!(completion.rollback(), RollbackTruth::NotNeeded);
        assert_eq!(
            completion.recovery_kind(),
            Some(ConfigResidueKind::UninstalledStageCleanup)
        );
        assert_eq!(
            completion.retry_recovery().unwrap(),
            LocalSetupRecoveryStatus::Complete
        );
        assert_eq!(completion.recovery_kind(), None);
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn installed_failure_is_registered_then_rolled_back_by_exact_directive() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let _publication_fault_scope =
            inject_publication_faults(&[PublicationFaultPoint::DirectorySync]);

        let completion = run_local(23, &target);

        assert_eq!(
            completion.publication_failure(),
            Some(ConfigPublicationFailureKind::DirectorySync)
        );
        assert_eq!(
            completion.rollback(),
            RollbackTruth::Complete { effects: 1 }
        );
        assert_eq!(completion.recovery_kind(), None);
        assert!(!target.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn incomplete_rollback_returns_the_matching_concrete_recovery_owner() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let _publication_fault_scope =
            inject_publication_faults(&[PublicationFaultPoint::DirectorySync]);
        let _rollback_fault_scope = inject_next_rollback_fault(RollbackFaultPoint::DirectorySync);

        let mut completion = run_local(29, &target);

        assert_eq!(
            completion.rollback(),
            RollbackTruth::Incomplete {
                rolled_back: 0,
                remaining: 1,
            }
        );
        assert_eq!(
            completion.recovery_kind(),
            Some(ConfigResidueKind::RemovedUnconfirmed)
        );
        assert!(!target.exists());
        assert_eq!(
            completion.retry_recovery().unwrap(),
            LocalSetupRecoveryStatus::Complete
        );
        assert_eq!(completion.recovery_kind(), None);
    }

    #[test]
    fn local_fixture_contract_has_no_credential_or_catalog_work() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = LocalSetupCoordinator::new(
            31,
            local_preview(directory.path().join("kitsu.toml")),
            compatible_binding(b"fixture-contract"),
        )
        .unwrap();
        let preview = coordinator.preview();
        assert_eq!(
            preview.selection(),
            crate::config::onboarding::SetupSelectionKind::Local
        );
        assert_eq!(preview.credential(), CredentialPlan::None);
        assert_eq!(preview.catalog(), CatalogIntent::OfflineOnly);
        assert_eq!(preview.adapter(), SetupAdapter::CompatibleChat);
    }

    #[test]
    fn coordinator_rejects_a_valid_but_incompatible_compiled_namespace_without_effects() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");

        assert_eq!(
            LocalSetupCoordinator::new(
                37,
                local_preview(&target),
                native_messages_binding(b"native-coordinator"),
            )
            .unwrap_err(),
            LocalSetupCoordinatorError::IncompatibleModelNamespace,
        );
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn selected_setup_rejects_preferred_selection_and_returns_both_untouched_owners() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let binding = compatible_binding(b"preferred");
        let coordinator =
            LocalSetupCoordinator::new(41, local_preview(&target), binding.clone()).unwrap();
        let proposal = selection_snapshot(binding.clone())
            .stage_preferred()
            .unwrap();

        let rejected = SelectedLocalSetupCoordinator::new(coordinator, proposal)
            .err()
            .unwrap();
        assert_eq!(
            rejected.kind(),
            SelectedLocalSetupAdmissionFailure::SelectionMustBeExplicit,
        );
        let (coordinator, proposal) = rejected.into_parts();
        assert_eq!(coordinator.preview().model(), "model-v1");
        assert_eq!(coordinator.binding(), &binding);
        assert_complete_proposal_truth(&proposal, &binding, SelectionBasis::Current);
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn selected_setup_rejects_exact_namespace_mismatches_and_returns_untouched_owners() {
        let directory = tempfile::tempdir().unwrap();

        for (transaction_id, suffix, proposal_binding) in [
            (43, "other-profile", compatible_binding(b"other-profile")),
            (45, "native-wire", native_messages_binding(b"native-wire")),
        ] {
            let target = directory.path().join(format!("{suffix}.toml"));
            let coordinator_profile = format!("coordinator-{suffix}");
            let coordinator_binding = compatible_binding(coordinator_profile.as_bytes());
            let coordinator = LocalSetupCoordinator::new(
                transaction_id,
                local_preview(&target),
                coordinator_binding.clone(),
            )
            .unwrap();
            let proposal = explicit_proposal(proposal_binding.clone());

            let rejected = SelectedLocalSetupCoordinator::new(coordinator, proposal)
                .err()
                .unwrap();
            assert_eq!(
                rejected.kind(),
                SelectedLocalSetupAdmissionFailure::ModelNamespaceMismatch,
            );
            let (coordinator, proposal) = rejected.into_parts();
            assert_eq!(coordinator.preview().model(), "model-v1");
            assert_eq!(coordinator.binding(), &coordinator_binding);
            assert_complete_proposal_truth(&proposal, &proposal_binding, SelectionBasis::Explicit);
            assert!(!target.exists());
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selected_setup_renders_exact_normalized_model_and_retains_complete_proposal_truth() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let binding = compatible_binding(b"rendered");
        let selected = selected_local(47, &target, binding.clone());
        assert_eq!(selected.preview().model(), "selected/model");
        assert_complete_proposal_truth(selected.proposal(), &binding, SelectionBasis::Explicit);

        let completion = selected.finish(LocalSetupDecision::Consent).unwrap();

        assert_eq!(completion.cause(), &SetupTerminalCause::Published);
        assert_complete_proposal_truth(completion.proposal(), &binding, SelectionBasis::Explicit);
        let text = std::fs::read_to_string(&target).unwrap();
        let config: crate::config::Config = toml::from_str(&text).unwrap();
        assert_eq!(
            config.gateways["local"].model.as_deref(),
            Some("selected/model")
        );
    }

    #[test]
    fn selected_decline_is_zero_effect_and_retains_proposal_truth() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let binding = compatible_binding(b"selected-decline");
        let completion = selected_local(49, &target, binding.clone())
            .finish(LocalSetupDecision::Decline)
            .unwrap();

        assert_eq!(
            completion.cause(),
            &SetupTerminalCause::Cancelled {
                at: SetupStage::AwaitingConsent,
            }
        );
        assert_eq!(completion.rollback(), RollbackTruth::NotNeeded);
        assert_complete_proposal_truth(completion.proposal(), &binding, SelectionBasis::Explicit);
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selected_collision_preserves_existing_bytes_and_retains_proposal_truth() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let original = b"preexisting = true\n";
        std::fs::write(&target, original).unwrap();
        let binding = compatible_binding(b"selected-collision");

        let completion = selected_local(51, &target, binding.clone())
            .finish(LocalSetupDecision::Consent)
            .unwrap();

        assert_eq!(completion.cause(), &SetupTerminalCause::Collision);
        assert_complete_proposal_truth(completion.proposal(), &binding, SelectionBasis::Explicit);
        assert_eq!(std::fs::read(&target).unwrap(), original);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selected_rollback_and_recovery_retain_the_entire_proposal() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let binding = compatible_binding(b"selected-recovery");
        let _publication_fault_scope =
            inject_publication_faults(&[PublicationFaultPoint::DirectorySync]);
        let _rollback_fault_scope = inject_next_rollback_fault(RollbackFaultPoint::DirectorySync);

        let mut completion = selected_local(53, &target, binding.clone())
            .finish(LocalSetupDecision::Consent)
            .unwrap();

        assert_eq!(
            completion.rollback(),
            RollbackTruth::Incomplete {
                rolled_back: 0,
                remaining: 1,
            }
        );
        assert_eq!(
            completion.recovery_kind(),
            Some(ConfigResidueKind::RemovedUnconfirmed)
        );
        assert_complete_proposal_truth(completion.proposal(), &binding, SelectionBasis::Explicit);
        assert_eq!(
            completion.retry_recovery().unwrap(),
            LocalSetupRecoveryStatus::Complete,
        );
        assert_complete_proposal_truth(completion.proposal(), &binding, SelectionBasis::Explicit);
        assert_eq!(completion.recovery_kind(), None);
    }

    #[test]
    fn selected_execution_error_retains_the_base_owner_and_entire_proposal() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("kitsu.toml");
        let binding = compatible_binding(b"selected-error-owner");
        let mut selected = selected_local(55, &target, binding.clone());
        selected.coordinator.transaction.consent().unwrap();

        let error = selected.finish(LocalSetupDecision::Consent).err().unwrap();

        assert_eq!(
            error.kind(),
            &LocalSetupExecutionFailure::UnexpectedDirective {
                expected: SetupStage::AwaitingConsent,
                actual: SetupStage::Acquire,
            },
        );
        assert_complete_proposal_truth(error.proposal(), &binding, SelectionBasis::Explicit);
        let (base, proposal) = error.into_parts();
        assert_eq!(
            base.kind(),
            &LocalSetupExecutionFailure::UnexpectedDirective {
                expected: SetupStage::AwaitingConsent,
                actual: SetupStage::Acquire,
            },
        );
        assert_complete_proposal_truth(&proposal, &binding, SelectionBasis::Explicit);
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
