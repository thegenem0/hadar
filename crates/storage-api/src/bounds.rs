use std::ops::{Bound, RangeBounds};

/// A key range to scan, expressed over owned key bytes.
///
/// Ranges are half-open by construction (start inclusive, end exclusive)
/// matching both the ordering of `[u8]` and `(key, range_end)` wire semantics.
/// A range whose start is not less than its end is empty rather than invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bounds {
    start: Bound<Vec<u8>>,
    end: Bound<Vec<u8>>,
}

impl Bounds {
    /// Covers every key in the store.
    #[must_use]
    pub fn all() -> Self {
        Self {
            start: Bound::Unbounded,
            end: Bound::Unbounded,
        }
    }

    /// Covers exactly `key` if present.
    #[must_use]
    pub fn point(key: impl Into<Vec<u8>>) -> Self {
        let key = key.into();
        Self {
            start: Bound::Included(key.clone()),
            end: Bound::Included(key),
        }
    }

    /// Covers `start` inclusive through `end` exclusive.
    ///
    /// An inverted or equal pair yields an empty range, not an error.
    #[must_use]
    pub fn between(start: impl Into<Vec<u8>>, end: impl Into<Vec<u8>>) -> Self {
        Self {
            start: Bound::Included(start.into()),
            end: Bound::Excluded(end.into()),
        }
    }

    /// Covers ever key at or after `start`.
    #[must_use]
    pub fn start_at(start: impl Into<Vec<u8>>) -> Self {
        Self {
            start: Bound::Included(start.into()),
            end: Bound::Unbounded,
        }
    }

    /// Covers every key before `end`.
    #[must_use]
    pub fn end_before(end: impl Into<Vec<u8>>) -> Self {
        Self {
            start: Bound::Unbounded,
            end: Bound::Excluded(end.into()),
        }
    }

    /// Covers every key beginning with `prefix`.
    ///
    /// The exclusive end is the prefix's byte-wise successor. The last byte
    /// below `0xff` is incremented and any trailing `0xff` bytes are dropped. A
    /// prefix that is empty or entirely `0xff` has no successor, so the range
    /// runs to the end of the keyspace.
    #[must_use]
    pub fn prefix(prefix: impl Into<Vec<u8>>) -> Self {
        let prefix = prefix.into();
        let mut end = prefix.clone();

        while let Some(last) = end.pop() {
            if last < u8::MAX {
                end.push(last + 1);
                return Self {
                    start: Bound::Included(prefix),
                    end: Bound::Excluded(end),
                };
            }
        }

        Self {
            start: Bound::Included(prefix),
            end: Bound::Unbounded,
        }
    }

    /// Moves the start bound past `key`, leaving the end bound alone.
    ///
    /// This is the paging step for a chunked scan: range with a limit, then
    /// advance past the last key returned.
    /// Long scans are expected to be written this way so they can yield
    /// between chunks instead of materializing the whole keyspace.
    pub fn advance_past(&mut self, key: &[u8]) {
        self.start = Bound::Excluded(key.to_vec());
    }
}

impl RangeBounds<[u8]> for Bounds {
    fn start_bound(&self) -> Bound<&[u8]> {
        self.start.as_ref().map(Vec::as_slice)
    }

    fn end_bound(&self) -> Bound<&[u8]> {
        self.end.as_ref().map(Vec::as_slice)
    }
}
