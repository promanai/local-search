#![forbid(unsafe_code)]

//! Versioned, transport-neutral contract between `LocalSearch` Agent and local clients.

mod codec;
mod contract;
mod ports;

pub use codec::{decode_frame, encode_frame};
pub use contract::{
    AGENT_API_VERSION, AGENT_CODEC_VERSION, AgentErrorCode, AgentRequest, AgentResponse,
    AgentResult, AgentWireError, Capabilities, Capability, CatalogItem, ContentSearchHit,
    ContentSearchRequest, ContentSearchResponse, IndexStatus, RequestOperation, ResponsePayload,
    ServiceHealth, WireContractError,
};
pub use ports::{CapabilitiesPort, CatalogLookupPort, CatalogSearchPort, IndexStatusPort};

/// Maximum accepted length-prefixed JSON frame.
pub const MAX_FRAME_BYTES: usize = 1_048_576;
/// Maximum UTF-8 bytes accepted for a request identifier.
pub const MAX_REQUEST_ID_BYTES: usize = 64;
/// Maximum UTF-8 bytes accepted for a catalog query.
pub const MAX_QUERY_BYTES: usize = 1_024;
/// Maximum public result count.
pub const MAX_TOP_K: u16 = 100;
/// Maximum document IDs accepted by one bounded lookup.
pub const MAX_LOOKUP_ITEMS: usize = 100;
/// Default request deadline applied when the caller supplies zero.
pub const DEFAULT_DEADLINE_MS: u32 = 2_000;
/// Largest request deadline admitted by local policy.
pub const MAX_DEADLINE_MS: u32 = 10_000;
