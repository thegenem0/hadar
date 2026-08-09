//! Executable form of the storage contract, shared by every backend.
//!
//! Each case is a plain function taking an engine; the
//! [`conformance_suite!`](crate::conformance_suite) macro wraps them as
//! `#[test]`s so a failure names the guarantee that broke rather than a line
//! number in a shared harness.

#![expect(
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "a conformance case reports failure by panicking; that is its return channel"
)]

use crate::{Bounds, ReadTxn, StorageEngine, WriteTxn};

/// Generates one `#[test]` per conformance case against a storage backend.
///
/// The argument is a *function producing an engine*, not an engine — each case
/// gets its own instance. Pass a path, not a closure literal.
///
/// # Examples
///
/// ```ignore
/// storage_api::conformance_suite!(storage_redb::RedbEngine::temporary);
/// ```
#[macro_export]
macro_rules! conformance_suite {
    ($new_engine:expr) => {
        #[test]
        fn get_missing_returns_none() {
            $crate::conform::get_missing_returns_none(&($new_engine)());
        }
        #[test]
        fn insert_then_get_round_trips() {
            $crate::conform::insert_then_get_round_trips(&($new_engine)());
        }
        #[test]
        fn insert_overwrites_existing_value() {
            $crate::conform::insert_overwrites_existing_value(&($new_engine)());
        }
        #[test]
        fn remove_returns_previous_value() {
            $crate::conform::remove_returns_previous_value(&($new_engine)());
        }
        #[test]
        fn remove_missing_returns_none() {
            $crate::conform::remove_missing_returns_none(&($new_engine)());
        }
        #[test]
        fn write_txn_reads_its_own_writes() {
            $crate::conform::write_txn_reads_its_own_writes(&($new_engine)());
        }
        #[test]
        fn writes_are_invisible_before_commit() {
            $crate::conform::writes_are_invisible_before_commit(&($new_engine)());
        }
        #[test]
        fn snapshot_is_stable_across_a_commit() {
            $crate::conform::snapshot_is_stable_across_a_commit(&($new_engine)());
        }
        #[test]
        fn read_opened_after_commit_observes_it() {
            $crate::conform::read_opened_after_commit_observes_it(&($new_engine)());
        }
        #[test]
        fn abort_discards_writes() {
            $crate::conform::abort_discards_writes(&($new_engine)());
        }
        #[test]
        fn drop_discards_writes() {
            $crate::conform::drop_discards_writes(&($new_engine)());
        }
        #[test]
        fn range_is_ascending() {
            $crate::conform::range_is_ascending(&($new_engine)());
        }
        #[test]
        fn range_orders_keys_as_unsigned_bytes() {
            $crate::conform::range_orders_keys_as_unsigned_bytes(&($new_engine)());
        }
        #[test]
        fn range_end_bound_is_exclusive() {
            $crate::conform::range_end_bound_is_exclusive(&($new_engine)());
        }
        #[test]
        fn range_unbounded_covers_keyspace() {
            $crate::conform::range_unbounded_covers_keyspace(&($new_engine)());
        }
        #[test]
        fn range_limit_takes_from_the_start() {
            $crate::conform::range_limit_takes_from_the_start(&($new_engine)());
        }
    };
}

/// Checks that a lookup of an absent key succeeds with `None` insted of failing.
pub fn get_missing_returns_none<E: StorageEngine>(engine: &E) {
    let txn = engine.begin_read().unwrap();
    assert_eq!(txn.get(b"absent").unwrap(), None);
}

/// Checks that a committed value is returned verbatim.
pub fn insert_then_get_round_trips<E: StorageEngine>(engine: &E) {
    put(engine, b"key", b"value");

    let txn = engine.begin_read().unwrap();
    assert_eq!(
        txn.get(b"key").unwrap().as_deref(),
        Some(b"value".as_slice())
    );
}

/// Checks that inserting over an existing key replaces its value
/// rather than appending a second entry.
pub fn insert_overwrites_existing_value<E: StorageEngine>(engine: &E) {
    put(engine, b"key", b"first");
    put(engine, b"key", b"second");

    let txn = engine.begin_read().unwrap();
    assert_eq!(
        txn.get(b"key").unwrap().as_deref(),
        Some(b"second".as_slice())
    );
    assert_eq!(txn.range(&Bounds::all(), None).unwrap().len(), 1);
}

/// Checks that removal reports the value that was displaced.
pub fn remove_returns_previous_value<E: StorageEngine>(engine: &E) {
    put(engine, b"key", b"value");

    let mut txn = engine.begin_write().unwrap();
    assert_eq!(
        txn.remove(b"key").unwrap().as_deref(),
        Some(b"value".as_slice())
    );
    txn.commit().unwrap();

    let txn = engine.begin_read().unwrap();
    assert_eq!(txn.get(b"key").unwrap(), None);
}

/// Checks that removing an absent key succeeds with `None` rather than failing.
pub fn remove_missing_returns_none<E: StorageEngine>(engine: &E) {
    let mut txn = engine.begin_write().unwrap();
    assert_eq!(txn.remove(b"absent").unwrap(), None);
    txn.commit().unwrap();
}

/// Checks read-your-own-writes.
/// A write transaction observes its own buffered changes before they are committed.
pub fn write_txn_reads_its_own_writes<E: StorageEngine>(engine: &E) {
    let mut txn = engine.begin_write().unwrap();
    txn.insert(b"key", b"value").unwrap();
    assert_eq!(
        txn.get(b"key").unwrap().as_deref(),
        Some(b"value".as_slice())
    );

    txn.remove(b"key").unwrap();
    assert_eq!(txn.get(b"key").unwrap(), None);
}

/// Checks that a read transaction opened while a write is in flight
/// does not observe the uncommitted write.
pub fn writes_are_invisible_before_commit<E: StorageEngine>(engine: &E) {
    let mut writer = engine.begin_write().unwrap();
    writer.insert(b"key", b"value").unwrap();

    let reader = engine.begin_read().unwrap();
    assert_eq!(reader.get(b"key").unwrap(), None);

    writer.commit().unwrap();
}

/// Checks snapshot isolation.
/// A read transaction's view does not change when a write commits underneath it.
///
/// Distinguishes a genuine snapshot from a live view of current state.
pub fn snapshot_is_stable_across_a_commit<E: StorageEngine>(engine: &E) {
    put(engine, b"key", b"before");

    let reader = engine.begin_read().unwrap();
    put(engine, b"key", b"after");

    assert_eq!(
        reader.get(b"key").unwrap().as_deref(),
        Some(b"before".as_slice()),
        "read transaction observed a commit made after it was opened"
    );
}

/// Checks that a read transaction opened after a commit observes it.
pub fn read_opened_after_commit_observes_it<E: StorageEngine>(engine: &E) {
    let reader_before = engine.begin_read().unwrap();
    put(engine, b"key", b"value");
    let reader_after = engine.begin_read().unwrap();

    assert_eq!(reader_before.get(b"key").unwrap(), None);
    assert_eq!(
        reader_after.get(b"key").unwrap().as_deref(),
        Some(b"value".as_slice())
    );
}

/// Checks that an explicitly aborted transaction leaves no trace.
pub fn abort_discards_writes<E: StorageEngine>(engine: &E) {
    let mut txn = engine.begin_write().unwrap();
    txn.insert(b"key", b"value").unwrap();
    txn.abort().unwrap();

    let txn = engine.begin_read().unwrap();
    assert_eq!(txn.get(b"key").unwrap(), None);
}

/// Checks that dropping a transaction without committing aborts it,
/// so an early return via `?` cannot leave a partial write applied.
pub fn drop_discards_writes<E: StorageEngine>(engine: &E) {
    {
        let mut txn = engine.begin_write().unwrap();
        txn.insert(b"key", b"value").unwrap();
    }

    let txn = engine.begin_read().unwrap();
    assert_eq!(
        txn.get(b"key").unwrap(),
        None,
        "a dropped write transaction was applied"
    );
}

/// Checks that a range returns pairs in ascending key order regardless of insertion order.
pub fn range_is_ascending<E: StorageEngine>(engine: &E) {
    for key in [b"c", b"a", b"d", b"b"] {
        put(engine, key, b"v");
    }

    let txn = engine.begin_read().unwrap();
    assert_eq!(keys(&txn, &Bounds::all(), None), [b"a", b"b", b"c", b"d"]);
}

/// Checks that keys order by unsigned byte comparison.
///
/// A backend comparing bytes as signed would sort `0x80` before `0x7f`.
pub fn range_orders_keys_as_unsigned_bytes<E: StorageEngine>(engine: &E) {
    for key in [[0xff_u8], [0x80], [0x7f], [0x00]] {
        put(engine, &key, b"v");
    }

    let txn = engine.begin_read().unwrap();
    assert_eq!(
        keys(&txn, &Bounds::all(), None),
        [[0x00], [0x7f], [0x80], [0xff]]
    );
}

/// Checks that a range's start bound is inclusive and its end bound exclusive.
pub fn range_end_bound_is_exclusive<E: StorageEngine>(engine: &E) {
    for key in [b"a", b"b", b"c"] {
        put(engine, key, b"v");
    }

    let txn = engine.begin_read().unwrap();
    assert_eq!(
        keys(
            &txn,
            &Bounds::between(b"a".as_slice(), b"c".as_slice()),
            None
        ),
        [b"a", b"b"]
    );
}

/// Checks that an unbounded range covers every key.
pub fn range_unbounded_covers_keyspace<E: StorageEngine>(engine: &E) {
    for key in [b"a", b"b", b"c"] {
        put(engine, key, b"v");
    }

    let txn = engine.begin_read().unwrap();
    assert_eq!(keys(&txn, &Bounds::all(), None), [b"a", b"b", b"c"]);
    assert_eq!(
        keys(&txn, &Bounds::start_at(b"b".as_slice()), None),
        [b"b", b"c"]
    );
    assert_eq!(
        keys(&txn, &Bounds::end_before(b"c".as_slice()), None),
        [b"a", b"b"]
    );
}

/// Checks that a limit truncates from the start of the range, not the end.
pub fn range_limit_takes_from_the_start<E: StorageEngine>(engine: &E) {
    for key in [b"a", b"b", b"c", b"d"] {
        put(engine, key, b"v");
    }

    let txn = engine.begin_read().unwrap();
    assert_eq!(keys(&txn, &Bounds::all(), Some(2)), [b"a", b"b"]);
    assert_eq!(keys(&txn, &Bounds::all(), Some(0)).len(), 0);
    assert_eq!(keys(&txn, &Bounds::all(), Some(99)).len(), 4);
}

fn put<E: StorageEngine>(engine: &E, key: &[u8], value: &[u8]) {
    let mut txn = engine.begin_write().unwrap();
    txn.insert(key, value).unwrap();
    txn.commit().unwrap();
}

fn keys<T: ReadTxn>(txn: &T, bounds: &Bounds, limit: Option<usize>) -> Vec<Vec<u8>> {
    txn.range(bounds, limit)
        .unwrap()
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}
