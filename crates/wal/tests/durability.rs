//! Checks the guarantees this crate exists to provide.
//!
//! Every case here is about what survives a restart. They run against a real
//! directory and a real store, because the interaction between the log's flush
//! and the store's applied marker is the concern under test.

#![expect(
    clippy::expect_used,
    reason = "test assertions surface failures by panicking"
)]

use std::sync::Arc;

use mvcc::{KvStore, Mutation};
use storage_api::{Bounds, MemEngine};
use wal::Wal;

fn put(key: &[u8], value: &[u8]) -> Mutation {
    Mutation::Put {
        key: key.to_vec(),
        value: value.to_vec(),
    }
}

fn values<E: storage_api::StorageEngine>(store: &KvStore<E>) -> Vec<Vec<u8>> {
    store
        .range(&Bounds::all(), None, None)
        .expect("store reads")
        .into_iter()
        .map(|record| record.value)
        .collect()
}

#[tokio::test]
async fn a_written_batch_is_visible_immediately() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let log = Wal::open(dir.path(), Arc::clone(&store)).expect("log opens");

    let revisions = log
        .write(vec![put(b"a", b"1"), put(b"b", b"2")])
        .await
        .expect("write succeeds");

    assert_eq!(revisions.len(), 2);
    assert_eq!(values(&store), [b"1".to_vec(), b"2".to_vec()]);
}

#[tokio::test]
async fn each_write_in_a_batch_takes_its_own_revision() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let log = Wal::open(dir.path(), Arc::clone(&store)).expect("log opens");

    let revisions = log
        .write(vec![put(b"a", b"1"), put(b"b", b"2"), put(b"c", b"3")])
        .await
        .expect("write succeeds");

    // Batching is a way to share a flush, not a way to share a revision:
    // clients must not be able to tell that their writes were grouped.
    let mains: Vec<_> = revisions
        .iter()
        .copied()
        .map(mvcc::Revision::main)
        .collect();
    assert_eq!(mains, [1, 2, 3]);
}

#[tokio::test]
async fn acknowledged_writes_survive_a_restart() {
    let dir = tempfile::tempdir().expect("temp dir");

    {
        let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
        let log = Wal::open(dir.path(), store).expect("log opens");
        log.write(vec![put(b"a", b"1")]).await.expect("write");
        log.write(vec![put(b"b", b"2")]).await.expect("write");
    }

    // A fresh store stands in for a process that lost everything not on disk.
    // Only the log can put the data back.
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let _log = Wal::open(dir.path(), Arc::clone(&store)).expect("log reopens");

    assert_eq!(values(&store), [b"1".to_vec(), b"2".to_vec()]);
}

#[tokio::test]
async fn replay_restores_revisions_not_just_values() {
    let dir = tempfile::tempdir().expect("temp dir");

    {
        let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
        let log = Wal::open(dir.path(), store).expect("log opens");
        log.write(vec![put(b"k", b"first")]).await.expect("write");
        log.write(vec![put(b"k", b"second")]).await.expect("write");
    }

    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let _log = Wal::open(dir.path(), Arc::clone(&store)).expect("log reopens");

    let record = store
        .range(&Bounds::point(b"k".as_slice()), None, None)
        .expect("store reads");
    let record = record.first().expect("the key is present");

    assert_eq!(record.version, 2, "replay lost the key's history");
    assert_eq!(
        record.created.main(),
        1,
        "replay moved the creation revision"
    );
    assert_eq!(store.current_revision().main(), 2);
}

#[tokio::test]
async fn replay_is_idempotent_across_repeated_restarts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));

    {
        let log = Wal::open(dir.path(), Arc::clone(&store)).expect("log opens");
        log.write(vec![put(b"a", b"1")]).await.expect("write");
    }

    // The store already holds the write, so reopening must not apply it again
    // and inflate the revision — this is what the applied marker prevents.
    for _ in 0..3 {
        let _log = Wal::open(dir.path(), Arc::clone(&store)).expect("log reopens");
        assert_eq!(store.current_revision().main(), 1);
        assert_eq!(values(&store), [b"1".to_vec()]);
    }
}

#[tokio::test]
async fn a_deletion_replays_as_a_deletion() {
    let dir = tempfile::tempdir().expect("temp dir");

    {
        let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
        let log = Wal::open(dir.path(), store).expect("log opens");
        log.write(vec![put(b"a", b"1"), put(b"b", b"2")])
            .await
            .expect("write");
        log.write(vec![Mutation::Delete { key: b"a".to_vec() }])
            .await
            .expect("write");
    }

    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let _log = Wal::open(dir.path(), Arc::clone(&store)).expect("log reopens");

    assert_eq!(
        values(&store),
        [b"2".to_vec()],
        "the deletion did not replay"
    );
}

#[tokio::test]
async fn concurrent_writers_all_get_answered() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let log = Arc::new(Wal::open(dir.path(), Arc::clone(&store)).expect("log opens"));

    // Enough concurrency to make batching actually happen, so the revision
    // splitting across a shared flush is exercised rather than assumed.
    let writes = (0..64_u32).map(|i| {
        let log = Arc::clone(&log);
        tokio::spawn(async move { log.write(vec![put(&i.to_be_bytes(), b"v")]).await })
    });

    let mut revisions = Vec::new();
    for write in writes {
        let written = write.await.expect("task runs").expect("write succeeds");
        revisions.extend(written.into_iter().map(mvcc::Revision::main));
    }

    revisions.sort_unstable();
    assert_eq!(
        revisions,
        (1..=64).collect::<Vec<_>>(),
        "revisions were duplicated or skipped across concurrent batches"
    );
}

/// Truncates the newest segment to `len`, as an interrupted append would.
fn tear_last_segment(dir: &std::path::Path, len: u64) {
    let mut segments: Vec<_> = std::fs::read_dir(dir)
        .expect("log directory is readable")
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "wal"))
        .collect();
    segments.sort();

    let last = segments.last().expect("a segment exists");
    std::fs::OpenOptions::new()
        .write(true)
        .open(last)
        .expect("segment opens")
        .set_len(len)
        .expect("segment truncates");
}

#[tokio::test]
async fn a_torn_final_record_is_discarded_and_the_rest_survives() {
    let dir = tempfile::tempdir().expect("temp dir");

    let complete = {
        let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
        let log = Wal::open(dir.path(), Arc::clone(&store)).expect("log opens");
        log.write(vec![put(b"a", b"1")]).await.expect("write");
        let after_first = std::fs::metadata(
            std::fs::read_dir(dir.path())
                .expect("readable")
                .filter_map(|e| Some(e.ok()?.path()))
                .find(|p| p.extension().is_some_and(|ext| ext == "wal"))
                .expect("a segment exists"),
        )
        .expect("segment is measurable")
        .len();

        log.write(vec![put(b"b", b"2")]).await.expect("write");
        after_first
    };

    // Cut the second record in half: the process died partway through
    // appending it, so it was never acknowledged.
    tear_last_segment(dir.path(), complete + 4);

    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let _log = Wal::open(dir.path(), Arc::clone(&store)).expect("log reopens");

    assert_eq!(
        values(&store),
        [b"1".to_vec()],
        "the torn record was replayed, or the complete one was lost"
    );
}

#[tokio::test]
async fn writes_after_a_torn_record_are_not_buried_by_it() {
    let dir = tempfile::tempdir().expect("temp dir");

    let complete = {
        let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
        let log = Wal::open(dir.path(), Arc::clone(&store)).expect("log opens");
        log.write(vec![put(b"a", b"1")]).await.expect("write");
        let path = std::fs::read_dir(dir.path())
            .expect("readable")
            .filter_map(|e| Some(e.ok()?.path()))
            .find(|p| p.extension().is_some_and(|ext| ext == "wal"))
            .expect("a segment exists");
        let len = std::fs::metadata(&path).expect("measurable").len();
        log.write(vec![put(b"b", b"2")]).await.expect("write");
        len
    };

    tear_last_segment(dir.path(), complete + 4);

    // Reopening must truncate the partial record before appending, or this
    // write lands after a record replay cannot read past — and vanishes on the
    // next restart.
    {
        let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
        let log = Wal::open(dir.path(), store).expect("log reopens");
        log.write(vec![put(b"c", b"3")]).await.expect("write");
    }

    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let _log = Wal::open(dir.path(), Arc::clone(&store)).expect("log reopens");

    assert_eq!(
        values(&store),
        [b"1".to_vec(), b"3".to_vec()],
        "a write after a torn record was lost"
    );
}
