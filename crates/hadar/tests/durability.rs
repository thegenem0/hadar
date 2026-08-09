//! End-to-end checks against the real storage backend.
#![expect(
    clippy::expect_used,
    reason = "test assertions surface failures by panicking"
)]

use std::path::Path;
use std::sync::Arc;

use mvcc::{KvStore, Mutation};
use storage_api::Bounds;
use storage_redb::RedbEngine;
use wal::Wal;

type Store = KvStore<RedbEngine>;

fn open(dir: &Path) -> Arc<Store> {
    let engine = RedbEngine::open(dir.join("store.redb")).expect("backend opens");
    Arc::new(KvStore::open(engine).expect("store opens"))
}

fn put(key: &[u8], value: &[u8]) -> Mutation {
    Mutation::Put {
        key: key.to_vec(),
        value: value.to_vec(),
    }
}

fn values(store: &Store) -> Vec<Vec<u8>> {
    store
        .range(&Bounds::all(), None, None)
        .expect("store reads")
        .into_iter()
        .map(|record| record.value)
        .collect()
}

#[tokio::test]
async fn writes_round_trip_through_the_real_backend() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = open(dir.path());
    let log = Wal::open(dir.path().join("wal"), Arc::clone(&store)).expect("log opens");

    log.write(vec![put(b"a", b"1"), put(b"b", b"2")])
        .await
        .expect("write succeeds");

    assert_eq!(values(&store), [b"1".to_vec(), b"2".to_vec()]);
}

#[tokio::test]
async fn a_reopened_store_recovers_from_the_log() {
    let dir = tempfile::tempdir().expect("temp dir");

    {
        let store = open(dir.path());
        let log = Wal::open(dir.path().join("wal"), Arc::clone(&store)).expect("log opens");
        log.write(vec![put(b"a", b"1")]).await.expect("write");
        log.write(vec![put(b"b", b"2")]).await.expect("write");
    }

    // Reopening reads whatever redb kept and replays the rest from the log.
    // With relaxed commits the two need not agree on their own — the applied
    // marker is what reconciles them.
    let store = open(dir.path());
    let _log = Wal::open(dir.path().join("wal"), Arc::clone(&store)).expect("log reopens");

    assert_eq!(values(&store), [b"1".to_vec(), b"2".to_vec()]);
    assert_eq!(store.current_revision().main(), 2);
}

#[tokio::test]
async fn repeated_reopening_does_not_replay_twice() {
    let dir = tempfile::tempdir().expect("temp dir");

    {
        let store = open(dir.path());
        let log = Wal::open(dir.path().join("wal"), Arc::clone(&store)).expect("log opens");
        log.write(vec![put(b"a", b"1")]).await.expect("write");
    }

    for _ in 0..3 {
        let store = open(dir.path());
        let _log = Wal::open(dir.path().join("wal"), Arc::clone(&store)).expect("log reopens");

        assert_eq!(
            store.current_revision().main(),
            1,
            "a reopen replayed records the store had already applied"
        );
        assert_eq!(values(&store), [b"1".to_vec()]);
    }
}

#[tokio::test]
async fn history_survives_a_reopen() {
    let dir = tempfile::tempdir().expect("temp dir");

    {
        let store = open(dir.path());
        let log = Wal::open(dir.path().join("wal"), Arc::clone(&store)).expect("log opens");
        log.write(vec![put(b"k", b"first")]).await.expect("write");
        log.write(vec![put(b"k", b"second")]).await.expect("write");
    }

    let store = open(dir.path());
    let _log = Wal::open(dir.path().join("wal"), Arc::clone(&store)).expect("log reopens");

    let record = store
        .range(&Bounds::point(b"k".as_slice()), None, None)
        .expect("store reads");
    let record = record.first().expect("the key is present");

    assert_eq!(record.version, 2, "the key's history did not survive");
    assert_eq!(record.created.main(), 1, "the creation revision moved");

    // The superseded value must still be readable at its own revision, which
    // is the point of keeping history rather than just the latest value.
    let historical = store
        .range(&Bounds::point(b"k".as_slice()), Some(1), None)
        .expect("store reads");
    assert_eq!(
        historical.first().expect("present at revision 1").value,
        b"first".to_vec()
    );
}
