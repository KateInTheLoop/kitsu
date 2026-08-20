mod support;

use kitsu_core::{CommitDurability, ExpectedRevision, LogicalHead, SessionCommitOutcome};
use support::{revision, MemoryRevisionStore, PublicationCut};

fn seeded_store() -> (MemoryRevisionStore, kitsu_core::RevisionId) {
    let mut store = MemoryRevisionStore::new("k04");
    let receipt = store
        .publish(ExpectedRevision::Missing, LogicalHead::Genesis)
        .unwrap();
    let revision = receipt.new_revision().clone();
    (store, revision)
}

#[test]
fn k04_prepublication_cuts_keep_old_revision_and_report_not_published() {
    for (cut, expected_mints) in [
        (PublicationCut::BeforeCandidate, 1),
        (PublicationCut::AfterCandidate, 2),
    ] {
        let (mut store, revision_a) = seeded_store();
        let outcome = store
            .publish_with_cut(
                ExpectedRevision::Exact(revision_a.clone()),
                LogicalHead::Genesis,
                cut,
            )
            .unwrap();

        assert!(matches!(
            outcome,
            SessionCommitOutcome::NotPublished { base_revision, .. }
                if base_revision == ExpectedRevision::Exact(revision_a.clone())
        ));
        assert_eq!(store.current_revision(), Some(&revision_a));
        assert_eq!(store.current_head(), Some(&LogicalHead::Genesis));
        assert_eq!((store.mints(), store.publications()), (expected_mints, 1));
    }
}

#[test]
fn k04_postpublication_cut_exposes_new_revision_as_unconfirmed() {
    let (mut store, revision_a) = seeded_store();
    let outcome = store
        .publish_with_cut(
            ExpectedRevision::Exact(revision_a.clone()),
            LogicalHead::Genesis,
            PublicationCut::AfterPublication,
        )
        .unwrap();

    let SessionCommitOutcome::Published { receipt } = outcome else {
        panic!("post-publication cut must report the installed revision");
    };
    assert_eq!(
        receipt.base_revision(),
        &ExpectedRevision::Exact(revision_a)
    );
    assert_eq!(receipt.new_revision(), &revision("revision-memory-k04-2"));
    assert_eq!(receipt.durability(), CommitDurability::PublishedUnconfirmed);
    assert_eq!(store.current_revision(), Some(receipt.new_revision()));
    assert_eq!(store.current_head(), Some(&LogicalHead::Genesis));
    assert_eq!((store.mints(), store.publications()), (2, 2));
}

#[test]
fn k04_confirmed_cut_exposes_exact_new_revision_and_durability() {
    let (mut store, revision_a) = seeded_store();
    let outcome = store
        .publish_with_cut(
            ExpectedRevision::Exact(revision_a.clone()),
            LogicalHead::Genesis,
            PublicationCut::Confirmed,
        )
        .unwrap();

    let SessionCommitOutcome::Published { receipt } = outcome else {
        panic!("confirmed cut must report the installed revision");
    };
    assert_eq!(
        receipt.base_revision(),
        &ExpectedRevision::Exact(revision_a)
    );
    assert_eq!(receipt.new_revision(), &revision("revision-memory-k04-2"));
    assert_eq!(receipt.durability(), CommitDurability::Confirmed);
    assert_eq!(store.current_revision(), Some(receipt.new_revision()));
    assert_eq!(store.current_head(), Some(&LogicalHead::Genesis));
    assert_eq!((store.mints(), store.publications()), (2, 2));
}
