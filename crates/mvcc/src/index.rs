use std::collections::BTreeMap;

use storage_api::Bounds;

use crate::revision::Revision;

/// What a read at some revision found for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    /// Revision of the write this read observes.
    pub modified: Revision,
    /// Revision at which the key's current iteration was created.
    pub created: Revision,
    /// Number of writes to the key since it was created, starting at one.
    pub version: u64,
}

/// In-memory map from key to revision history.
///
/// Rebuilt from the backend at startup rather than persisted.
#[derive(Debug, Default)]
pub(crate) struct Index {
    keys: BTreeMap<Vec<u8>, History>,
}

/// One key's revision history, split into generations.
///
/// A generation spans a key's life from creation to deletion.
/// Deleting and recreating a key starts a new generation, keeping
/// `create_revision` and `version` correct across the cycle.
#[derive(Debug)]
struct History {
    generations: Vec<Generation>,
}

#[derive(Debug)]
struct Generation {
    /// Revision the key was created at.
    /// Survives compaction even when the creating revision is dropped,
    /// as clients still observe it.
    created: Revision,
    /// Total writes in this generation, including any dropped by compaction.
    /// Retained so `version` stays correct after revisions are trimmed.
    ver: u64,
    revs: Vec<Revision>,
    /// Whether this generation ends in a deletion.
    closed: bool,
}

impl Index {
    /// Records a write of `key` at `revision`.
    pub(crate) fn put(&mut self, key: &[u8], revision: Revision) {
        let history = self.keys.entry(key.to_vec()).or_insert_with(|| History {
            generations: Vec::new(),
        });

        match history.generations.last_mut() {
            Some(generation) if !generation.closed => {
                generation.revs.push(revision);
                generation.ver += 1;
            }
            _ => history.generations.push(Generation {
                created: revision,
                ver: 1,
                revs: vec![revision],
                closed: false,
            }),
        }
    }

    // Records a deletion of `key` at `revision`.
    ///
    /// Returns `false` if the key does not currently exist,
    /// in which case nothing is recorded.
    pub(crate) fn tombstone(&mut self, key: &[u8], revision: Revision) -> bool {
        let Some(history) = self.keys.get_mut(key) else {
            return false;
        };

        let Some(generation) = history.generations.last_mut() else {
            return false;
        };

        if generation.closed {
            return false;
        }

        generation.revs.push(revision);
        generation.ver += 1;
        generation.closed = true;

        true
    }

    /// Returns every key within `bounds` visible at revision `at`, ascending.
    pub(crate) fn range(&self, bounds: &Bounds, at: u64) -> Vec<(Vec<u8>, Found)> {
        self.keys
            .range::<[u8], _>((
                std::ops::RangeBounds::start_bound(bounds),
                std::ops::RangeBounds::end_bound(bounds),
            ))
            .filter_map(|(key, history)| history.at(at).map(|found| (key.clone(), found)))
            .collect()
    }

    /// Discards history that no read at or after `at` could observe, returning
    /// the revisions whose backend records are now unreachable.
    ///
    /// A read at `at` must still return exactly what it would have before, so
    /// the revision current at `at` is retained.
    ///
    /// A key whose state at `at` is "deleted" needs no record at all.
    /// Reads at or after `at` see it absent, and earlier reads are refused as
    /// compacted.
    pub(crate) fn compact(&mut self, at: u64) -> Vec<Revision> {
        let mut dropped = Vec::new();
        self.keys
            .retain(|_, history| history.compact(at, &mut dropped));

        dropped
    }

    /// Returns what a read of `key` at revision `at` observes.
    ///
    /// The store reaches for [`range`](Self::range) even for a single key, so
    /// this exists to let the index's own tests assert a per-key requirement.
    #[cfg(test)]
    pub(crate) fn get(&self, key: &[u8], at: u64) -> Option<Found> {
        self.keys.get(key)?.at(at)
    }

    /// Returns the number of keys currently tracked, live or tombstoned.
    #[cfg(test)]
    pub(crate) fn tracked_keys(&self) -> usize {
        self.keys.len()
    }
}

impl History {
    fn locate(&self, at: u64) -> Option<(usize, usize)> {
        for (index, generation) in self.generations.iter().enumerate().rev() {
            debug_assert!(
                !generation.revs.is_empty(),
                "generation {index} retained with no revisions"
            );

            let (Some(first), Some(last)) = (generation.revs.first(), generation.revs.last())
            else {
                continue;
            };

            // A closed generation whose deletion is at or before `at` means the
            // key was gone at `at`, and any earlier generation is irrelevant.
            if generation.closed && last.main() <= at {
                return None;
            }

            if first.main() <= at {
                let visible = generation.revs.partition_point(|rev| rev.main() <= at);
                return Some((index, visible - 1));
            }
        }
        None
    }

    fn at(&self, at: u64) -> Option<Found> {
        let (gen_index, rev_index) = self.locate(at)?;
        let generation = self.generations.get(gen_index)?;
        let modified = *generation.revs.get(rev_index)?;

        // Writes trimmed by an earlier compaction are still counted by `ver`,
        // so version is derived by counting back from the generation's end.
        let trailing = (generation.revs.len() - 1 - rev_index) as u64;

        Some(Found {
            modified,
            created: generation.created,
            version: generation.ver - trailing,
        })
    }

    fn compact(&mut self, at: u64, dropped: &mut Vec<Revision>) -> bool {
        let current = self.locate(at);

        for (gen_index, generation) in self.generations.iter_mut().enumerate() {
            let last_index = generation.revs.len() - 1;
            let mut kept = Vec::with_capacity(generation.revs.len());

            for (rev_index, rev) in generation.revs.iter().enumerate() {
                let is_current = current == Some((gen_index, rev_index));
                let is_tombstone = generation.closed && rev_index == last_index;

                // Everything after `at` stays visible to later reads.
                // The revision current at `at` stays too, unless it is a deletion,
                // which no surviving read needs to see.
                if rev.main() > at || (is_current && !is_tombstone) {
                    kept.push(*rev);
                } else {
                    dropped.push(*rev);
                }
            }

            generation.revs = kept;
        }

        self.generations
            .retain(|generation| !generation.revs.is_empty());

        !self.generations.is_empty()
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test assertions surface failures by panicking"
)]
mod tests {
    use super::Index;
    use crate::revision::Revision;

    fn rev(main: u64) -> Revision {
        Revision::new(main, 0).unwrap()
    }

    #[test]
    fn reports_creation_and_version_of_a_new_key() {
        let mut index = Index::default();
        index.put(b"k", rev(1));

        let found = index.get(b"k", 1).unwrap();
        assert_eq!(found.created, rev(1));
        assert_eq!(found.modified, rev(1));
        assert_eq!(found.version, 1);
    }

    #[test]
    fn version_counts_writes_while_creation_stays_fixed() {
        let mut index = Index::default();
        for main in 1..=3 {
            index.put(b"k", rev(main));
        }

        let found = index.get(b"k", 3).unwrap();
        assert_eq!(found.created, rev(1), "creation moved on overwrite");
        assert_eq!(found.version, 3);
        assert_eq!(found.modified, rev(3));
    }

    #[test]
    fn reads_observe_the_revision_current_at_that_point() {
        let mut index = Index::default();
        index.put(b"k", rev(1));
        index.put(b"k", rev(5));

        assert_eq!(index.get(b"k", 4).unwrap().modified, rev(1));
        assert_eq!(index.get(b"k", 5).unwrap().modified, rev(5));
    }

    #[test]
    fn a_key_is_invisible_before_it_was_created() {
        let mut index = Index::default();
        index.put(b"k", rev(7));

        assert!(index.get(b"k", 6).is_none());
        assert!(index.get(b"k", 7).is_some());
    }

    #[test]
    fn deletion_hides_the_key_from_that_revision_onward() {
        let mut index = Index::default();
        index.put(b"k", rev(1));
        assert!(index.tombstone(b"k", rev(2)));

        assert!(
            index.get(b"k", 1).is_some(),
            "history before the delete is lost"
        );
        assert!(index.get(b"k", 2).is_none());
        assert!(index.get(b"k", 3).is_none());
    }

    #[test]
    fn deleting_an_absent_or_already_deleted_key_is_not_an_event() {
        let mut index = Index::default();
        assert!(!index.tombstone(b"missing", rev(1)));

        index.put(b"k", rev(1));
        assert!(index.tombstone(b"k", rev(2)));
        assert!(!index.tombstone(b"k", rev(3)));
    }

    #[test]
    fn recreating_a_key_restarts_creation_and_version() {
        let mut index = Index::default();
        index.put(b"k", rev(1));
        index.put(b"k", rev(2));
        index.tombstone(b"k", rev(3));
        index.put(b"k", rev(4));

        let found = index.get(b"k", 4).unwrap();
        assert_eq!(found.created, rev(4), "recreate inherited the old creation");
        assert_eq!(found.version, 1, "recreate inherited the old version count");

        // The earlier incarnation is still readable at its own revisions.
        let earlier = index.get(b"k", 2).unwrap();
        assert_eq!(earlier.created, rev(1));
        assert_eq!(earlier.version, 2);
    }

    #[test]
    fn compaction_preserves_what_a_read_at_the_watermark_sees() {
        let mut index = Index::default();
        index.put(b"k", rev(1));
        index.put(b"k", rev(5));
        index.put(b"k", rev(9));

        let before = index.get(b"k", 5).unwrap();
        let dropped = index.compact(5);

        assert_eq!(index.get(b"k", 5).unwrap(), before);
        assert_eq!(
            dropped,
            vec![rev(1)],
            "compaction dropped a reachable revision"
        );
    }

    #[test]
    fn compaction_keeps_revisions_newer_than_the_watermark() {
        let mut index = Index::default();
        index.put(b"k", rev(1));
        index.put(b"k", rev(5));
        index.put(b"k", rev(9));

        index.compact(5);
        assert_eq!(index.get(b"k", 9).unwrap().modified, rev(9));
    }

    #[test]
    fn version_survives_compaction_trimming_its_history() {
        let mut index = Index::default();
        for main in 1..=4 {
            index.put(b"k", rev(main));
        }

        let before = index.get(b"k", 4).unwrap().version;
        index.compact(3);

        assert_eq!(
            index.get(b"k", 4).unwrap().version,
            before,
            "version was recomputed from the trimmed history"
        );
    }

    #[test]
    fn compaction_forgets_a_key_deleted_below_the_watermark() {
        let mut index = Index::default();
        index.put(b"k", rev(1));
        index.tombstone(b"k", rev(2));

        let dropped = index.compact(3);

        assert_eq!(
            index.tracked_keys(),
            0,
            "tombstoned key retained after compaction"
        );
        assert_eq!(
            dropped.len(),
            2,
            "both the value and its tombstone should go"
        );
    }

    #[test]
    fn compaction_retains_a_key_recreated_above_the_watermark() {
        let mut index = Index::default();
        index.put(b"k", rev(1));
        index.tombstone(b"k", rev(2));
        index.put(b"k", rev(8));

        index.compact(3);

        assert_eq!(index.tracked_keys(), 1);
        assert_eq!(index.get(b"k", 8).unwrap().created, rev(8));
    }

    #[test]
    fn range_returns_keys_in_order_within_bounds() {
        let mut index = Index::default();
        for key in [b"a", b"b", b"c"] {
            index.put(key, rev(1));
        }

        let found = index.range(
            &storage_api::Bounds::between(b"a".as_slice(), b"c".as_slice()),
            1,
        );
        let keys: Vec<_> = found.into_iter().map(|(key, _)| key).collect();
        assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec()]);
    }

    #[test]
    fn range_reflects_the_requested_revision() {
        let mut index = Index::default();
        index.put(b"a", rev(1));
        index.put(b"b", rev(5));

        assert_eq!(index.range(&storage_api::Bounds::all(), 1).len(), 1);
        assert_eq!(index.range(&storage_api::Bounds::all(), 5).len(), 2);
    }
}
