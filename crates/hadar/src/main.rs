//! Entry point: reads configuration, selects a backend, and runs the server.
use anyhow::{Context, Result};
use hadar::Config;
use storage_redb::RedbEngine;

type Engine = RedbEngine;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_args()?;
    hadar::run::<Engine, _>(&config, |path| {
        Engine::open(path).context("opening the redb database")
    })
}
