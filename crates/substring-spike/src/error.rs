use crate::PolicyError;
use thiserror::Error;

/// Failure categories emitted by the experimental implementation.
#[derive(Debug, Error)]
pub enum ExperimentError {
    /// Query cost policy rejected the request before backend execution.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// Tantivy indexing or query execution failed.
    #[error("Tantivy operation failed: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    /// A filesystem operation failed.
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// Report serialization failed.
    #[error("report serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    /// A required field was absent or malformed in an indexed document.
    #[error("invalid experimental document: {0}")]
    InvalidDocument(&'static str),
    /// A worker thread failed while collecting process metrics.
    #[error("metrics worker failed")]
    MetricsWorker,
    /// A requested numeric conversion cannot be represented safely.
    #[error("numeric value is outside the supported range")]
    NumericRange,
}

/// Result type used by the START-003 experiment.
pub type ExperimentResult<T> = Result<T, ExperimentError>;
