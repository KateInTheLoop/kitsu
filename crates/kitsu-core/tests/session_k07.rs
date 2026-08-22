mod support;

use std::collections::HashMap;

use kitsu_core::{ExpectedRevision, LogicalHead, SessionCommitReceipt, SessionId};
use support::{MemoryRevisionStore, PublishFailure};

#[derive(Clone, Debug, PartialEq, Eq)]
struct FenceToken {
    session: SessionId,
    generation: u64,
}

impl FenceToken {
    fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FenceFailure {
    Busy {
        session: SessionId,
    },
    SessionMismatch {
        token_session: SessionId,
        target_session: SessionId,
    },
    Lost {
        session: SessionId,
        expected_generation: u64,
        observed_generation: Option<u64>,
    },
}

#[derive(Debug, Default)]
struct MemoryFenceTable {
    active: HashMap<SessionId, u64>,
    next_generation: u64,
}

impl MemoryFenceTable {
    fn new() -> Self {
        Self::default()
    }

    fn acquire(&mut self, session: &SessionId) -> Result<FenceToken, FenceFailure> {
        if self.active.contains_key(session) {
            return Err(FenceFailure::Busy {
                session: session.clone(),
            });
        }
        self.next_generation += 1;
        self.active.insert(session.clone(), self.next_generation);
        Ok(FenceToken {
            session: session.clone(),
            generation: self.next_generation,
        })
    }

    fn revoke(&mut self, session: &SessionId) {
        self.active.remove(session);
    }

    fn confirm(&self, target: &SessionId, token: &FenceToken) -> Result<(), FenceFailure> {
        if &token.session != target {
            return Err(FenceFailure::SessionMismatch {
                token_session: token.session.clone(),
                target_session: target.clone(),
            });
        }
        let observed = self.active.get(target).copied();
        if observed == Some(token.generation) {
            Ok(())
        } else {
            Err(FenceFailure::Lost {
                session: target.clone(),
                expected_generation: token.generation,
                observed_generation: observed,
            })
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FencedPublishFailure {
    Fence(FenceFailure),
    Publish(PublishFailure),
}

trait FencedPublish {
    fn publish_fenced(
        &mut self,
        fences: &MemoryFenceTable,
        token: &FenceToken,
        expected: ExpectedRevision,
        head: LogicalHead,
    ) -> Result<SessionCommitReceipt, FencedPublishFailure>;
}

impl FencedPublish for MemoryRevisionStore {
    fn publish_fenced(
        &mut self,
        fences: &MemoryFenceTable,
        token: &FenceToken,
        expected: ExpectedRevision,
        head: LogicalHead,
    ) -> Result<SessionCommitReceipt, FencedPublishFailure> {
        fences
            .confirm(self.session_id(), token)
            .map_err(FencedPublishFailure::Fence)?;
        self.publish(expected, head)
            .map_err(FencedPublishFailure::Publish)
    }
}

#[test]
fn k07_same_session_excludes_a_second_writer_while_independent_session_progresses() {
    let mut fences = MemoryFenceTable::new();
    let mut alpha = MemoryRevisionStore::new("k07-alpha");
    let mut beta = MemoryRevisionStore::new("k07-beta");

    let alpha_writer = fences.acquire(alpha.session_id()).unwrap();
    assert_eq!(
        fences.acquire(alpha.session_id()),
        Err(FenceFailure::Busy {
            session: alpha.session_id().clone(),
        })
    );

    let beta_writer = fences.acquire(beta.session_id()).unwrap();
    assert!(matches!(
        alpha.publish_fenced(
            &fences,
            &beta_writer,
            ExpectedRevision::Missing,
            LogicalHead::Genesis,
        ),
        Err(FencedPublishFailure::Fence(
            FenceFailure::SessionMismatch { .. }
        ))
    ));
    assert_eq!((alpha.mints(), alpha.publications()), (0, 0));
    alpha
        .publish_fenced(
            &fences,
            &alpha_writer,
            ExpectedRevision::Missing,
            LogicalHead::Genesis,
        )
        .unwrap();
    beta.publish_fenced(
        &fences,
        &beta_writer,
        ExpectedRevision::Missing,
        LogicalHead::Genesis,
    )
    .unwrap();

    assert_eq!((alpha.mints(), alpha.publications()), (1, 1));
    assert_eq!((beta.mints(), beta.publications()), (1, 1));
    assert_eq!(alpha.current_head(), Some(&LogicalHead::Genesis));
    assert_eq!(beta.current_head(), Some(&LogicalHead::Genesis));
}

#[test]
fn k07_lost_and_replaced_fences_refuse_before_mint_or_publication() {
    let mut fences = MemoryFenceTable::new();
    let mut store = MemoryRevisionStore::new("k07-loss");
    let first_writer = fences.acquire(store.session_id()).unwrap();
    let first = store
        .publish_fenced(
            &fences,
            &first_writer,
            ExpectedRevision::Missing,
            LogicalHead::Genesis,
        )
        .unwrap();
    let revision_a = first.new_revision().clone();

    fences.revoke(store.session_id());
    assert!(matches!(
        store.publish_fenced(
            &fences,
            &first_writer,
            ExpectedRevision::Exact(revision_a.clone()),
            LogicalHead::Genesis,
        ),
        Err(FencedPublishFailure::Fence(FenceFailure::Lost {
            observed_generation: None,
            ..
        }))
    ));
    assert_eq!((store.mints(), store.publications()), (1, 1));

    let successor_writer = fences.acquire(store.session_id()).unwrap();
    assert!(successor_writer.generation() > first_writer.generation());
    assert!(matches!(
        store.publish_fenced(
            &fences,
            &first_writer,
            ExpectedRevision::Exact(revision_a.clone()),
            LogicalHead::Genesis,
        ),
        Err(FencedPublishFailure::Fence(FenceFailure::Lost {
            observed_generation: Some(_),
            ..
        }))
    ));
    assert_eq!((store.mints(), store.publications()), (1, 1));

    let second = store
        .publish_fenced(
            &fences,
            &successor_writer,
            ExpectedRevision::Exact(revision_a),
            LogicalHead::Genesis,
        )
        .unwrap();
    assert_eq!(store.current_revision(), Some(second.new_revision()));
    assert_eq!(store.current_head(), Some(&LogicalHead::Genesis));
    assert_eq!((store.mints(), store.publications()), (2, 2));
}
