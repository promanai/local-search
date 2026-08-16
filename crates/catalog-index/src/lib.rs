#![forbid(unsafe_code)]

//! Disposable, rebuildable Tantivy projection of the durable `SQLite` catalog state.

mod index;
mod projector;

pub use index::{
    CATALOG_SCHEMA_ID, CatalogFingerprint, CatalogIndex, CatalogIndexError, CatalogIndexResult,
    CatalogQueryMode, CatalogReader,
};
pub use projector::{
    ProjectionRunSummary, ProjectionWorker, ProjectionWorkerError, ProjectionWorkerOptions,
    ProjectionWorkerResult, RecoveryKind,
};
