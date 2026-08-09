/// Distinguishes store metadata from the revision records.
const METADATA_PREFIX: u8 = 0x00;

/// Identifies the durable marker recording how far the log has been applied.
const APPLIED: u8 = 0x01;

/// The backend key holding the applied-position marker.
pub(crate) const APPLIED_KEY: [u8; 2] = [METADATA_PREFIX, APPLIED];

/// Encodes an applied position for storage.
pub(crate) fn encode_position(position: u64) -> [u8; 8] {
    position.to_be_bytes()
}

/// Decodes an applied position, rejecting anything
/// the encoder could not have produced.
pub(crate) fn decode_position(bytes: &[u8]) -> Option<u64> {
    bytes.try_into().ok().map(u64::from_be_bytes)
}
