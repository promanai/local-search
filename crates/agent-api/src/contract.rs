use std::collections::BTreeSet;

use localsearch_core::{
    AgentProtocolVersion, Availability, DocumentId, DocumentVersion, FileKey, FileKind, FileLinkId,
    IndexGeneration, RankingVersion, SearchRequest, SearchResponse,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    DEFAULT_DEADLINE_MS, MAX_DEADLINE_MS, MAX_LOOKUP_ITEMS, MAX_QUERY_BYTES, MAX_REQUEST_ID_BYTES,
    MAX_TOP_K,
};

/// Agent semantic API version implemented by this crate.
pub const AGENT_API_VERSION: AgentProtocolVersion = AgentProtocolVersion::new(2);
/// Length-prefixed JSON codec version implemented by this crate.
pub const AGENT_CODEC_VERSION: u32 = 1;

/// Stable v0.1 data capability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Search catalog metadata.
    #[serde(rename = "search.catalog")]
    SearchCatalog,
    /// Search the separately enabled local document-content projection.
    #[serde(rename = "search.content")]
    SearchContent,
    /// Read current catalog metadata by stable document identity.
    #[serde(rename = "read.metadata")]
    ReadMetadata,
    /// Read sanitized materialized-index status.
    #[serde(rename = "index.status")]
    IndexStatus,
}

/// Capabilities and limits negotiated by an authorized local client.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Capabilities {
    /// Agent semantic protocol versions accepted by the server.
    pub agent_api_versions: Vec<AgentProtocolVersion>,
    /// IPC codec versions accepted by the server.
    pub codec_versions: Vec<u32>,
    /// Capabilities granted by trusted local configuration, never request input.
    pub granted: BTreeSet<Capability>,
    /// Maximum result count after server clamping.
    pub maximum_top_k: u16,
    /// Maximum accepted request-frame bytes.
    pub maximum_frame_bytes: u32,
    /// Product ranking semantics currently returned.
    pub ranking_version: RankingVersion,
}

/// One independently identifiable Agent request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentRequest {
    /// Agent semantic API version.
    pub protocol_version: AgentProtocolVersion,
    /// IPC framing/encoding version.
    pub codec_version: u32,
    /// Opaque client correlation identifier.
    pub request_id: String,
    /// Relative service deadline. The server clamps it to policy.
    pub deadline_ms: u32,
    /// Requested bounded operation.
    #[serde(flatten)]
    pub operation: RequestOperation,
}

/// Bounded Agent operations. No admin, settings, or filesystem mutation exists here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum RequestOperation {
    /// Search catalog metadata.
    CatalogSearch(SearchRequest),
    /// Search the explicitly enabled content projection.
    ContentSearch(ContentSearchRequest),
    /// Resolve one current catalog projection.
    CatalogGetItem { document_id: DocumentId },
    /// Resolve a bounded set of unique current projections.
    CatalogGetItems { document_ids: Vec<DocumentId> },
    /// Read sanitized index/backlog health.
    IndexGetStatus,
    /// Negotiate supported versions, grants, and limits.
    AgentGetCapabilities,
    /// Lightweight process/index readiness probe.
    AgentGetHealth,
}

impl AgentRequest {
    /// Validates structural and product-policy bounds before dispatch.
    ///
    /// # Errors
    ///
    /// Returns a stable contract error without echoing sensitive payload data.
    pub fn validate(&self) -> Result<(), WireContractError> {
        if self.protocol_version != AGENT_API_VERSION {
            return Err(WireContractError::UnsupportedProtocolVersion);
        }
        if self.codec_version != AGENT_CODEC_VERSION {
            return Err(WireContractError::UnsupportedCodecVersion);
        }
        if self.request_id.is_empty() || self.request_id.len() > MAX_REQUEST_ID_BYTES {
            return Err(WireContractError::InvalidRequest(
                "request_id must be non-empty and bounded",
            ));
        }
        if self.deadline_ms > MAX_DEADLINE_MS {
            return Err(WireContractError::InvalidRequest(
                "deadline exceeds the v0.1 policy maximum",
            ));
        }
        match &self.operation {
            RequestOperation::CatalogSearch(request) => {
                if request.query.is_empty() || request.query.len() > MAX_QUERY_BYTES {
                    return Err(WireContractError::InvalidRequest(
                        "query must be non-empty and bounded",
                    ));
                }
                if request.top_k == 0 || request.top_k > MAX_TOP_K {
                    return Err(WireContractError::InvalidRequest(
                        "top_k must be within the v0.1 policy bound",
                    ));
                }
                if request.filters.extensions.len() > 32 {
                    return Err(WireContractError::InvalidRequest(
                        "too many extension filters",
                    ));
                }
            }
            RequestOperation::ContentSearch(request) => {
                if request.query.is_empty() || request.query.len() > MAX_QUERY_BYTES {
                    return Err(WireContractError::InvalidRequest(
                        "content query must be non-empty and bounded",
                    ));
                }
                if request.top_k == 0 || request.top_k > MAX_TOP_K {
                    return Err(WireContractError::InvalidRequest(
                        "content top_k must be within the policy bound",
                    ));
                }
            }
            RequestOperation::CatalogGetItems { document_ids } => {
                if document_ids.is_empty() || document_ids.len() > MAX_LOOKUP_ITEMS {
                    return Err(WireContractError::InvalidRequest(
                        "document lookup collection must be non-empty and bounded",
                    ));
                }
                let unique = document_ids.iter().collect::<BTreeSet<_>>();
                if unique.len() != document_ids.len() {
                    return Err(WireContractError::InvalidRequest(
                        "document lookup IDs must be unique",
                    ));
                }
            }
            RequestOperation::CatalogGetItem { .. }
            | RequestOperation::IndexGetStatus
            | RequestOperation::AgentGetCapabilities
            | RequestOperation::AgentGetHealth => {}
        }
        Ok(())
    }

    /// Returns the clamped effective request deadline.
    #[must_use]
    pub fn effective_deadline_ms(&self) -> u32 {
        if self.deadline_ms == 0 {
            DEFAULT_DEADLINE_MS
        } else {
            self.deadline_ms.min(MAX_DEADLINE_MS)
        }
    }
}

/// Explicit request for the separate opt-in content projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentSearchRequest {
    /// Text terms to locate inside eligible documents.
    pub query: String,
    /// Maximum number of current catalog items returned.
    pub top_k: u16,
}

/// Current metadata returned by stable `DocumentId` lookup.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogItem {
    /// Search projection identity.
    pub document_id: DocumentId,
    /// Physical object identity.
    pub object_key: FileKey,
    /// Namespace-link identity.
    pub file_link_id: FileLinkId,
    /// Logical version of the current projection.
    pub document_version: DocumentVersion,
    /// Current display name.
    pub name: String,
    /// Current derived full path.
    pub resolved_path: String,
    /// Normalized extension without a leading dot.
    pub extension: Option<String>,
    /// Portable filesystem kind.
    pub kind: FileKind,
    /// Size in bytes.
    pub size: u64,
    /// Last modification timestamp when known.
    pub modified_at_unix_ms: Option<i64>,
    /// Current source availability.
    pub availability: Availability,
}

/// One content match after current durable metadata has been re-resolved by `DocumentId`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentSearchHit {
    /// Current catalog item; stale/deleted indexed identities are omitted.
    pub item: CatalogItem,
    /// One-based content result position.
    pub rank: u32,
}

/// Sanitized content-search result. No raw source text or backend score is exposed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentSearchResponse {
    /// Independent schema contract used by the content generation.
    pub content_schema: String,
    /// Service-side elapsed time in microseconds.
    pub took_micros: u64,
    /// Current metadata for matching content identities.
    pub hits: Vec<ContentSearchHit>,
}

/// Sanitized durable-backend status.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexStatus {
    /// Whether a searchable generation is active.
    pub ready: bool,
    /// Active index generation, if one exists.
    pub index_generation: Option<IndexGeneration>,
    /// Live searchable document count.
    pub document_count: u64,
    /// Latest durable outbox sequence.
    pub durable_sequence: u64,
    /// Last sequence made durable in the active index.
    pub applied_sequence: u64,
    /// Number of durable mutations still pending projection.
    pub backlog_mutations: u64,
}

/// Minimal process readiness result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceHealth {
    /// Agent process can serve requests.
    pub service_ready: bool,
    /// Durable graph can be opened.
    pub graph_ready: bool,
    /// Search projection is active.
    pub index_ready: bool,
}

/// Successful response variants.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ResponsePayload {
    /// Ranked catalog results.
    Search(SearchResponse),
    /// Results from the separate opt-in content projection.
    ContentSearch(ContentSearchResponse),
    /// One current item.
    CatalogItem(CatalogItem),
    /// Bounded lookup results, ordered like the request after missing items are omitted.
    CatalogItems(Vec<CatalogItem>),
    /// Sanitized projection status.
    IndexStatus(IndexStatus),
    /// Negotiated capabilities and limits.
    Capabilities(Capabilities),
    /// Process readiness.
    Health(ServiceHealth),
}

/// Stable public error categories.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    /// Malformed or structurally invalid request.
    InvalidRequest,
    /// Unsupported Agent semantic API version.
    UnsupportedProtocolVersion,
    /// Unsupported IPC codec version.
    UnsupportedCodecVersion,
    /// Required capability is not implemented.
    UnsupportedCapability,
    /// Endpoint authentication failed.
    Unauthorized,
    /// Authenticated client lacks the required grant.
    Forbidden,
    /// Query exceeds product cost policy.
    QueryPolicyRejected,
    /// Request deadline expired.
    DeadlineExceeded,
    /// Request was cancelled.
    Cancelled,
    /// Stable identity is not current.
    NotFound,
    /// Source or backend is temporarily unavailable.
    Unavailable,
    /// No searchable generation is ready.
    IndexNotReady,
    /// Resource policy rejected the work.
    ResourceExhausted,
    /// Safe catch-all with details retained only in local diagnostics.
    Internal,
}

/// Redacted wire error. It never includes queries, names, paths, or backend causes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentWireError {
    /// Stable machine-readable category.
    pub code: AgentErrorCode,
    /// Safe bounded summary.
    pub message: String,
}

impl std::fmt::Display for AgentWireError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for AgentWireError {}

/// Exactly one response for one request ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentResponse {
    /// Agent semantic API version used for this response.
    pub protocol_version: AgentProtocolVersion,
    /// Correlation identifier copied only after it passes request validation.
    pub request_id: String,
    /// Successful payload; mutually exclusive with `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResponsePayload>,
    /// Stable redacted failure; mutually exclusive with `result`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AgentWireError>,
}

impl AgentResponse {
    /// Creates a successful response.
    #[must_use]
    pub fn success(request_id: String, payload: ResponsePayload) -> Self {
        Self {
            protocol_version: AGENT_API_VERSION,
            request_id,
            result: Some(payload),
            error: None,
        }
    }

    /// Creates a redacted error response.
    #[must_use]
    pub fn failure(request_id: String, code: AgentErrorCode, message: &str) -> Self {
        Self {
            protocol_version: AGENT_API_VERSION,
            request_id,
            result: None,
            error: Some(AgentWireError {
                code,
                message: message.to_owned(),
            }),
        }
    }

    /// Validates the success/error union invariant.
    ///
    /// # Errors
    ///
    /// Returns [`WireContractError::InvalidResponse`] unless exactly one union arm is populated.
    pub fn validate(&self) -> Result<(), WireContractError> {
        match (&self.result, &self.error) {
            (Some(_), None) | (None, Some(_)) => Ok(()),
            _ => Err(WireContractError::InvalidResponse),
        }
    }
}

/// Service-side Agent operation result.
pub type AgentResult<T> = Result<T, AgentWireError>;

/// Local contract/codec failure before service dispatch.
#[derive(Debug, Error)]
pub enum WireContractError {
    /// Frame exceeds the allocation bound.
    #[error("frame exceeds maximum size")]
    FrameTooLarge,
    /// Length prefix or payload ended prematurely.
    #[error("incomplete frame")]
    IncompleteFrame,
    /// JSON did not match the authoritative DTO.
    #[error("invalid JSON wire payload")]
    InvalidJson(#[source] serde_json::Error),
    /// Agent semantic API version is unsupported.
    #[error("unsupported Agent protocol version")]
    UnsupportedProtocolVersion,
    /// IPC codec version is unsupported.
    #[error("unsupported Agent codec version")]
    UnsupportedCodecVersion,
    /// Request violates a public structural bound.
    #[error("invalid request: {0}")]
    InvalidRequest(&'static str),
    /// Response did not contain exactly one success/error arm.
    #[error("invalid response union")]
    InvalidResponse,
}
