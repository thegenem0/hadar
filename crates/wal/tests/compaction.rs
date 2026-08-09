//! Checks that the log discards records only once the store can rebuild them.
#![expect(
    clippy::expect_used,
    reason = "test assertions surface failures by panicking"
)]

use std::path::Path;
use std::sync::Arc;

use mvcc::{KvStore, Mutation};
use storage_api::{Bounds, MemEngine};
use wal::{Options, Wal};

/// Small enough that a handful of records rolls the segment over, so rollover
/// and compaction are exercised by tests rather than only by production load.
const TINY_SEGMENT: u64 = 64;

fn put(key: &[u8], value: &[u8]) -> Mutation {
    Mutation::Put {
        key: key.to_vec(),
        value: value.to_vec(),
    }
}

fn segments(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .expect("log directory is readable")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "wal").then(|| path.file_name()?.to_str().map(str::to_owned))?
        })
        .collect();
    names.sort();
    names
}

fn values<E: storage_api::StorageEngine>(store: &KvStore<E>) -> Vec<Vec<u8>> {
    store
        .range(&Bounds::all(), None, None)
        .expect("store reads")
        .into_iter()
        .map(|record| record.value)
        .collect()
}

async fn fill(log: &Wal<MemEngine>, count: u32) {
    for i in 0..count {
        log.write(vec![put(&i.to_be_bytes(), b"v")])
            .await
            .expect("write succeeds");
    }
}

#[tokio::test]
async fn writing_past_the_limit_rolls_over_to_a_new_segment() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let options = Options {
        segment_limit: TINY_SEGMENT,
    };
    let log = Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log opens");

    fill(&log, 20).await;

    assert!(
        segments(dir.path()).len() > 1,
        "the log never rolled over: {:?}",
        segments(dir.path())
    );
}

#[tokio::test]
async fn a_rolled_over_log_still_replays_completely() {
    let dir = tempfile::tempdir().expect("temp dir");
    let options = Options {
        segment_limit: TINY_SEGMENT,
    };

    {
        let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
        let log = Wal::with_options(dir.path(), store, options).expect("log opens");
        fill(&log, 20).await;
    }

    // Segment names carry the record index each one starts at. If rollover
    // names them wrongly, replay across the boundary loses or repeats records.
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let _log = Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log reopens");

    assert_eq!(store.current_revision().main(), 20);
    assert_eq!(values(&store).len(), 20);
}

#[tokio::test]
async fn checkpointing_removes_only_fully_durable_segments() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let options = Options {
        segment_limit: TINY_SEGMENT,
    };
    let mut log = Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log opens");

    fill(&log, 20).await;
    let before = segments(dir.path());

    let removed = log.checkpoint().expect("checkpoint succeeds");

    assert!(removed > 0, "checkpoint freed nothing from {before:?}");
    assert_eq!(
        segments(dir.path()).len(),
        before.len() - removed,
        "more segments vanished than were reported"
    );
    // The segment being appended to has no successor, so it can never qualify
    // for removal — the writer's target must survive.
    assert!(
        segments(dir.path()).contains(before.last().expect("a segment existed")),
        "compaction removed the active segment"
    );
}

#[tokio::test]
async fn data_survives_a_checkpoint_that_discarded_its_records() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let options = Options {
        segment_limit: TINY_SEGMENT,
    };
    let mut log = Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log opens");

    fill(&log, 20).await;
    log.checkpoint().expect("checkpoint succeeds");

    // Writing after a checkpoint has to keep working, and the records written
    // before it must remain readable even though their log entries are gone.
    log.write(vec![put(b"after", b"v")])
        .await
        .expect("write succeeds");

    assert_eq!(store.current_revision().main(), 21);
    assert_eq!(values(&store).len(), 21);
}

#[tokio::test]
async fn recovery_after_a_checkpoint_does_not_recount_from_zero() {
    let dir = tempfile::tempdir().expect("temp dir");
    let options = Options {
        segment_limit: TINY_SEGMENT,
    };
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));

    {
        let mut log =
            Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log opens");
        fill(&log, 20).await;
        log.checkpoint().expect("checkpoint succeeds");
        log.write(vec![put(b"after", b"v")])
            .await
            .expect("write succeeds");
    }

    // The surviving segments no longer start at record zero. Recovery has to
    // take each segment's starting index from its name; counting from the
    // first surviving record would replay everything again.
    let reopened = Wal::with_options(dir.path(), Arc::clone(&store), options);
    assert!(reopened.is_ok(), "reopen failed after compaction");

    assert_eq!(
        store.current_revision().main(),
        21,
        "recovery replayed records the store had already applied"
    );
}
