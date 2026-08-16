use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{AgentProtocolVersion, IdParseError};

/// Stable, backend-neutral category suitable for adapter mapping.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The input is malformed or violates a basic domain invariant.
    InvalidRequest,
    /// The peer requested an unsupported Agent protocol version.
    UnsupportedProtocolVersion,
    /// The operation or feature is not available through this capability set.
    UnsupportedCapability,
    /// The caller is not authenticated.
    Unauthorized,
    /// The authenticated caller is not allowed to perform the operation.
    Forbidden,
    /// Query cost or shape violates product policy.
    QueryPolicyRejected,
    /// The operation exceeded its deadline.
    DeadlineExceeded,
    /// The operation was cancelled.
    Cancelled,
    /// The requested domain object does not exist.
    NotFound,
    /// The object or source exists but is currently unavailable.
    Unavailable,
    /// The required materialized index is not ready.
    IndexNotReady,
    /// A bounded resource or capacity limit was reached.
    ResourceExhausted,
    /// An internal failure that must not expose backend details.
    Internal,
}

/// Backend-neutral domain failure.
#[derive(Debug, Error)]
pub enum DomainError {
    /// A canonical ID was malformed or used with the wrong type.
    #[error(transparent)]
    InvalidId(#[from] IdParseError),

    /// A request violated a domain invariant.
    #[error("invalid request: {reason}")]
    InvalidRequest {
        /// Safe diagnostic reason.
        reason: String,
    },

    /// The requested Agent protocol is unsupported.
    #[error("unsupported Agent protocol {requested}; supported version is {supported}")]
    UnsupportedProtocolVersion {
        /// Version requested by the peer.
        requested: AgentProtocolVersion,
        /// Version supported by this implementation.
        supported: AgentProtocolVersion,
    },

    /// A required capability is unavailable.
    #[error("unsupported capability: {capability}")]
    UnsupportedCapability {
        /// Stable capability name.
        capability: String,
    },

    /// The caller is unauthenticated.
    #[error("unauthorized")]
    Unauthorized,

    /// The caller lacks permission for the requested operation.
    #[error("forbidden")]
    Forbidden,

    /// Query policy rejected the operation before backend execution.
    #[error("query rejected by policy: {reason}")]
    QueryPolicyRejected {
        /// Safe policy reason.
        reason: String,
    },

    /// The operation exceeded its deadline.
    #[error("deadline exceeded")]
    DeadlineExceeded,

    /// The operation was cancelled.
    #[error("cancelled")]
    Cancelled,

    /// A domain entity was not found.
    #[error("{entity} not found")]
    NotFound {
        /// Safe entity category, never a private path.
        entity: &'static str,
    },

    /// A domain entity or source is temporarily unavailable.
    #[error("{entity} is unavailable")]
    Unavailable {
        /// Safe entity category, never a private path.
        entity: &'static str,
    },

    /// The materialized search index is not ready.
    #[error("index not ready")]
    IndexNotReady,

    /// A bounded resource limit was reached.
    #[error("resource exhausted: {resource}")]
    ResourceExhausted {
        /// Safe resource category.
        resource: &'static str,
    },

    /// Internal details have been intentionally hidden from public adapters.
    #[error("internal error")]
    Internal,
}

impl DomainError {
    /// Returns the stable category used by adapters.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidId(_) | Self::InvalidRequest { .. } => ErrorCode::InvalidRequest,
            Self::UnsupportedProtocolVersion { .. } => ErrorCode::UnsupportedProtocolVersion,
            Self::UnsupportedCapability { .. } => ErrorCode::UnsupportedCapability,
            Self::Unauthorized => ErrorCode::Unauthorized,
            Self::Forbidden => ErrorCode::Forbidden,
            Self::QueryPolicyRejected { .. } => ErrorCode::QueryPolicyRejected,
            Self::DeadlineExceeded => ErrorCode::DeadlineExceeded,
            Self::Cancelled => ErrorCode::Cancelled,
            Self::NotFound { .. } => ErrorCode::NotFound,
            Self::Unavailable { .. } => ErrorCode::Unavailable,
            Self::IndexNotReady => ErrorCode::IndexNotReady,
            Self::ResourceExhausted { .. } => ErrorCode::ResourceExhausted,
            Self::Internal => ErrorCode::Internal,
        }
    }
}

/// Result alias for backend-neutral domain operations.
pub type DomainResult<T> = Result<T, DomainError>;
