use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
};

use localsearch_core::{
    Availability, CatalogDocument, CatalogIdentity, DocumentId, DocumentVersion, FileId128,
    FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata, FileObjectSnapshot,
    IndexMutation, MutationBatch, MutationSeq, ReconciliationReason, SequencedMutation, VolumeId,
};
use localsearch_platform_core::{ProviderCheckpoint, VolumeDescriptor};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
    types::Type,
};
use sha2::{Digest, Sha256};

use crate::{
    ApplySummary, DesiredPayloadMaintenanceSummary, GRAPH_SCHEMA_VERSION, GraphError,
    GraphIntegrityIssue, GraphMutation, GraphMutationBatch, GraphResult, GraphStats,
    GraphStorageStats, ObservationFinalizeSummary, ObservationScanMode, ObservationScanPhase,
    ObservationSession, OutboxMaintenanceSummary, PathRefreshJob, ProjectionRefreshSummary,
    ProjectorCheckpoint, ResolvedPath, VolumeState,
    migrations::{MIGRATION_001, MIGRATION_002, MIGRATION_003, MIGRATION_004, MIGRATION_005},
};

/// Persistent filesystem graph and its atomic provider-checkpoint boundary.
pub struct FilesystemGraph {
    connection: Connection,
}

#[derive(Clone, Copy)]
enum ProjectionOutboxMode {
    Durable,
    Rebuildable,
}

impl FilesystemGraph {
    /// Opens or creates a durable graph database.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot open, configure, or migrate the database.
    pub fn open(path: impl AsRef<Path>) -> GraphResult<Self> {
        let connection = Connection::open(path)?;
        Self::initialize(connection, true)
    }

    /// Opens an existing durable graph for latency-sensitive read-only access.
    ///
    /// Unlike [`Self::open`], this path never negotiates journal mode and never runs migrations.
    /// That keeps API readers out of `SQLite`'s writer-lock path while ingestion or projection ACKs
    /// are active.
    ///
    /// # Errors
    ///
    /// Returns a graph error when the database cannot be opened read-only or its schema is not
    /// already at the current version.
    pub fn open_read_only(path: impl AsRef<Path>) -> GraphResult<Self> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.execute_batch(
            "PRAGMA query_only = ON; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 250;",
        )?;
        let found: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if found != GRAPH_SCHEMA_VERSION {
            return Err(GraphError::Invariant(format!(
                "read-only graph schema version {found} does not match required version {GRAPH_SCHEMA_VERSION}"
            )));
        }
        Ok(Self { connection })
    }

    /// Opens an isolated in-memory graph, primarily for contract tests.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot configure or migrate the database.
    pub fn open_in_memory() -> GraphResult<Self> {
        Self::initialize(Connection::open_in_memory()?, false)
    }

    fn initialize(connection: Connection, durable: bool) -> GraphResult<Self> {
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
        let found: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if found > GRAPH_SCHEMA_VERSION {
            return Err(GraphError::Invariant(format!(
                "database schema version {found} is newer than supported version {GRAPH_SCHEMA_VERSION}"
            )));
        }
        if durable && found == 0 {
            // This must be selected before the first table is created. It lets bounded outbox
            // maintenance return free tail pages without a second full-size VACUUM copy.
            connection.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")?;
        }
        if durable {
            connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        }
        if found < 1 {
            connection.execute_batch(MIGRATION_001)?;
        }
        if found < 2 {
            connection.execute_batch(MIGRATION_002)?;
        }
        if found < 3 {
            connection.execute_batch(MIGRATION_003)?;
        }
        if found < 4 {
            connection.execute_batch(MIGRATION_004)?;
        }
        if found < 5 {
            connection.execute_batch(MIGRATION_005)?;
        }
        Ok(Self { connection })
    }

    /// Atomically ingests a provider snapshot and saves its continuation checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the portable batch is invalid or the transaction cannot commit.
    pub fn ingest_snapshot(
        &mut self,
        descriptor: VolumeDescriptor,
        checkpoint: ProviderCheckpoint,
        events: impl IntoIterator<Item = localsearch_core::FilesystemEvent>,
    ) -> GraphResult<ApplySummary> {
        let volume_id = descriptor.volume_id;
        let mut mutations = vec![GraphMutation::UpsertVolume { descriptor }];
        mutations.extend(events.into_iter().map(GraphMutation::from));
        self.apply_batch(&GraphMutationBatch {
            volume_id,
            checkpoint,
            mutations,
        })
    }

    /// Applies graph changes and the opaque source checkpoint in one `SQLite` transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identity, missing prerequisites, invariant violations, numeric
    /// overflow, or `SQLite` failure. On error, neither state nor checkpoint is committed.
    pub fn apply_batch(&mut self, batch: &GraphMutationBatch) -> GraphResult<ApplySummary> {
        self.apply_batch_with_outbox_mode(batch, ProjectionOutboxMode::Durable)
    }

    /// Applies graph changes without materializing a rebuildable projection outbox.
    ///
    /// This is only valid before the first projection consumer is registered. The consumer check
    /// and graph changes share the same immediate transaction, so a concurrent registration can
    /// never race with history suppression. Desired catalog state is still updated atomically and
    /// remains the source for a future consumer's full rebuild.
    ///
    /// # Errors
    ///
    /// Returns an error when any projection consumer is already registered, or for the same
    /// validation and storage failures as [`Self::apply_batch`].
    pub fn apply_rebuildable_batch(
        &mut self,
        batch: &GraphMutationBatch,
    ) -> GraphResult<ApplySummary> {
        self.apply_batch_with_outbox_mode(batch, ProjectionOutboxMode::Rebuildable)
    }

    fn apply_batch_with_outbox_mode(
        &mut self,
        batch: &GraphMutationBatch,
        outbox_mode: ProjectionOutboxMode,
    ) -> GraphResult<ApplySummary> {
        batch.validate()?;
        // Acquire the single WAL writer slot before reading the generation. A deferred
        // read-then-write transaction can otherwise fail with SQLITE_BUSY_SNAPSHOT when the
        // projection consumer ACKs on another connection between those two operations.
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if matches!(outbox_mode, ProjectionOutboxMode::Rebuildable)
            && projection_consumers_exist(&transaction)?
        {
            return Err(GraphError::Invariant(
                "rebuildable graph batch requires zero projection consumers".to_owned(),
            ));
        }
        let current_generation = transaction
            .query_row(
                "SELECT generation FROM graph_volumes WHERE volume_id = ?1",
                [batch.volume_id.as_bytes().as_slice()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let generation = current_generation
            .checked_add(1)
            .ok_or(GraphError::NumericRange("graph generation"))?;

        let mut refresh_jobs = 0_u64;
        let mut affected_links = BTreeSet::new();
        for mutation in &batch.mutations {
            collect_affected_links(&transaction, mutation, &mut affected_links)?;
            refresh_jobs += apply_mutation(&transaction, mutation, generation)?;
        }

        let mut outbox_mutations = 0_u64;
        for file_link_id in affected_links {
            outbox_mutations +=
                sync_catalog_projection(&transaction, file_link_id, generation, outbox_mode)?;
        }

        let updated = transaction.execute(
            "UPDATE graph_volumes SET generation = ?2 WHERE volume_id = ?1",
            params![batch.volume_id.as_bytes().as_slice(), generation],
        )?;
        if updated != 1 {
            return Err(GraphError::Invariant(
                "batch must upsert its volume before graph observations".to_owned(),
            ));
        }
        transaction.execute(
            "INSERT INTO graph_provider_checkpoints(volume_id, provider_id, format_version, opaque)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(volume_id) DO UPDATE SET
               provider_id = excluded.provider_id,
               format_version = excluded.format_version,
               opaque = excluded.opaque",
            params![
                batch.volume_id.as_bytes().as_slice(),
                batch.checkpoint.provider_id,
                i64::from(batch.checkpoint.format_version),
                batch.checkpoint.opaque,
            ],
        )?;
        transaction.commit()?;

        Ok(ApplySummary {
            mutations: u64::try_from(batch.mutations.len())
                .map_err(|_| GraphError::NumericRange("mutation count"))?,
            generation: u64::try_from(generation)
                .map_err(|_| GraphError::NumericRange("graph generation"))?,
            refresh_jobs_enqueued: refresh_jobs,
            outbox_mutations_appended: outbox_mutations,
        })
    }

    /// Reports whether at least one materialized projection consumer is registered.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot inspect projection checkpoints.
    pub fn has_projection_consumers(&self) -> GraphResult<bool> {
        projection_consumers_exist(&self.connection)
    }

    /// Loads the last checkpoint committed with graph state for a volume.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot read the checkpoint.
    pub fn checkpoint(&self, volume_id: VolumeId) -> GraphResult<Option<ProviderCheckpoint>> {
        self.connection
            .query_row(
                "SELECT provider_id, format_version, opaque
                 FROM graph_provider_checkpoints WHERE volume_id = ?1",
                [volume_id.as_bytes().as_slice()],
                |row| {
                    Ok(ProviderCheckpoint {
                        provider_id: row.get(0)?,
                        format_version: row.get(1)?,
                        volume_id,
                        opaque: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(GraphError::from)
    }

    /// Starts or restarts a crash-safe authoritative scan for one selected volume.
    ///
    /// Existing rows are retained while broker pages arrive. Their observation generations become
    /// the bounded stale-set cutoff when the scan reaches its final checkpoint.
    ///
    /// # Errors
    ///
    /// Returns a graph error when the durable scan boundary cannot be committed atomically.
    pub fn begin_observation_scan(
        &mut self,
        descriptor: &VolumeDescriptor,
        mode: ObservationScanMode,
        reason: ReconciliationReason,
    ) -> GraphResult<ObservationSession> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_generation = transaction
            .query_row(
                "SELECT generation FROM graph_volumes WHERE volume_id = ?1",
                [descriptor.volume_id.as_bytes().as_slice()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let scan_generation = current_generation
            .checked_add(1)
            .ok_or(GraphError::NumericRange("observation scan generation"))?;
        upsert_volume(&transaction, descriptor)?;
        transaction.execute(
            "UPDATE graph_volumes
             SET state = ?2, reconciliation_reason = ?3, generation = ?4
             WHERE volume_id = ?1",
            params![
                descriptor.volume_id.as_bytes().as_slice(),
                volume_state_code(VolumeState::NeedsReconciliation),
                reconciliation_reason_code(reason),
                scan_generation,
            ],
        )?;
        enqueue_volume_projection_refresh(&transaction, descriptor.volume_id, scan_generation)?;
        transaction.execute(
            "INSERT INTO graph_volume_observation_sessions(
               volume_id, scan_generation, scan_mode, phase,
               final_provider_id, final_format_version, final_checkpoint
             ) VALUES (?1, ?2, ?3, 0, NULL, NULL, NULL)
             ON CONFLICT(volume_id) DO UPDATE SET
               scan_generation = excluded.scan_generation,
               scan_mode = excluded.scan_mode,
               phase = 0,
               final_provider_id = NULL,
               final_format_version = NULL,
               final_checkpoint = NULL",
            params![
                descriptor.volume_id.as_bytes().as_slice(),
                scan_generation,
                observation_scan_mode_code(mode),
            ],
        )?;
        transaction.commit()?;
        Ok(ObservationSession {
            volume_id: descriptor.volume_id,
            scan_generation: u64::try_from(scan_generation)
                .map_err(|_| GraphError::NumericRange("observation scan generation"))?,
            mode,
            phase: ObservationScanPhase::Scanning,
        })
    }

    /// Commits one bounded broker scan page while retaining the durable scan session.
    ///
    /// # Errors
    ///
    /// Returns a graph error for a missing/non-scanning session or an invalid event batch.
    pub fn apply_observation_scan_page(
        &mut self,
        volume_id: VolumeId,
        events: Vec<localsearch_core::FilesystemEvent>,
    ) -> GraphResult<ApplySummary> {
        let session = self.observation_session(volume_id)?.ok_or_else(|| {
            GraphError::Invariant("observation scan page has no durable session".to_owned())
        })?;
        if session.phase != ObservationScanPhase::Scanning {
            return Err(GraphError::Invariant(
                "observation scan page arrived after finalization started".to_owned(),
            ));
        }
        if events.is_empty() {
            return Ok(ApplySummary {
                generation: session.scan_generation,
                ..ApplySummary::default()
            });
        }
        self.apply_batch(&GraphMutationBatch::from_events(
            volume_id,
            observation_scan_checkpoint(session),
            events,
        ))
    }

    /// Persists the provider checkpoint returned after the last full-scan page.
    ///
    /// The checkpoint remains staged until bounded stale-link and stale-object sweeps complete.
    ///
    /// # Errors
    ///
    /// Returns a graph error for a cross-volume checkpoint or missing scanning session.
    pub fn stage_observation_checkpoint(
        &mut self,
        volume_id: VolumeId,
        checkpoint: &ProviderCheckpoint,
    ) -> GraphResult<()> {
        if checkpoint.volume_id != volume_id {
            return Err(GraphError::InvalidBatch(
                "observation checkpoint crosses volumes".to_owned(),
            ));
        }
        let changed = self.connection.execute(
            "UPDATE graph_volume_observation_sessions
             SET phase = 1, final_provider_id = ?2,
                 final_format_version = ?3, final_checkpoint = ?4
             WHERE volume_id = ?1 AND phase = 0",
            params![
                volume_id.as_bytes().as_slice(),
                checkpoint.provider_id,
                i64::from(checkpoint.format_version),
                checkpoint.opaque,
            ],
        )?;
        require_single_update(changed, "observation checkpoint has no scanning session")
    }

    /// Loads restart-safe full-volume observation progress.
    ///
    /// # Errors
    ///
    /// Returns a graph error when the durable session cannot be decoded.
    pub fn observation_session(
        &self,
        volume_id: VolumeId,
    ) -> GraphResult<Option<ObservationSession>> {
        self.connection
            .query_row(
                "SELECT scan_generation, scan_mode, phase
                 FROM graph_volume_observation_sessions WHERE volume_id = ?1",
                [volume_id.as_bytes().as_slice()],
                |row| {
                    let generation = row.get::<_, i64>(0)?;
                    Ok(ObservationSession {
                        volume_id,
                        scan_generation: u64::try_from(generation).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                Type::Integer,
                                Box::new(error),
                            )
                        })?,
                        mode: observation_scan_mode_from_code(row.get(1)?)?,
                        phase: observation_scan_phase_from_code(row.get(2)?)?,
                    })
                },
            )
            .optional()
            .map_err(GraphError::from)
    }

    /// Removes one bounded page of rows absent from a completed authoritative scan.
    ///
    /// On the terminal object page, the provider checkpoint, online state, projection refresh, and
    /// session removal commit atomically. Incremental reads are unsafe until `completed` is true.
    ///
    /// # Errors
    ///
    /// Returns a graph error when stale-row convergence or checkpoint activation cannot commit.
    pub fn finalize_observation_scan(
        &mut self,
        volume_id: VolumeId,
        maximum_rows: u32,
    ) -> GraphResult<ObservationFinalizeSummary> {
        if maximum_rows == 0 {
            return Ok(ObservationFinalizeSummary::default());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = transaction
            .query_row(
                "SELECT scan_generation, phase, final_provider_id,
                        final_format_version, final_checkpoint
                 FROM graph_volume_observation_sessions WHERE volume_id = ?1",
                [volume_id.as_bytes().as_slice()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<Vec<u8>>>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                GraphError::Invariant("observation finalization has no durable session".to_owned())
            })?;
        let (scan_generation, phase, provider_id, format_version, opaque) = session;
        if phase == 0 {
            return Err(GraphError::Invariant(
                "observation finalization started before provider checkpoint".to_owned(),
            ));
        }
        let current_generation: i64 = transaction.query_row(
            "SELECT generation FROM graph_volumes WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
            |row| row.get(0),
        )?;
        let generation = current_generation
            .checked_add(1)
            .ok_or(GraphError::NumericRange(
                "observation finalization generation",
            ))?;
        let mut summary = ObservationFinalizeSummary::default();
        if phase == 1 {
            summary.stale_links_removed = sweep_stale_observation_links(
                &transaction,
                volume_id,
                scan_generation,
                generation,
                maximum_rows,
            )?;
        } else {
            let (tombstoned, terminal) = sweep_stale_observation_objects(
                &transaction,
                volume_id,
                scan_generation,
                maximum_rows,
            )?;
            summary.stale_objects_tombstoned = tombstoned;
            if terminal {
                activate_observation_checkpoint(
                    &transaction,
                    volume_id,
                    generation,
                    provider_id,
                    format_version,
                    opaque,
                )?;
                summary.completed = true;
            }
        }
        if !summary.completed {
            transaction.execute(
                "UPDATE graph_volumes SET generation = ?2 WHERE volume_id = ?1",
                params![volume_id.as_bytes().as_slice(), generation],
            )?;
        }
        transaction.commit()?;
        Ok(summary)
    }

    /// Reads one consecutive bounded projection-outbox batch after `sequence`.
    ///
    /// # Errors
    ///
    /// Returns a graph error when durable mutations cannot be read or decoded.
    pub fn read_outbox(
        &self,
        sequence: Option<MutationSeq>,
        limit: u32,
    ) -> GraphResult<MutationBatch> {
        let after = sequence.map_or(0, |value| value.0);
        let after = i64::try_from(after).map_err(|_| GraphError::NumericRange("outbox cursor"))?;
        let mut statement = self.connection.prepare(
            "SELECT sequence, mutation_json FROM graph_projection_outbox
             WHERE sequence > ?1 ORDER BY sequence LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after, i64::from(limit)], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut mutations = Vec::new();
        for row in rows {
            let (sequence, payload) = row?;
            mutations.push(SequencedMutation {
                sequence: MutationSeq(
                    u64::try_from(sequence)
                        .map_err(|_| GraphError::NumericRange("outbox sequence"))?,
                ),
                mutation: serde_json::from_str(&payload)?,
            });
        }
        Ok(MutationBatch { mutations })
    }

    /// Returns the newest durable projection sequence, or zero when the outbox is empty.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot inspect the outbox.
    pub fn latest_outbox_sequence(&self) -> GraphResult<MutationSeq> {
        let sequence: i64 = self.connection.query_row(
            "SELECT max(value) FROM (
               SELECT coalesce(max(sequence), 0) AS value FROM graph_projection_outbox
               UNION ALL
               SELECT coalesce(max(last_sequence), 0) AS value FROM graph_projection_consumers
               UNION ALL
               SELECT coalesce(max(seq), 0) AS value FROM sqlite_sequence
                 WHERE name = 'graph_projection_outbox'
             )",
            [],
            |row| row.get(0),
        )?;
        Ok(MutationSeq(u64::try_from(sequence).map_err(|_| {
            GraphError::NumericRange("outbox sequence")
        })?))
    }

    /// Returns all current desired catalog documents for index reconstruction.
    ///
    /// # Errors
    ///
    /// Returns a graph error when desired state cannot be read or decoded.
    pub fn desired_catalog_documents(&self) -> GraphResult<Vec<CatalogDocument>> {
        let mut statement = self.connection.prepare(
            "SELECT d.document_id, d.volume_id, d.file_id, d.file_link_id,
                    d.document_version, d.projection_path, d.document_json,
                    link.name, object.kind, object.size, object.created_at_unix_ms,
                    object.modified_at_unix_ms, object.hidden, object.availability, volume.state
             FROM graph_catalog_documents AS d
             JOIN graph_file_links AS link ON link.file_link_id = d.file_link_id
             JOIN graph_file_objects AS object
               ON object.volume_id = d.volume_id AND object.file_id = d.file_id
             JOIN graph_volumes AS volume ON volume.volume_id = d.volume_id
             ORDER BY d.document_id",
        )?;
        let rows = statement.query_map([], stored_catalog_projection_from_row)?;
        let mut documents = Vec::new();
        for row in rows {
            documents.push(row?.into_document()?);
        }
        Ok(documents)
    }

    /// Loads one current desired catalog document by stable projection identity.
    ///
    /// # Errors
    ///
    /// Returns a graph error when desired state cannot be read or decoded.
    pub fn desired_catalog_document(
        &self,
        document_id: DocumentId,
    ) -> GraphResult<Option<CatalogDocument>> {
        desired_catalog_document_connection(&self.connection, document_id)
    }

    /// Returns current link/object identities for one provider-scoped volume.
    ///
    /// This compact projection supports large reconciliation passes without decoding every stored
    /// catalog JSON payload.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot read or decode an identity.
    pub fn desired_catalog_identities(
        &self,
        volume_id: VolumeId,
    ) -> GraphResult<Vec<(FileLinkId, FileKey)>> {
        let mut statement = self.connection.prepare(
            "SELECT file_link_id, file_id FROM graph_catalog_documents
             WHERE volume_id = ?1 ORDER BY file_link_id",
        )?;
        let rows = statement.query_map([volume_id.as_bytes().as_slice()], |row| {
            Ok((
                link_id_from_blob(row.get(0)?)?,
                FileKey::new(volume_id, file_id_from_blob(row.get(1)?)?),
            ))
        })?;
        let mut identities = Vec::new();
        for row in rows {
            identities.push(row?);
        }
        Ok(identities)
    }

    /// Reads a deterministic bounded page of current desired catalog state.
    ///
    /// # Errors
    ///
    /// Returns a graph error when desired state cannot be read or decoded.
    pub fn desired_catalog_page(
        &self,
        after: Option<DocumentId>,
        limit: u32,
    ) -> GraphResult<Vec<CatalogDocument>> {
        let mut documents = Vec::new();
        if let Some(cursor) = after {
            let mut statement = self.connection.prepare(
                "SELECT d.document_id, d.volume_id, d.file_id, d.file_link_id,
                        d.document_version, d.projection_path, d.document_json,
                        link.name, object.kind, object.size, object.created_at_unix_ms,
                        object.modified_at_unix_ms, object.hidden, object.availability, volume.state
                 FROM graph_catalog_documents AS d
                 JOIN graph_file_links AS link ON link.file_link_id = d.file_link_id
                 JOIN graph_file_objects AS object
                   ON object.volume_id = d.volume_id AND object.file_id = d.file_id
                 JOIN graph_volumes AS volume ON volume.volume_id = d.volume_id
                 WHERE d.document_id > ?1
                 ORDER BY d.document_id LIMIT ?2",
            )?;
            let rows = statement.query_map(
                params![cursor.as_bytes().as_slice(), i64::from(limit)],
                stored_catalog_projection_from_row,
            )?;
            for row in rows {
                documents.push(row?.into_document()?);
            }
        } else {
            let mut statement = self.connection.prepare(
                "SELECT d.document_id, d.volume_id, d.file_id, d.file_link_id,
                        d.document_version, d.projection_path, d.document_json,
                        link.name, object.kind, object.size, object.created_at_unix_ms,
                        object.modified_at_unix_ms, object.hidden, object.availability, volume.state
                 FROM graph_catalog_documents AS d
                 JOIN graph_file_links AS link ON link.file_link_id = d.file_link_id
                 JOIN graph_file_objects AS object
                   ON object.volume_id = d.volume_id AND object.file_id = d.file_id
                 JOIN graph_volumes AS volume ON volume.volume_id = d.volume_id
                 ORDER BY d.document_id LIMIT ?1",
            )?;
            let rows =
                statement.query_map([i64::from(limit)], stored_catalog_projection_from_row)?;
            for row in rows {
                documents.push(row?.into_document()?);
            }
        }
        Ok(documents)
    }

    /// Loads durable progress for one materialized-index consumer.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot read the checkpoint.
    pub fn projector_checkpoint(
        &self,
        consumer_id: &str,
    ) -> GraphResult<Option<ProjectorCheckpoint>> {
        self.connection
            .query_row(
                "SELECT last_sequence, index_generation FROM graph_projection_consumers
                 WHERE consumer_id = ?1",
                [consumer_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .map(|(last_sequence, index_generation)| {
                Ok(ProjectorCheckpoint {
                    consumer_id: consumer_id.to_owned(),
                    last_sequence: u64::try_from(last_sequence)
                        .map_err(|_| GraphError::NumericRange("projector sequence"))?,
                    index_generation: u64::try_from(index_generation)
                        .map_err(|_| GraphError::NumericRange("index generation"))?,
                })
            })
            .transpose()
    }

    /// Advances a projector checkpoint after its Tantivy commit is durable.
    ///
    /// # Errors
    ///
    /// Returns an error for sequence regression, a cursor beyond the durable outbox, or `SQLite`
    /// failure.
    pub fn acknowledge_projection(
        &self,
        consumer_id: &str,
        last_sequence: MutationSeq,
        index_generation: u64,
    ) -> GraphResult<()> {
        let latest = self.latest_outbox_sequence()?.0;
        if last_sequence.0 > latest {
            return Err(GraphError::Invariant(
                "projector checkpoint cannot advance beyond durable outbox".to_owned(),
            ));
        }
        if let Some(current) = self.projector_checkpoint(consumer_id)?
            && last_sequence.0 < current.last_sequence
        {
            return Err(GraphError::Invariant(
                "projector checkpoint cannot move backwards".to_owned(),
            ));
        }
        self.connection.execute(
            "INSERT INTO graph_projection_consumers(consumer_id, last_sequence, index_generation)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(consumer_id) DO UPDATE SET
               last_sequence = excluded.last_sequence,
               index_generation = excluded.index_generation",
            params![
                consumer_id,
                i64::try_from(last_sequence.0)
                    .map_err(|_| GraphError::NumericRange("projector sequence"))?,
                i64::try_from(index_generation)
                    .map_err(|_| GraphError::NumericRange("index generation"))?,
            ],
        )?;
        Ok(())
    }

    /// Removes one materialized-index consumer checkpoint before an explicit projection reset.
    /// Desired catalog state and other consumers are unchanged.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot remove the named checkpoint.
    pub fn reset_projection_consumer(&self, consumer_id: &str) -> GraphResult<bool> {
        Ok(self.connection.execute(
            "DELETE FROM graph_projection_consumers WHERE consumer_id = ?1",
            [consumer_id],
        )? > 0)
    }

    /// Deletes outbox entries acknowledged by every registered consumer.
    ///
    /// Desired catalog state remains sufficient for a full rebuild. With no registered consumer,
    /// nothing is deleted.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot prune acknowledged rows.
    pub fn prune_consumed_outbox(&self) -> GraphResult<u64> {
        let minimum = self.connection.query_row(
            "SELECT min(last_sequence) FROM graph_projection_consumers",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        let Some(minimum) = minimum else {
            return Ok(0);
        };
        let deleted = self.connection.execute(
            "DELETE FROM graph_projection_outbox WHERE sequence <= ?1",
            [minimum],
        )?;
        u64::try_from(deleted).map_err(|_| GraphError::NumericRange("pruned outbox rows"))
    }

    /// Reports whether at least one outbox row is acknowledged by every registered consumer.
    ///
    /// This indexed existence check lets a scheduler retry deferred compaction even when the
    /// ordinary projection backlog is already zero.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot inspect consumer progress.
    pub fn consumed_outbox_maintenance_pending(&self) -> GraphResult<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM graph_projection_outbox
                   WHERE sequence <= (
                     SELECT min(last_sequence) FROM graph_projection_consumers
                   ) LIMIT 1
                 )",
                [],
                |row| row.get(0),
            )
            .map_err(GraphError::from)
    }

    /// Reports whether legacy full-document desired payloads still need bounded compaction.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot inspect desired state.
    pub fn legacy_desired_payload_maintenance_pending(&self) -> GraphResult<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM graph_catalog_documents
                   WHERE projection_path IS NULL LIMIT 1
                 )",
                [],
                |row| row.get(0),
            )
            .map_err(GraphError::from)
    }

    /// Deletes at most `maximum_rows` rebuildable outbox rows in one short transaction.
    ///
    /// The safe cursor is the oldest registered consumer checkpoint. When explicitly allowed and
    /// no consumer exists, current desired catalog state is sufficient for every future consumer
    /// to perform its normal full rebuild. The AUTOINCREMENT high-water mark preserves the durable
    /// sequence even when all rows are removed.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid bound or when `SQLite` maintenance cannot commit.
    pub fn prune_rebuildable_outbox_bounded(
        &mut self,
        maximum_rows: u32,
        allow_without_consumers: bool,
    ) -> GraphResult<OutboxMaintenanceSummary> {
        if maximum_rows == 0 || maximum_rows > 100_000 {
            return Err(GraphError::InvalidBatch(
                "outbox maintenance bound must be between 1 and 100000".to_owned(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let consumer_minimum = transaction.query_row(
            "SELECT min(last_sequence) FROM graph_projection_consumers",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        let safe_through = if let Some(minimum) = consumer_minimum {
            Some(minimum)
        } else if allow_without_consumers {
            transaction
                .query_row(
                    "SELECT seq FROM sqlite_sequence WHERE name = 'graph_projection_outbox'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
        } else {
            None
        };
        let Some(safe_through) = safe_through else {
            transaction.commit()?;
            return Ok(OutboxMaintenanceSummary::default());
        };
        let deleted = transaction.execute(
            "DELETE FROM graph_projection_outbox WHERE sequence IN (
               SELECT sequence FROM graph_projection_outbox
               WHERE sequence <= ?1 ORDER BY sequence LIMIT ?2
             )",
            params![safe_through, i64::from(maximum_rows)],
        )?;
        let backlog_remaining = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM graph_projection_outbox WHERE sequence <= ?1 LIMIT 1
             )",
            [safe_through],
            |row| row.get::<_, bool>(0),
        )?;
        transaction.commit()?;
        Ok(OutboxMaintenanceSummary {
            safe_through_sequence: Some(
                u64::try_from(safe_through)
                    .map_err(|_| GraphError::NumericRange("outbox maintenance cursor"))?,
            ),
            deleted_rows: u64::try_from(deleted)
                .map_err(|_| GraphError::NumericRange("pruned outbox rows"))?,
            backlog_remaining,
        })
    }

    /// Rewrites a bounded page of legacy full-document desired payloads to compact path payloads.
    ///
    /// Normalized identity, link name, and metadata remain authoritative in their graph tables.
    /// This transaction copies only the already-materialized resolved path before clearing the
    /// redundant JSON document. Freed pages are left for bounded reusable-page maintenance.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid bound, malformed legacy payload, or `SQLite` failure.
    pub fn compact_legacy_desired_payloads_bounded(
        &mut self,
        maximum_rows: u32,
    ) -> GraphResult<DesiredPayloadMaintenanceSummary> {
        if maximum_rows == 0 || maximum_rows > 100_000 {
            return Err(GraphError::InvalidBatch(
                "desired payload maintenance bound must be between 1 and 100000".to_owned(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let legacy_rows = {
            let mut statement = transaction.prepare(
                "SELECT document_id, document_json FROM graph_catalog_documents
                 WHERE projection_path IS NULL ORDER BY document_id LIMIT ?1",
            )?;
            let rows = statement.query_map([i64::from(maximum_rows)], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (document_id, payload) in &legacy_rows {
            let document = serde_json::from_str::<CatalogDocument>(payload)?;
            let fingerprint = catalog_content_fingerprint(&document)?;
            transaction.execute(
                "UPDATE graph_catalog_documents
                 SET projection_path = ?2, projection_fingerprint = ?3, document_json = ''
                 WHERE document_id = ?1 AND projection_path IS NULL",
                params![document_id, document.resolved_path, fingerprint.as_slice()],
            )?;
        }
        let backlog_remaining = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM graph_catalog_documents WHERE projection_path IS NULL LIMIT 1
             )",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(DesiredPayloadMaintenanceSummary {
            rewritten_rows: u64::try_from(legacy_rows.len())
                .map_err(|_| GraphError::NumericRange("compacted desired payload rows"))?,
            backlog_remaining,
        })
    }

    /// Returns physical allocation and reusable-page counters without scanning graph rows.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` storage metadata is unavailable or outside numeric bounds.
    pub fn storage_stats(&self) -> GraphResult<GraphStorageStats> {
        let page_size: i64 = self
            .connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        let page_count: i64 = self
            .connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let freelist: i64 = self
            .connection
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        let auto_vacuum: i64 = self
            .connection
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
        let page_size =
            u64::try_from(page_size).map_err(|_| GraphError::NumericRange("SQLite page size"))?;
        let allocated_pages =
            u64::try_from(page_count).map_err(|_| GraphError::NumericRange("SQLite page count"))?;
        let reusable_pages = u64::try_from(freelist)
            .map_err(|_| GraphError::NumericRange("SQLite freelist count"))?;
        Ok(GraphStorageStats {
            page_size_bytes: page_size,
            allocated_pages,
            reusable_pages,
            allocated_bytes: page_size.saturating_mul(allocated_pages),
            reusable_bytes: page_size.saturating_mul(reusable_pages),
            incremental_vacuum: auto_vacuum == 2,
        })
    }

    /// Returns up to `maximum_pages` reusable tail pages to the filesystem.
    ///
    /// Existing databases created without incremental auto-vacuum safely return zero; they still
    /// reuse freed pages internally and can be physically shrunk only by an explicit full vacuum.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid bound or unavailable `SQLite` storage metadata.
    pub fn reclaim_reusable_pages(&self, maximum_pages: u32) -> GraphResult<u64> {
        if maximum_pages == 0 || maximum_pages > 4_096 {
            return Err(GraphError::InvalidBatch(
                "page reclaim bound must be between 1 and 4096".to_owned(),
            ));
        }
        let initial = self.storage_stats()?;
        if !initial.incremental_vacuum || initial.reusable_pages == 0 {
            return Ok(0);
        }
        let target = u64::from(maximum_pages).min(initial.reusable_pages);
        let mut reclaimed = 0_u64;
        while reclaimed < target {
            let requested = target.saturating_sub(reclaimed);
            let before = self.storage_stats()?;
            self.connection
                .execute_batch(&format!("PRAGMA incremental_vacuum({requested});"))?;
            let after = self.storage_stats()?;
            let progress = before.allocated_pages.saturating_sub(after.allocated_pages);
            if progress == 0 {
                break;
            }
            reclaimed = reclaimed.saturating_add(progress);
        }
        Ok(reclaimed)
    }

    /// Returns the durable lifecycle state of a volume.
    ///
    /// # Errors
    ///
    /// Returns a graph error for an unknown state encoding or `SQLite` read failure.
    pub fn volume_state(&self, volume_id: VolumeId) -> GraphResult<Option<VolumeState>> {
        self.connection
            .query_row(
                "SELECT state FROM graph_volumes WHERE volume_id = ?1",
                [volume_id.as_bytes().as_slice()],
                |row| volume_state_from_code(row.get(0)?),
            )
            .optional()
            .map_err(GraphError::from)
    }

    /// Resolves the current path of a link by walking authoritative parent-object relationships.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a missing link/parent, parent cycle, ambiguous hard-linked
    /// directory, provider traversal boundary, excessive depth, or `SQLite` failure.
    pub fn resolve_path(
        &self,
        file_link_id: FileLinkId,
        depth_limit: usize,
    ) -> GraphResult<ResolvedPath> {
        if depth_limit == 0 {
            return Err(GraphError::DepthLimit(0));
        }
        resolve_path_connection(&self.connection, file_link_id, depth_limit)
    }

    /// Returns pending bounded subtree-path refresh jobs in durable order.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot read the queue.
    pub fn pending_refresh_jobs(&self, limit: u32) -> GraphResult<Vec<PathRefreshJob>> {
        let mut statement = self.connection.prepare(
            "SELECT job_id, volume_id, root_file_id, enqueued_generation
             FROM graph_path_refresh_jobs WHERE state = 0 ORDER BY job_id LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::from(limit)], |row| {
            let volume = volume_id_from_blob(row.get(1)?)?;
            let file = file_id_from_blob(row.get(2)?)?;
            let generation = row.get::<_, i64>(3)?;
            Ok(PathRefreshJob {
                job_id: row.get(0)?,
                volume_id: volume,
                root_object: FileKey::new(volume, file),
                enqueued_generation: u64::try_from(generation).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(3, Type::Integer, Box::new(error))
                })?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(GraphError::from)
    }

    /// Processes one path-refresh job with a bounded scan and durable cursor.
    ///
    /// Unchanged documents do not generate outbox traffic. This makes a directory rename cheap at
    /// mutation time while allowing descendants to converge in bounded restart-safe steps.
    ///
    /// # Errors
    ///
    /// Returns a graph error when path derivation or the durable refresh transaction fails.
    pub fn refresh_projection_paths(
        &mut self,
        maximum_links: u32,
    ) -> GraphResult<ProjectionRefreshSummary> {
        if maximum_links == 0 {
            return Ok(ProjectionRefreshSummary::default());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job = transaction
            .query_row(
                "SELECT job_id, volume_id, projection_scan_cursor
                 FROM graph_path_refresh_jobs WHERE state = 0 ORDER BY job_id LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((job_id, volume_blob, cursor)) = job else {
            return Ok(ProjectionRefreshSummary::default());
        };
        let volume_id = volume_id_from_blob(volume_blob)?;
        let generation: i64 = transaction.query_row(
            "SELECT generation FROM graph_volumes WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
            |row| row.get(0),
        )?;
        let links = if let Some(cursor) = cursor {
            let mut statement = transaction.prepare(
                "SELECT file_link_id FROM graph_file_links
                 WHERE volume_id = ?1 AND file_link_id > ?2
                 ORDER BY file_link_id LIMIT ?3",
            )?;
            let rows = statement.query_map(
                params![
                    volume_id.as_bytes().as_slice(),
                    cursor,
                    i64::from(maximum_links),
                ],
                |row| link_id_from_blob(row.get(0)?),
            )?;
            rows.collect::<Result<Vec<_>, _>>()?
        } else {
            let mut statement = transaction.prepare(
                "SELECT file_link_id FROM graph_file_links
                 WHERE volume_id = ?1 ORDER BY file_link_id LIMIT ?2",
            )?;
            let rows = statement.query_map(
                params![volume_id.as_bytes().as_slice(), i64::from(maximum_links)],
                |row| link_id_from_blob(row.get(0)?),
            )?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut appended = 0_u64;
        for link in &links {
            appended += sync_catalog_projection(
                &transaction,
                *link,
                generation,
                ProjectionOutboxMode::Durable,
            )?;
        }
        let complete = links.len()
            < usize::try_from(maximum_links)
                .map_err(|_| GraphError::NumericRange("refresh batch size"))?;
        if complete {
            transaction.execute(
                "UPDATE graph_path_refresh_jobs SET state = 2 WHERE job_id = ?1",
                [job_id],
            )?;
        } else if let Some(last) = links.last() {
            transaction.execute(
                "UPDATE graph_path_refresh_jobs SET projection_scan_cursor = ?2 WHERE job_id = ?1",
                params![job_id, last.as_bytes().as_slice()],
            )?;
        }
        transaction.commit()?;
        Ok(ProjectionRefreshSummary {
            links_scanned: u64::try_from(links.len())
                .map_err(|_| GraphError::NumericRange("refresh scan count"))?,
            outbox_mutations_appended: appended,
            job_completed: complete,
        })
    }

    /// Refreshes one bounded page after a volume-wide availability/reconciliation transition.
    ///
    /// The state transition commits immediately while this durable cursor fans projection updates
    /// out over bounded transactions. A newer transition resets the cursor to the beginning, so
    /// every desired document eventually reflects the latest volume state.
    ///
    /// # Errors
    ///
    /// Returns a graph error when desired-state refresh or its durable cursor cannot commit.
    pub fn refresh_volume_projection(
        &mut self,
        maximum_links: u32,
    ) -> GraphResult<ProjectionRefreshSummary> {
        if maximum_links == 0 {
            return Ok(ProjectionRefreshSummary::default());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job = transaction
            .query_row(
                "SELECT volume_id, projection_scan_cursor
                 FROM graph_volume_projection_refresh_jobs
                 WHERE state = 0 ORDER BY volume_id LIMIT 1",
                [],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<Vec<u8>>>(1)?)),
            )
            .optional()?;
        let Some((volume_blob, cursor)) = job else {
            return Ok(ProjectionRefreshSummary::default());
        };
        let volume_id = volume_id_from_blob(volume_blob)?;
        let generation: i64 = transaction.query_row(
            "SELECT generation FROM graph_volumes WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
            |row| row.get(0),
        )?;
        let links = if let Some(cursor) = cursor {
            let mut statement = transaction.prepare(
                "SELECT file_link_id FROM graph_catalog_documents
                 WHERE volume_id = ?1 AND file_link_id > ?2
                 ORDER BY file_link_id LIMIT ?3",
            )?;
            let rows = statement.query_map(
                params![
                    volume_id.as_bytes().as_slice(),
                    cursor,
                    i64::from(maximum_links),
                ],
                |row| link_id_from_blob(row.get(0)?),
            )?;
            rows.collect::<Result<Vec<_>, _>>()?
        } else {
            let mut statement = transaction.prepare(
                "SELECT file_link_id FROM graph_catalog_documents
                 WHERE volume_id = ?1 ORDER BY file_link_id LIMIT ?2",
            )?;
            let rows = statement.query_map(
                params![volume_id.as_bytes().as_slice(), i64::from(maximum_links)],
                |row| link_id_from_blob(row.get(0)?),
            )?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut appended = 0_u64;
        for link in &links {
            appended += sync_catalog_projection(
                &transaction,
                *link,
                generation,
                ProjectionOutboxMode::Durable,
            )?;
        }
        let complete = links.len()
            < usize::try_from(maximum_links)
                .map_err(|_| GraphError::NumericRange("volume refresh batch size"))?;
        if complete {
            transaction.execute(
                "UPDATE graph_volume_projection_refresh_jobs
                 SET state = 2 WHERE volume_id = ?1",
                [volume_id.as_bytes().as_slice()],
            )?;
        } else if let Some(last) = links.last() {
            transaction.execute(
                "UPDATE graph_volume_projection_refresh_jobs
                 SET projection_scan_cursor = ?2 WHERE volume_id = ?1",
                params![volume_id.as_bytes().as_slice(), last.as_bytes().as_slice()],
            )?;
        }
        transaction.commit()?;
        Ok(ProjectionRefreshSummary {
            links_scanned: u64::try_from(links.len())
                .map_err(|_| GraphError::NumericRange("volume refresh scan count"))?,
            outbox_mutations_appended: appended,
            job_completed: complete,
        })
    }

    /// Reports whether a durable path or volume projection refresh remains pending.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot inspect refresh queues.
    pub fn projection_refresh_maintenance_pending(&self) -> GraphResult<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM graph_path_refresh_jobs WHERE state = 0
                   UNION ALL
                   SELECT 1 FROM graph_volume_projection_refresh_jobs WHERE state = 0
                 )",
                [],
                |row| row.get(0),
            )
            .map_err(GraphError::from)
    }

    /// Marks a path refresh job complete without rewriting graph identity.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot update the durable queue.
    pub fn complete_refresh_job(&self, job_id: i64) -> GraphResult<bool> {
        Ok(self.connection.execute(
            "UPDATE graph_path_refresh_jobs SET state = 2 WHERE job_id = ?1 AND state = 0",
            [job_id],
        )? == 1)
    }

    /// Returns aggregate state counts for verification and benchmarks.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot query the graph.
    pub fn stats(&self) -> GraphResult<GraphStats> {
        let (live_objects, live_files, live_file_bytes, tombstoned_objects) =
            self.connection.query_row(
                "SELECT
                    coalesce(sum(CASE WHEN tombstoned = 0 THEN 1 ELSE 0 END), 0),
                    coalesce(sum(CASE WHEN tombstoned = 0 AND kind = 0 THEN 1 ELSE 0 END), 0),
                    coalesce(sum(CASE WHEN tombstoned = 0 AND kind = 0 THEN size ELSE 0 END), 0),
                    coalesce(sum(CASE WHEN tombstoned = 1 THEN 1 ELSE 0 END), 0)
                 FROM graph_file_objects",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )?;
        Ok(GraphStats {
            volumes: count(&self.connection, "SELECT count(*) FROM graph_volumes")?,
            live_objects: nonnegative_u64(live_objects, "live object count")?,
            live_files: nonnegative_u64(live_files, "live file count")?,
            live_file_bytes: nonnegative_u64(live_file_bytes, "live file bytes")?,
            tombstoned_objects: nonnegative_u64(tombstoned_objects, "tombstoned object count")?,
            links: count(&self.connection, "SELECT count(*) FROM graph_file_links")?,
            pending_refresh_jobs: count(
                &self.connection,
                "SELECT
                   (SELECT count(*) FROM graph_path_refresh_jobs WHERE state = 0) +
                   (SELECT count(*) FROM graph_volume_projection_refresh_jobs WHERE state = 0)",
            )?,
        })
    }

    /// Finds contained orphan and missing-parent corruption without walking healthy branches.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot inspect durable state.
    pub fn audit_integrity(&self, limit: u32) -> GraphResult<Vec<GraphIntegrityIssue>> {
        let mut statement = self.connection.prepare(
            "SELECT 0 AS issue_kind,
                    object.volume_id, object.file_id, NULL, NULL, NULL
             FROM graph_file_objects AS object
             LEFT JOIN graph_file_links AS link
               ON link.volume_id = object.volume_id AND link.file_id = object.file_id
             WHERE object.tombstoned = 0 AND link.file_link_id IS NULL
             UNION ALL
             SELECT 1 AS issue_kind,
                    NULL, NULL, link.file_link_id, link.parent_volume_id, link.parent_file_id
             FROM graph_file_links AS link
             LEFT JOIN graph_file_objects AS parent
               ON parent.volume_id = link.parent_volume_id AND parent.file_id = link.parent_file_id
             WHERE link.parent_volume_id IS NOT NULL AND parent.file_id IS NULL
             LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::from(limit)], |row| {
            let kind: i64 = row.get(0)?;
            match kind {
                0 => {
                    let volume = volume_id_from_blob(row.get(1)?)?;
                    let file = file_id_from_blob(row.get(2)?)?;
                    Ok(GraphIntegrityIssue::OrphanObject {
                        object_key: FileKey::new(volume, file),
                    })
                }
                1 => {
                    let link = link_id_from_blob(row.get(3)?)?;
                    let parent_volume = volume_id_from_blob(row.get(4)?)?;
                    let parent_file = file_id_from_blob(row.get(5)?)?;
                    Ok(GraphIntegrityIssue::MissingParent {
                        file_link_id: link,
                        parent_key: FileKey::new(parent_volume, parent_file),
                    })
                }
                _ => Err(rusqlite::Error::InvalidQuery),
            }
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(GraphError::from)
    }

    /// Checkpoints the write-ahead log so database-size measurements include committed state.
    ///
    /// # Errors
    ///
    /// Returns a graph error when `SQLite` cannot checkpoint the write-ahead log.
    pub fn prepare_size_measurement(&self) -> GraphResult<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA optimize;")?;
        Ok(())
    }

    /// Returns the compiled and migrated schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        GRAPH_SCHEMA_VERSION
    }
}

fn sweep_stale_observation_links(
    transaction: &Transaction<'_>,
    volume_id: VolumeId,
    scan_generation: i64,
    generation: i64,
    maximum_rows: u32,
) -> GraphResult<u64> {
    let stale_links = {
        let mut statement = transaction.prepare(
            "SELECT file_link_id, file_id FROM graph_file_links
             WHERE volume_id = ?1 AND observation_generation < ?2
             ORDER BY observation_generation, file_link_id LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                volume_id.as_bytes().as_slice(),
                scan_generation,
                i64::from(maximum_rows),
            ],
            |row| {
                Ok((
                    link_id_from_blob(row.get(0)?)?,
                    FileKey::new(volume_id, file_id_from_blob(row.get(1)?)?),
                ))
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (link, object) in &stale_links {
        remove_link_internal(transaction, *link, *object, generation, false)?;
        sync_catalog_projection(
            transaction,
            *link,
            generation,
            ProjectionOutboxMode::Durable,
        )?;
    }
    if stale_links.len()
        < usize::try_from(maximum_rows)
            .map_err(|_| GraphError::NumericRange("observation sweep size"))?
    {
        transaction.execute(
            "UPDATE graph_volume_observation_sessions SET phase = 2 WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
        )?;
    }
    u64::try_from(stale_links.len()).map_err(|_| GraphError::NumericRange("stale link count"))
}

fn sweep_stale_observation_objects(
    transaction: &Transaction<'_>,
    volume_id: VolumeId,
    scan_generation: i64,
    maximum_rows: u32,
) -> GraphResult<(u64, bool)> {
    let stale_objects = {
        let mut statement = transaction.prepare(
            "SELECT file_id FROM graph_file_objects AS object
             WHERE volume_id = ?1 AND tombstoned = 0
               AND observation_generation < ?2
               AND NOT EXISTS(
                 SELECT 1 FROM graph_file_links AS link
                 WHERE link.volume_id = object.volume_id AND link.file_id = object.file_id
               )
             ORDER BY observation_generation, file_id LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                volume_id.as_bytes().as_slice(),
                scan_generation,
                i64::from(maximum_rows),
            ],
            |row| file_id_from_blob(row.get(0)?),
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for file_id in &stale_objects {
        transaction.execute(
            "UPDATE graph_file_objects SET tombstoned = 1
             WHERE volume_id = ?1 AND file_id = ?2",
            params![
                volume_id.as_bytes().as_slice(),
                file_id.as_bytes().as_slice(),
            ],
        )?;
    }
    let terminal = stale_objects.len()
        < usize::try_from(maximum_rows)
            .map_err(|_| GraphError::NumericRange("observation sweep size"))?;
    Ok((
        u64::try_from(stale_objects.len())
            .map_err(|_| GraphError::NumericRange("stale object count"))?,
        terminal,
    ))
}

fn activate_observation_checkpoint(
    transaction: &Transaction<'_>,
    volume_id: VolumeId,
    generation: i64,
    provider_id: Option<String>,
    format_version: Option<i64>,
    opaque: Option<Vec<u8>>,
) -> GraphResult<()> {
    let provider_id = provider_id
        .ok_or_else(|| GraphError::Invariant("final observation provider is missing".to_owned()))?;
    let format_version = format_version
        .ok_or_else(|| GraphError::Invariant("final observation format is missing".to_owned()))?;
    let opaque = opaque.ok_or_else(|| {
        GraphError::Invariant("final observation checkpoint is missing".to_owned())
    })?;
    transaction.execute(
        "INSERT INTO graph_provider_checkpoints(
           volume_id, provider_id, format_version, opaque
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(volume_id) DO UPDATE SET
           provider_id = excluded.provider_id,
           format_version = excluded.format_version,
           opaque = excluded.opaque",
        params![
            volume_id.as_bytes().as_slice(),
            provider_id,
            format_version,
            opaque,
        ],
    )?;
    transaction.execute(
        "UPDATE graph_volumes
         SET state = ?2, reconciliation_reason = NULL, generation = ?3
         WHERE volume_id = ?1",
        params![
            volume_id.as_bytes().as_slice(),
            volume_state_code(VolumeState::Online),
            generation,
        ],
    )?;
    enqueue_volume_projection_refresh(transaction, volume_id, generation)?;
    transaction.execute(
        "DELETE FROM graph_volume_observation_sessions WHERE volume_id = ?1",
        [volume_id.as_bytes().as_slice()],
    )?;
    Ok(())
}

fn apply_mutation(
    transaction: &Transaction<'_>,
    mutation: &GraphMutation,
    generation: i64,
) -> GraphResult<u64> {
    match mutation {
        GraphMutation::UpsertVolume { descriptor } => {
            upsert_volume(transaction, descriptor)?;
            Ok(0)
        }
        GraphMutation::SetVolumeState { volume_id, state } => {
            require_single_update(
                transaction.execute(
                    "UPDATE graph_volumes SET state = ?2 WHERE volume_id = ?1",
                    params![volume_id.as_bytes().as_slice(), volume_state_code(*state)],
                )?,
                "volume state update references an unknown volume",
            )?;
            enqueue_volume_projection_refresh(transaction, *volume_id, generation)
        }
        GraphMutation::UpsertObject { object } => {
            upsert_object(transaction, object, generation)?;
            Ok(0)
        }
        GraphMutation::UpsertLink {
            link,
            traversal_boundary,
        } => upsert_link(transaction, link, *traversal_boundary, generation),
        GraphMutation::RemoveLink {
            file_link_id,
            object_key,
        } => remove_link(transaction, *file_link_id, *object_key, generation),
        GraphMutation::TombstoneObject { object_key } => {
            tombstone_object(transaction, *object_key)?;
            Ok(0)
        }
        GraphMutation::RequireReconciliation { volume_id, reason } => {
            require_single_update(
                transaction.execute(
                    "UPDATE graph_volumes
                     SET state = ?2, reconciliation_reason = ?3 WHERE volume_id = ?1",
                    params![
                        volume_id.as_bytes().as_slice(),
                        volume_state_code(VolumeState::NeedsReconciliation),
                        reconciliation_reason_code(*reason),
                    ],
                )?,
                "reconciliation references an unknown volume",
            )?;
            enqueue_volume_projection_refresh(transaction, *volume_id, generation)
        }
    }
}

fn collect_affected_links(
    transaction: &Transaction<'_>,
    mutation: &GraphMutation,
    affected: &mut BTreeSet<FileLinkId>,
) -> GraphResult<()> {
    match mutation {
        GraphMutation::UpsertLink { link, .. } => {
            affected.insert(link.file_link_id);
            for (stale_link, _) in stale_observation_link_conflicts(transaction, link)? {
                affected.insert(stale_link);
            }
        }
        GraphMutation::RemoveLink { file_link_id, .. } => {
            affected.insert(*file_link_id);
        }
        GraphMutation::UpsertObject { object } => {
            collect_object_link_ids(transaction, object.object_key, affected)?;
        }
        GraphMutation::TombstoneObject { object_key } => {
            collect_object_link_ids(transaction, *object_key, affected)?;
            let mut statement = transaction.prepare(
                "SELECT file_link_id FROM graph_catalog_documents
                 WHERE volume_id = ?1 AND file_id = ?2",
            )?;
            let rows = statement.query_map(
                params![
                    object_key.volume_id.as_bytes().as_slice(),
                    object_key.file_id.as_bytes().as_slice(),
                ],
                |row| link_id_from_blob(row.get(0)?),
            )?;
            for row in rows {
                affected.insert(row?);
            }
        }
        GraphMutation::SetVolumeState { .. }
        | GraphMutation::RequireReconciliation { .. }
        | GraphMutation::UpsertVolume { .. } => {}
    }
    Ok(())
}

fn collect_object_link_ids(
    transaction: &Transaction<'_>,
    object: FileKey,
    affected: &mut BTreeSet<FileLinkId>,
) -> GraphResult<()> {
    let mut statement = transaction.prepare(
        "SELECT file_link_id FROM graph_file_links WHERE volume_id = ?1 AND file_id = ?2",
    )?;
    let rows = statement.query_map(
        params![
            object.volume_id.as_bytes().as_slice(),
            object.file_id.as_bytes().as_slice(),
        ],
        |row| link_id_from_blob(row.get(0)?),
    )?;
    for row in rows {
        affected.insert(row?);
    }
    Ok(())
}

fn sync_catalog_projection(
    transaction: &Transaction<'_>,
    file_link_id: FileLinkId,
    generation: i64,
    outbox_mode: ProjectionOutboxMode,
) -> GraphResult<u64> {
    let document_id = document_id_for_link(file_link_id);
    let version =
        u64::try_from(generation).map_err(|_| GraphError::NumericRange("document version"))?;
    let Some(link) = load_link(transaction, file_link_id)? else {
        return delete_catalog_projection(transaction, document_id, version, outbox_mode);
    };

    let document = match build_catalog_document(transaction, &link, version) {
        Ok(document) => document,
        Err(
            GraphError::MissingParent(_)
            | GraphError::AmbiguousParent(_)
            | GraphError::ParentCycle(_)
            | GraphError::TraversalBoundary(_)
            | GraphError::DepthLimit(_),
        ) => return delete_catalog_projection(transaction, document_id, version, outbox_mode),
        Err(error) => return Err(error),
    };
    let fingerprint = catalog_content_fingerprint(&document)?;
    let previous = transaction
        .query_row(
            "SELECT projection_fingerprint, document_json
             FROM graph_catalog_documents WHERE document_id = ?1",
            [document_id.as_bytes().as_slice()],
            |row| Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let content_unchanged = match previous {
        Some((Some(previous_fingerprint), _)) => previous_fingerprint == fingerprint,
        Some((None, legacy_payload)) => {
            let previous_document = serde_json::from_str::<CatalogDocument>(&legacy_payload)?;
            catalog_content_equal(&previous_document, &document)
        }
        None => false,
    };
    if content_unchanged {
        return Ok(0);
    }
    transaction.execute(
        "INSERT INTO graph_catalog_documents(
           document_id, volume_id, file_id, file_link_id, document_version,
           document_json, projection_path, projection_fingerprint
         ) VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, ?7)
         ON CONFLICT(document_id) DO UPDATE SET
           volume_id = excluded.volume_id,
           file_id = excluded.file_id,
           file_link_id = excluded.file_link_id,
           document_version = excluded.document_version,
           document_json = excluded.document_json,
           projection_path = excluded.projection_path,
           projection_fingerprint = excluded.projection_fingerprint",
        params![
            document_id.as_bytes().as_slice(),
            link.object_key.volume_id.as_bytes().as_slice(),
            link.object_key.file_id.as_bytes().as_slice(),
            file_link_id.as_bytes().as_slice(),
            generation,
            &document.resolved_path,
            fingerprint.as_slice(),
        ],
    )?;
    if matches!(outbox_mode, ProjectionOutboxMode::Durable) {
        append_outbox(
            transaction,
            document_id,
            version,
            &IndexMutation::Upsert { document },
        )?;
        Ok(1)
    } else {
        Ok(0)
    }
}

fn delete_catalog_projection(
    transaction: &Transaction<'_>,
    document_id: DocumentId,
    version: u64,
    outbox_mode: ProjectionOutboxMode,
) -> GraphResult<u64> {
    let existed = transaction
        .query_row(
            "SELECT 1 FROM graph_catalog_documents WHERE document_id = ?1",
            [document_id.as_bytes().as_slice()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !existed {
        return Ok(0);
    }
    if matches!(outbox_mode, ProjectionOutboxMode::Durable) {
        append_outbox(
            transaction,
            document_id,
            version,
            &IndexMutation::Delete {
                document_id,
                document_version: DocumentVersion(version),
            },
        )?;
    }
    transaction.execute(
        "DELETE FROM graph_catalog_documents WHERE document_id = ?1",
        [document_id.as_bytes().as_slice()],
    )?;
    Ok(u64::from(matches!(
        outbox_mode,
        ProjectionOutboxMode::Durable
    )))
}

fn projection_consumers_exist(connection: &Connection) -> GraphResult<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM graph_projection_consumers LIMIT 1)",
            [],
            |row| row.get(0),
        )
        .map_err(GraphError::from)
}

struct StoredCatalogProjection {
    document_id: DocumentId,
    object_key: FileKey,
    file_link_id: FileLinkId,
    document_version: u64,
    projection_path: Option<String>,
    legacy_document_json: String,
    name: String,
    metadata: FileMetadata,
    volume_state: VolumeState,
}

impl StoredCatalogProjection {
    fn into_document(self) -> GraphResult<CatalogDocument> {
        let Some(resolved_path) = self.projection_path else {
            return serde_json::from_str(&self.legacy_document_json).map_err(GraphError::from);
        };
        let mut metadata = self.metadata;
        metadata.availability = match self.volume_state {
            VolumeState::Online => metadata.availability,
            VolumeState::Offline => Availability::Offline,
            VolumeState::NeedsReconciliation => Availability::Unknown,
        };
        Ok(CatalogDocument {
            identity: CatalogIdentity::new(self.object_key, self.file_link_id, self.document_id),
            document_version: DocumentVersion(self.document_version),
            extension: extension_for_name(&self.name),
            name: self.name,
            resolved_path,
            metadata,
        })
    }
}

fn stored_catalog_projection_from_row(row: &Row<'_>) -> rusqlite::Result<StoredCatalogProjection> {
    let document_id = DocumentId::from_bytes(array_from_blob(row.get(0)?)?);
    let volume_id = volume_id_from_blob(row.get(1)?)?;
    let file_id = file_id_from_blob(row.get(2)?)?;
    let document_version: i64 = row.get(4)?;
    let size: i64 = row.get(9)?;
    Ok(StoredCatalogProjection {
        document_id,
        object_key: FileKey::new(volume_id, file_id),
        file_link_id: link_id_from_blob(row.get(3)?)?,
        document_version: u64::try_from(document_version)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, document_version))?,
        projection_path: row.get(5)?,
        legacy_document_json: row.get(6)?,
        name: row.get(7)?,
        metadata: FileMetadata {
            kind: file_kind_from_code(row.get(8)?)?,
            size: u64::try_from(size)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, size))?,
            created_at_unix_ms: row.get(10)?,
            modified_at_unix_ms: row.get(11)?,
            hidden: row.get(12)?,
            availability: availability_from_code(row.get(13)?)?,
        },
        volume_state: volume_state_from_code(row.get(14)?)?,
    })
}

fn desired_catalog_document_connection(
    connection: &Connection,
    document_id: DocumentId,
) -> GraphResult<Option<CatalogDocument>> {
    connection
        .query_row(
            "SELECT d.document_id, d.volume_id, d.file_id, d.file_link_id,
                    d.document_version, d.projection_path, d.document_json,
                    link.name, object.kind, object.size, object.created_at_unix_ms,
                    object.modified_at_unix_ms, object.hidden, object.availability, volume.state
             FROM graph_catalog_documents AS d
             JOIN graph_file_links AS link ON link.file_link_id = d.file_link_id
             JOIN graph_file_objects AS object
               ON object.volume_id = d.volume_id AND object.file_id = d.file_id
             JOIN graph_volumes AS volume ON volume.volume_id = d.volume_id
             WHERE d.document_id = ?1",
            [document_id.as_bytes().as_slice()],
            stored_catalog_projection_from_row,
        )
        .optional()?
        .map(StoredCatalogProjection::into_document)
        .transpose()
}

fn extension_for_name(name: &str) -> Option<String> {
    name.rsplit_once('.')
        .and_then(|(_, extension)| (!extension.is_empty()).then(|| extension.to_lowercase()))
}

fn build_catalog_document(
    connection: &Connection,
    link: &StoredLink,
    version: u64,
) -> GraphResult<CatalogDocument> {
    let mut metadata = load_object_metadata(connection, link.object_key)?;
    metadata.availability = match volume_state(connection, link.object_key.volume_id)? {
        VolumeState::Online => metadata.availability,
        VolumeState::Offline => Availability::Offline,
        VolumeState::NeedsReconciliation => Availability::Unknown,
    };
    let path = resolve_path_connection(connection, link.file_link_id, 512)?.display();
    let extension = extension_for_name(&link.name);
    let document_id = document_id_for_link(link.file_link_id);
    Ok(CatalogDocument {
        identity: CatalogIdentity::new(link.object_key, link.file_link_id, document_id),
        document_version: DocumentVersion(version),
        name: link.name.clone(),
        resolved_path: path,
        extension,
        metadata,
    })
}

fn volume_state(connection: &Connection, volume_id: VolumeId) -> GraphResult<VolumeState> {
    connection
        .query_row(
            "SELECT state FROM graph_volumes WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
            |row| volume_state_from_code(row.get(0)?),
        )
        .optional()?
        .ok_or_else(|| {
            GraphError::Invariant("catalog object references a missing volume".to_owned())
        })
}

fn load_object_metadata(connection: &Connection, object: FileKey) -> GraphResult<FileMetadata> {
    connection
        .query_row(
            "SELECT kind, size, created_at_unix_ms, modified_at_unix_ms, hidden, availability
             FROM graph_file_objects
             WHERE volume_id = ?1 AND file_id = ?2 AND tombstoned = 0",
            params![
                object.volume_id.as_bytes().as_slice(),
                object.file_id.as_bytes().as_slice(),
            ],
            |row| {
                let size: i64 = row.get(1)?;
                Ok(FileMetadata {
                    kind: file_kind_from_code(row.get(0)?)?,
                    size: u64::try_from(size)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, size))?,
                    created_at_unix_ms: row.get(2)?,
                    modified_at_unix_ms: row.get(3)?,
                    hidden: row.get(4)?,
                    availability: availability_from_code(row.get(5)?)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| GraphError::Invariant("catalog link references a missing object".to_owned()))
}

fn append_outbox(
    transaction: &Transaction<'_>,
    document_id: DocumentId,
    document_version: u64,
    mutation: &IndexMutation,
) -> GraphResult<()> {
    transaction.execute(
        "INSERT INTO graph_projection_outbox(document_id, document_version, mutation_json)
         VALUES (?1, ?2, ?3)",
        params![
            document_id.as_bytes().as_slice(),
            i64::try_from(document_version)
                .map_err(|_| GraphError::NumericRange("document version"))?,
            serde_json::to_string(mutation)?,
        ],
    )?;
    Ok(())
}

const fn document_id_for_link(file_link_id: FileLinkId) -> DocumentId {
    DocumentId::from_bytes(file_link_id.into_bytes())
}

fn catalog_content_equal(left: &CatalogDocument, right: &CatalogDocument) -> bool {
    left.identity == right.identity
        && left.name == right.name
        && left.resolved_path == right.resolved_path
        && left.extension == right.extension
        && left.metadata == right.metadata
}

fn catalog_content_fingerprint(document: &CatalogDocument) -> GraphResult<[u8; 32]> {
    let payload = serde_json::to_vec(&(
        &document.identity,
        &document.name,
        &document.resolved_path,
        &document.extension,
        &document.metadata,
    ))?;
    Ok(Sha256::digest(payload).into())
}

fn upsert_volume(transaction: &Transaction<'_>, descriptor: &VolumeDescriptor) -> GraphResult<()> {
    let mount_points = serde_json::to_string(&descriptor.mount_points)
        .map_err(|error| GraphError::InvalidBatch(error.to_string()))?;
    transaction.execute(
        "INSERT INTO graph_volumes(
           volume_id, display_name, mount_points_json, filesystem, removable, local, state
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(volume_id) DO UPDATE SET
           display_name = excluded.display_name,
           mount_points_json = excluded.mount_points_json,
           filesystem = excluded.filesystem,
           removable = excluded.removable,
           local = excluded.local,
           state = excluded.state,
           reconciliation_reason = NULL",
        params![
            descriptor.volume_id.as_bytes().as_slice(),
            descriptor.display_name,
            mount_points,
            descriptor.filesystem,
            descriptor.removable,
            descriptor.local,
            volume_state_code(VolumeState::Online),
        ],
    )?;
    Ok(())
}

fn upsert_object(
    transaction: &Transaction<'_>,
    object: &FileObjectSnapshot,
    generation: i64,
) -> GraphResult<()> {
    let size =
        i64::try_from(object.metadata.size).map_err(|_| GraphError::NumericRange("file size"))?;
    transaction.execute(
        "INSERT INTO graph_file_objects(
           volume_id, file_id, kind, size, created_at_unix_ms, modified_at_unix_ms,
           hidden, availability, tombstoned, observation_generation
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)
         ON CONFLICT(volume_id, file_id) DO UPDATE SET
           kind = excluded.kind,
           size = excluded.size,
           created_at_unix_ms = excluded.created_at_unix_ms,
           modified_at_unix_ms = excluded.modified_at_unix_ms,
           hidden = excluded.hidden,
           availability = excluded.availability,
           tombstoned = 0,
           observation_generation = excluded.observation_generation",
        params![
            object.object_key.volume_id.as_bytes().as_slice(),
            object.object_key.file_id.as_bytes().as_slice(),
            file_kind_code(object.metadata.kind),
            size,
            object.metadata.created_at_unix_ms,
            object.metadata.modified_at_unix_ms,
            object.metadata.hidden,
            availability_code(object.metadata.availability),
            generation,
        ],
    )?;
    Ok(())
}

fn stale_observation_link_conflicts(
    transaction: &Transaction<'_>,
    link: &FileLinkSnapshot,
) -> GraphResult<Vec<(FileLinkId, FileKey)>> {
    let cutoff = transaction
        .query_row(
            "SELECT scan_generation FROM graph_volume_observation_sessions
             WHERE volume_id = ?1 AND phase = 0",
            [link.object_key.volume_id.as_bytes().as_slice()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(cutoff) = cutoff else {
        return Ok(Vec::new());
    };
    let mut conflicts = Vec::new();
    if let Some((volume_blob, file_blob, observed)) = transaction
        .query_row(
            "SELECT volume_id, file_id, observation_generation
             FROM graph_file_links WHERE file_link_id = ?1",
            [link.file_link_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?
    {
        let old_volume = volume_id_from_blob(volume_blob)?;
        let old_file = file_id_from_blob(file_blob)?;
        let old_object = FileKey::new(old_volume, old_file);
        if old_object != link.object_key && observed < cutoff {
            conflicts.push((link.file_link_id, old_object));
        }
    }
    let namespace_conflict = if let Some(parent) = link.parent_key {
        transaction
            .query_row(
                "SELECT file_link_id, volume_id, file_id FROM graph_file_links
                 WHERE parent_volume_id = ?1 AND parent_file_id = ?2 AND name = ?3
                   AND file_link_id <> ?4 AND observation_generation < ?5",
                params![
                    parent.volume_id.as_bytes().as_slice(),
                    parent.file_id.as_bytes().as_slice(),
                    link.name,
                    link.file_link_id.as_bytes().as_slice(),
                    cutoff,
                ],
                |row| {
                    Ok((
                        link_id_from_blob(row.get(0)?)?,
                        FileKey::new(
                            volume_id_from_blob(row.get(1)?)?,
                            file_id_from_blob(row.get(2)?)?,
                        ),
                    ))
                },
            )
            .optional()?
    } else {
        transaction
            .query_row(
                "SELECT file_link_id, volume_id, file_id FROM graph_file_links
                 WHERE volume_id = ?1 AND parent_volume_id IS NULL AND name = ?2
                   AND file_link_id <> ?3 AND observation_generation < ?4",
                params![
                    link.object_key.volume_id.as_bytes().as_slice(),
                    link.name,
                    link.file_link_id.as_bytes().as_slice(),
                    cutoff,
                ],
                |row| {
                    Ok((
                        link_id_from_blob(row.get(0)?)?,
                        FileKey::new(
                            volume_id_from_blob(row.get(1)?)?,
                            file_id_from_blob(row.get(2)?)?,
                        ),
                    ))
                },
            )
            .optional()?
    };
    if let Some(conflict) = namespace_conflict {
        conflicts.push(conflict);
    }
    Ok(conflicts)
}

fn upsert_link(
    transaction: &Transaction<'_>,
    link: &FileLinkSnapshot,
    traversal_boundary: bool,
    generation: i64,
) -> GraphResult<u64> {
    let object_exists = transaction
        .query_row(
            "SELECT 1 FROM graph_file_objects WHERE volume_id = ?1 AND file_id = ?2",
            params![
                link.object_key.volume_id.as_bytes().as_slice(),
                link.object_key.file_id.as_bytes().as_slice(),
            ],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !object_exists {
        return Err(GraphError::Invariant(
            "link observation precedes its physical object".to_owned(),
        ));
    }

    for (stale_link, stale_object) in stale_observation_link_conflicts(transaction, link)? {
        remove_link_internal(transaction, stale_link, stale_object, generation, false)?;
    }

    let existing = transaction
        .query_row(
            "SELECT volume_id, file_id, parent_volume_id, parent_file_id, name
             FROM graph_file_links WHERE file_link_id = ?1",
            [link.file_link_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let mut refresh_jobs = 0;
    if let Some((volume, file, old_parent_volume, old_parent_file, old_name)) = existing {
        if volume.as_slice() != link.object_key.volume_id.as_bytes()
            || file.as_slice() != link.object_key.file_id.as_bytes()
        {
            return Err(GraphError::Invariant(
                "a file-link ID cannot be rebound to another object".to_owned(),
            ));
        }
        let expected_parent = parent_owned_blobs(link.parent_key);
        let parent_changed = (expected_parent.0.as_deref(), expected_parent.1.as_deref())
            != (old_parent_volume.as_deref(), old_parent_file.as_deref());
        if (parent_changed || old_name != link.name) && is_directory(transaction, link.object_key)?
        {
            refresh_jobs += enqueue_refresh(transaction, link.object_key, generation)?;
        }
    }

    let (parent_volume, parent_file) = parent_owned_blobs(link.parent_key);
    transaction.execute(
        "INSERT INTO graph_file_links(
           file_link_id, volume_id, file_id, parent_volume_id, parent_file_id, name,
           traversal_boundary, observation_generation
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(file_link_id) DO UPDATE SET
           parent_volume_id = excluded.parent_volume_id,
           parent_file_id = excluded.parent_file_id,
           name = excluded.name,
           traversal_boundary = excluded.traversal_boundary,
           observation_generation = excluded.observation_generation",
        params![
            link.file_link_id.as_bytes().as_slice(),
            link.object_key.volume_id.as_bytes().as_slice(),
            link.object_key.file_id.as_bytes().as_slice(),
            parent_volume,
            parent_file,
            link.name,
            traversal_boundary,
            generation,
        ],
    )?;
    transaction.execute(
        "UPDATE graph_file_objects SET tombstoned = 0 WHERE volume_id = ?1 AND file_id = ?2",
        params![
            link.object_key.volume_id.as_bytes().as_slice(),
            link.object_key.file_id.as_bytes().as_slice(),
        ],
    )?;
    Ok(refresh_jobs)
}

fn remove_link(
    transaction: &Transaction<'_>,
    file_link_id: FileLinkId,
    expected: FileKey,
    generation: i64,
) -> GraphResult<u64> {
    remove_link_internal(transaction, file_link_id, expected, generation, true)
}

fn remove_link_internal(
    transaction: &Transaction<'_>,
    file_link_id: FileLinkId,
    expected: FileKey,
    generation: i64,
    refresh_directory_paths: bool,
) -> GraphResult<u64> {
    let existing = transaction
        .query_row(
            "SELECT volume_id, file_id FROM graph_file_links WHERE file_link_id = ?1",
            [file_link_id.as_bytes().as_slice()],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    let Some((volume, file)) = existing else {
        return Ok(0);
    };
    if volume.as_slice() != expected.volume_id.as_bytes()
        || file.as_slice() != expected.file_id.as_bytes()
    {
        return Err(GraphError::Invariant(
            "link removal object identity does not match durable state".to_owned(),
        ));
    }

    let directory = is_directory(transaction, expected)?;
    transaction.execute(
        "DELETE FROM graph_file_links WHERE file_link_id = ?1",
        [file_link_id.as_bytes().as_slice()],
    )?;
    let remaining: i64 = transaction.query_row(
        "SELECT count(*) FROM graph_file_links WHERE volume_id = ?1 AND file_id = ?2",
        params![
            expected.volume_id.as_bytes().as_slice(),
            expected.file_id.as_bytes().as_slice(),
        ],
        |row| row.get(0),
    )?;
    if remaining == 0 {
        transaction.execute(
            "UPDATE graph_file_objects SET tombstoned = 1 WHERE volume_id = ?1 AND file_id = ?2",
            params![
                expected.volume_id.as_bytes().as_slice(),
                expected.file_id.as_bytes().as_slice(),
            ],
        )?;
    }
    if directory && refresh_directory_paths {
        enqueue_refresh(transaction, expected, generation)
    } else {
        Ok(0)
    }
}

fn tombstone_object(transaction: &Transaction<'_>, object: FileKey) -> GraphResult<()> {
    let links: i64 = transaction.query_row(
        "SELECT count(*) FROM graph_file_links WHERE volume_id = ?1 AND file_id = ?2",
        params![
            object.volume_id.as_bytes().as_slice(),
            object.file_id.as_bytes().as_slice(),
        ],
        |row| row.get(0),
    )?;
    if links != 0 {
        return Err(GraphError::Invariant(
            "object cannot be tombstoned while namespace links remain".to_owned(),
        ));
    }
    require_single_update(
        transaction.execute(
            "UPDATE graph_file_objects SET tombstoned = 1 WHERE volume_id = ?1 AND file_id = ?2",
            params![
                object.volume_id.as_bytes().as_slice(),
                object.file_id.as_bytes().as_slice(),
            ],
        )?,
        "object tombstone references an unknown object",
    )
}

fn enqueue_refresh(
    transaction: &Transaction<'_>,
    object: FileKey,
    generation: i64,
) -> GraphResult<u64> {
    let inserted = transaction.execute(
        "INSERT OR IGNORE INTO graph_path_refresh_jobs(
           volume_id, root_file_id, enqueued_generation, state
         ) VALUES (?1, ?2, ?3, 0)",
        params![
            object.volume_id.as_bytes().as_slice(),
            object.file_id.as_bytes().as_slice(),
            generation,
        ],
    )?;
    u64::try_from(inserted).map_err(|_| GraphError::NumericRange("refresh job count"))
}

fn enqueue_volume_projection_refresh(
    transaction: &Transaction<'_>,
    volume_id: VolumeId,
    generation: i64,
) -> GraphResult<u64> {
    let changed = transaction.execute(
        "INSERT INTO graph_volume_projection_refresh_jobs(
           volume_id, enqueued_generation, projection_scan_cursor, state
         ) VALUES (?1, ?2, NULL, 0)
         ON CONFLICT(volume_id) DO UPDATE SET
           enqueued_generation = excluded.enqueued_generation,
           projection_scan_cursor = NULL,
           state = 0",
        params![volume_id.as_bytes().as_slice(), generation],
    )?;
    u64::try_from(changed).map_err(|_| GraphError::NumericRange("volume refresh job count"))
}

fn is_directory(transaction: &Transaction<'_>, object: FileKey) -> GraphResult<bool> {
    let kind = transaction
        .query_row(
            "SELECT kind FROM graph_file_objects WHERE volume_id = ?1 AND file_id = ?2",
            params![
                object.volume_id.as_bytes().as_slice(),
                object.file_id.as_bytes().as_slice(),
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| {
            GraphError::Invariant("directory check references unknown object".to_owned())
        })?;
    Ok(kind == file_kind_code(FileKind::Directory))
}

#[derive(Debug)]
struct StoredLink {
    file_link_id: FileLinkId,
    object_key: FileKey,
    parent_key: Option<FileKey>,
    name: String,
    traversal_boundary: bool,
}

fn load_link(connection: &Connection, id: FileLinkId) -> GraphResult<Option<StoredLink>> {
    connection
        .query_row(
            "SELECT volume_id, file_id, parent_volume_id, parent_file_id, name, traversal_boundary
             FROM graph_file_links WHERE file_link_id = ?1",
            [id.as_bytes().as_slice()],
            |row| stored_link_from_row(id, row),
        )
        .optional()
        .map_err(GraphError::from)
}

fn load_only_link_for_object(connection: &Connection, object: FileKey) -> GraphResult<StoredLink> {
    let mut statement = connection.prepare(
        "SELECT file_link_id, parent_volume_id, parent_file_id, name, traversal_boundary
         FROM graph_file_links WHERE volume_id = ?1 AND file_id = ?2 ORDER BY file_link_id LIMIT 2",
    )?;
    let mut rows = statement.query(params![
        object.volume_id.as_bytes().as_slice(),
        object.file_id.as_bytes().as_slice(),
    ])?;
    let Some(first) = rows.next()? else {
        return Err(GraphError::MissingParent(object));
    };
    let link_id = link_id_from_blob(first.get(0)?)?;
    let result = StoredLink {
        file_link_id: link_id,
        object_key: object,
        parent_key: parent_from_columns(first, 1, 2)?,
        name: first.get(3)?,
        traversal_boundary: first.get(4)?,
    };
    if rows.next()?.is_some() {
        return Err(GraphError::AmbiguousParent(object));
    }
    Ok(result)
}

fn stored_link_from_row(id: FileLinkId, row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredLink> {
    let volume = volume_id_from_blob(row.get(0)?)?;
    let file = file_id_from_blob(row.get(1)?)?;
    Ok(StoredLink {
        file_link_id: id,
        object_key: FileKey::new(volume, file),
        parent_key: parent_from_columns(row, 2, 3)?,
        name: row.get(4)?,
        traversal_boundary: row.get(5)?,
    })
}

fn resolve_path_connection(
    connection: &Connection,
    file_link_id: FileLinkId,
    depth_limit: usize,
) -> GraphResult<ResolvedPath> {
    if depth_limit == 0 {
        return Err(GraphError::DepthLimit(0));
    }
    let mut current =
        load_link(connection, file_link_id)?.ok_or(GraphError::LinkNotFound(file_link_id))?;
    let mut components = Vec::new();
    let mut seen = HashSet::new();
    let mut is_target = true;

    loop {
        if components.len() >= depth_limit {
            return Err(GraphError::DepthLimit(depth_limit));
        }
        if current.traversal_boundary && !is_target {
            return Err(GraphError::TraversalBoundary(current.file_link_id));
        }
        components.push(current.name);
        let Some(parent) = current.parent_key else {
            break;
        };
        if !seen.insert(parent) {
            return Err(GraphError::ParentCycle(parent));
        }
        current = load_only_link_for_object(connection, parent)?;
        is_target = false;
    }
    components.reverse();
    Ok(ResolvedPath { components })
}

fn parent_from_columns(
    row: &rusqlite::Row<'_>,
    volume_index: usize,
    file_index: usize,
) -> rusqlite::Result<Option<FileKey>> {
    let volume: Option<Vec<u8>> = row.get(volume_index)?;
    let file: Option<Vec<u8>> = row.get(file_index)?;
    match (volume, file) {
        (None, None) => Ok(None),
        (Some(volume), Some(file)) => Ok(Some(FileKey::new(
            volume_id_from_blob(volume)?,
            file_id_from_blob(file)?,
        ))),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn volume_id_from_blob(blob: Vec<u8>) -> rusqlite::Result<VolumeId> {
    Ok(VolumeId::from_bytes(array_from_blob(blob)?))
}

fn file_id_from_blob(blob: Vec<u8>) -> rusqlite::Result<FileId128> {
    Ok(FileId128::from_bytes(array_from_blob(blob)?))
}

fn link_id_from_blob(blob: Vec<u8>) -> rusqlite::Result<FileLinkId> {
    Ok(FileLinkId::from_bytes(array_from_blob(blob)?))
}

fn array_from_blob(blob: Vec<u8>) -> rusqlite::Result<[u8; 16]> {
    blob.try_into().map_err(|_| rusqlite::Error::InvalidQuery)
}

fn parent_owned_blobs(parent: Option<FileKey>) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    parent.map_or((None, None), |key| {
        (
            Some(key.volume_id.as_bytes().to_vec()),
            Some(key.file_id.as_bytes().to_vec()),
        )
    })
}

fn require_single_update(updated: usize, message: &str) -> GraphResult<()> {
    if updated == 1 {
        Ok(())
    } else {
        Err(GraphError::Invariant(message.to_owned()))
    }
}

fn count(connection: &Connection, sql: &str) -> GraphResult<u64> {
    aggregate_u64(connection, sql, "aggregate count")
}

fn aggregate_u64(connection: &Connection, sql: &str, field: &'static str) -> GraphResult<u64> {
    let value: i64 = connection.query_row(sql, [], |row| row.get(0))?;
    nonnegative_u64(value, field)
}

fn nonnegative_u64(value: i64, field: &'static str) -> GraphResult<u64> {
    u64::try_from(value).map_err(|_| GraphError::NumericRange(field))
}

const fn file_kind_code(kind: FileKind) -> i64 {
    match kind {
        FileKind::File => 0,
        FileKind::Directory => 1,
        FileKind::Symlink => 2,
        FileKind::Special => 3,
        FileKind::Other => 4,
    }
}

fn file_kind_from_code(code: i64) -> rusqlite::Result<FileKind> {
    match code {
        0 => Ok(FileKind::File),
        1 => Ok(FileKind::Directory),
        2 => Ok(FileKind::Symlink),
        3 => Ok(FileKind::Special),
        4 => Ok(FileKind::Other),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(0, code)),
    }
}

const fn availability_code(availability: Availability) -> i64 {
    match availability {
        Availability::Online => 0,
        Availability::Offline => 1,
        Availability::Unknown => 2,
    }
}

fn availability_from_code(code: i64) -> rusqlite::Result<Availability> {
    match code {
        0 => Ok(Availability::Online),
        1 => Ok(Availability::Offline),
        2 => Ok(Availability::Unknown),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(0, code)),
    }
}

const fn volume_state_code(state: VolumeState) -> i64 {
    match state {
        VolumeState::Online => 0,
        VolumeState::Offline => 1,
        VolumeState::NeedsReconciliation => 2,
    }
}

fn volume_state_from_code(code: i64) -> rusqlite::Result<VolumeState> {
    match code {
        0 => Ok(VolumeState::Online),
        1 => Ok(VolumeState::Offline),
        2 => Ok(VolumeState::NeedsReconciliation),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(0, code)),
    }
}

const fn observation_scan_mode_code(mode: ObservationScanMode) -> i64 {
    match mode {
        ObservationScanMode::Initial => 0,
        ObservationScanMode::Reconcile => 1,
    }
}

fn observation_scan_mode_from_code(code: i64) -> rusqlite::Result<ObservationScanMode> {
    match code {
        0 => Ok(ObservationScanMode::Initial),
        1 => Ok(ObservationScanMode::Reconcile),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(0, code)),
    }
}

fn observation_scan_phase_from_code(code: i64) -> rusqlite::Result<ObservationScanPhase> {
    match code {
        0 => Ok(ObservationScanPhase::Scanning),
        1 => Ok(ObservationScanPhase::SweepingLinks),
        2 => Ok(ObservationScanPhase::SweepingObjects),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(0, code)),
    }
}

fn observation_scan_checkpoint(session: ObservationSession) -> ProviderCheckpoint {
    ProviderCheckpoint {
        provider_id: "localsearch-observation-scan".to_owned(),
        format_version: 1,
        volume_id: session.volume_id,
        opaque: session.scan_generation.to_be_bytes().to_vec(),
    }
}

const fn reconciliation_reason_code(reason: ReconciliationReason) -> i64 {
    match reason {
        ReconciliationReason::SourceHistoryUnavailable => 0,
        ReconciliationReason::EventOverflow => 1,
        ReconciliationReason::InconsistentSnapshot => 2,
        ReconciliationReason::ProviderRequested => 3,
    }
}

#[cfg(test)]
mod tests {
    use localsearch_core::{
        Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
        FileObjectSnapshot, FilesystemEvent, VolumeId,
    };
    use localsearch_platform_core::{ProviderCheckpoint, VolumeDescriptor};
    use rusqlite::{Connection, params};

    use super::FilesystemGraph;
    use crate::{
        GRAPH_SCHEMA_VERSION,
        migrations::{MIGRATION_001, MIGRATION_002},
    };

    #[test]
    fn start_005_schema_migrates_forward_without_rebuild() {
        let connection = Connection::open_in_memory().expect("connection");
        connection
            .execute_batch(MIGRATION_001)
            .expect("START-005 schema");
        let graph = FilesystemGraph::initialize(connection, false).expect("migrate to START-006");
        assert_eq!(graph.schema_version(), GRAPH_SCHEMA_VERSION);
        let tables: i64 = graph
            .connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN (
                   'graph_catalog_documents',
                   'graph_projection_outbox',
                   'graph_projection_consumers'
                 )",
                [],
                |row| row.get(0),
            )
            .expect("projection tables");
        assert_eq!(tables, 3);
    }

    #[test]
    fn start_006_schema_adds_compact_projection_columns_without_rebuild() {
        let connection = Connection::open_in_memory().expect("connection");
        connection
            .execute_batch(&format!("{MIGRATION_001}{MIGRATION_002}"))
            .expect("START-006 schema");
        let graph = FilesystemGraph::initialize(connection, false).expect("migrate to START-014");
        assert_eq!(graph.schema_version(), GRAPH_SCHEMA_VERSION);
        let compact_columns: i64 = graph
            .connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('graph_catalog_documents')
                 WHERE name IN ('projection_path', 'projection_fingerprint')",
                [],
                |row| row.get(0),
            )
            .expect("compact columns");
        assert_eq!(compact_columns, 2);
        let volume_refresh_objects: i64 = graph
            .connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE (type = 'table' AND name = 'graph_volume_projection_refresh_jobs')
                    OR (type = 'index' AND name IN (
                      'graph_catalog_documents_volume_link', 'graph_file_links_volume_link'
                    ))",
                [],
                |row| row.get(0),
            )
            .expect("volume refresh schema");
        assert_eq!(volume_refresh_objects, 3);
        let observation_objects: i64 = graph
            .connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE (type = 'table' AND name = 'graph_volume_observation_sessions')
                    OR (type = 'index' AND name IN (
                      'graph_file_links_observation_sweep',
                      'graph_file_objects_observation_sweep'
                    ))",
                [],
                |row| row.get(0),
            )
            .expect("observation schema");
        assert_eq!(observation_objects, 3);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the migration fixture keeps its complete legacy row lifecycle visible"
    )]
    fn legacy_desired_payload_compaction_is_bounded_and_lossless() {
        let volume_id = VolumeId::from_u128(1);
        let root = FileKey::new(volume_id, FileId128::from_u128(1));
        let file = FileKey::new(volume_id, FileId128::from_u128(2));
        let mut graph = FilesystemGraph::open_in_memory().expect("graph");
        graph
            .ingest_snapshot(
                VolumeDescriptor {
                    volume_id,
                    display_name: Some("compact".to_owned()),
                    mount_points: vec!["root".to_owned()],
                    filesystem: Some("test".to_owned()),
                    removable: false,
                    local: true,
                },
                ProviderCheckpoint {
                    provider_id: "compact-test".to_owned(),
                    format_version: 1,
                    volume_id,
                    opaque: vec![1],
                },
                [
                    FilesystemEvent::ObjectObserved {
                        object: FileObjectSnapshot {
                            object_key: root,
                            metadata: FileMetadata {
                                kind: FileKind::Directory,
                                size: 0,
                                created_at_unix_ms: None,
                                modified_at_unix_ms: None,
                                hidden: false,
                                availability: Availability::Online,
                            },
                        },
                    },
                    FilesystemEvent::LinkObserved {
                        link: FileLinkSnapshot {
                            file_link_id: FileLinkId::from_u128(1),
                            object_key: root,
                            parent_key: None,
                            name: "root".to_owned(),
                        },
                    },
                    FilesystemEvent::ObjectObserved {
                        object: FileObjectSnapshot {
                            object_key: file,
                            metadata: FileMetadata {
                                kind: FileKind::File,
                                size: 42,
                                created_at_unix_ms: None,
                                modified_at_unix_ms: Some(7),
                                hidden: false,
                                availability: Availability::Online,
                            },
                        },
                    },
                    FilesystemEvent::LinkObserved {
                        link: FileLinkSnapshot {
                            file_link_id: FileLinkId::from_u128(2),
                            object_key: file,
                            parent_key: Some(root),
                            name: "note.txt".to_owned(),
                        },
                    },
                ],
            )
            .expect("snapshot");
        let before = graph
            .desired_catalog_documents()
            .expect("desired documents");
        for document in &before {
            graph
                .connection
                .execute(
                    "UPDATE graph_catalog_documents
                     SET projection_path = NULL, projection_fingerprint = NULL, document_json = ?2
                     WHERE document_id = ?1",
                    params![
                        document.identity.document_id.as_bytes().as_slice(),
                        serde_json::to_string(document).expect("legacy JSON"),
                    ],
                )
                .expect("restore legacy row");
        }

        let first = graph
            .compact_legacy_desired_payloads_bounded(1)
            .expect("first compact page");
        assert_eq!(first.rewritten_rows, 1);
        assert!(first.backlog_remaining);
        let second = graph
            .compact_legacy_desired_payloads_bounded(1)
            .expect("second compact page");
        assert_eq!(second.rewritten_rows, 1);
        assert!(!second.backlog_remaining);
        assert_eq!(
            graph
                .desired_catalog_documents()
                .expect("compacted desired"),
            before
        );
        let redundant_bytes: i64 = graph
            .connection
            .query_row(
                "SELECT coalesce(sum(length(document_json)), 0)
                 FROM graph_catalog_documents",
                [],
                |row| row.get(0),
            )
            .expect("redundant bytes");
        assert_eq!(redundant_bytes, 0);
    }
}
