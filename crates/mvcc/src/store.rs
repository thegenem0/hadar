use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use storage_api::{Bounds, ReadTxn, StorageEngine, WriteTxn};

use crate::{error::Error, index::Index, record, revision::Revision};

/// Records read per page when rebuilding the index at startup.
const SCAN_PAGE: usize = 1024;

/// Records removed per backend transaction during compaction.
const COMPACT_CHUNK: usize = 1024;

/// A key/value pair as observed at some revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The user key.
    pub key: Vec<u8>,
    /// The stored value.
    pub value: Vec<u8>,
    /// Revision at which this key's current incarnation was created.
    pub created: Revision,
    /// Revision of the write this record reflects.
    pub modified: Revision,
    /// Number of writes since creation, starting at one.
    pub version: u64,
}

/// A revision-indexed keyspace over a storage backend.
///
/// Every write advances a global revision, and past revisions
/// stay readable until compacted away.
///
/// # Concurrency
///
/// Every method is synchronous and takes `&self`,
/// so a `KvStore` can be shared without an outer lock.
///
/// Reads take a shared lock on the in-memory index.
///
/// Writes take an exclusive lock for the duration of the backend transaction,
/// which the storage contract already serializes anyway.
///
/// Because these are blocking calls, an async caller must not invoke them
/// directly on a runtime worker once a write path can contend.
#[derive(Debug)]
pub struct KvStore<E: StorageEngine> {
    engine: E,
    state: RwLock<State>,
}

#[derive(Debug)]
struct State {
    index: Index,
    current: Revision,
    compacted: u64,
}

impl<E: StorageEngine> KvStore<E> {
    /// Opens a store over `engine`, rebuilding the index from its contents.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend cannot be read,
    /// or if it holds a record this module did not write.
    pub fn open(engine: E) -> Result<Self, Error> {
        let mut index = Index::default();
        let mut current = Revision::zero();

        let txn = engine.begin_read().map_err(Error::storage)?;
        let mut bounds = record::all_records();

        // A store's whole history can be far larger than memory,
        // so we page over it in `SCAN_PAGE` chunks
        loop {
            let page = txn
                .range(&bounds, Some(SCAN_PAGE))
                .map_err(Error::storage)?;

            let Some((last_key, _)) = page.last() else {
                break;
            };
            bounds.advance_past(last_key);

            for (raw_key, raw_value) in &page {
                let (revision, tombstone) = record::parse_key(raw_key)
                    .ok_or_else(|| Error::corrupt("unrecognized revision record key"))?;

                let (key, _) = record::decode(raw_value)
                    .ok_or_else(|| Error::corrupt("truncated revision record"))?;

                if tombstone {
                    index.tombstone(key, revision);
                } else {
                    index.put(key, revision);
                }
                current = current.max(revision);
            }
        }

        Ok(Self {
            engine,
            state: RwLock::new(State {
                index,
                current,
                compacted: 0,
            }),
        })
    }

    /// Returns the revision of the most recent write.
    #[must_use]
    pub fn current_revision(&self) -> Revision {
        self.read_state().current
    }

    /// Writes `value` under `key`, returning the revision it was written at.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend write fails, in which case the store's
    /// revision does not advance.
    pub fn put(&self, key: &[u8], value: &[u8]) -> Result<Revision, Error> {
        let mut state = self.write_state();
        let revision = state.current.next_main();

        let mut txn = self.engine.begin_write().map_err(Error::storage)?;
        txn.insert(
            &record::backend_key(revision, false),
            &record::encode(key, value),
        )
        .map_err(Error::storage)?;
        txn.commit().map_err(Error::storage)?;

        // Applied only after the backend commit, so a failed write
        // leaves the index true to what is on disk.
        state.index.put(key, revision);
        state.current = revision;

        Ok(revision)
    }

    /// Deletes every key within `bounds`, returning the revision and the count.
    ///
    /// Deleting nothing does not advance the revision.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend write fails, in which case nothing is deleted.
    pub fn delete_range(&self, bounds: &Bounds) -> Result<(Revision, u64), Error> {
        let mut state = self.write_state();
        let revision = state.current.next_main();

        let victims: Vec<Vec<u8>> = state
            .index
            .range(bounds, state.current.main())
            .into_iter()
            .map(|(key, _)| key)
            .collect();

        if victims.is_empty() {
            return Ok((state.current, 0));
        }

        let mut txn = self.engine.begin_write().map_err(Error::storage)?;
        for (offset, key) in victims.iter().enumerate() {
            let at = Revision::new(revision.main(), offset as u64)
                .ok_or_else(|| Error::corrupt("delete exceeded the addressable sub range"))?;

            txn.insert(&record::backend_key(at, true), &record::encode(key, &[]))
                .map_err(Error::storage)?;
        }

        txn.commit().map_err(Error::storage)?;

        for (offset, key) in victims.iter().enumerate() {
            if let Some(at) = Revision::new(revision.main(), offset as u64) {
                state.index.tombstone(key, at);
            }
        }
        state.current = revision;

        Ok((revision, victims.len() as u64))
    }

    /// Returns the keys within `bounds` as they were at revision `at`.
    ///
    /// `at` of `None` reads the current revision.
    /// A `limit` caps the number of records returned, taken in ascending key order.
    ///
    /// # Errors
    ///
    /// Returns a compacted error if `at` predates the compaction watermark, a
    /// future-revision error if it exceeds the current revision, and a storage
    /// error if a record the index expects is missing from the backend.
    pub fn range(
        &self,
        bounds: &Bounds,
        at: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<Record>, Error> {
        let state = self.read_state();
        let at = Self::resolve(&state, at)?;

        let visible = state.index.range(bounds, at);
        let txn = self.engine.begin_write().map_err(Error::storage)?;

        let mut records = Vec::new();
        for (key, found) in visible {
            if limit.is_some_and(|l| records.len() >= l) {
                break;
            }

            let raw = txn
                .get(&record::backend_key(found.modified, false))
                .map_err(Error::storage)?
                .ok_or_else(|| Error::corrupt("index references a missing revision record"))?;

            let (_, value) =
                record::decode(&raw).ok_or_else(|| Error::corrupt("truncated revision record"))?;

            records.push(Record {
                key,
                value: value.to_vec(),
                created: found.created,
                modified: found.modified,
                version: found.version,
            });
        }

        Ok(records)
    }

    /// Discards history below `at`, returning how many records were removed.
    ///
    /// Reads at or after `at` are unaffected, and reads below it are refused as
    /// compacted from this point on.
    ///
    /// # Errors
    ///
    /// Returns an error if `at` exceeds the current revision, if it would move
    /// the watermark backwards, or if the backend write fails.
    pub fn compact(&self, at: u64) -> Result<usize, Error> {
        let mut state = self.write_state();
        if at > state.current.main() {
            return Err(Error::future_revision(at, state.current.main()));
        }
        if at < state.compacted {
            return Err(Error::compacted(at, state.compacted));
        }

        let dropped = state.index.compact(at);

        // Chunked so a large compaction cannot monopolize a thread.
        // This shape matches what a yielding async caller would require.
        for chunk in dropped.chunks(COMPACT_CHUNK) {
            let mut txn = self.engine.begin_write().map_err(Error::storage)?;
            for revision in chunk {
                txn.remove(&record::backend_key(*revision, false))
                    .map_err(Error::storage)?;

                txn.remove(&record::backend_key(*revision, true))
                    .map_err(Error::storage)?;
            }
            txn.commit().map_err(Error::storage)?;
        }
        state.compacted = at;

        Ok(dropped.len())
    }

    fn resolve(state: &State, at: Option<u64>) -> Result<u64, Error> {
        let at = at.unwrap_or_else(|| state.current.main());
        if at > state.current.main() {
            return Err(Error::future_revision(at, state.current.main()));
        }
        if at < state.compacted {
            return Err(Error::compacted(at, state.compacted));
        }
        Ok(at)
    }

    fn read_state(&self) -> RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_state(&self) -> RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }
}
