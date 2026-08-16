#![forbid(unsafe_code)]

//! Backend-neutral identities, models, errors, and version contracts for `LocalSearch`.
//!
//! This crate deliberately has no dependency on `Tantivy`, `SQLite`, Windows APIs, `Tauri`,
//! or `MCP`. Adapters translate these domain contracts at architectural boundaries.

mod errors;
mod ids;
mod models;
mod versions;

pub use errors::{DomainError, DomainResult, ErrorCode};
pub use ids::{
    CatalogIdentity, DocumentId, FileId128, FileKey, FileLinkId, IdParseError, MachineId, VolumeId,
};
pub use models::{
    Availability, CatalogDocument, FileKind, FileLinkSnapshot, FileMetadata, FileObjectSnapshot,
    FilesystemEvent, IndexMutation, MatchType, MutationBatch, ReconciliationReason, SearchFilter,
    SearchHit, SearchRequest, SearchResponse, SearchScope, SequencedMutation,
};
pub use versions::{
    AGENT_PROTOCOL_VERSION, AgentProtocolVersion, DOMAIN_SCHEMA_VERSION, DocumentVersion,
    DomainSchemaVersion, IndexGeneration, MutationSeq, RankingVersion,
};
