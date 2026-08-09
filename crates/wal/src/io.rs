//! Where segments reach storage.
//!
//! Production has one implementation.
//! The faulty one exists so tests can reach failures a
//! real filesystem will not produce on demand.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
};

use crate::Error;

/// How segment files are opened, and how they behave once open.
#[derive(Debug, Clone)]
pub(crate) enum Media {
    Real,
    #[cfg(feature = "test-util")]
    Faulty(std::sync::Arc<Faults>),
}

impl Media {
    /// Opens `path`, failing if it already exists.
    pub(crate) fn create(&self, path: &Path) -> Result<Sink, Error> {
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(path)
            .map_err(|e| Error::io("creating a segment", e))?;

        Ok(self.wrap(file))
    }

    /// Opens `path` for appending, requiring it to exist.
    pub(crate) fn append(&self, path: &Path) -> Result<Sink, Error> {
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| Error::io("reopening a segment", e))?;

        Ok(self.wrap(file))
    }

    fn wrap(&self, file: File) -> Sink {
        match self {
            Self::Real => Sink::Real(file),
            #[cfg(feature = "test-util")]
            Self::Faulty(faults) => Sink::Faulty(file, std::sync::Arc::clone(faults)),
        }
    }
}

/// An open segment file.
#[derive(Debug)]
pub(crate) enum Sink {
    Real(File),
    #[cfg(feature = "test-util")]
    Faulty(File, std::sync::Arc<Faults>),
}

impl Sink {
    /// Appends `bytes` in full.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    /// It may have written part of `bytes` first, which the caller is responsible for rewinding.
    pub(crate) fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        match self {
            Self::Real(file) => write_all(file, bytes),
            #[cfg(feature = "test-util")]
            Self::Faulty(file, faults) => faults.write_all(file, bytes),
        }
    }

    /// Flushes appended bytes to durable storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the flush fails, leaving the bytes written but not durable.
    pub(crate) fn sync_data(&mut self) -> Result<(), Error> {
        match self {
            Self::Real(file) => sync_data(file),
            #[cfg(feature = "test-util")]
            Self::Faulty(file, faults) => faults.sync_data(file),
        }
    }

    /// Discards everything past `len` and flushes the change.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be truncated or flushed.
    pub(crate) fn truncate(&mut self, len: u64) -> Result<(), Error> {
        match self {
            Self::Real(file) => truncate(file, len),
            #[cfg(feature = "test-util")]
            Self::Faulty(file, faults) => faults.truncate(file, len),
        }
    }
}

fn write_all(file: &mut File, bytes: &[u8]) -> Result<(), Error> {
    file.write_all(bytes)
        .map_err(|e| Error::io("appending to a segment", e))
}

fn sync_data(file: &File) -> Result<(), Error> {
    file.sync_data()
        .map_err(|e| Error::io("flushing a segment to disk", e))
}

fn truncate(file: &File, len: u64) -> Result<(), Error> {
    file.set_len(len)
        .map_err(|e| Error::io("truncating a partial record", e))?;

    file.sync_all()
        .map_err(|e| Error::io("flushing a truncated segment", e))
}

/// Failures to inject into a log's I/O.
///
/// A real filesystem will not refuse an fsync or stop a write halfway on request,
/// so the paths that handle those failures would otherwise be unreachable from a test.
/// Each setting arms a single upcoming operation and is consumed by it.
#[cfg(feature = "test-util")]
#[derive(Debug)]
pub struct Faults {
    fail_syncs: std::sync::atomic::AtomicUsize,
    short_write: std::sync::atomic::AtomicUsize,
    fail_truncates: std::sync::atomic::AtomicUsize,
    syncs: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "test-util")]
impl Faults {
    /// Creates a handle with nothing armed.
    ///
    /// Shared, as the log holds one and the test keeps another
    /// to arm it while the log is running.
    #[must_use]
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            fail_syncs: std::sync::atomic::AtomicUsize::new(0),
            // `usize::MAX` means unarmed, so a zero-byte write is expressible.
            short_write: std::sync::atomic::AtomicUsize::new(usize::MAX),
            fail_truncates: std::sync::atomic::AtomicUsize::new(0),
            syncs: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Makes the next `count` flushes fail.
    pub fn fail_next_syncs(&self, count: usize) {
        self.fail_syncs
            .store(count, std::sync::atomic::Ordering::Relaxed);
    }

    /// Makes the next `count` rewinds fail.
    ///
    /// A failed rewind is what leaves torn bytes in a segment that cannot be
    /// removed, which is the only way to reach the poisoned path.
    pub fn fail_next_truncates(&self, count: usize) {
        self.fail_truncates
            .store(count, std::sync::atomic::Ordering::Relaxed);
    }

    /// Makes the next write stop after `bytes` and fail.
    ///
    /// The bytes it did write are flushed, so what a reader would find
    /// after a crash at that instant is what the test sees.
    pub fn shorten_next_write(&self, bytes: usize) {
        self.short_write
            .store(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns how many flushes have succeeded.
    #[must_use]
    pub fn syncs(&self) -> usize {
        self.syncs.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn write_all(&self, file: &mut File, bytes: &[u8]) -> Result<(), Error> {
        use std::sync::atomic::Ordering;

        let limit = self.short_write.swap(usize::MAX, Ordering::Relaxed);
        if limit == usize::MAX {
            return write_all(file, bytes);
        }

        let cut = limit.min(bytes.len());
        write_all(file, &bytes[..cut])?;
        drop(sync_data(file));

        Err(Error::io(
            "appending to a segment",
            std::io::Error::from(std::io::ErrorKind::WriteZero),
        ))
    }

    fn sync_data(&self, file: &File) -> Result<(), Error> {
        use std::sync::atomic::Ordering;

        let armed = self
            .fail_syncs
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok();
        if armed {
            return Err(Error::io(
                "flushing a segment to disk",
                std::io::Error::other("injected flush failure"),
            ));
        }

        sync_data(file)?;
        self.syncs.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn truncate(&self, file: &File, len: u64) -> Result<(), Error> {
        use std::sync::atomic::Ordering;

        let armed = self
            .fail_truncates
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok();
        if armed {
            return Err(Error::io(
                "truncating a partial record",
                std::io::Error::other("injected rewind failure"),
            ));
        }

        truncate(file, len)
    }
}
