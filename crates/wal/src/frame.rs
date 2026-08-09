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
