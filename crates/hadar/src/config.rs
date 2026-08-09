use std::path::PathBuf;

use anyhow::{Context, bail};

/// Where the server keeps its state and how it was asked to run.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding the store and the write-ahead log.
    pub data_dir: PathBuf,
}

/// Directory used when none is given.
const DEFAULT_DATA_DIR: &str = "hadar-data";

impl Config {
    /// Reads configuration from command-line arguments.
    ///
    /// # Errors
    ///
    /// Returns an error if more than one argument is given,
    /// or if `--help` was asked for.
    pub fn from_args() -> anyhow::Result<Self> {
        let mut args = std::env::args_os().skip(1);

        let data_dir = match args.next() {
            Some(arg) if arg == "-h" || arg == "--help" => bail!(Self::usage()),
            Some(arg) => PathBuf::from(arg),
            None => PathBuf::from(DEFAULT_DATA_DIR),
        };
        if args.next().is_some() {
            bail!(Self::usage())
        }

        Ok(Self { data_dir })
    }

    /// Returns the paths this configuration implies,
    /// creating the data directory if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn prepare(&self) -> anyhow::Result<Paths> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating data directory {}", self.data_dir.display()))?;

        Ok(Paths {
            store: self.data_dir.join("store.redb"),
            log: self.data_dir.join("wal"),
        })
    }

    fn usage() -> String {
        format!("usage: hadar [DATA_DIR]    (default: {DEFAULT_DATA_DIR})")
    }
}

/// Locations within the data directory.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The storage backend's database file.
    pub store: PathBuf,
    /// The directory holding write-ahead log segments.
    pub log: PathBuf,
}
