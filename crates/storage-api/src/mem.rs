use std::collections::BTreeMap;
use std::ops::RangeBounds;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::{Bounds, Error, Pair, ReadTxn, StorageEngine, WriteTxn};

type Map = BTreeMap<Vec<u8>, Vec<u8>>;

/// An in-memory [`StorageEngine`] that exists only to exercise the contract.
///
/// This is not a production backend!
/// It holds the entire keyspace in memory and copies it wholesale on every transaction.
/// Its purpose is to prove the trait boundary is implementable without reference to any
/// particular storage library.
///
/// If a change to the trait can only be satisfied by the real backend,
/// the trait has leaked something backend-specific.
///
/// Cloning shares the underlying keyspace.
#[derive(Debug, Clone, Default)]
pub struct MemEngine {
    shared: Arc<Shared>,
}

#[derive(Debug, Default)]
struct Shared {
    committed: Mutex<Map>,
    writer: Gate,
}

/// A binary semaphore enforcing the single-writer guarantee.
#[derive(Debug, Default)]
struct Gate {
    held: Mutex<bool>,
    released: Condvar,
}

/// Snapshot-isolated read transaction over a [`MemEngine`].
#[derive(Debug)]
pub struct MemRead {
    snapshot: Map,
}

/// Exclusive write transaction over a [`MemEngine`].
#[derive(Debug)]
pub struct MemWrite {
    shared: Arc<Shared>,
    working: Map,
    finished: bool,
}

impl MemEngine {
    /// Creates an engine with an empty keyspace.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl StorageEngine for MemEngine {
    type ReadTxn = MemRead;
    type WriteTxn = MemWrite;

    fn begin_read(&self) -> Result<Self::ReadTxn, Error> {
        Ok(MemRead {
            snapshot: lock(&self.shared.committed).clone(),
        })
    }

    fn begin_write(&self) -> Result<Self::WriteTxn, Error> {
        self.shared.writer.acquire();
        let working = lock(&self.shared.committed).clone();
        Ok(MemWrite {
            shared: Arc::clone(&self.shared),
            working,
            finished: false,
        })
    }
}

impl ReadTxn for MemRead {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.snapshot.get(key).cloned())
    }

    fn range(&self, bounds: &Bounds, limit: Option<usize>) -> Result<Vec<Pair>, Error> {
        Ok(collect(&self.snapshot, bounds, limit))
    }
}

impl ReadTxn for MemWrite {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.working.get(key).cloned())
    }

    fn range(&self, bounds: &Bounds, limit: Option<usize>) -> Result<Vec<Pair>, Error> {
        Ok(collect(&self.working, bounds, limit))
    }
}

impl WriteTxn for MemWrite {
    fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        self.working.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn remove(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.working.remove(key))
    }

    fn commit(mut self) -> Result<(), Error> {
        *lock(&self.shared.committed) = std::mem::take(&mut self.working);
        self.finish();
        Ok(())
    }

    /// Commits, since an in-memory keyspace has no stable storage to reach.
    fn commit_durable(self) -> Result<(), Error> {
        self.commit()
    }

    fn abort(mut self) -> Result<(), Error> {
        self.finish();
        Ok(())
    }
}

impl MemWrite {
    /// Releases the writer slot exactly once, whether reached via `commit`, `abort`, or `Drop`.
    fn finish(&mut self) {
        if !self.finished {
            self.finished = true;
            self.shared.writer.release();
        }
    }
}

impl Drop for MemWrite {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Gate {
    fn acquire(&self) {
        let mut held = lock(&self.held);
        while *held {
            held = self
                .released
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *held = true;
    }

    fn release(&self) {
        *lock(&self.held) = false;
        self.released.notify_one();
    }
}

/// Locks `mutex`, treating poisoning as recoverable.
///
/// A panic in a test holding this lock should surface as that test's failure,
/// not as an unrelated poisoning panic in every subsequent one.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn collect(map: &Map, bounds: &Bounds, limit: Option<usize>) -> Vec<Pair> {
    let pairs = map
        .range::<[u8], _>((bounds.start_bound(), bounds.end_bound()))
        .map(|(key, value)| (key.clone(), value.clone()));

    match limit {
        Some(limit) => pairs.take(limit).collect(),
        None => pairs.collect(),
    }
}
