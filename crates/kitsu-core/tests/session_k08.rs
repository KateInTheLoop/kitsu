mod support;

use kitsu_core::{CommitDurability, ExpectedRevision, LogicalHead, SessionLookupState};
use support::{revision, MemoryRevisionStore, PublishFailure};

#[test]
fn k08_stale_writer_is_zero_effect_and_retry_publishes_one_successor() {
    let mut store = MemoryRevisionStore::new("k08");

    let first = store
        .publish(ExpectedRevision::Missing, LogicalHead::Genesis)
        .unwrap();
    let revision_a = first.new_revision().clone();
    assert_eq!(revision_a, revision("revision-memory-k08-1"));
    assert_eq!(first.base_revision(), &ExpectedRevision::Missing);
    assert_eq!(first.durability(), CommitDurability::Confirmed);

    let second = store
        .publish(
            ExpectedRevision::Exact(revision_a.clone()),
            LogicalHead::Genesis,
        )
        .unwrap();
    let revision_b = second.new_revision().clone();
    assert_eq!(revision_b, revision("revision-memory-k08-2"));
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
    assert_eq!(retry.new_revision(), &revision("revision-memory-k08-3"));
    assert_eq!(store.current_head(), Some(&LogicalHead::Genesis));
    assert_eq!((store.mints(), store.publications()), (3, 3));
}

#[test]
fn k08_no_clobber_create_conflicts_before_mint_or_publication() {
    let mut store = MemoryRevisionStore::new("k08");
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
