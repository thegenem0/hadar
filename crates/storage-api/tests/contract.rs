//! Runs the storage contract against the in-memory reference engine.

storage_api::conformance_suite!(storage_api::MemEngine::new);
