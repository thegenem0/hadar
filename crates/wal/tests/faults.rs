//! Checks the failure paths a real filesystem will not produce on demand.
#![expect(
    clippy::expect_used,
    reason = "test assertions surface failures by panicking"
)]

use std::sync::Arc;

use mvcc::{KvStore, Mutation};
use storage_api::{Bounds, MemEngine};
use wal::{Faults, Options, Wal};

fn put(key: &[u8], value: &[u8]) -> Mutation {
    Mutation::Put {
        key: key.to_vec(),
        value: value.to_vec(),
    }
}

fn values(store: &KvStore<MemEngine>) -> Vec<Vec<u8>> {
    store
        .range(&Bounds::all(), None, None)
        .expect("store reads")
        .into_iter()
        .map(|record| record.value)
        .collect()
}

#[tokio::test]
async fn a_refused_flush_is_not_acknowledged() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let faults = Faults::new();
    let log = Wal::with_faults(
        dir.path(),
        Arc::clone(&store),
        Options::default(),
        Arc::clone(&faults),
    )
    .expect("log opens");

    faults.fail_next_syncs(1);
    let error = log
        .write(vec![put(b"key", b"v")])
        .await
        .expect_err("a write whose flush failed was acknowledged");

    // The caller has to be able to tell a device failure from corruption, or
    // it cannot decide whether retrying is sane.
    assert!(error.is_io(), "reported {error} rather than an I/O failure");

    // Nothing durable, so nothing visible.
    assert_eq!(store.current_revision().main(), 0);
    assert!(values(&store).is_empty());
}

#[tokio::test]
async fn the_log_keeps_working_after_a_refused_flush() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let faults = Faults::new();
    let log = Wal::with_faults(
        dir.path(),
        Arc::clone(&store),
        Options::default(),
        Arc::clone(&faults),
    )
    .expect("log opens");

    faults.fail_next_syncs(1);
    drop(log.write(vec![put(b"refused", b"v")]).await);

    log.write(vec![put(b"after", b"v")])
        .await
        .expect("the log accepts writes again");

    assert_eq!(store.current_revision().main(), 1);
}

#[tokio::test]
async fn a_torn_write_does_not_bury_the_writes_after_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let faults = Faults::new();

    {
        let log = Wal::with_faults(
            dir.path(),
            Arc::clone(&store),
            Options::default(),
            Arc::clone(&faults),
        )
        .expect("log opens");

        log.write(vec![put(b"first", b"1")])
            .await
            .expect("write succeeds");

        // Six bytes lands inside the frame header, so what remains cannot be
        // decoded as anything.
        faults.shorten_next_write(6);
        let error = log
            .write(vec![put(b"torn", b"x")])
            .await
            .expect_err("a partial append was acknowledged");
        assert!(error.is_io(), "reported {error} rather than an I/O failure");

        log.write(vec![put(b"third", b"3")])
            .await
            .expect("write succeeds");
    }

    // Replay stops at the first record it cannot read. If the torn bytes were
    // left where they fell, "third" sits on the far side of them and is lost —
    // an acknowledged write, gone.
    let recovered = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let _log = Wal::open(dir.path(), Arc::clone(&recovered)).expect("log reopens");

    assert_eq!(values(&recovered), vec![b"1".to_vec(), b"3".to_vec()]);
}

#[tokio::test]
async fn a_segment_a_failed_rewind_poisoned_takes_no_more_writes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let faults = Faults::new();
    let log = Wal::with_faults(
        dir.path(),
        Arc::clone(&store),
        Options::default(),
        Arc::clone(&faults),
    )
    .expect("log opens");

    log.write(vec![put(b"first", b"1")])
        .await
        .expect("write succeeds");

    // Tear the write, then refuse the rewind that would clean it up.
    faults.shorten_next_write(6);
    faults.fail_next_truncates(1);
    let error = log
        .write(vec![put(b"torn", b"x")])
        .await
        .expect_err("a partial append was acknowledged");
    assert!(error.is_io(), "reported {error} rather than an I/O failure");

    // The torn bytes are still in the file. Appending after them is the burial
    // the rewind exists to prevent, so the segment has to refuse.
    let refused = log
        .write(vec![put(b"after", b"v")])
        .await
        .expect_err("a poisoned segment accepted a write");
    assert!(
        refused.is_io(),
        "reported {refused} rather than an I/O failure"
    );

    assert_eq!(store.current_revision().main(), 1);
}
