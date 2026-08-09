//! Revision-indexed keyspace — the MVCC layer over a storage backend.
//!
//! [`KvStore`] gives every write a monotonically increasing revision and keeps
//! superseded values readable until they are compacted away, which is what
//! makes historical reads and, later, watch replay possible.
//!
//! # Structure
//!
//! Two pieces cooperate. An in-memory index maps each key to its revision
//! history and answers "which revision of this key was current at revision X"
//! without any backend access. The backend holds one record per revision,
//! keyed so that byte order matches revision order. Only fetching a value
//! touches storage.
//!
//! The index is derived state, rebuilt by replaying the backend's records at
//! startup rather than persisted, so it cannot disagree with what is on disk.
//!
//! # Backend independence
//!
//! This crate depends on `storage-api` alone and never names a concrete
//! backend, so it can be used as a standalone library and tested against any
//! engine that satisfies the contract.
mod error;
mod index;
mod meta;
mod mutation;
mod record;
mod revision;
mod store;

#[doc(inline)]
pub use crate::error::Error;

#[doc(inline)]
pub use crate::index::Found;

#[doc(inline)]
pub use crate::mutation::Mutation;

#[doc(inline)]
pub use crate::revision::Revision;

#[doc(inline)]
pub use crate::store::{KvStore, Record};
