#![forbid(unsafe_code)]

//! Portable, versioned, metadata-only wire contract for the elevated `WinFS` broker.

use localsearch_core::{FilesystemEvent, VolumeId};
use localsearch_platform_core::{
    ChangeBatch, PlatformCapabilities, ProviderCheckpoint, VolumeDescriptor,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

/// Broker semantic protocol implemented by v0.1.
pub const BROKER_PROTOCOL_VERSION: u32 = 1;
/// Broker JSON framing codec implemented by v0.1.
pub const BROKER_CODEC_VERSION: u32 = 1;
/// Maximum encoded JSON payload, excluding its four-byte prefix.
pub const MAX_BROKER_FRAME_BYTES: usize = 1_048_576;
/// Maximum events requested or returned in one broker page.
pub const MAX_BROKER_PAGE_EVENTS: u16 = 256;
/// Maximum UTF-8 request identifier bytes retained by the replay window.
pub const MAX_BROKER_REQUEST_ID_BYTES: usize = 64;
/// Default bounded broker operation deadline.
pub const DEFAULT_BROKER_DEADLINE_MS: u32 = 5_000;
/// Maximum caller-selectable broker operation deadline.
pub const MAX_BROKER_DEADLINE_MS: u32 = 30_000;

/// One independently authenticated broker request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrokerRequest {
    /// Semantic broker API version.
    pub protocol_version: u32,
    /// Framing codec version.
    pub codec_version: u32,
    /// Opaque unique identifier protected by the bounded replay window.
    pub request_id: String,
    /// Relative operation deadline.
    pub deadline_ms: u32,
    /// One allowlisted metadata operation.
    #[serde(flatten)]
    pub operation: BrokerOperation,
}

/// Complete v0.1 broker allowlist. No arbitrary path or content operation exists.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum BrokerOperation {
    /// Read protocol versions, limits, provider capabilities, and operation names.
    BrokerGetCapabilities,
    /// Discover provider-visible volumes.
    DiscoverVolumes,
    /// Start a bounded-queue initial scan or reconciliation for a canonical volume.
    StartScan { volume_id: VolumeId, mode: ScanMode },
    /// Pull one bounded page from a server-owned scan.
    ReadScanPage { scan_id: u64, maximum_events: u16 },
    /// Cancel and release a server-owned scan.
    CancelScan { scan_id: u64 },
    /// Read one bounded page from a durable provider checkpoint.
    ReadChanges {
        checkpoint: ProviderCheckpoint,
        maximum_events: u16,
    },
}

/// Full-enumeration reason accepted by `StartScan`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanMode {
    /// Establish first authoritative state and checkpoint.
    Initial,
    /// Repair state after a continuity or integrity failure.
    Reconcile,
}

impl BrokerRequest {
    /// Validate versions and every public resource bound before dispatch.
    ///
    /// # Errors
    ///
    /// Returns a stable contract category without echoing request metadata.
    pub fn validate(&self) -> Result<(), BrokerContractError> {
        if self.protocol_version != BROKER_PROTOCOL_VERSION {
            return Err(BrokerContractError::UnsupportedProtocolVersion);
        }
        if self.codec_version != BROKER_CODEC_VERSION {
            return Err(BrokerContractError::UnsupportedCodecVersion);
        }
        if self.request_id.is_empty() || self.request_id.len() > MAX_BROKER_REQUEST_ID_BYTES {
            return Err(BrokerContractError::InvalidRequest(
                "request_id is not bounded",
            ));
        }
        if self.deadline_ms > MAX_BROKER_DEADLINE_MS {
            return Err(BrokerContractError::InvalidRequest(
                "deadline exceeds policy",
            ));
        }
        match &self.operation {
            BrokerOperation::ReadScanPage { maximum_events, .. }
            | BrokerOperation::ReadChanges { maximum_events, .. }
                if *maximum_events == 0 || *maximum_events > MAX_BROKER_PAGE_EVENTS =>
            {
                Err(BrokerContractError::InvalidRequest(
                    "event page exceeds policy",
                ))
            }
            BrokerOperation::ReadChanges { checkpoint, .. }
                if checkpoint.opaque.len() > 65_536
                    || checkpoint.provider_id.len() > 64
                    || checkpoint.provider_id.is_empty() =>
            {
                Err(BrokerContractError::InvalidRequest(
                    "provider checkpoint exceeds policy",
                ))
            }
            _ => Ok(()),
        }
    }

    /// Return the caller deadline after applying the documented default.
    #[must_use]
    pub fn effective_deadline_ms(&self) -> u32 {
        if self.deadline_ms == 0 {
            DEFAULT_BROKER_DEADLINE_MS
        } else {
            self.deadline_ms.min(MAX_BROKER_DEADLINE_MS)
        }
    }
}

/// Stable operation names reported during capability negotiation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerOperationName {
    /// Capability negotiation.
    BrokerGetCapabilities,
    /// Volume discovery.
    DiscoverVolumes,
    /// Initial/reconciliation scan creation.
    StartScan,
    /// Bounded scan page pull.
    ReadScanPage,
    /// Scan cancellation.
    CancelScan,
    /// Bounded journal page pull.
    ReadChanges,
}

/// Broker and underlying provider capabilities returned to an authenticated Agent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrokerCapabilities {
    /// Supported semantic versions.
    pub protocol_versions: Vec<u32>,
    /// Supported framing versions.
    pub codec_versions: Vec<u32>,
    /// Exact metadata-only operation allowlist.
    pub allowed_operations: Vec<BrokerOperationName>,
    /// Maximum accepted encoded payload.
    pub maximum_frame_bytes: u32,
    /// Maximum returned page size.
    pub maximum_page_events: u16,
    /// Bounded number of simultaneous scan producers.
    pub maximum_active_scans: u16,
    /// Native provider behavior without exposing native structures.
    pub provider: PlatformCapabilities,
}

/// Successful broker responses.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum BrokerPayload {
    /// Version, allowlist, and provider capability negotiation.
    Capabilities(BrokerCapabilities),
    /// Current provider-visible volumes.
    Volumes(Vec<VolumeDescriptor>),
    /// Server-minted bounded scan handle.
    ScanStarted { scan_id: u64 },
    /// One bounded scan page.
    ScanPage(ScanPage),
    /// One bounded incremental change page.
    Changes {
        events: Vec<FilesystemEvent>,
        batch: ChangeBatch,
    },
    /// Scan cancellation acknowledgement.
    ScanCancelled { scan_id: u64 },
}

/// Pull response from a background full-enumeration producer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScanPage {
    /// Canonical provider observations in source order.
    pub events: Vec<FilesystemEvent>,
    /// Final opaque checkpoint, present only when `complete` is true.
    pub checkpoint: Option<ProviderCheckpoint>,
    /// Whether the producer completed successfully.
    pub complete: bool,
    /// Whether another pull may immediately return data or terminal state.
    pub has_more: bool,
}

/// Stable broker failure category.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerErrorCode {
    /// Request shape or bounds failed validation.
    InvalidRequest,
    /// Semantic broker version is unsupported.
    UnsupportedProtocolVersion,
    /// Framing codec version is unsupported.
    UnsupportedCodecVersion,
    /// Request identifier is already present in the replay window.
    ReplayRejected,
    /// Requested scan or volume is not current.
    NotFound,
    /// Caller lacks required native metadata access.
    PermissionDenied,
    /// Provider/platform operation is not supported.
    Unsupported,
    /// Native source is temporarily unavailable.
    Unavailable,
    /// Durable source history no longer covers the checkpoint.
    SourceHistoryGap,
    /// A bounded queue/scan/session limit was reached.
    ResourceExhausted,
    /// Deadline expired.
    DeadlineExceeded,
    /// Caller cancellation stopped the operation.
    Cancelled,
    /// Redacted internal failure.
    Internal,
}

/// Redacted broker error with no names, paths, checkpoints, or native causes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrokerWireError {
    /// Stable category.
    pub code: BrokerErrorCode,
    /// Safe bounded description.
    pub message: String,
}

/// Exactly one response for one accepted request ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrokerResponse {
    /// Semantic response version.
    pub protocol_version: u32,
    /// Correlation identifier after validation.
    pub request_id: String,
    /// Successful payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<BrokerPayload>,
    /// Stable redacted failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BrokerWireError>,
}

impl BrokerResponse {
    /// Create one successful response.
    #[must_use]
    pub fn success(request_id: String, payload: BrokerPayload) -> Self {
        Self {
            protocol_version: BROKER_PROTOCOL_VERSION,
            request_id,
            result: Some(payload),
            error: None,
        }
    }

    /// Create one redacted failure response.
    #[must_use]
    pub fn failure(request_id: String, code: BrokerErrorCode, message: &str) -> Self {
        Self {
            protocol_version: BROKER_PROTOCOL_VERSION,
            request_id,
            result: None,
            error: Some(BrokerWireError {
                code,
                message: message.to_owned(),
            }),
        }
    }

    /// Validate the success/error union.
    ///
    /// # Errors
    ///
    /// Rejects a response containing zero or two union arms.
    pub fn validate(&self) -> Result<(), BrokerContractError> {
        match (&self.result, &self.error) {
            (Some(_), None) | (None, Some(_)) => Ok(()),
            _ => Err(BrokerContractError::InvalidResponse),
        }
    }
}

/// Bounded broker codec and DTO validation failure.
#[derive(Debug, Error)]
pub enum BrokerContractError {
    /// Declared or encoded payload exceeds one MiB.
    #[error("broker frame exceeds maximum size")]
    FrameTooLarge,
    /// Prefix/payload is incomplete or inconsistent.
    #[error("incomplete broker frame")]
    IncompleteFrame,
    /// JSON does not match the versioned DTO.
    #[error("invalid broker JSON")]
    InvalidJson(#[source] serde_json::Error),
    /// Semantic version mismatch.
    #[error("unsupported broker protocol version")]
    UnsupportedProtocolVersion,
    /// Codec version mismatch.
    #[error("unsupported broker codec version")]
    UnsupportedCodecVersion,
    /// Public policy bound failed.
    #[error("invalid broker request: {0}")]
    InvalidRequest(&'static str),
    /// Response union is invalid.
    #[error("invalid broker response union")]
    InvalidResponse,
}

/// Encode one exact length-prefixed bounded JSON frame.
///
/// # Errors
///
/// Returns JSON or size contract failure.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, BrokerContractError> {
    let payload = serde_json::to_vec(value).map_err(BrokerContractError::InvalidJson)?;
    if payload.len() > MAX_BROKER_FRAME_BYTES {
        return Err(BrokerContractError::FrameTooLarge);
    }
    let length = u32::try_from(payload.len()).map_err(|_| BrokerContractError::FrameTooLarge)?;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decode exactly one bounded length-prefixed JSON frame.
///
/// # Errors
///
/// Rejects hostile lengths before allocating or deserializing a payload.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, BrokerContractError> {
    let prefix: [u8; 4] = frame
        .get(..4)
        .ok_or(BrokerContractError::IncompleteFrame)?
        .try_into()
        .map_err(|_| BrokerContractError::IncompleteFrame)?;
    let length = usize::try_from(u32::from_le_bytes(prefix))
        .map_err(|_| BrokerContractError::FrameTooLarge)?;
    if length > MAX_BROKER_FRAME_BYTES {
        return Err(BrokerContractError::FrameTooLarge);
    }
    let payload = frame.get(4..).ok_or(BrokerContractError::IncompleteFrame)?;
    if payload.len() != length {
        return Err(BrokerContractError::IncompleteFrame);
    }
    serde_json::from_slice(payload).map_err(BrokerContractError::InvalidJson)
}
