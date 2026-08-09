//! Behavioral checks against the public store API.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions surface failures by panicking"
)]

use mvcc::{KvStore, Mutation, Revision};
use storage_api::{Bounds, MemEngine};

fn store() -> KvStore<MemEngine> {
    KvStore::open(MemEngine::new()).unwrap()
}

fn put(key: &[u8], value: &[u8]) -> Mutation {
    Mutation::Put {
        key: key.to_vec(),
        value: value.to_vec(),
    }
}

/// Applies a batch, advancing the log position by one per mutation.
///
/// The store treats the position as opaque, so the tests only have to keep it
/// moving forward the way a real log would.
fn apply(store: &KvStore<MemEngine>, batch: &[Mutation]) -> Vec<Revision> {
    let upto = store.applied() + batch.len() as u64;
    store.apply(batch, upto).unwrap()
}

fn write(store: &KvStore<MemEngine>, key: &[u8], value: &[u8]) -> Revision {
    apply(store, &[put(key, value)])[0]
}

/// Resolves `bounds` to the keys it currently matches and deletes them.
///
/// Resolution belongs to the caller now: the store applies mutations that
/// already name their keys, so a replayed delete cannot match a different set
/// than the original did.
fn delete_range(store: &KvStore<MemEngine>, bounds: &Bounds) -> u64 {
    let victims: Vec<Mutation> = store
        .range(bounds, None, None)
        .unwrap()
        .into_iter()
        .map(|record| Mutation::Delete { key: record.key })
        .collect();

    let deleted = victims.len() as u64;
    if !victims.is_empty() {
        apply(store, &victims);
    }
    deleted
}

fn values(records: &[mvcc::Record]) -> Vec<&[u8]> {
    records
        .iter()
        .map(|record| record.value.as_slice())
        .collect()
}

#[test]
fn an_empty_store_starts_at_revision_zero() {
    let store = store();
    assert_eq!(store.current_revision().main(), 0);
    assert!(store.range(&Bounds::all(), None, None).unwrap().is_empty());
}

#[test]
fn each_write_advances_the_revision_by_one() {
    let store = store();
    assert_eq!(write(&store, b"a", b"1").main(), 1);
    assert_eq!(write(&store, b"b", b"1").main(), 2);
    assert_eq!(write(&store, b"a", b"2").main(), 3);
}

#[test]
fn a_read_at_an_old_revision_sees_the_value_of_that_time() {
    let store = store();
    write(&store, b"k", b"first");
    let old = write(&store, b"k", b"second").main();
    write(&store, b"k", b"third");

    let historical = store
        .range(&Bounds::point(b"k".as_slice()), Some(old), None)
        .unwrap();
    assert_eq!(values(&historical), [b"second".as_slice()]);

    let current = store
        .range(&Bounds::point(b"k".as_slice()), None, None)
        .unwrap();
    assert_eq!(values(&current), [b"third".as_slice()]);
}

#[test]
fn creation_and_version_track_a_keys_life() {
    let store = store();
    let created = write(&store, b"k", b"1");
    write(&store, b"k", b"2");

    let record = store
        .range(&Bounds::point(b"k".as_slice()), None, None)
        .unwrap();
    let record = record.first().unwrap();
    assert_eq!(record.created, created);
    assert_eq!(record.version, 2);
}

#[test]
fn a_recreated_key_reports_its_new_creation_revision() {
    let store = store();
    write(&store, b"k", b"first");
    delete_range(&store, &Bounds::point(b"k".as_slice()));
    let recreated = write(&store, b"k", b"second");

    let record = store
        .range(&Bounds::point(b"k".as_slice()), None, None)
        .unwrap();
    let record = record.first().unwrap();
    assert_eq!(record.created, recreated);
    assert_eq!(record.version, 1);
}

#[test]
fn deleted_keys_vanish_from_the_current_revision_but_not_the_past() {
    let store = store();
    let written = write(&store, b"k", b"v").main();
    let deleted = delete_range(&store, &Bounds::point(b"k".as_slice()));

    assert_eq!(deleted, 1);
    assert!(store.range(&Bounds::all(), None, None).unwrap().is_empty());
    assert_eq!(
        store
            .range(&Bounds::all(), Some(written), None)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn deleting_nothing_does_not_advance_the_revision() {
    let store = store();
    write(&store, b"k", b"v");
    let before = store.current_revision();

    let deleted = delete_range(&store, &Bounds::prefix(b"absent".as_slice()));
    assert_eq!(deleted, 0);
    assert_eq!(
        store.current_revision(),
        before,
        "a delete matching nothing consumed a revision"
    );
}

#[test]
fn a_ranged_delete_removes_every_key_in_bounds_at_one_revision() {
    let store = store();
    for key in [b"a", b"b", b"c"] {
        write(&store, key, b"v");
    }

    let deleted = delete_range(&store, &Bounds::between(b"a".as_slice(), b"c".as_slice()));

    assert_eq!(deleted, 2);
    let remaining = store.range(&Bounds::all(), None, None).unwrap();
    assert_eq!(remaining.len(), 1, "the exclusive end bound was deleted");
}

#[test]
fn range_respects_bounds_and_limit() {
    let store = store();
    for key in [b"a", b"b", b"c", b"d"] {
        write(&store, key, b"v");
    }

    assert_eq!(store.range(&Bounds::all(), None, Some(2)).unwrap().len(), 2);
    assert_eq!(
        store
            .range(
                &Bounds::between(b"b".as_slice(), b"d".as_slice()),
                None,
                None
            )
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn reading_below_the_compaction_watermark_is_refused() {
    let store = store();
    let early = write(&store, b"k", b"first").main();
    write(&store, b"k", b"second");
    let watermark = write(&store, b"k", b"third").main();

    store.compact(watermark).unwrap();

    let error = store.range(&Bounds::all(), Some(early), None).unwrap_err();
    assert!(
        error.is_compacted(),
        "expected a compacted error, got {error}"
    );

    // The watermark itself stays readable — compaction preserves it.
    assert_eq!(
        values(&store.range(&Bounds::all(), Some(watermark), None).unwrap()),
        [b"third".as_slice()]
    );
}

#[test]
fn compaction_reclaims_superseded_records() {
    let store = store();
    write(&store, b"k", b"first");
    write(&store, b"k", b"second");
    let watermark = write(&store, b"k", b"third").main();

    assert_eq!(store.compact(watermark).unwrap(), 2);
    assert_eq!(
        store.compact(watermark).unwrap(),
        0,
        "compaction was not idempotent"
    );
}

#[test]
fn reading_a_future_revision_is_refused() {
    let store = store();
    write(&store, b"k", b"v");

    let error = store.range(&Bounds::all(), Some(99), None).unwrap_err();
    assert!(
        error.is_future_revision(),
        "expected a future-revision error, got {error}"
    );
}

#[test]
fn compaction_cannot_move_backwards() {
    let store = store();
    for _ in 0..3 {
        write(&store, b"k", b"v");
    }
    store.compact(3).unwrap();

    assert!(store.compact(1).unwrap_err().is_compacted());
}

#[test]
fn reopening_rebuilds_the_index_from_the_backend() {
    let engine = MemEngine::new();
    let written = {
        let store = KvStore::open(engine.clone()).unwrap();
        write(&store, b"a", b"1");
        write(&store, b"b", b"2");
        delete_range(&store, &Bounds::point(b"a".as_slice()));
        write(&store, b"a", b"3")
    };

    let reopened = KvStore::open(engine).unwrap();
    assert_eq!(reopened.current_revision(), written);

    let records = reopened.range(&Bounds::all(), None, None).unwrap();
    assert_eq!(values(&records), [b"3".as_slice(), b"2".as_slice()]);

    // The delete/recreate cycle must survive the rebuild, not just the values.
    let a = records.first().unwrap();
    assert_eq!(a.created, written);
    assert_eq!(a.version, 1);
}
