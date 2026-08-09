use mvcc::Mutation;

use crate::{error::Error, frame};

/// Version of the record encoding, carried by every record.
const VERSION: u8 = 1;

const PUT: u8 = 1;
const DELETE: u8 = 2;

/// Appends `mutation` to `out` as a framed log record.
///
/// # Errors
///
/// Returns an error if the encoded record exceeds the frame size limit.
pub(crate) fn encode(mutation: &Mutation, out: &mut Vec<u8>) -> Result<(), Error> {
    let (kind, value) = match mutation {
        Mutation::Put { value, .. } => (PUT, value.as_slice()),
        Mutation::Delete { .. } => (DELETE, [].as_slice()),
    };
    let key = mutation.key();

    let Ok(key_len) = u32::try_from(key.len()) else {
        return Err(Error::corrupt("key length exceeds the record format"));
    };

    let mut payload = Vec::with_capacity(6 + key.len() + value.len());
    payload.push(VERSION);
    payload.push(kind);
    payload.extend_from_slice(&key_len.to_be_bytes());
    payload.extend_from_slice(key);
    payload.extend_from_slice(value);

    frame::encode(&payload, out)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<(Mutation, usize), Error> {
    let (payload, width) = frame::decode(bytes)?;

    let Some((&version, payload)) = payload.split_first() else {
        return Err(Error::corrupt("record is empty"));
    };
    if version != VERSION {
        return Err(Error::corrupt("record was written by an unknown version"));
    }

    let Some((&kind, payload)) = payload.split_first() else {
        return Err(Error::corrupt("record has no kind"));
    };
    let Some((key_len, payload)) = payload.split_at_checked(4) else {
        return Err(Error::corrupt("record has no key length"));
    };
    let Ok(key_len) = TryInto::<[u8; 4]>::try_into(key_len) else {
        return Err(Error::corrupt("record key length is malformed"));
    };
    let Some((key, value)) = payload.split_at_checked(u32::from_be_bytes(key_len) as usize) else {
        return Err(Error::corrupt("record key runs past its end"));
    };

    let mutation = match kind {
        PUT => Mutation::Put {
            key: key.to_vec(),
            value: value.to_vec(),
        },
        DELETE if value.is_empty() => Mutation::Delete { key: key.to_vec() },
        DELETE => return Err(Error::corrupt("deletion record carries a value")),
        _ => return Err(Error::corrupt("record names an unknown kind")),
    };

    Ok((mutation, width))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "test assertions surface failures by panicking"
)]
mod tests {
    use proptest::prelude::*;

    use mvcc::Mutation;

    use super::{VERSION, decode, encode};
    use crate::frame::HEADER_LEN;

    fn encoded(mutation: &Mutation) -> Vec<u8> {
        let mut out = Vec::new();
        encode(mutation, &mut out).expect("mutation is encodable");
        out
    }

    fn any_mutation() -> impl Strategy<Value = Mutation> {
        prop_oneof![
            (
                prop::collection::vec(any::<u8>(), 0..64),
                prop::collection::vec(any::<u8>(), 0..64)
            )
                .prop_map(|(key, value)| Mutation::Put { key, value }),
            prop::collection::vec(any::<u8>(), 0..64).prop_map(|key| Mutation::Delete { key }),
        ]
    }

    #[test]
    fn a_put_round_trips() {
        let mutation = Mutation::Put {
            key: b"key".to_vec(),
            value: b"value".to_vec(),
        };
        let (decoded, _) = decode(&encoded(&mutation)).expect("record decodes");
        assert_eq!(decoded, mutation);
    }

    #[test]
    fn a_delete_round_trips() {
        let mutation = Mutation::Delete {
            key: b"key".to_vec(),
        };
        let (decoded, _) = decode(&encoded(&mutation)).expect("record decodes");
        assert_eq!(decoded, mutation);
    }

    #[test]
    fn an_empty_value_is_distinct_from_a_deletion() {
        let put = encoded(&Mutation::Put {
            key: b"key".to_vec(),
            value: Vec::new(),
        });
        let delete = encoded(&Mutation::Delete {
            key: b"key".to_vec(),
        });

        assert_ne!(put, delete, "a put of nothing encodes as a deletion");
    }

    #[test]
    fn records_decode_back_to_back() {
        let first = Mutation::Put {
            key: b"a".to_vec(),
            value: b"1".to_vec(),
        };
        let second = Mutation::Delete { key: b"b".to_vec() };

        let mut log = encoded(&first);
        log.extend_from_slice(&encoded(&second));

        let (decoded, width) = decode(&log).expect("first record decodes");
        assert_eq!(decoded, first);
        let (decoded, _) = decode(&log[width..]).expect("second record decodes");
        assert_eq!(decoded, second);
    }

    #[test]
    fn a_record_from_a_future_version_is_refused() {
        let mut bytes = encoded(&Mutation::Delete {
            key: b"key".to_vec(),
        });
        bytes[HEADER_LEN] = VERSION + 1;

        // The checksum still has to be repaired, or this would be caught as
        // corruption before the version is ever read.
        let payload_len = bytes.len() - HEADER_LEN;
        let mut hasher = crc32fast::Hasher::new();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the payload was encodable, so its length fits"
        )]
        hasher.update(&(payload_len as u32).to_be_bytes());
        hasher.update(&bytes[HEADER_LEN..]);
        let checksum = hasher.finalize().to_be_bytes();
        bytes[..4].copy_from_slice(&checksum);

        let error = decode(&bytes).expect_err("an unknown version cannot decode");
        assert!(
            error.is_corrupt(),
            "reported {error} rather than corruption"
        );
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let mutation = Mutation::Delete {
            key: b"key".to_vec(),
        };
        let mut out = Vec::new();
        encode(&mutation, &mut out).expect("mutation is encodable");

        // Rebuild the frame around a payload naming a kind that does not
        // exist, so the checksum matches and the kind is what fails.
        let mut payload = out[HEADER_LEN..].to_vec();
        payload[1] = 0xee;
        let mut bytes = Vec::new();
        crate::frame::encode(&payload, &mut bytes).expect("payload is framable");

        let error = decode(&bytes).expect_err("an unknown kind cannot decode");
        assert!(
            error.is_corrupt(),
            "reported {error} rather than corruption"
        );
    }

    proptest! {
        #[test]
        fn any_mutation_round_trips(mutation in any_mutation()) {
            let bytes = encoded(&mutation);
            let (decoded, width) = decode(&bytes).expect("record decodes");

            prop_assert_eq!(decoded, mutation);
            prop_assert_eq!(width, bytes.len());
        }

        #[test]
        fn any_truncated_record_is_reported_as_truncation(
            mutation in any_mutation(),
            cut in any::<prop::sample::Index>(),
        ) {
            let bytes = encoded(&mutation);
            let cut = cut.index(bytes.len());

            let error = decode(&bytes[..cut]).expect_err("a partial record cannot decode");
            prop_assert!(error.is_truncated(), "reported {} rather than truncation", error);
        }

        #[test]
        fn a_sequence_of_mutations_replays_in_order(
            mutations in prop::collection::vec(any_mutation(), 1..16),
        ) {
            let mut log = Vec::new();
            for mutation in &mutations {
                encode(mutation, &mut log).expect("mutation is encodable");
            }

            let mut decoded = Vec::new();
            let mut rest = log.as_slice();
            while !rest.is_empty() {
                let (mutation, width) = decode(rest).expect("record decodes");
                decoded.push(mutation);
                rest = &rest[width..];
            }

            prop_assert_eq!(decoded, mutations);
        }
    }
}
