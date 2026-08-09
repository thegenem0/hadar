//! etcd-compatible gRPC surface over the MVCC store.
//!
//! This crate owns the wire boundary: it generates etcd's message and service
//! definitions from the vendored `.proto` files, and translates between them
//! and `mvcc`'s types. Nothing above `mvcc` is aware of protobuf, and nothing
//! here reimplements store semantics.
//!
//! # Scope
//!
//! Phase 0 serves the `KV` service only, and within it `Range`, `Put`,
//! `DeleteRange`, and `Compact`. Requests naming a feature that is not built
//! yet — leases, `prev_kv`, `Txn`, sorting by anything but the key — are
//! answered with `Unimplemented` rather than served approximately, so a client
//! is never told a filter applied when it did not.
//!
//! # Compatibility
//!
//! Status codes and messages are reproduced verbatim from upstream, because
//! etcd's own client maps them back to typed errors by exact description.

/// etcd wire types, generated from the vendored `.proto` definitions.
pub mod pb {
    #![allow(
        missing_docs,
        unused_qualifications,
        clippy::default_trait_access,
        clippy::doc_markdown,
        clippy::must_use_candidate,
        clippy::too_many_lines,
        reason = "prost/tonic generated code"
    )]

    include!(concat!(env!("OUT_DIR"), "/mod.rs"));
}
