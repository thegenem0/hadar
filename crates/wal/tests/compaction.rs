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

/// Rolls over constantly, and never checkpoints unless a test asks it to.
fn tiny() -> Options {
    Options {
        segment_limit: TINY_SEGMENT,
        ..Options::default()
    }
}

enum LogDir {
    Discarded(tempfile::TempDir),
    Kept(std::path::PathBuf),
}

impl LogDir {
    fn new() -> Self {
        if std::env::var_os("HADAR_KEEP_WAL").is_none() {
            return Self::Discarded(tempfile::tempdir().expect("temp dir"));
        }

        let name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .replace("::", "-");
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);

        // Start clean, or a previous run's segments would be replayed as if
        // they were this one's.
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path).expect("log directory is creatable");
        println!("log kept at {}", path.display());

        Self::Kept(path)
    }

    fn path(&self) -> &Path {
        match self {
            Self::Discarded(dir) => dir.path(),
            Self::Kept(path) => path,
        }
    }
}

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
    let dir = LogDir::new();
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let options = tiny();
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
    let dir = LogDir::new();
    let options = tiny();

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
    let dir = LogDir::new();
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let options = tiny();

    let mut log = Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log opens");

    fill(&log, 20).await;
    let before = segments(dir.path());

    let removed = log.checkpoint().await.expect("checkpoint succeeds");

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
    let dir = LogDir::new();
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let options = tiny();

    let mut log = Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log opens");

    fill(&log, 20).await;
    log.checkpoint().await.expect("checkpoint succeeds");

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
    let dir = LogDir::new();
    let options = tiny();

    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));

    {
        let mut log =
            Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log opens");
        fill(&log, 20).await;
        log.checkpoint().await.expect("checkpoint succeeds");
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

#[tokio::test]
async fn the_log_checkpoints_itself_without_being_asked() {
    let dir = LogDir::new();
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let options = Options {
        segment_limit: TINY_SEGMENT,
        // Two segments' worth, so every checkpoint finds a sealed segment
        // behind the active one.
        checkpoint_interval: TINY_SEGMENT * 2,
    };
    let log = Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log opens");

    fill(&log, 200).await;

    // Left alone, this burst rolls over roughly fifty times.
    // Nothing here calls checkpoint, so a log that stays small
    // proves the writer is compacting on its own.
    assert!(
        segments(dir.path()).len() < 8,
        "the log grew unbounded: {:?}",
        segments(dir.path())
    );
    assert_eq!(store.current_revision().main(), 200);
}

#[tokio::test]
async fn records_survive_the_log_checkpointing_itself() {
    let dir = LogDir::new();
    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let options = Options {
        segment_limit: TINY_SEGMENT,
        checkpoint_interval: TINY_SEGMENT * 2,
    };

    {
        let log = Wal::with_options(dir.path(), Arc::clone(&store), options).expect("log opens");
        fill(&log, 200).await;
    }

    // Most of these records no longer exist in the log at all.
    // They are readable because the checkpoint that discarded them
    // flushed the store first.
    let reopened = Wal::with_options(dir.path(), Arc::clone(&store), options);
    assert!(reopened.is_ok(), "reopen failed after self-compaction");

    assert_eq!(store.current_revision().main(), 200);
    assert_eq!(values(&store).len(), 200);
}

#[tokio::test]
async fn a_hole_in_a_sealed_segment_is_refused() {
    let dir = LogDir::new();
    let options = tiny();

    {
        let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
        let log = Wal::with_options(dir.path(), store, options).expect("log opens");
        fill(&log, 20).await;
    }

    // Chop the tail off a segment that is not the last one. Rollover happens
    // only at a batch boundary, so a sealed segment ending mid-record can only
    // mean records went missing between it and the segment after it.
    let names = segments(dir.path());
    let sealed = dir.path().join(names.first().expect("a segment existed"));
    let bytes = std::fs::read(&sealed).expect("segment is readable");
    std::fs::write(&sealed, &bytes[..bytes.len() - 5]).expect("segment is writable");

    let store = Arc::new(KvStore::open(MemEngine::new()).expect("store opens"));
    let error = Wal::with_options(dir.path(), store, options)
        .expect_err("a hole in the middle of the log was accepted");

    assert!(
        error.is_corrupt(),
        "reported {error} rather than corruption"
    );
}
