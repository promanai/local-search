#![forbid(unsafe_code)]

//! Durable, platform-neutral filesystem graph backed by `SQLite`.
//!
//! Provider observations are translated into [`GraphMutation`] values before any SQL is run.
//! A mutation batch and its opaque provider checkpoint commit in one `SQLite` transaction. This is
//! the transaction boundary that `START-006` will extend with a projection outbox.

mod domain;
mod error;
mod migrations;
mod store;

pub use domain::{
    ApplySummary, DesiredPayloadMaintenanceSummary, GraphIntegrityIssue, GraphMutation,
    GraphMutationBatch, GraphStats, GraphStorageStats, ObservationFinalizeSummary,
    ObservationScanMode, ObservationScanPhase, ObservationSession, OutboxMaintenanceSummary,
    PathRefreshJob, ProjectionRefreshSummary, ProjectorCheckpoint, ResolvedPath, VolumeState,
};
pub use error::{GraphError, GraphResult};
pub use migrations::GRAPH_SCHEMA_VERSION;
pub use store::FilesystemGraph;
