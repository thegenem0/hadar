use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt;

/// Failure returned by a storage backend operation.
///
/// Errors are classified into a closed set of causes that callers can
/// branch on via the `is_*` predicates. The classification is intentionally
/// coarse so adding a new internal failure mode to a backend is
/// never a breaking change for its consumers.
#[derive(Debug, thiserror::Error)]
#[error("{kind}: {context}")]
pub struct Error {
    kind: Kind,
    context: Cow<'static, str>,
    #[source]
    source: Option<Box<dyn StdError>>,
}

/// Classification of a storage failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Io,
    Corrupt,
    Conflict,
    Other,
}

impl Error {
    /// Reports an I/O failure against the underlying file or device.
    ///
    /// The context should name the operation attempted, not restate the source error.
    #[must_use]
    pub fn io(context: impl Into<Cow<'static, str>>) -> Self {
        Self::new(Kind::Io, context)
    }

    /// Reports on-disk state that failed a structural or checksum validation.
    ///
    /// The context should identify the offending location so a corruption report
    /// is actionable without a debugger.
    #[must_use]
    pub fn corrupt(context: impl Into<Cow<'static, str>>) -> Self {
        Self::new(Kind::Corrupt, context)
    }

    /// Reports a transaction that could not complete because of concurrent activity.
    #[must_use]
    pub fn conflict(context: impl Into<Cow<'static, str>>) -> Self {
        Self::new(Kind::Conflict, context)
    }

    /// Reports a backend failure that does not fit the other classifications.
    ///
    /// Prefer a specific constructor where one applies.
    /// A backend that reports everything as `other` gives callers nothing to branch on.
    #[must_use]
    pub fn other(context: impl Into<Cow<'static, str>>) -> Self {
        Self::new(Kind::Other, context)
    }

    /// Attaches the underlying cause of this error.
    #[must_use]
    pub fn with_source<E>(mut self, source: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        self.source = Some(Box::new(source));
        self
    }

    /// Returns `true` if the underlying file or device reported an I/O failure.
    #[must_use]
    pub fn is_io(&self) -> bool {
        self.kind == Kind::Io
    }

    /// Returns `true` if on-disk state failed validation.
    ///
    /// A corrupt error is never retryable and never the caller's fault.
    /// It means the store's persistent state can no longer be trusted.
    #[must_use]
    pub fn is_corrupt(&self) -> bool {
        self.kind == Kind::Corrupt
    }

    /// Returns `true` if concurrent activity prevented the transaction from completing.
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        self.kind == Kind::Conflict
    }

    fn new(kind: Kind, context: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind,
            context: context.into(),
            source: None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Io => "i/o failure",
            Self::Corrupt => "corrupt on-disk state",
            Self::Conflict => "transaction conflict",
            Self::Other => "storage failure",
        };
        f.write_str(text)
    }
}
