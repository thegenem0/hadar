/// A single resolved change to the keyspace.
///
/// Mutations are the unit the store applies and the write-ahead log records.
/// They are deliberately *resolved*: a ranged delete becomes one [`Delete`] per
/// matching key, decided by whoever issued it, rather than a range the store
/// re-evaluates. Re-evaluating a range at apply time would let the same log
/// produce different results against different state, which is exactly what
/// replay must not do.
///
/// [`Delete`]: Mutation::Delete
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutation {
    /// Stores a value under a key, replacing any existing value.
    Put {
        /// Key to write.
        key: Vec<u8>,
        /// Value to store.
        value: Vec<u8>,
    },
    /// Removes a single key.
    Delete {
        /// Key to remove.
        key: Vec<u8>,
    },
}

impl Mutation {
    /// Returns the key this mutation affects.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        match self {
            Self::Put { key, .. } | Self::Delete { key } => key,
        }
    }
}
