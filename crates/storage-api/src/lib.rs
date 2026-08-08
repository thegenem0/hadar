//! Backend-agnostic storage engine contract for Hadar's MVCC layer.
//!
//! Defines the transaction model every storage backend must satisfy.

mod bounds;
mod engine;
mod error;

#[cfg(feature = "test-util")]
pub mod conform;

#[cfg(feature = "test-util")]
mod mem;

#[doc(inline)]
pub use crate::bounds::Bounds;

#[doc(inline)]
pub use crate::engine::{Pair, ReadTxn, StorageEngine, WriteTxn};

#[doc(inline)]
pub use crate::error::Error;

#[cfg(feature = "test-util")]
#[doc(inline)]
pub use crate::mem::{MemEngine, MemRead, MemWrite};
