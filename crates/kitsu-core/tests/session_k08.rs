use kitsu_core::{
    CommitDurability, ExpectedRevision, LogicalHead, RevisionId, SessionCommitError,
    SessionCommitReceipt, SessionId, SessionLookupState, StoreId,
};

fn revision(value: &str) -> RevisionId {
    RevisionId::new(value).unwrap()
}

#[derive(Debug, PartialEq, Eq)]
enum PublishFailure {
    Conflict(SessionLookupState),
    InvalidReceipt(SessionCommitError),
}

#[derive(Debug)]
struct PublishedRevision {
    revision: RevisionId,
    head: LogicalHead,
}

#[derive(Debug)]
struct MemoryRevisionStore {
    store: StoreId,
    session: SessionId,
    current: Option<PublishedRevision>,
    mints: usize,
    publications: usize,
}

impl MemoryRevisionStore {
    fn new() -> Self {
        Self {
            store: StoreId::new("store-memory-k08").unwrap(),
            session: SessionId::new("session-memory-k08").unwrap(),
            current: None,
            mints: 0,
            publications: 0,
        }
    }

    fn publish(
        &mut self,
        expected: ExpectedRevision,
        head: LogicalHead,
    ) -> Result<SessionCommitReceipt, PublishFailure> {
        expected
            .admit_observed(self.current.as_ref().map(|current| &current.revision))
            .map_err(PublishFailure::Conflict)?;

        let next = revision(&format!("revision-memory-{}", self.mints + 1));
        let receipt = SessionCommitReceipt::new(
            self.store.clone(),
            self.session.clone(),
            expected,
            next.clone(),
            head.clone(),
            CommitDurability::Confirmed,
        )
        .map_err(PublishFailure::InvalidReceipt)?;

        self.mints += 1;
        self.current = Some(PublishedRevision {
            revision: next,
            head,
        });
        self.publications += 1;
        Ok(receipt)
    }

    fn current_revision(&self) -> Option<&RevisionId> {
        self.current.as_ref().map(|current| &current.revision)
    }

    fn current_head(&self) -> Option<&LogicalHead> {
        self.current.as_ref().map(|current| &current.head)
    }

    fn mints(&self) -> usize {
        self.mints
    }

    fn publications(&self) -> usize {
        self.publications
    }
}

#[test]
fn k08_stale_writer_is_zero_effect_and_retry_publishes_one_successor() {
    let mut store = MemoryRevisionStore::new();

    let first = store
        .publish(ExpectedRevision::Missing, LogicalHead::Genesis)
        .unwrap();
    let revision_a = first.new_revision().clone();
    assert_eq!(revision_a, revision("revision-memory-1"));
    assert_eq!(first.base_revision(), &ExpectedRevision::Missing);
    assert_eq!(first.durability(), CommitDurability::Confirmed);

    let second = store
        .publish(
            ExpectedRevision::Exact(revision_a.clone()),
            LogicalHead::Genesis,
        )
        .unwrap();
    let revision_b = second.new_revision().clone();
    assert_eq!(revision_b, revision("revision-memory-2"));
    assert_eq!((store.mints(), store.publications()), (2, 2));

    assert_eq!(
        store.publish(
            ExpectedRevision::Exact(revision_a.clone()),
            LogicalHead::Genesis,
        ),
        Err(PublishFailure::Conflict(SessionLookupState::Conflict {
            expected: ExpectedRevision::Exact(revision_a),
            observed: Some(revision_b.clone()),
        }))
    );
    assert_eq!((store.mints(), store.publications()), (2, 2));
    assert_eq!(store.current_revision(), Some(&revision_b));

    let retry = store
        .publish(
            ExpectedRevision::Exact(revision_b.clone()),
            LogicalHead::Genesis,
        )
        .unwrap();
    assert_eq!(retry.base_revision(), &ExpectedRevision::Exact(revision_b));
    assert_eq!(retry.new_revision(), &revision("revision-memory-3"));
    assert_eq!(store.current_head(), Some(&LogicalHead::Genesis));
    assert_eq!((store.mints(), store.publications()), (3, 3));
}

#[test]
fn k08_no_clobber_create_conflicts_before_mint_or_publication() {
    let mut store = MemoryRevisionStore::new();
    let first = store
        .publish(ExpectedRevision::Missing, LogicalHead::Genesis)
        .unwrap();
    let observed = first.new_revision().clone();

    assert_eq!(
        store.publish(ExpectedRevision::Missing, LogicalHead::Genesis),
        Err(PublishFailure::Conflict(SessionLookupState::Conflict {
            expected: ExpectedRevision::Missing,
            observed: Some(observed.clone()),
        }))
    );
    assert_eq!((store.mints(), store.publications()), (1, 1));
    assert_eq!(store.current_revision(), Some(&observed));
}
