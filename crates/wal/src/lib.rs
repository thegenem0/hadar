//! Write-ahead log provides durability and crash recovery for the MVCC store.
//!
//! # Why this exists
//!
//! The storage contract promises atomicity and visibility ordering, not
//! durability — see `storage_api::WriteTxn::commit`. This crate supplies the
//! missing guarantee: a mutation is appended and fsynced here before it is
//! acknowledged, so an acknowledged write survives a crash and an
//! unacknowledged one is never visible after one.
//!
//! It also makes durability *cheap*. Committing each write to the store
//! separately costs one fsync apiece; batching appends into a single fsync
//! amortizes that across every writer in the batch, which is where this layer
//! earns its keep.
//!
//! # Scope
//!
//! This is a plain durability log for the MVCC store, deliberately not shaped
//! like a Raft log. Whether the two merge is an open decision for Phase 3, and
//! anticipating it here would bake in a choice that has not been made.
//!
//! # On-disk format
//!
//! Every record is framed as a CRC32 checksum, a big-endian length, and a
//! payload. The checksum covers the length as well as the payload, so a
//! corrupted length cannot silently reframe everything after it.
//!
//! Reading distinguishes two failures, and the distinction is what makes
//! recovery correct. A frame that ends early is what a kill mid-append
//! leaves behind: the write was never acknowledged, so recovery stops there
//! and treats it as the end of the log. A frame that fails its checksum is
//! damage, and recovery refuses to continue past it.

mod error;
mod frame;
mod record;
mod segment;
mod wal;
mod writer;

#[doc(inline)]
pub use crate::error::Error;

#[doc(inline)]
pub use crate::wal::{Options, Wal};
