use crate::revision::{ENCODED_LEN, Revision};

/// Distinguishes the revision records from any other keyspace `mvcc` owns.
const REVISION_PREFIX: u8 = b't';

/// Marks a backend key as a deletion.
const TOMBSTONE: u8 = 0x01;

/// Builds the backend key holding the record written at `revision`.
pub(crate) fn backend_key(revision: Revision, tombstone: bool) -> Vec<u8> {
    let mut key = Vec::with_capacity(ENCODED_LEN + 2);
    key.push(REVISION_PREFIX);
    key.extend_from_slice(&revision.encode());
    if tombstone {
        key.push(TOMBSTONE);
    }

    key
}

/// Bounds covering every revision record.
pub(crate) fn all_records() -> storage_api::Bounds {
    storage_api::Bounds::prefix([REVISION_PREFIX])
}

/// Splits a backend key into its revision and whether it marks a deletion.
///
/// Returns `None` for anything this module did not write, so a foreign or
/// corrupt key is rejected rather than misread as a revision.
pub(crate) fn parse_key(key: &[u8]) -> Option<(Revision, bool)> {
    let (&prefix, rest) = key.split_first()?;
    if prefix != REVISION_PREFIX {
        return None;
    }

    let (encoded, tombstone) = match rest.len() {
        ENCODED_LEN => (rest, false),
        len if len == ENCODED_LEN + 1 && rest[ENCODED_LEN] == TOMBSTONE => {
            (&rest[..ENCODED_LEN], true)
        }
        _ => return None,
    };

    Some((Revision::decode(encoded)?, tombstone))
}

/// Encodes the user key and value stored at a revision.
pub(crate) fn encode(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + key.len() + value.len());
    let key_len = u32::try_from(key.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&key_len.to_be_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(value);

    out
}

/// Splits a stored record back into its key and value.
///
/// Returns `None` if the record is truncated or its
/// length prefix overruns the buffer.
pub(crate) fn decode(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let (len, rest) = bytes.split_at_checked(4)?;
    let key_len = u32::from_be_bytes(len.try_into().ok()?) as usize;

    rest.split_at_checked(key_len)
}
