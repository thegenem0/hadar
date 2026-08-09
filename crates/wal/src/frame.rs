use crate::error::Error;

/// Bytes of framing that precede every payload.
pub(crate) const HEADER_LEN: usize = 8;

/// Largest payload a single frame may carry.
///
/// 64 MiB is far above any plausible single mutation,
/// and below a size that would threaten a server's memory.
pub(crate) const MAX_PAYLOAD_LEN: usize = 64 * 1024 * 1024;

/// Appends `payload` to `out` as a framed record.
///
/// # Errors
///
/// Returns an error if the payload exceeds [`MAX_PAYLOAD_LEN`].
pub(crate) fn encode(payload: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    let Ok(len) = u32::try_from(payload.len()) else {
        return Err(Error::corrupt("payload lenght exceeds the frame format"));
    };
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(Error::corrupt("payload exceeds the maximum frame lenght"));
    }

    let len = len.to_be_bytes();
    out.extend_from_slice(&checksum(len, payload).to_be_bytes());
    out.extend_from_slice(&len);
    out.extend_from_slice(payload);

    Ok(())
}

/// Reads the frame at the start of `bytes`, returning its payload and width.
///
/// The width is what a caller advances by to reach the next frame.
///
/// # Errors
///
/// Returns a truncated error if `bytes` ends before the frame does, and a
/// corrupt error if the checksum disagrees or the length is implausible.
pub(crate) fn decode(bytes: &[u8]) -> Result<(&[u8], usize), Error> {
    let Some((header, rest)) = bytes.split_at_checked(HEADER_LEN) else {
        return Err(Error::truncated());
    };

    let (expected, len) = header.split_at(4);
    let Ok(len): Result<[u8; 4], _> = len.try_into() else {
        return Err(Error::corrupt("frame header is malformed"));
    };
    let payload_len = u32::from_be_bytes(len) as usize;

    if payload_len > MAX_PAYLOAD_LEN {
        return Err(Error::corrupt("frame claims an implausible length"));
    }

    let Some(payload) = rest.get(..payload_len) else {
        return Err(Error::truncated());
    };

    if checksum(len, payload).to_be_bytes() != expected {
        return Err(Error::corrupt("frame checksum does not match its contents"));
    }

    Ok((payload, HEADER_LEN + payload_len))
}

fn checksum(len: [u8; 4], payload: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&len);
    hasher.update(payload);
    hasher.finalize()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "test assertions surface failures by panicking"
)]
mod tests {
    use proptest::prelude::*;

    use super::{HEADER_LEN, decode, encode};

    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        encode(payload, &mut out).expect("payload is within the frame limit");
        out
    }

    #[test]
    fn a_frame_round_trips() {
        let bytes = framed(b"payload");
        let (payload, width) = decode(&bytes).expect("frame decodes");

        assert_eq!(payload, b"payload");
        assert_eq!(width, bytes.len());
    }

    #[test]
    fn an_empty_payload_is_a_valid_frame() {
        let bytes = framed(b"");
        let (payload, width) = decode(&bytes).expect("frame decodes");

        assert!(payload.is_empty());
        assert_eq!(width, HEADER_LEN);
    }

    #[test]
    fn frames_decode_back_to_back() {
        let mut log = framed(b"first");
        log.extend_from_slice(&framed(b"second"));

        let (first, width) = decode(&log).expect("first frame decodes");
        assert_eq!(first, b"first");
        let (second, _) = decode(&log[width..]).expect("second frame decodes");
        assert_eq!(second, b"second");
    }

    #[test]
    fn a_frame_cut_short_reads_as_truncated() {
        let bytes = framed(b"payload");

        // Every prefix is a write that was interrupted partway, which is what
        // a kill mid-append leaves behind.
        for cut in 0..bytes.len() {
            let error = decode(&bytes[..cut]).expect_err("a partial frame cannot decode");
            assert!(
                error.is_truncated(),
                "a {cut}-byte prefix reported {error} rather than truncation"
            );
        }
    }

    #[test]
    fn a_corrupted_payload_fails_its_checksum() {
        let mut bytes = framed(b"payload");
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;

        let error = decode(&bytes).expect_err("a damaged payload cannot decode");
        assert!(
            error.is_corrupt(),
            "reported {error} rather than corruption"
        );
    }

    #[test]
    fn a_corrupted_length_is_caught_by_the_checksum() {
        let mut bytes = framed(b"payload");
        // Shrink the claimed length so the frame still fits in the buffer: the
        // read would otherwise succeed and reframe everything after it.
        bytes[7] -= 1;

        let error = decode(&bytes).expect_err("a damaged length cannot decode");
        assert!(
            error.is_corrupt(),
            "reported {error} rather than corruption"
        );
    }

    #[test]
    fn an_implausible_length_is_rejected_before_allocating() {
        let mut bytes = framed(b"payload");
        bytes[4..8].copy_from_slice(&u32::MAX.to_be_bytes());

        let error = decode(&bytes).expect_err("an implausible length cannot decode");
        assert!(
            error.is_corrupt(),
            "reported {error} rather than corruption"
        );
    }

    proptest! {
        #[test]
        fn any_payload_round_trips(payload in prop::collection::vec(any::<u8>(), 0..2048)) {
            let bytes = framed(&payload);
            let (decoded, width) = decode(&bytes).expect("frame decodes");

            prop_assert_eq!(decoded, payload.as_slice());
            prop_assert_eq!(width, bytes.len());
        }

        #[test]
        fn any_truncation_is_reported_as_truncation(
            payload in prop::collection::vec(any::<u8>(), 0..512),
            cut in 0_usize..512,
        ) {
            let bytes = framed(&payload);
            let cut = cut % bytes.len().max(1);

            let error = decode(&bytes[..cut]).expect_err("a partial frame cannot decode");
            prop_assert!(error.is_truncated(), "reported {} rather than truncation", error);
        }

        #[test]
        fn any_single_bit_flip_is_caught(
            payload in prop::collection::vec(any::<u8>(), 1..512),
            index in any::<prop::sample::Index>(),
            bit in 0_u32..8,
        ) {
            let mut bytes = framed(&payload);
            let position = index.index(bytes.len());
            bytes[position] ^= 1 << bit;

            // A flip anywhere in the frame must be caught.
            let error = decode(&bytes).expect_err("a damaged frame cannot decode");
            prop_assert!(
                error.is_corrupt() || error.is_truncated(),
                "reported {}", error
            );
        }
    }
}
