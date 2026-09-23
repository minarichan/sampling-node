use thiserror::Error;

/// Failures produced while fetching or verifying data-availability samples.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DaError {
    #[error("header not found: {0}")]
    HeaderNotFound(String),

    #[error("sample unavailable at row {row}, column {col}")]
    SampleUnavailable { row: u32, col: u32 },

    #[error("sample coordinate is outside the data square")]
    CoordinateOutOfRange,

    #[error("commitment verification failed")]
    InvalidProof,

    #[error("{0}")]
    Unsupported(&'static str),

    #[error("{0}")]
    Message(String),
}
