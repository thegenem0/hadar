use std::{fmt, ops::RangeBounds, path::Path};

use redb::{
    Database, ReadOnlyTable, ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition,
    WriteTransaction,
};
use storage_api::{Bounds, Error, Pair, ReadTxn, StorageEngine, WriteTxn};

use crate::error;

/// The single flat keyspace.
/// Logical namespaces are expressed by key prefix in consuming layers,
/// so the backend never needs a table-per-namespace concept
/// that a different engine might not have.
const TABLE: TableDefinition<'_, &[u8], &[u8]> = TableDefinition::new("hadar");

/// A [`StorageEngine`] backed by redb.
///
/// This is the only crate that references redb types.
/// Everything above it works through the `storage-api` contract,
/// so replacing the backend touches nothing outside this crate and the composition root.
#[derive(Debug)]
pub struct RedbEngine {
    db: Database,
}

/// Snapsot-isolated read transaction over a [`RedbEngine`].
#[derive(Debug)]
pub struct RedbRead {
    // The transaction is retained alongside the table so the snapshot stays
    // pinned for the reader's lifetime, rather than relying on the table alone
    // to hold redb's internal references.
    _txn: ReadTransaction,
    table: ReadOnlyTable<&'static [u8], &'static [u8]>,
}

/// Exclusive write transaction over a [`RedbEngine`].
pub struct RedbWrite {
    txn: WriteTransaction,
}

impl fmt::Debug for RedbWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedbWrite").finish_non_exhaustive()
    }
}

impl RedbEngine {
    /// Opens, or creates, a database at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened, is not a redb database,
    /// or fails redb's integrity check on load.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let db = Database::create(path).map_err(|e| error::database("opening database", e))?;
        let engine = Self { db };
        engine.create_table()?;
        Ok(engine)
    }

    /// Creates the keyspace table if it does not yet exist.
    ///
    /// Doing this once at open time means a read transaction always finds the
    /// table, so "no writes yet" and "table missing" never have to be
    /// disambiguated on the read path.
    fn create_table(&self) -> Result<(), Error> {
        let txn = self
            .db
            .begin_write()
            .map_err(|e| error::transaction("creating keyspace", e))?;

        txn.open_table(TABLE)
            .map_err(|e| error::table("creating keyspace", e))?;

        txn.commit()
            .map_err(|e| error::commit("creating keyspace", e))
    }
}

/// Test-only
#[cfg(feature = "test-util")]
impl RedbEngine {
    /// Creates an engine backed by memory rather than a file.
    ///
    /// Nothing is persisted and no temporary file is created,
    /// which keeps the conformance suite free of filesystem cleanup.
    ///
    /// # Panics
    ///
    /// Panics if redb cannot initialize an in-memory database.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "a test fixture that cannot be built must fail loudly, not return Result"
    )]
    pub fn temporary() -> Self {
        let db = Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .expect("INVARIANT: in-memory redb database is always constructible");

        let engine = Self { db };

        engine
            .create_table()
            .expect("INVARIANT: keyspace creation cannot fail on a fresh in-memory database");

        engine
    }
}

impl StorageEngine for RedbEngine {
    type ReadTxn = RedbRead;
    type WriteTxn = RedbWrite;

    fn begin_read(&self) -> Result<Self::ReadTxn, Error> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| error::transaction("beginning read", e))?;

        let table = txn
            .open_table(TABLE)
            .map_err(|e| error::table("opening keyspace for read", e))?;

        Ok(RedbRead { _txn: txn, table })
    }

    fn begin_write(&self) -> Result<Self::WriteTxn, Error> {
        let txn = self
            .db
            .begin_write()
            .map_err(|e| error::transaction("beginning write", e))?;

        Ok(RedbWrite { txn })
    }
}

impl ReadTxn for RedbRead {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let found = self
            .table
            .get(key)
            .map_err(|e| error::storage("reading key", e))?;

        Ok(found.map(|val| val.value().to_vec()))
    }

    fn range(&self, bounds: &Bounds, limit: Option<usize>) -> Result<Vec<Pair>, Error> {
        collect(&self.table, bounds, limit)
    }
}

impl ReadTxn for RedbWrite {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let table = self
            .txn
            .open_table(TABLE)
            .map_err(|e| error::table("opening keyspace for read", e))?;

        let found = table
            .get(key)
            .map_err(|e| error::storage("reading key", e))?;

        Ok(found.map(|v| v.value().to_vec()))
    }

    fn range(&self, bounds: &Bounds, limit: Option<usize>) -> Result<Vec<Pair>, Error> {
        let table = self
            .txn
            .open_table(TABLE)
            .map_err(|e| error::table("opening keyspace for read", e))?;

        collect(&table, bounds, limit)
    }
}

impl WriteTxn for RedbWrite {
    fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        let mut table = self
            .txn
            .open_table(TABLE)
            .map_err(|e| error::table("opening keyspace for write", e))?;

        table
            .insert(key, value)
            .map_err(|e| error::storage("inserting key", e))?;

        Ok(())
    }

    fn remove(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let mut table = self
            .txn
            .open_table(TABLE)
            .map_err(|e| error::table("opening keyspace for write", e))?;

        let prev = table
            .remove(key)
            .map_err(|e| error::storage("removing key", e))?;

        Ok(prev.map(|v| v.value().to_vec()))
    }

    fn commit(self) -> Result<(), Error> {
        self.txn
            .commit()
            .map_err(|e| error::commit("committing write", e))
    }

    fn abort(self) -> Result<(), Error> {
        self.txn
            .abort()
            .map_err(|e| error::storage("aborting write", e))
    }
}

/// Materializes a bounded range, translating owned [`Bounds`]
/// into the `(Bound, Bound)` pair redb's range API accepts.
fn collect<T>(table: &T, bounds: &Bounds, limit: Option<usize>) -> Result<Vec<Pair>, Error>
where
    T: ReadableTable<&'static [u8], &'static [u8]>,
{
    let rows = table
        .range::<&[u8]>((bounds.start_bound(), bounds.end_bound()))
        .map_err(|e| error::storage("opening range", e))?;

    let mut pairs = Vec::new();
    for row in rows {
        if limit.is_some_and(|l| pairs.len() >= l) {
            break;
        }
        let (k, v) = row.map_err(|e| error::storage("reading page", e))?;
        pairs.push((k.value().to_vec(), v.value().to_vec()));
    }

    Ok(pairs)
}
