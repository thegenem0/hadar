use std::{borrow::Cow, fmt};

/// Failure returned by a write-ahead log operation.
#[derive(Debug, thiserror::Error)]
#[error("{kind}")]
pub struct Error {
    kind: Kind,
    #[source]
    source: Option<std::io::Error>,
}

#[derive(Debug)]
enum Kind {
    Shutdown,
    Apply(Cow<'static, str>),
    Io(Cow<'static, str>),
    Corrupt(Cow<'static, str>),
    Truncated,
}

impl Error {
    /// Reports a failure of the underlying device or filesystem.
    pub(crate) fn io(context: impl Into<Cow<'static, str>>, source: std::io::Error) -> Self {
        Self {
            kind: Kind::Io(context.into()),
            source: Some(source),
        }
    }

    /// Reports a log that has stopped accepting writes.
    pub(crate) fn shutdown() -> Self {
        Self {
            kind: Kind::Shutdown,
            source: None,
        }
    }

    /// Reports a store that refused a durably logged batch.
    pub(crate) fn apply(detail: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind: Kind::Apply(detail.into()),
            source: None,
        }
    }

    /// Reports a write that failed because its batch did.
    pub(crate) fn batch_failed(detail: &str) -> Self {
        Self {
            kind: Kind::Apply(Cow::Owned(detail.to_owned())),
            source: None,
        }
    }

    /// Reports log content that cannot be what this crate wrote.
    pub(crate) fn corrupt(detail: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind: Kind::Corrupt(detail.into()),
            source: None,
        }
    }

    /// Reports a record that ends before it is complete.
    pub(crate) fn truncated() -> Self {
        Self {
            kind: Kind::Truncated,
            source: None,
        }
    }

    /// Returns `true` if the device or filesystem failed.
    #[must_use]
    pub fn is_io(&self) -> bool {
        matches!(self.kind, Kind::Io(_))
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
            Self::Shutdown => f.write_str("log is shut down and accepts no writes"),
            Self::Apply(detail) => write!(f, "applying a logged batch failed: {detail}"),
            Self::Io(context) => write!(f, "log i/o failed while {context}"),
            Self::Corrupt(detail) => write!(f, "corrupt log record: {detail}"),
            Self::Truncated => f.write_str("log record ends mid-record"),
        }
    }
}
