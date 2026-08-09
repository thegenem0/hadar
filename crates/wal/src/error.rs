use std::{borrow::Cow, fmt};

/// Failure returned by a write-ahead log operation.
#[derive(Debug, thiserror::Error)]
#[error("{kind}")]
pub struct Error {
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Corrupt(Cow<'static, str>),
    Truncated,
}

impl Error {
    /// Reports log content that cannot be what this crate wrote.
    pub(crate) fn corrupt(detail: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind: Kind::Corrupt(detail.into()),
        }
    }

    /// Reports a record that ends before it is complete.
    pub(crate) fn truncated() -> Self {
        Self {
            kind: Kind::Truncated,
        }
    }

    /// Returns `true` if the log holds bytes this crate could not have written.
    #[must_use]
    pub fn is_corrupt(&self) -> bool {
        matches!(self.kind, Kind::Corrupt(_))
    }

    /// Returns `true` if a record ends before it is complete.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        matches!(self.kind, Kind::Truncated)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt(detail) => write!(f, "corrupt log record: {detail}"),
            Self::Truncated => f.write_str("log record ends mid-record"),
        }
    }
}
