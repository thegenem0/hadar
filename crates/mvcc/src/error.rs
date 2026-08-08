use std::{borrow::Cow, fmt};

/// Failure returned by a [`KvStore`](crate::KvStore) operation.
#[derive(Debug, thiserror::Error)]
#[error("{kind}")]
pub struct Error {
    kind: Kind,
    source: Option<storage_api::Error>,
}

#[derive(Debug)]
enum Kind {
    Compacted { requested: u64, watermark: u64 },
    FutureRevision { requested: u64, current: u64 },
    Corrupt(Cow<'static, str>),
    Storage,
}

impl Error {
    pub(crate) fn compacted(requested: u64, watermark: u64) -> Self {
        Self {
            kind: Kind::Compacted {
                requested,
                watermark,
            },
            source: None,
        }
    }

    pub(crate) fn future_revision(requested: u64, current: u64) -> Self {
        Self {
            kind: Kind::FutureRevision { requested, current },
            source: None,
        }
    }

    pub(crate) fn corrupt(detail: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind: Kind::Corrupt(detail.into()),
            source: None,
        }
    }

    pub(crate) fn storage(source: storage_api::Error) -> Self {
        Self {
            kind: Kind::Storage,
            source: Some(source),
        }
    }

    /// Returns `true` if the requested revision has been compacted away.
    #[must_use]
    pub fn is_compacted(&self) -> bool {
        matches!(self.kind, Kind::Compacted { .. })
    }

    /// Returns `true` if the requested revision has not happened yet.
    #[must_use]
    pub fn is_future_revision(&self) -> bool {
        matches!(self.kind, Kind::FutureRevision { .. })
    }

    /// Returns `true` if the backend holds state this store cannot interpret.
    #[must_use]
    pub fn is_corrupt(&self) -> bool {
        matches!(self.kind, Kind::Corrupt(_))
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compacted {
                requested,
                watermark,
            } => write!(
                f,
                "revision {requested} has been compacted away; oldest readable is {watermark}"
            ),
            Self::FutureRevision { requested, current } => write!(
                f,
                "revision {requested} is ahead of the current revision {current}"
            ),
            Self::Corrupt(detail) => write!(f, "corrupt store state: {detail}"),
            Self::Storage => f.write_str("storage backend failure"),
        }
    }
}
