use std::sync::Arc;

use anyhow::{Context, Result};
use mvcc::KvStore;
use storage_api::StorageEngine;
use wal::Wal;

use crate::config::Config;

/// Opens the store and log, then serves until asked to stop.
///
/// # Errors
///
/// Returns an error if the data directory cannot be prepared, the store or log
/// cannot be opened, or recovery finds a damaged log.
#[tracing::instrument(skip(open))]
pub fn run<E, F>(config: &Config, open: F) -> Result<()>
where
    E: StorageEngine,
    F: FnOnce(&std::path::Path) -> Result<E>,
{
    let paths = config.prepare()?;

    let engine = open(&paths.store).context("opening the storage backend")?;
    let store = Arc::new(KvStore::open(engine).context("opening the key-value store")?);

    let mut log = Wal::open(&paths.log, Arc::clone(&store)).context("opening the wal")?;

    tracing::info!(
        data_dir = %config.data_dir.display(),
        revision = store.current_revision().main(),
        "hadar started"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    runtime.block_on(shutdown_signal())?;

    tracing::info!("shutting down");
    log.shutdown();
    tracing::info!(revision = store.current_revision().main(), "hadar stopped");
    Ok(())
}

async fn shutdown_signal() -> Result<()> {
    tokio::signal::ctrl_c()
        .await
        .context("waiting for a shutdown signal")
}
