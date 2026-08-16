use localsearch_core::{FileKey, FileLinkId};
use thiserror::Error;

/// Failure while validating, mutating, or resolving the durable filesystem graph.
#[derive(Debug, Error)]
pub enum GraphError {
    /// `SQLite` storage failed.
    #[error("SQLite graph operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A durable projection payload could not be encoded or decoded.
    #[error("projection serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    /// A mutation batch violated the platform-neutral ingestion contract.
    #[error("invalid graph mutation batch: {0}")]
    InvalidBatch(String),
    /// A mutation would violate durable graph identity.
    #[error("graph invariant violation: {0}")]
    Invariant(String),
    /// The requested link is not present.
    #[error("filesystem link not found: {0}")]
    LinkNotFound(FileLinkId),
    /// A parent object has no current namespace link.
    #[error("missing parent link for object: {0:?}")]
    MissingParent(FileKey),
    /// A directory parent has multiple links and therefore no deterministic current path.
    #[error("ambiguous parent path for object: {0:?}")]
    AmbiguousParent(FileKey),
    /// A parent cycle was detected while deriving a path.
    #[error("parent cycle detected at object: {0:?}")]
    ParentCycle(FileKey),
    /// Resolution crossed a reparse or provider traversal boundary.
    #[error("path traversal stopped at boundary link: {0}")]
    TraversalBoundary(FileLinkId),
    /// Resolution exceeded its defensive depth limit.
    #[error("path depth exceeds configured limit of {0}")]
    DepthLimit(usize),
    /// A numeric value cannot be represented by `SQLite`'s signed integer storage.
    #[error("numeric value is outside SQLite range: {0}")]
    NumericRange(&'static str),
}

/// Result type used by filesystem graph operations.
pub type GraphResult<T> = Result<T, GraphError>;
