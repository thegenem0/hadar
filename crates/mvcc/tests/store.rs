//! Behavioral checks against the public store API.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions surface failures by panicking"
)]

use mvcc::KvStore;
use storage_api::{Bounds, MemEngine};

fn store() -> KvStore<MemEngine> {
    KvStore::open(MemEngine::new()).unwrap()
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
    assert_eq!(store.put(b"a", b"1").unwrap().main(), 1);
    assert_eq!(store.put(b"b", b"1").unwrap().main(), 2);
    assert_eq!(store.put(b"a", b"2").unwrap().main(), 3);
}

#[test]
fn a_read_at_an_old_revision_sees_the_value_of_that_time() {
    let store = store();
    store.put(b"k", b"first").unwrap();
    let old = store.put(b"k", b"second").unwrap().main();
    store.put(b"k", b"third").unwrap();

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
    let created = store.put(b"k", b"1").unwrap();
    store.put(b"k", b"2").unwrap();

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
    store.put(b"k", b"first").unwrap();
    store.delete_range(&Bounds::point(b"k".as_slice())).unwrap();
    let recreated = store.put(b"k", b"second").unwrap();

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
    let written = store.put(b"k", b"v").unwrap().main();
    let (_, deleted) = store.delete_range(&Bounds::point(b"k".as_slice())).unwrap();

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
    store.put(b"k", b"v").unwrap();
    let before = store.current_revision();

    let (revision, deleted) = store
        .delete_range(&Bounds::prefix(b"absent".as_slice()))
        .unwrap();
    assert_eq!(deleted, 0);
    assert_eq!(revision, before);
    assert_eq!(store.current_revision(), before);
}

#[test]
fn a_ranged_delete_removes_every_key_in_bounds_at_one_revision() {
    let store = store();
    for key in [b"a", b"b", b"c"] {
        store.put(key, b"v").unwrap();
    }

    let (revision, deleted) = store
        .delete_range(&Bounds::between(b"a".as_slice(), b"c".as_slice()))
        .unwrap();

    assert_eq!(deleted, 2);
    let remaining = store.range(&Bounds::all(), None, None).unwrap();
    assert_eq!(remaining.len(), 1, "the exclusive end bound was deleted");
    assert_eq!(store.current_revision(), revision);
}

#[test]
fn range_respects_bounds_and_limit() {
    let store = store();
    for key in [b"a", b"b", b"c", b"d"] {
        store.put(key, b"v").unwrap();
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
    let early = store.put(b"k", b"first").unwrap().main();
    store.put(b"k", b"second").unwrap();
    let watermark = store.put(b"k", b"third").unwrap().main();

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
    store.put(b"k", b"first").unwrap();
    store.put(b"k", b"second").unwrap();
    let watermark = store.put(b"k", b"third").unwrap().main();

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
    store.put(b"k", b"v").unwrap();

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
        store.put(b"k", b"v").unwrap();
    }
    store.compact(3).unwrap();

    assert!(store.compact(1).unwrap_err().is_compacted());
}

#[test]
fn reopening_rebuilds_the_index_from_the_backend() {
    let engine = MemEngine::new();
    let written = {
        let store = KvStore::open(engine.clone()).unwrap();
        store.put(b"a", b"1").unwrap();
        store.put(b"b", b"2").unwrap();
        store.delete_range(&Bounds::point(b"a".as_slice())).unwrap();
        store.put(b"a", b"3").unwrap()
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
