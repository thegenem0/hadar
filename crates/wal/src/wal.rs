use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use mvcc::{KvStore, Mutation, Revision};
use storage_api::StorageEngine;

use crate::{error::Error, record, segment, writer::Writer};

/// A durable write-ahead log fronting a revision-indexed store.
///
/// Writes are recorded here and flushed before they are applied, so an
/// acknowledged write survives a crash and an unacknowledged one is never
/// visible afterwards. Concurrent writes share a flush, which is what makes
/// durability affordable.
///
/// # Recovery
///
/// Opening replays whatever the store has not yet applied. The store records
/// how far it has applied in the same transaction as the data, so the two
/// cannot disagree, and replays from that point exactly.
#[derive(Debug)]
pub struct Wal<E: StorageEngine> {
    writer: Writer,
    store: Arc<KvStore<E>>,
}

impl<E: StorageEngine> Wal<E> {
    /// Opens the log in `dir`, replaying anything `store` has not applied.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read, a segment is damaged,
    /// or replaying its records into the store fails.
    pub fn open(dir: impl Into<PathBuf>, store: Arc<KvStore<E>>) -> Result<Self, Error> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|e| Error::io("creating the log directory", e))?;

        let recovered = recover(&dir, &store)?;
        let segment = match recovered.tail {
            Some(tail) => segment::Segment::reopen(&tail.path, tail.first, tail.valid)?,
            None => segment::Segment::create(&dir, 0)?,
        };

        Ok(Self {
            writer: Writer::start(Arc::clone(&store), dir, segment),
            store,
        })
    }

    /// Records `mutations` durably and applies them, returning their revisions.
    ///
    /// The call returns once the batch is both flushed and visible.
    ///
    /// # Errors
    ///
    /// Returns an error if the log cannot be written or the store cannot apply
    /// the batch. Neither acknowledges the write.
    pub async fn write(&self, mutations: Vec<Mutation>) -> Result<Vec<Revision>, Error> {
        if mutations.is_empty() {
            return Ok(Vec::new());
        }

        self.writer.write(mutations).await
    }

    /// Returns the store this log protects.
    #[must_use]
    pub fn store(&self) -> &Arc<KvStore<E>> {
        &self.store
    }

    /// Stops accepting writes and finishes those already queued.
    pub fn shutdown(&mut self) {
        self.writer.shutdown();
    }
}

/// Where the log ends, once every complete record has been replayed.
struct Recovered {
    tail: Option<Tail>,
}

/// The segment new records append to, and the point they append at.
struct Tail {
    path: PathBuf,
    first: u64,
    /// Bytes holding complete records. Anything past this is a partial write.
    valid: u64,
}

/// Replays records the store has not applied and locates the end of the log.
fn recover<E: StorageEngine>(dir: &Path, store: &KvStore<E>) -> Result<Recovered, Error> {
    let mut seen = 0_u64;
    let mut pending = Vec::new();
    let mut tail = None;

    for (first, path) in segment::list(dir)? {
        let bytes = segment::read(&path)?;
        let mut rest = bytes.as_slice();
        let mut valid = 0_u64;

        while !rest.is_empty() {
            match record::decode(rest) {
                Ok((mutation, width)) => {
                    seen += 1;
                    if seen > store.applied() {
                        pending.push(mutation);
                    }
                    valid += width as u64;
                    rest = &rest[width..];
                }

                // A record that ends early was left mid-append.
                // It was never acknowledged, so no client is waiting on it and
                // it is discarded rather than replayed.
                Err(error) if error.is_truncated() => break,
                Err(error) => return Err(error),
            }
        }

        tail = Some(Tail { path, first, valid });
    }

    if !pending.is_empty() {
        store
            .apply(&pending, seen)
            .map_err(|e| Error::apply(e.to_string()))?;
    }

    Ok(Recovered { tail })
}
