use crate::{Bounds, Error};

/// A key/value pair returned by a scan.
pub type Pair = (Vec<u8>, Vec<u8>);

/// A transactional, ordered, byte-keyed storage backend.
///
/// # Contract
///
/// Every implementation must satisfy the following, and the conformance suite checks each point.
/// These are guarantees the layers above depend on, not descriptions of any particular backend's behavior.
///
/// **Ordering.** Keys are ordered by unsigned byte-wise comparison,
/// not of any text encoding. Ranges iterate ascending.
///
/// **Single writer.** At most one write transaction exists at a time.
/// [`begin_write`](StorageEngine::begin_write) blocks until the previous one
/// commits, aborts, or is dropped, rather than failing or allowing two writers
/// to proceed concurrently.
///
/// **Snapshot isolation.** A read transaction observes the keyspace as of the
/// last commit preceding its creation, and that view never changes for the
/// transaction's lifetime. Concurrent commits are invisible to it. Read
/// transactions never block writers and are never blocked by them.
///
/// **Atomic, totally ordered commits.** A commit applies all of its writes or
/// none of them. Commits are totally ordered, and a read transaction opened
/// after a commit returns observes that commit and every commit preceding it.
///
/// **Durability is not promised.** See [`WriteTxn::commit`].
pub trait StorageEngine: Send + Sync + 'static {
    /// Snapshot-isolated view of the keyspace produced by
    /// [`begin_read`](StorageEngine::begin_read).
    type ReadTxn: ReadTxn;

    /// Exclusive write transaction produced by
    /// [`begin_write`](StorageEngine::begin_write).
    type WriteTxn: WriteTxn;

    /// Opens a read transaction over a consistent snapshot of the keyspace.
    ///
    /// Any number of read transactions may be open at once, and they neither
    /// block nor are blocked by a concurrent writer.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot be established.
    fn begin_read(&self) -> Result<Self::ReadTxn, Error>;

    /// Opens the exclusive write transaction, blocking until it is available.
    ///
    /// Because there is exactly one writer, holding a write transaction open
    /// stalls every other would-be writer. Keep the critical section short and
    /// never hold one across an `.await` point.
    ///
    /// # Errors
    ///
    /// Returns an error if the transaction cannot be started.
    fn begin_write(&self) -> Result<Self::WriteTxn, Error>;
}

/// Read access to a consistent snapshot of the keyspace.
///
/// Implemented by both read transactions and write transactions.
/// A write transaction additionally observes its own uncommitted writes.
pub trait ReadTxn: Send {
    /// Returns the value stored under `key`, or `None` if the key is absent.
    ///
    /// # Errors
    ///
    /// Returns an error if the lookup fails, which for most backends means an
    /// I/O failure or corrupt on-disk state. A missing key is not an error.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error>;

    /// Returns the key/value pairs within `bounds`, in ascending key order.
    ///
    /// A `limit` caps the number of pairs returned, taken from the start of
    /// the range, and `None` returns every pair within the bounds. Callers scanning
    /// a large keyspace should page rather than materializing the whole range at once.
    ///
    /// # Errors
    ///
    /// Returns an error if the scan fails partway; the returned `Vec` is then
    /// discarded, so a caller never observes a partial range.
    fn range(&self, bounds: &Bounds, limit: Option<usize>) -> Result<Vec<Pair>, Error>;
}

/// Exclusive write access to the keyspace.
///
/// Writes are buffered and become visible to other transactions only on
/// [`commit`](WriteTxn::commit).
///
/// Reads through this transaction observe its own buffered writes,
/// while reads through any other transaction do not.
///
/// Dropping a write transaction without committing aborts it.
/// Early returns discard the partial transaction rather than leaving it applied.
/// This makes `?` safe to use freely inside a write path.
pub trait WriteTxn: ReadTxn {
    /// Stores `value` under `key`, replacing any existing value.
    ///
    /// # Errors
    ///
    /// Returns an error if the write cannot be buffered.
    fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<(), Error>;

    /// Removes `key`, returning the value it held, or `None` if it was absent.
    ///
    /// # Errors
    ///
    /// Returns an error if the removal cannot be buffered.
    /// Removing a key that does not exist is not an error.
    fn remove(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, Error>;

    /// Applies every buffered write atomically and releases the writer slot.
    ///
    /// After this returns `Ok`, a newly opened read transaction observes all of these writes.
    /// Consuming `self` makes use-after-commit a compile error.
    ///
    /// # Durability
    ///
    /// Commit guarantees atomicity and visibility ordering.
    /// It does **not** guarantee the writes have reached stable storage.
    /// Durability is the write-ahead log's responsibility, layered above this trait.
    /// An implementation that does happen to fsync on commit is exceeding this
    /// contract, and callers must not rely on that.
    ///
    /// # Errors
    ///
    /// Returns an error if the commit fails, in which case none of the writes are applied.
    fn commit(self) -> Result<(), Error>;

    /// Discards every buffered write and releases the writer slot.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails while releasing resources.
    /// The writes are discarded either way.
    fn abort(self) -> Result<(), Error>;
}
