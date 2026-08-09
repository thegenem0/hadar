/// A point in the store's history.
///
/// A revision is a pair.
/// `main` advances once per write transaction and is what clients see as `mod_revision`
/// `sub` distinguishes the individual keys written *within* one transaction,
/// which share a `main` but must occupy distinct backend keys.
///
/// Revisions order by `main` then `sub`, which is the order writes were
/// applied, and their byte encoding preserves that order exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision {
    main: u64,
    sub: u64,
}

/// Width of an encoded revision.
/// Two big-endian `u64`s and a `_` separator.
pub(crate) const ENCODED_LEN: usize = 17;
const SEPARATOR: u8 = b'_';

impl Revision {
    /// Creates a revision, rejecting values the wire format cannot carry.
    ///
    /// A `main` above [`i64::MAX`] could not round-trip to a client
    /// and is refused here rather than silently truncating at the API boundary.
    #[must_use]
    pub fn new(main: u64, sub: u64) -> Option<Self> {
        if main > i64::MAX.cast_unsigned() || sub > i64::MAX.cast_unsigned() {
            return None;
        }

        Some(Self { main, sub })
    }

    /// The revision of an empty store, before any write has happened.
    ///
    /// No record is ever written at this revision.
    /// It exists so that the first write lands at main 1.
    #[must_use]
    pub fn zero() -> Self {
        Self { main: 0, sub: 0 }
    }

    /// Returns the transaction revision, which clients see as `mod_revision`.
    #[must_use]
    pub fn main(self) -> u64 {
        self.main
    }

    /// Returns the position of this write within its transaction.
    #[must_use]
    pub fn sub(self) -> u64 {
        self.sub
    }

    /// Returns the first revision of the transaction after this one.
    ///
    /// # Panics
    ///
    /// Panics if `main` has reached [`i64::MAX`], which indicates
    /// a corrupt counter rather than genuine exhaustion.
    #[must_use]
    pub fn next_main(self) -> Self {
        Self::new(self.main + 1, 0).unwrap_or_else(|| {
            panic!(
                "INVARIANT: revision counter exhausted at main={}, sub={}",
                self.main, self.sub
            )
        })
    }

    /// Returns the next revision within the same transaction.
    ///
    /// # Panics
    ///
    /// Panics if `sub` has reached [`i64::MAX`], meaning a single
    /// transaction wrote more keys than the wire format can describe.
    #[must_use]
    pub fn next_sub(self) -> Self {
        Self::new(self.main, self.sub + 1).unwrap_or_else(|| {
            panic!(
                "INVARIANT: transaction at main={} exceeded the addressable sub range",
                self.main
            )
        })
    }

    /// Encodes the revision such that byte order matches revision order.
    #[must_use]
    pub(crate) fn encode(self) -> [u8; ENCODED_LEN] {
        let mut out = [0_u8; ENCODED_LEN];
        out[..8].copy_from_slice(&self.main.to_be_bytes());
        out[8] = SEPARATOR;
        out[9..].copy_from_slice(&self.sub.to_be_bytes());

        out
    }

    /// Decodes a revision previously written by [`encode`](Self::encode).
    ///
    /// Returns `None` for any input that this encoding could not have produced,
    /// so a corrupt or foreign backend key is rejected rather than silently misread.
    #[must_use]
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != ENCODED_LEN || bytes[8] != SEPARATOR {
            return None;
        }

        let main = u64::from_be_bytes(bytes[..8].try_into().ok()?);
        let sub = u64::from_be_bytes(bytes[9..].try_into().ok()?);

        Self::new(main, sub)
    }

    /// Returns the transaction revision in the wire's signed representation.
    ///
    /// Total by construction: [`new`](Self::new) refuses anything above
    /// [`i64::MAX`], which is what makes this conversion lossless.
    #[must_use]
    #[expect(
        clippy::cast_possible_wrap,
        reason = "Revision::new rejects any value above i64::MAX"
    )]
    pub fn as_wire(self) -> i64 {
        self.main as i64
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test assertions surface failures by panicking"
)]
mod tests {
    use super::{ENCODED_LEN, Revision};

    fn rev(main: u64, sub: u64) -> Revision {
        Revision::new(main, sub).unwrap()
    }

    #[test]
    fn rejects_revisions_the_wire_cannot_carry() {
        assert!(Revision::new(i64::MAX.cast_unsigned(), 0).is_some());
        assert!(Revision::new(i64::MAX.cast_unsigned() + 1, 0).is_none());
        assert!(Revision::new(0, i64::MAX.cast_unsigned() + 1).is_none());
    }

    #[test]
    fn orders_by_main_then_sub() {
        assert!(rev(1, 0) < rev(1, 1));
        assert!(rev(1, 9) < rev(2, 0));
    }

    #[test]
    fn byte_order_matches_revision_order() {
        let ordered = [rev(0, 0), rev(0, 1), rev(1, 0), rev(2, 5), rev(300, 0)];
        for pair in ordered.windows(2) {
            assert!(
                pair[0].encode() < pair[1].encode(),
                "encoding reordered {:?} against {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn encoding_round_trips() {
        for revision in [rev(0, 0), rev(1, 2), rev(i64::MAX.cast_unsigned(), 7)] {
            assert_eq!(Revision::decode(&revision.encode()), Some(revision));
        }
    }

    #[test]
    fn rejects_malformed_encodings() {
        assert_eq!(Revision::decode(&[]), None);
        assert_eq!(Revision::decode(&[0; ENCODED_LEN - 1]), None);
        assert_eq!(Revision::decode(&[0; ENCODED_LEN + 1]), None);

        let mut wrong_separator = rev(1, 1).encode();
        wrong_separator[8] = b'-';
        assert_eq!(Revision::decode(&wrong_separator), None);
    }

    #[test]
    fn advancing_moves_forward() {
        assert_eq!(rev(4, 9).next_main(), rev(5, 0));
        assert_eq!(rev(4, 9).next_sub(), rev(4, 10));
    }
}
