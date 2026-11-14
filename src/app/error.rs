use std::io;

use thiserror::Error;

/// Classified application errors. Every message is a static string or an
/// `io::ErrorKind`, so `Display`/`Debug` can never carry tokens, user paths,
/// file contents, or full sharing URLs.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppError {
    #[error("invalid input: {0}")]
    InvalidInput(&'static str),
    /// A destination or credential must be configured; nothing was sent elsewhere.
    #[error("setup needed: {0}")]
    NeedsSetup(&'static str),
    /// The target is referenced by active work and cannot change identity now.
    #[error("in use: {0}")]
    InUse(&'static str),
    #[error("credential error: {0}")]
    Credential(&'static str),
    #[error("I/O error while {stage}: {kind}")]
    Io {
        stage: &'static str,
        kind: io::ErrorKind,
    },
    #[error("capture error: {0}")]
    Capture(&'static str),
    #[error("encoder error: {0}")]
    Encoder(&'static str),
    #[error("HTTP error: {0}")]
    Http(&'static str),
    #[error("network transport failed")]
    Network,
    #[error("TLS verification or negotiation failed")]
    Tls,
    #[error("invalid server protocol: {0}")]
    Protocol(&'static str),
    #[error("server returned HTTP {code}")]
    HttpStatus {
        code: u16,
        retry_after: Option<std::time::Duration>,
    },
    #[error("upload paused")]
    Paused,
    /// The remote outcome is unknown; it must be resolved, never blindly retried.
    #[error("result uncertain: {0}")]
    Uncertain(&'static str),
    #[error("cancelled")]
    Cancelled,
    #[error("persistence error: {0}")]
    Persistence(&'static str),
    /// Bytes on disk no longer match the size/SHA-256 recorded when they were written.
    #[error("stored file no longer matches its recorded size and fingerprint")]
    ArtifactChanged,
}

impl AppError {
    pub(crate) fn io(stage: &'static str, err: &io::Error) -> Self {
        AppError::Io {
            stage,
            kind: err.kind(),
        }
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode::{ConstraintViolation, DatabaseBusy, DatabaseLocked};
        match err {
            rusqlite::Error::SqliteFailure(e, _) if e.code == ConstraintViolation => {
                AppError::Persistence("database constraint violated")
            }
            rusqlite::Error::SqliteFailure(e, _)
                if matches!(e.code, DatabaseBusy | DatabaseLocked) =>
            {
                AppError::Persistence("database is busy")
            }
            _ => AppError::Persistence("database operation failed"),
        }
    }
}
