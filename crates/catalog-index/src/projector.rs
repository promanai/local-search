use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use localsearch_core::{DocumentId, MutationSeq};
use localsearch_filesystem_graph::{FilesystemGraph, GraphError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CATALOG_SCHEMA_ID, CatalogIndex, CatalogIndexError};

/// Projection worker failure.
#[derive(Debug, Error)]
pub enum ProjectionWorkerError {
    /// Durable `SQLite` state failed.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// Materialized catalog failed.
    #[error(transparent)]
    Catalog(#[from] CatalogIndexError),
    /// Generation directory operation failed.
    #[error("projection generation filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// Outbox sequencing violated the consumer contract.
    #[error("projection outbox invariant failed: {0}")]
    Outbox(String),
}

/// Result type for projection worker operations.
pub type ProjectionWorkerResult<T> = Result<T, ProjectionWorkerError>;

/// Fixed resource and batching policy for one worker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionWorkerOptions {
    /// Maximum mutations in one Tantivy commit.
    pub maximum_batch_mutations: u32,
    /// Maximum batches processed by one bounded run.
    pub maximum_batches: u32,
    /// Maximum wall time for one bounded run.
    pub maximum_run_time: Duration,
    /// Tantivy writer heap budget.
    pub writer_heap_bytes: usize,
    /// Desired-state records read per rebuild page.
    pub rebuild_page_size: u32,
}

impl Default for ProjectionWorkerOptions {
    fn default() -> Self {
        Self {
            maximum_batch_mutations: 10_000,
            maximum_batches: 100,
            maximum_run_time: Duration::from_secs(30),
            writer_heap_bytes: 128 * 1_024 * 1_024,
            rebuild_page_size: 10_000,
        }
    }
}

/// Recovery path selected at worker startup.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryKind {
    /// Existing active generation opened successfully.
    ExistingGeneration,
    /// Missing or corrupt active generation was rebuilt from `SQLite` desired state.
    RebuiltGeneration,
}

/// Metrics from one bounded worker run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionRunSummary {
    /// Startup recovery path.
    pub recovery: RecoveryKind,
    /// Active materialized generation after the run.
    pub index_generation: u64,
    /// Number of Tantivy commits made for outbox batches.
    pub committed_batches: u32,
    /// Number of canonical mutations applied.
    pub applied_mutations: u64,
    /// Last durable sequence acknowledged after a Tantivy commit.
    pub applied_sequence: u64,
    /// Whether pending outbox work remains.
    pub backlog_remaining: bool,
}

/// Single logical projection writer and recovery coordinator.
pub struct ProjectionWorker {
    root: PathBuf,
    consumer_id: String,
    options: ProjectionWorkerOptions,
}

impl ProjectionWorker {
    /// Creates a worker rooted at a directory containing disposable generations.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, options: ProjectionWorkerOptions) -> Self {
        Self {
            root: root.into(),
            consumer_id: CATALOG_SCHEMA_ID.to_owned(),
            options,
        }
    }

    /// Runs startup recovery and bounded outbox consumption.
    ///
    /// ACK happens only after each whole Tantivy commit. Replaying a committed-but-unacknowledged
    /// batch is safe because every upsert deletes its document ID before adding current state.
    ///
    /// # Errors
    ///
    /// Returns an error for graph, outbox, generation, poison-mutation, or Tantivy failure. The
    /// durable `SQLite` truth remains available for retry or rebuild.
    pub fn run(&self, graph: &FilesystemGraph) -> ProjectionWorkerResult<ProjectionRunSummary> {
        fs::create_dir_all(&self.root)?;
        let (recovery, generation) = self.recover(graph)?;
        let started = Instant::now();
        let mut batches = 0_u32;
        let mut applied = 0_u64;

        loop {
            if batches >= self.options.maximum_batches
                || started.elapsed() >= self.options.maximum_run_time
            {
                break;
            }
            let checkpoint = graph
                .projector_checkpoint(&self.consumer_id)?
                .ok_or_else(|| {
                    ProjectionWorkerError::Outbox("missing active checkpoint".to_owned())
                })?;
            let batch = graph.read_outbox(
                Some(MutationSeq(checkpoint.last_sequence)),
                self.options.maximum_batch_mutations,
            )?;
            if batch.mutations.is_empty() {
                break;
            }
            batch
                .validate()
                .map_err(|error| ProjectionWorkerError::Outbox(error.to_string()))?;
            let index = CatalogIndex::open(&generation_path(&self.root, generation))?;
            let mut writer = index.writer(self.options.writer_heap_bytes)?;
            for mutation in &batch.mutations {
                writer.apply(&mutation.mutation)?;
            }
            writer.commit()?;
            writer.wait_merging_threads()?;
            let last = batch.last_sequence().ok_or_else(|| {
                ProjectionWorkerError::Outbox("non-empty batch lost its tail".to_owned())
            })?;
            graph.acknowledge_projection(&self.consumer_id, last, generation)?;
            applied += u64::try_from(batch.mutations.len())
                .map_err(|_| ProjectionWorkerError::Outbox("batch length overflow".to_owned()))?;
            batches += 1;
        }

        let checkpoint = graph
            .projector_checkpoint(&self.consumer_id)?
            .ok_or_else(|| ProjectionWorkerError::Outbox("missing final checkpoint".to_owned()))?;
        let backlog_remaining = checkpoint.last_sequence < graph.latest_outbox_sequence()?.0;
        Ok(ProjectionRunSummary {
            recovery,
            index_generation: generation,
            committed_batches: batches,
            applied_mutations: applied,
            applied_sequence: checkpoint.last_sequence,
            backlog_remaining,
        })
    }

    /// Opens the generation named by the atomic `SQLite` activation checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when no generation is active or it cannot be opened.
    pub fn active_index(&self, graph: &FilesystemGraph) -> ProjectionWorkerResult<CatalogIndex> {
        let checkpoint = graph
            .projector_checkpoint(&self.consumer_id)?
            .ok_or_else(|| {
                ProjectionWorkerError::Outbox("no active catalog generation".to_owned())
            })?;
        Ok(CatalogIndex::open(&generation_path(
            &self.root,
            checkpoint.index_generation,
        ))?)
    }

    fn recover(&self, graph: &FilesystemGraph) -> ProjectionWorkerResult<(RecoveryKind, u64)> {
        if let Some(checkpoint) = graph.projector_checkpoint(&self.consumer_id)? {
            let path = generation_path(&self.root, checkpoint.index_generation);
            if CatalogIndex::open(&path).is_ok() {
                return Ok((
                    RecoveryKind::ExistingGeneration,
                    checkpoint.index_generation,
                ));
            }
            return self.rebuild(graph, checkpoint.index_generation.saturating_add(1));
        }
        self.rebuild(graph, 1)
    }

    fn rebuild(
        &self,
        graph: &FilesystemGraph,
        minimum_generation: u64,
    ) -> ProjectionWorkerResult<(RecoveryKind, u64)> {
        let generation = next_unused_generation(&self.root, minimum_generation);
        let path = generation_path(&self.root, generation);
        let index = CatalogIndex::create(&path)?;
        let mut writer = index.writer(self.options.writer_heap_bytes)?;
        let mut after: Option<DocumentId> = None;
        loop {
            let documents = graph.desired_catalog_page(after, self.options.rebuild_page_size)?;
            if documents.is_empty() {
                break;
            }
            for document in &documents {
                writer.add_current(document)?;
            }
            after = documents
                .last()
                .map(|document| document.identity.document_id);
            if documents.len()
                < usize::try_from(self.options.rebuild_page_size).map_err(|_| {
                    ProjectionWorkerError::Outbox("rebuild page overflow".to_owned())
                })?
            {
                break;
            }
        }
        writer.commit()?;
        writer.wait_merging_threads()?;
        let sequence = graph.latest_outbox_sequence()?;
        graph.acknowledge_projection(&self.consumer_id, sequence, generation)?;
        Ok((RecoveryKind::RebuiltGeneration, generation))
    }
}

fn generation_path(root: &Path, generation: u64) -> PathBuf {
    root.join(format!("generation-{generation:020}"))
}

fn next_unused_generation(root: &Path, minimum: u64) -> u64 {
    let mut generation = minimum;
    while generation_path(root, generation).exists() {
        generation = generation.saturating_add(1);
    }
    generation
}
