//! redb-backed implementation of Hadar's storage contract.
//!
//! This crate exists so that exactly one place in the workspace knows what
//! redb is. It implements [`storage_api::StorageEngine`] and exposes nothing
//! of redb through its own public signatures, which is what keeps a future
//! backend swap scoped to this crate plus the composition root.
//!
//! Keys live in a single flat keyspace; logical namespaces are expressed by
//! key prefix in the layer above rather than by separate tables, since a
//! table-per-namespace concept is not something every engine provides.
//!
//! # Durability
//!
//! redb fsyncs on commit by default, so this backend is more durable than the
//! contract requires. That is deliberate — the contract promises atomicity and
//! visibility ordering only, and the write-ahead log above it will take over
//! durability. Do not let callers come to depend on the stronger behavior.
mod engine;
mod error;

#[doc(inline)]
pub use crate::engine::{RedbEngine, RedbRead, RedbWrite};
