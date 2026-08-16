use localsearch_core::{
    Availability, FileKey, FileLinkId, FileLinkSnapshot, FileObjectSnapshot, FilesystemEvent,
    ReconciliationReason, VolumeId,
};
use localsearch_platform_core::{ProviderCheckpoint, VolumeDescriptor};
use serde::{Deserialize, Serialize};

use crate::{GraphError, GraphResult};

/// Durable lifecycle state of a discovered volume.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeState {
    /// The source is accessible and its graph can be updated.
    Online,
    /// The source is known but currently disconnected.
    Offline,
    /// Incremental continuity was lost and a reconciliation is required.
    NeedsReconciliation,
}

/// Full-volume observation mode persisted across Agent and broker restarts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationScanMode {
    /// Establish the first authoritative snapshot for a selected volume.
    Initial,
    /// Repair a volume after source continuity or integrity was lost.
    Reconcile,
}

/// Durable phase of one full-volume observation session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationScanPhase {
    /// Broker pages are still being committed to the graph.
    Scanning,
    /// Namespace links absent from the new snapshot are being removed in bounded pages.
    SweepingLinks,
    /// Unlinked objects absent from the new snapshot are being tombstoned in bounded pages.
    SweepingObjects,
}

/// Restart-safe full-volume observation progress.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservationSession {
    /// Volume being bootstrapped or reconciled.
    pub volume_id: VolumeId,
    /// First graph generation reserved for this scan; older rows are stale candidates.
    pub scan_generation: u64,
    /// Whether this is initial bootstrap or recovery.
    pub mode: ObservationScanMode,
    /// Current durable scan/finalization phase.
    pub phase: ObservationScanPhase,
}

/// Bounded stale-row sweep result for one observation maintenance transaction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObservationFinalizeSummary {
    /// Namespace links removed by this transaction.
    pub stale_links_removed: u64,
    /// Previously unlinked objects tombstoned by this transaction.
    pub stale_objects_tombstoned: u64,
    /// Whether the final provider checkpoint is now authoritative and the session is closed.
    pub completed: bool,
}

/// A platform-neutral state transition applied to the durable graph.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mutation", rename_all = "snake_case")]
pub enum GraphMutation {
    /// Create or update a volume without interpreting provider-native state.
    UpsertVolume {
        /// Portable volume description.
        descriptor: VolumeDescriptor,
    },
    /// Change the durable volume lifecycle state.
    SetVolumeState {
        /// Affected volume.
        volume_id: VolumeId,
        /// New lifecycle state.
        state: VolumeState,
    },
    /// Create or replace object metadata.
    UpsertObject {
        /// Latest object state.
        object: FileObjectSnapshot,
    },
    /// Create or replace a namespace link.
    UpsertLink {
        /// Latest link state.
        link: FileLinkSnapshot,
        /// Whether traversal through this link must stop at a provider boundary.
        traversal_boundary: bool,
    },
    /// Remove one namespace link while preserving other hard links.
    RemoveLink {
        /// Link identity.
        file_link_id: FileLinkId,
        /// Expected physical object identity.
        object_key: FileKey,
    },
    /// Tombstone an object after its final link has disappeared.
    TombstoneObject {
        /// Physical object identity.
        object_key: FileKey,
    },
    /// Record that source reconciliation is required.
    RequireReconciliation {
        /// Affected volume.
        volume_id: VolumeId,
        /// Portable recovery category.
        reason: ReconciliationReason,
    },
}

impl From<FilesystemEvent> for GraphMutation {
    fn from(event: FilesystemEvent) -> Self {
        match event {
            FilesystemEvent::ObjectObserved { object } => Self::UpsertObject { object },
            FilesystemEvent::LinkObserved { link } => Self::UpsertLink {
                link,
                traversal_boundary: false,
            },
            FilesystemEvent::LinkRemoved {
                file_link_id,
                object_key,
            } => Self::RemoveLink {
                file_link_id,
                object_key,
            },
            FilesystemEvent::ObjectRemoved { object_key } => Self::TombstoneObject { object_key },
            FilesystemEvent::ReconciliationRequired { volume_id, reason } => {
                Self::RequireReconciliation { volume_id, reason }
            }
        }
    }
}

/// One atomic provider-to-graph ingestion unit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphMutationBatch {
    /// Volume whose state is being changed.
    pub volume_id: VolumeId,
    /// Opaque continuation point committed with the graph changes.
    pub checkpoint: ProviderCheckpoint,
    /// Ordered, idempotent graph changes. An empty batch may advance the checkpoint.
    pub mutations: Vec<GraphMutation>,
}

impl GraphMutationBatch {
    /// Builds a batch from canonical provider events.
    #[must_use]
    pub fn from_events(
        volume_id: VolumeId,
        checkpoint: ProviderCheckpoint,
        events: impl IntoIterator<Item = FilesystemEvent>,
    ) -> Self {
        Self {
            volume_id,
            checkpoint,
            mutations: events.into_iter().map(GraphMutation::from).collect(),
        }
    }

    /// Validates volume isolation before SQL execution.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidBatch`] if the checkpoint or any mutation crosses volumes.
    pub fn validate(&self) -> GraphResult<()> {
        if self.checkpoint.volume_id != self.volume_id {
            return Err(GraphError::InvalidBatch(
                "checkpoint volume does not match mutation batch".to_owned(),
            ));
        }
        for mutation in &self.mutations {
            validate_mutation_volume(mutation, self.volume_id)?;
        }
        Ok(())
    }
}

/// A derived, platform-neutral current path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedPath {
    /// Ordered display-name components from the provider root to the selected link.
    pub components: Vec<String>,
}

impl ResolvedPath {
    /// Returns a stable display form without imposing an operating-system path syntax.
    #[must_use]
    pub fn display(&self) -> String {
        self.components.join("/")
    }
}

/// One bounded deferred path-cache refresh request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PathRefreshJob {
    /// Durable queue identifier.
    pub job_id: i64,
    /// Volume containing the renamed or moved directory.
    pub volume_id: VolumeId,
    /// Directory object whose descendants may have changed derived paths.
    pub root_object: FileKey,
    /// Graph generation that enqueued the work.
    pub enqueued_generation: u64,
}

/// Counts returned after one atomic batch.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApplySummary {
    /// Number of supplied mutations.
    pub mutations: u64,
    /// Resulting volume generation.
    pub generation: u64,
    /// Number of new bounded refresh jobs created by this batch.
    pub refresh_jobs_enqueued: u64,
    /// Number of durable projection mutations appended by this batch.
    pub outbox_mutations_appended: u64,
}

/// Durable progress of one materialized-index consumer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectorCheckpoint {
    /// Stable consumer identity, normally the catalog schema identifier.
    pub consumer_id: String,
    /// Last outbox sequence made durable in the materialized index.
    pub last_sequence: u64,
    /// Monotonic materialized-index generation.
    pub index_generation: u64,
}

/// Result of one bounded deferred projection-path refresh step.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionRefreshSummary {
    /// Number of link rows examined.
    pub links_scanned: u64,
    /// Number of changed catalog documents appended to the outbox.
    pub outbox_mutations_appended: u64,
    /// Whether the selected refresh job reached the end of the catalog.
    pub job_completed: bool,
}

/// Compact durable graph statistics.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphStats {
    /// Known volumes.
    pub volumes: u64,
    /// Non-tombstoned objects.
    pub live_objects: u64,
    /// Non-tombstoned regular file objects.
    pub live_files: u64,
    /// Logical bytes represented by non-tombstoned regular file objects.
    pub live_file_bytes: u64,
    /// Tombstoned objects retained for convergence.
    pub tombstoned_objects: u64,
    /// Current namespace links.
    pub links: u64,
    /// Pending path refresh jobs.
    pub pending_refresh_jobs: u64,
}

/// Physical `SQLite` allocation and reusable-page accounting for the durable graph.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphStorageStats {
    /// `SQLite` page size in bytes.
    pub page_size_bytes: u64,
    /// Pages currently allocated by the main database file.
    pub allocated_pages: u64,
    /// Allocated pages that can be reused without growing the database file.
    pub reusable_pages: u64,
    /// Main-database bytes represented by `allocated_pages`.
    pub allocated_bytes: u64,
    /// Bytes already reusable inside the main database file.
    pub reusable_bytes: u64,
    /// Whether bounded incremental vacuum can return tail pages to the filesystem.
    pub incremental_vacuum: bool,
}

/// Result of one bounded projection-outbox maintenance transaction.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutboxMaintenanceSummary {
    /// Highest sequence safe to discard while preserving every registered consumer.
    pub safe_through_sequence: Option<u64>,
    /// Rows removed by this bounded transaction.
    pub deleted_rows: u64,
    /// Whether more safe rows remain for a later maintenance quantum.
    pub backlog_remaining: bool,
}

/// Result of one bounded legacy desired-payload compaction transaction.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DesiredPayloadMaintenanceSummary {
    /// Legacy full-document JSON rows rewritten to compact path-only payloads.
    pub rewritten_rows: u64,
    /// Whether more legacy payload rows remain for a later maintenance quantum.
    pub backlog_remaining: bool,
}

/// A contained integrity problem that can be reconciled without disabling the whole graph.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum GraphIntegrityIssue {
    /// A live object has no current namespace link.
    OrphanObject {
        /// Unreachable physical object.
        object_key: FileKey,
    },
    /// A link refers to a parent object that is not present in the durable graph.
    MissingParent {
        /// Affected namespace link.
        file_link_id: FileLinkId,
        /// Missing parent identity.
        parent_key: FileKey,
    },
}

fn validate_mutation_volume(mutation: &GraphMutation, expected: VolumeId) -> GraphResult<()> {
    let actual = match mutation {
        GraphMutation::UpsertVolume { descriptor } => descriptor.volume_id,
        GraphMutation::SetVolumeState { volume_id, .. }
        | GraphMutation::RequireReconciliation { volume_id, .. } => *volume_id,
        GraphMutation::UpsertObject { object } => object.object_key.volume_id,
        GraphMutation::UpsertLink { link, .. } => {
            if let Some(parent) = link.parent_key
                && parent.volume_id != link.object_key.volume_id
            {
                return Err(GraphError::InvalidBatch(
                    "a filesystem link cannot cross volumes".to_owned(),
                ));
            }
            link.object_key.volume_id
        }
        GraphMutation::RemoveLink { object_key, .. }
        | GraphMutation::TombstoneObject { object_key } => object_key.volume_id,
    };
    if actual != expected {
        return Err(GraphError::InvalidBatch(
            "mutation volume does not match batch volume".to_owned(),
        ));
    }
    Ok(())
}

impl From<Availability> for VolumeState {
    fn from(value: Availability) -> Self {
        match value {
            Availability::Online => Self::Online,
            Availability::Offline => Self::Offline,
            Availability::Unknown => Self::NeedsReconciliation,
        }
    }
}
