use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::error::Error;

/// Extension marking a file as a log segment.
const EXTENSION: &str = "wal";

/// Size at which the active segment is closed and a new one started.
///
/// Compaction discards whole segments, so this sets the granularity of reclamation.
/// No space is freed until every record in a segment is below the snapshot point.
pub(crate) const SEGMENT_LIMIT: u64 = 64 * 1024 * 1024;

/// The append target for new records.
#[derive(Debug)]
pub(crate) struct Segment {
    file: File,
    /// Bytes written to this segment so far.
    written: u64,
    /// Size at which this segment is considered full.
    limit: u64,
}

impl Segment {
    /// Creates a segment whose first record will be numbered `first`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be created.
    pub(crate) fn create(dir: &Path, first: u64, limit: u64) -> Result<Self, Error> {
        let path = dir.join(format!("{first:020}.{EXTENSION}"));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::io("creating a segment", e))?;

        Ok(Self {
            file,
            written: 0,
            limit,
        })
    }

    /// Reopens an existing segment for appending, discarding anything past `valid`.
    ///
    /// A crash can leave a partial record at the end of the last segment.
    /// Appending after it would bury an unreadable record in the middle of the
    /// file, and replay stops at the first one it cannot read.
    /// Truncating first keeps the log readable to its end.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or truncated.
    pub(crate) fn reopen(path: &Path, valid: u64, limit: u64) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| Error::io("reopening a segment", e))?;

        file.set_len(valid)
            .map_err(|e| Error::io("truncating a partial record", e))?;

        file.sync_all()
            .map_err(|e| Error::io("flushing a truncated segment", e))?;

        Ok(Self {
            file,
            written: valid,
            limit,
        })
    }

    /// Appends `bytes` and flushes them to durable storage.
    ///
    /// Returning only after the flush is what lets a caller acknowledge a write.
    /// Until this returns, the records may exist only in a buffer that a crash discards.
    ///
    /// # Errors
    ///
    /// Returns an error if the append or the flush fails. Leaves the
    /// records unacknowledged, a partial tail is discarded on replay.
    pub(crate) fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.file
            .write_all(bytes)
            .map_err(|e| Error::io("appending to a segment", e))?;

        self.file
            .sync_data()
            .map_err(|e| Error::io("flushing a segment to disk", e))?;

        self.written += bytes.len() as u64;
        Ok(())
    }

    /// Returns whether this segment has reached the size at which to roll over.
    pub(crate) fn is_full(&self) -> bool {
        self.written >= self.limit
    }

    /// Returns the size at which this segment rolls over.
    pub(crate) fn limit(&self) -> u64 {
        self.limit
    }
}

/// Returns the segment files in `dir`, ordered by their first record.
///
/// Names are fixed-width and zero-padded, so lexical order is numeric order and
/// the directory listing needs no parsing to be sorted.
///
/// # Errors
///
/// Returns an error if the directory cannot be read.
pub(crate) fn list(dir: &Path) -> Result<Vec<(u64, PathBuf)>, Error> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::io("listing segments", e))?;

    let mut segments = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| Error::io("listing segments", e))?.path();
        if path.extension().is_none_or(|ext| ext != EXTENSION) {
            continue;
        }
        let Some(first) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u64>().ok())
        else {
            return Err(Error::corrupt("segment file name is not a record index"));
        };

        segments.push((first, path));
    }

    segments.sort_unstable_by_key(|(first, _)| *first);
    Ok(segments)
}

/// Reads a segment's bytes in full.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub(crate) fn read(path: &Path) -> Result<Vec<u8>, Error> {
    let mut file = File::open(path).map_err(|e| Error::io("opening a segment", e))?;
    let mut bytes = Vec::new();

    file.read_to_end(&mut bytes)
        .map_err(|e| Error::io("reading a segment", e))?;

    Ok(bytes)
}

/// Deletes `path`.
///
/// # Errors
///
/// Returns an error if the file cannot be removed.
pub(crate) fn remove(path: &Path) -> Result<(), Error> {
    std::fs::remove_file(path).map_err(|e| Error::io("removing a compacted segment", e))
}
