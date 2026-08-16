use localsearch_core::{DocumentId, SearchRequest, SearchResponse};

use crate::{AgentResult, Capabilities, CatalogItem, IndexStatus};

/// Backend-neutral catalog search boundary owned by Agent.
pub trait CatalogSearchPort {
    /// Executes one bounded product search.
    ///
    /// # Errors
    ///
    /// Returns a stable wire error for policy, deadline, cancellation, or backend failure.
    fn search(&self, request: &SearchRequest, deadline_ms: u32) -> AgentResult<SearchResponse>;
}

/// Backend-neutral stable-identity metadata lookup boundary.
pub trait CatalogLookupPort {
    /// Resolves one current catalog item.
    ///
    /// # Errors
    ///
    /// Returns `NotFound`, `Unavailable`, or a redacted backend error.
    fn get_catalog_item(&self, document_id: DocumentId) -> AgentResult<CatalogItem>;
    /// Resolves bounded current items without path-based authority.
    ///
    /// # Errors
    ///
    /// Returns `Unavailable` or a redacted backend error.
    fn get_catalog_items(&self, document_ids: &[DocumentId]) -> AgentResult<Vec<CatalogItem>>;
}

/// Sanitized materialized-index status boundary.
pub trait IndexStatusPort {
    /// Returns status without backend-native objects or sensitive inventory.
    ///
    /// # Errors
    ///
    /// Returns `IndexNotReady`, `Unavailable`, or a redacted backend error.
    fn index_status(&self) -> AgentResult<IndexStatus>;
}

/// Version/capability negotiation boundary.
pub trait CapabilitiesPort {
    /// Returns versions, server limits, and the already-authorized client's grants.
    fn capabilities(&self) -> Capabilities;
}
