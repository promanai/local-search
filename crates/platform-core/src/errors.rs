use thiserror::Error;

/// Stable error category returned by a platform adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformErrorKind {
    /// The current user or process lacks required access.
    PermissionDenied,
    /// The platform or filesystem cannot provide the requested capability.
    Unsupported,
    /// A device, mount, or source is temporarily unavailable.
    Unavailable,
    /// A checkpoint belongs to another provider, volume, or format version.
    InvalidCheckpoint,
    /// The source no longer retains history required to continue incrementally.
    SourceHistoryGap,
    /// The operation exceeded a bounded resource limit.
    ResourceExhausted,
    /// Cooperative cancellation stopped the operation.
    Cancelled,
    /// A native input/output operation failed.
    Io,
    /// The adapter violated an internal invariant.
    Internal,
}

/// Platform-adapter failure with a stable category and safe diagnostic context.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("platform operation {operation} failed: {kind:?}{detail}")]
pub struct PlatformError {
    /// Stable failure category used by recovery policy.
    pub kind: PlatformErrorKind,
    /// Backend-neutral operation name.
    pub operation: &'static str,
    /// Optional diagnostic text that must not be parsed for control flow.
    pub detail: String,
}

impl PlatformError {
    /// Creates a categorized platform error.
    #[must_use]
    pub fn new(
        kind: PlatformErrorKind,
        operation: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation,
            detail: detail.into(),
        }
    }
}

/// Result returned across the platform boundary.
pub type PlatformResult<T> = Result<T, PlatformError>;
