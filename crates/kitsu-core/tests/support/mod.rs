use kitsu_core::{
    CommitDurability, ExpectedRevision, LogicalHead, RevisionId, SessionCommitError,
    SessionCommitOutcome, SessionCommitReceipt, SessionId, SessionLookupState, StoreId,
};

pub fn revision(value: &str) -> RevisionId {
    RevisionId::new(value).unwrap()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationCut {
    BeforeCandidate,
    AfterCandidate,
    AfterPublication,
    Confirmed,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PublishFailure {
    Conflict(SessionLookupState),
    InvalidReceipt(SessionCommitError),
}

#[derive(Debug)]
struct PublishedRevision {
    revision: RevisionId,
    head: LogicalHead,
}

#[derive(Debug)]
pub struct MemoryRevisionStore {
    store: StoreId,
    session: SessionId,
    revision_prefix: String,
    current: Option<PublishedRevision>,
    mints: usize,
    publications: usize,
}

impl MemoryRevisionStore {
    pub fn new(fixture: &str) -> Self {
        Self {
            store: StoreId::new(format!("store-memory-{fixture}")).unwrap(),
            session: SessionId::new(format!("session-memory-{fixture}")).unwrap(),
            revision_prefix: format!("revision-memory-{fixture}"),
            current: None,
            mints: 0,
            publications: 0,
        }
    }

    pub fn publish(
        &mut self,
        expected: ExpectedRevision,
        head: LogicalHead,
    ) -> Result<SessionCommitReceipt, PublishFailure> {
        match self.publish_with_cut(expected, head, PublicationCut::Confirmed)? {
            SessionCommitOutcome::Published { receipt } => Ok(receipt),
            SessionCommitOutcome::NotPublished { .. } => {
                unreachable!("confirmed fixture publication must install")
            }
        }
    }

    pub fn publish_with_cut(
        &mut self,
        expected: ExpectedRevision,
        head: LogicalHead,
        cut: PublicationCut,
    ) -> Result<SessionCommitOutcome, PublishFailure> {
        expected
            .admit_observed(self.current.as_ref().map(|current| &current.revision))
            .map_err(PublishFailure::Conflict)?;

        if cut == PublicationCut::BeforeCandidate {
            return Ok(self.not_published(expected));
        }

        let next = revision(&format!("{}-{}", self.revision_prefix, self.mints + 1));
        self.mints += 1;
        if cut == PublicationCut::AfterCandidate {
            return Ok(self.not_published(expected));
        }

        let durability = if cut == PublicationCut::AfterPublication {
            CommitDurability::PublishedUnconfirmed
        } else {
            CommitDurability::Confirmed
        };
        let receipt = SessionCommitReceipt::new(
            self.store.clone(),
            self.session.clone(),
            expected,
            next.clone(),
            head.clone(),
            durability,
        )
        .map_err(PublishFailure::InvalidReceipt)?;

        self.current = Some(PublishedRevision {
            revision: next,
            head,
        });
        self.publications += 1;
        Ok(SessionCommitOutcome::Published { receipt })
    }

    fn not_published(&self, base_revision: ExpectedRevision) -> SessionCommitOutcome {
        SessionCommitOutcome::NotPublished {
            store: self.store.clone(),
            session: self.session.clone(),
            base_revision,
        }
    }

    pub fn current_revision(&self) -> Option<&RevisionId> {
        self.current.as_ref().map(|current| &current.revision)
    }

    pub fn current_head(&self) -> Option<&LogicalHead> {
        self.current.as_ref().map(|current| &current.head)
    }

    pub fn mints(&self) -> usize {
        self.mints
    }

    pub fn publications(&self) -> usize {
        self.publications
    }
}
