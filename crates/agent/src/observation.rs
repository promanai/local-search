use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use localsearch_broker_api::{MAX_BROKER_PAGE_EVENTS, ScanMode, ScanPage};
use localsearch_broker_client::{BrokerFilesystemProvider, BrokerTransport};
use localsearch_core::{FilesystemEvent, ReconciliationReason, VolumeId};
use localsearch_filesystem_graph::{
    FilesystemGraph, GraphError, GraphMutation, GraphMutationBatch, ObservationScanMode,
    ObservationScanPhase, ObservationSession, VolumeState,
};
use localsearch_platform_core::{
    ChangeBatch, FilesystemProvider, PlatformError, PlatformErrorKind, ProviderCheckpoint,
    VolumeDescriptor,
};
use thiserror::Error;

const DISCOVERY_REFRESH: Duration = Duration::from_secs(30);

/// Explicit volume policy for opt-in broker-backed observation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ObservationSelection {
    /// Observe every currently attached local NTFS volume.
    #[default]
    AllLocalNtfs,
    /// Observe only the listed canonical volume identities.
    Volumes(BTreeSet<VolumeId>),
    /// Resolve user-facing mount roots to canonical volume identities during discovery.
    VolumesAndMountPoints {
        /// Canonical identities selected directly.
        volume_ids: BTreeSet<VolumeId>,
        /// Case-insensitive Windows mount roots, such as `C:\\`.
        mount_points: BTreeSet<String>,
    },
}

/// One bounded provider quantum used by the observation controller.
pub trait ObservationSource: Send + Sync {
    /// Discover currently visible volumes.
    ///
    /// # Errors
    ///
    /// Returns a portable provider error when discovery is unavailable.
    fn discover_volumes(&self) -> Result<Vec<VolumeDescriptor>, PlatformError>;
    /// Start one paged initial scan or reconciliation.
    ///
    /// # Errors
    ///
    /// Returns a portable provider error when the scan cannot be created.
    fn start_scan(&self, volume_id: VolumeId, mode: ScanMode) -> Result<u64, PlatformError>;
    /// Pull one bounded page from a started scan.
    ///
    /// # Errors
    ///
    /// Returns a portable provider error for an invalid/expired scan or source failure.
    fn read_scan_page(&self, scan_id: u64, maximum_events: u16) -> Result<ScanPage, PlatformError>;
    /// Cancel an abandoned server-owned scan.
    ///
    /// # Errors
    ///
    /// Returns a portable provider error when cancellation cannot be acknowledged.
    fn cancel_scan(&self, scan_id: u64) -> Result<(), PlatformError>;
    /// Pull one bounded incremental journal page.
    ///
    /// # Errors
    ///
    /// Returns a portable provider error, including a typed source-history gap.
    fn read_changes(
        &self,
        checkpoint: &ProviderCheckpoint,
        maximum_events: u32,
    ) -> Result<(Vec<FilesystemEvent>, ChangeBatch), PlatformError>;
}

impl<T: BrokerTransport> ObservationSource for BrokerFilesystemProvider<T> {
    fn discover_volumes(&self) -> Result<Vec<VolumeDescriptor>, PlatformError> {
        FilesystemProvider::discover_volumes(self)
    }

    fn start_scan(&self, volume_id: VolumeId, mode: ScanMode) -> Result<u64, PlatformError> {
        self.start_paged_scan(volume_id, mode)
    }

    fn read_scan_page(&self, scan_id: u64, maximum_events: u16) -> Result<ScanPage, PlatformError> {
        self.read_paged_scan(scan_id, maximum_events)
    }

    fn cancel_scan(&self, scan_id: u64) -> Result<(), PlatformError> {
        self.cancel_paged_scan(scan_id)
    }

    fn read_changes(
        &self,
        checkpoint: &ProviderCheckpoint,
        maximum_events: u32,
    ) -> Result<(Vec<FilesystemEvent>, ChangeBatch), PlatformError> {
        let mut events = Vec::new();
        let batch =
            FilesystemProvider::read_changes(self, checkpoint, maximum_events, &mut |event| {
                events.push(event);
                Ok(())
            })?;
        Ok((events, batch))
    }
}

/// Observable outcome from one scheduler-bounded controller step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationStep {
    /// No selected, currently attached volume was available.
    Idle,
    /// A durable scan session was prepared; the broker handle is started on a later quantum.
    ScanPrepared { volume_id: VolumeId },
    /// A broker scan handle was created or recreated.
    ScanStarted { volume_id: VolumeId },
    /// One full-scan page was committed.
    ScanPage {
        volume_id: VolumeId,
        events: u32,
        complete: bool,
    },
    /// One bounded stale-row finalization page committed.
    Finalized {
        volume_id: VolumeId,
        removed: u64,
        complete: bool,
    },
    /// One incremental checkpoint/event page committed atomically.
    Incremental {
        volume_id: VolumeId,
        events: u32,
        has_more: bool,
    },
    /// Source continuity was lost and durable reconciliation is now required.
    ReconciliationRequired { volume_id: VolumeId },
    /// A temporarily unavailable selected volume was marked offline.
    Offline { volume_id: VolumeId },
}

/// Redacted observation-controller failure.
#[derive(Debug, Error)]
pub enum ObservationError {
    /// Durable graph state could not advance.
    #[error("filesystem observation graph operation failed")]
    Graph(#[from] GraphError),
    /// Broker/provider operation failed with a portable category.
    #[error("filesystem observation provider operation failed: {0:?}")]
    Provider(PlatformErrorKind),
}

#[derive(Clone, Copy)]
struct ActiveScan {
    volume_id: VolumeId,
    scan_id: u64,
}

/// Round-robin, restart-safe broker observation state owned by the Agent scheduler.
pub struct BrokerObservationController<S: ObservationSource> {
    source: S,
    graph_path: PathBuf,
    selection: ObservationSelection,
    volumes: Vec<VolumeDescriptor>,
    cursor: usize,
    active_scan: Option<ActiveScan>,
    last_discovery: Instant,
}

impl<S: ObservationSource> BrokerObservationController<S> {
    /// Discovers the initial opt-in volume set and binds it to one durable graph.
    ///
    /// # Errors
    ///
    /// Returns a portable provider category if discovery fails.
    pub fn new(
        source: S,
        graph_path: impl AsRef<Path>,
        selection: ObservationSelection,
    ) -> Result<Self, ObservationError> {
        let volumes = selected_volumes(&source.discover_volumes().map_err(provider)?, &selection);
        Ok(Self {
            source,
            graph_path: graph_path.as_ref().to_owned(),
            selection,
            volumes,
            cursor: 0,
            active_scan: None,
            last_discovery: Instant::now(),
        })
    }

    /// Executes at most one broker page or one bounded graph-finalization transaction.
    ///
    /// # Errors
    ///
    /// Returns a redacted graph/provider error while leaving durable progress restartable.
    pub fn step(&mut self, maximum_events: u16) -> Result<ObservationStep, ObservationError> {
        let maximum_events = maximum_events.clamp(1, MAX_BROKER_PAGE_EVENTS);
        if let Some(active) = self.active_scan.take() {
            return self.read_active_scan(active, maximum_events);
        }
        self.refresh_discovery_if_due()?;
        if self.volumes.is_empty() {
            return Ok(ObservationStep::Idle);
        }
        if self.cursor >= self.volumes.len() {
            self.cursor = 0;
        }
        let descriptor = self.volumes[self.cursor].clone();
        self.cursor = (self.cursor + 1) % self.volumes.len();
        let mut graph = FilesystemGraph::open(&self.graph_path)?;
        if let Some(session) = graph.observation_session(descriptor.volume_id)? {
            return self.handle_session(&mut graph, &descriptor, session, maximum_events);
        }
        let checkpoint = graph.checkpoint(descriptor.volume_id)?;
        if checkpoint.is_none()
            || graph.volume_state(descriptor.volume_id)? == Some(VolumeState::NeedsReconciliation)
        {
            let mode = if checkpoint.is_some() {
                ObservationScanMode::Reconcile
            } else {
                ObservationScanMode::Initial
            };
            graph.begin_observation_scan(&descriptor, mode, scan_reason(mode))?;
            return Ok(ObservationStep::ScanPrepared {
                volume_id: descriptor.volume_id,
            });
        }
        let Some(checkpoint) = checkpoint else {
            return Err(ObservationError::Graph(GraphError::Invariant(
                "observation checkpoint disappeared".to_owned(),
            )));
        };
        self.read_incremental(&mut graph, &descriptor, checkpoint, maximum_events)
    }

    fn handle_session(
        &mut self,
        graph: &mut FilesystemGraph,
        descriptor: &VolumeDescriptor,
        session: ObservationSession,
        maximum_events: u16,
    ) -> Result<ObservationStep, ObservationError> {
        match session.phase {
            ObservationScanPhase::Scanning => {
                let scan_id = self
                    .source
                    .start_scan(descriptor.volume_id, broker_scan_mode(session.mode))
                    .map_err(provider)?;
                if let Err(error) = graph.begin_observation_scan(
                    descriptor,
                    session.mode,
                    scan_reason(session.mode),
                ) {
                    let _ = self.source.cancel_scan(scan_id);
                    return Err(error.into());
                }
                self.active_scan = Some(ActiveScan {
                    volume_id: descriptor.volume_id,
                    scan_id,
                });
                Ok(ObservationStep::ScanStarted {
                    volume_id: descriptor.volume_id,
                })
            }
            ObservationScanPhase::SweepingLinks | ObservationScanPhase::SweepingObjects => {
                let summary = graph
                    .finalize_observation_scan(descriptor.volume_id, u32::from(maximum_events))?;
                Ok(ObservationStep::Finalized {
                    volume_id: descriptor.volume_id,
                    removed: summary
                        .stale_links_removed
                        .saturating_add(summary.stale_objects_tombstoned),
                    complete: summary.completed,
                })
            }
        }
    }

    fn read_incremental(
        &self,
        graph: &mut FilesystemGraph,
        descriptor: &VolumeDescriptor,
        checkpoint: ProviderCheckpoint,
        maximum_events: u16,
    ) -> Result<ObservationStep, ObservationError> {
        match self
            .source
            .read_changes(&checkpoint, u32::from(maximum_events))
        {
            Ok((events, batch)) => {
                let event_count = u32::try_from(events.len()).unwrap_or(u32::MAX);
                let requires_reconciliation = events
                    .iter()
                    .any(|event| matches!(event, FilesystemEvent::ReconciliationRequired { .. }));
                let mut mutations = Vec::with_capacity(events.len().saturating_add(1));
                if graph.volume_state(descriptor.volume_id)? == Some(VolumeState::Offline) {
                    mutations.push(GraphMutation::SetVolumeState {
                        volume_id: descriptor.volume_id,
                        state: VolumeState::Online,
                    });
                }
                mutations.extend(events.into_iter().map(GraphMutation::from));
                graph.apply_batch(&GraphMutationBatch {
                    volume_id: descriptor.volume_id,
                    checkpoint: batch.checkpoint,
                    mutations,
                })?;
                if requires_reconciliation {
                    Ok(ObservationStep::ReconciliationRequired {
                        volume_id: descriptor.volume_id,
                    })
                } else {
                    Ok(ObservationStep::Incremental {
                        volume_id: descriptor.volume_id,
                        events: event_count,
                        has_more: batch.has_more,
                    })
                }
            }
            Err(error)
                if matches!(
                    error.kind,
                    PlatformErrorKind::SourceHistoryGap | PlatformErrorKind::InvalidCheckpoint
                ) =>
            {
                graph.apply_batch(&GraphMutationBatch {
                    volume_id: descriptor.volume_id,
                    checkpoint,
                    mutations: vec![GraphMutation::RequireReconciliation {
                        volume_id: descriptor.volume_id,
                        reason: if error.kind == PlatformErrorKind::SourceHistoryGap {
                            ReconciliationReason::SourceHistoryUnavailable
                        } else {
                            ReconciliationReason::ProviderRequested
                        },
                    }],
                })?;
                Ok(ObservationStep::ReconciliationRequired {
                    volume_id: descriptor.volume_id,
                })
            }
            Err(error) if error.kind == PlatformErrorKind::Unavailable => {
                if graph.volume_state(descriptor.volume_id)? != Some(VolumeState::Offline) {
                    graph.apply_batch(&GraphMutationBatch {
                        volume_id: descriptor.volume_id,
                        checkpoint,
                        mutations: vec![GraphMutation::SetVolumeState {
                            volume_id: descriptor.volume_id,
                            state: VolumeState::Offline,
                        }],
                    })?;
                }
                Ok(ObservationStep::Offline {
                    volume_id: descriptor.volume_id,
                })
            }
            Err(error) => Err(provider(error)),
        }
    }

    fn read_active_scan(
        &mut self,
        active: ActiveScan,
        maximum_events: u16,
    ) -> Result<ObservationStep, ObservationError> {
        let page = match self.source.read_scan_page(active.scan_id, maximum_events) {
            Ok(page) => page,
            Err(error) => {
                let _ = self.source.cancel_scan(active.scan_id);
                return Err(provider(error));
            }
        };
        let event_count = u32::try_from(page.events.len()).unwrap_or(u32::MAX);
        let mut graph = FilesystemGraph::open(&self.graph_path)?;
        graph.apply_observation_scan_page(active.volume_id, page.events)?;
        if page.complete {
            let checkpoint = page
                .checkpoint
                .ok_or_else(|| ObservationError::Provider(PlatformErrorKind::Internal))?;
            graph.stage_observation_checkpoint(active.volume_id, &checkpoint)?;
        } else {
            self.active_scan = Some(active);
        }
        Ok(ObservationStep::ScanPage {
            volume_id: active.volume_id,
            events: event_count,
            complete: page.complete,
        })
    }

    fn refresh_discovery_if_due(&mut self) -> Result<(), ObservationError> {
        if self.last_discovery.elapsed() < DISCOVERY_REFRESH {
            return Ok(());
        }
        self.volumes = selected_volumes(
            &self.source.discover_volumes().map_err(provider)?,
            &self.selection,
        );
        self.cursor = self.cursor.min(self.volumes.len().saturating_sub(1));
        self.last_discovery = Instant::now();
        Ok(())
    }
}

impl<S> Drop for BrokerObservationController<S>
where
    S: ObservationSource,
{
    fn drop(&mut self) {
        if let Some(active) = self.active_scan.take() {
            let _ = self.source.cancel_scan(active.scan_id);
        }
    }
}

fn selected_volumes(
    discovered: &[VolumeDescriptor],
    selection: &ObservationSelection,
) -> Vec<VolumeDescriptor> {
    discovered
        .iter()
        .filter(|volume| {
            volume.local
                && volume
                    .filesystem
                    .as_deref()
                    .is_some_and(|filesystem| filesystem.eq_ignore_ascii_case("ntfs"))
                && match selection {
                    ObservationSelection::AllLocalNtfs => true,
                    ObservationSelection::Volumes(selected) => selected.contains(&volume.volume_id),
                    ObservationSelection::VolumesAndMountPoints {
                        volume_ids,
                        mount_points,
                    } => {
                        volume_ids.contains(&volume.volume_id)
                            || volume.mount_points.iter().any(|discovered| {
                                mount_points
                                    .iter()
                                    .any(|selected| mount_point_matches(discovered, selected))
                            })
                    }
                }
        })
        .cloned()
        .collect()
}

fn mount_point_matches(discovered: &str, selected: &str) -> bool {
    discovered
        .trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(selected.trim_end_matches(['\\', '/']))
}

const fn broker_scan_mode(mode: ObservationScanMode) -> ScanMode {
    match mode {
        ObservationScanMode::Initial => ScanMode::Initial,
        ObservationScanMode::Reconcile => ScanMode::Reconcile,
    }
}

const fn scan_reason(mode: ObservationScanMode) -> ReconciliationReason {
    match mode {
        ObservationScanMode::Initial => ReconciliationReason::ProviderRequested,
        ObservationScanMode::Reconcile => ReconciliationReason::SourceHistoryUnavailable,
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err consumes the provider error while this boundary intentionally retains only its category"
)]
fn provider(error: PlatformError) -> ObservationError {
    let PlatformError { kind, .. } = error;
    ObservationError::Provider(kind)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use localsearch_core::{
        Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
        FileObjectSnapshot,
    };

    use super::*;

    fn volume() -> VolumeId {
        VolumeId::from_u128(700)
    }

    fn descriptor() -> VolumeDescriptor {
        VolumeDescriptor {
            volume_id: volume(),
            display_name: Some("observed".to_owned()),
            mount_points: vec!["root".to_owned()],
            filesystem: Some("NTFS".to_owned()),
            removable: false,
            local: true,
        }
    }

    fn checkpoint(marker: u8) -> ProviderCheckpoint {
        ProviderCheckpoint {
            provider_id: "fake-usn".to_owned(),
            format_version: 1,
            volume_id: volume(),
            opaque: vec![marker],
        }
    }

    fn object(size: u64) -> FileObjectSnapshot {
        FileObjectSnapshot {
            object_key: FileKey::new(volume(), FileId128::from_u128(1)),
            metadata: FileMetadata {
                kind: FileKind::File,
                size,
                created_at_unix_ms: None,
                modified_at_unix_ms: Some(1),
                hidden: false,
                availability: Availability::Online,
            },
        }
    }

    fn initial_events() -> Vec<FilesystemEvent> {
        vec![
            FilesystemEvent::ObjectObserved { object: object(10) },
            FilesystemEvent::LinkObserved {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(101),
                    object_key: object(10).object_key,
                    parent_key: None,
                    name: "observed.txt".to_owned(),
                },
            },
        ]
    }

    #[derive(Clone)]
    struct FakeSource {
        state: Arc<Mutex<FakeState>>,
    }

    struct FakeState {
        pages: VecDeque<ScanPage>,
        changes: VecDeque<Result<(Vec<FilesystemEvent>, ChangeBatch), PlatformError>>,
        started_modes: Vec<ScanMode>,
    }

    impl ObservationSource for FakeSource {
        fn discover_volumes(&self) -> Result<Vec<VolumeDescriptor>, PlatformError> {
            Ok(vec![descriptor()])
        }

        fn start_scan(&self, _volume_id: VolumeId, mode: ScanMode) -> Result<u64, PlatformError> {
            self.state.lock().expect("state").started_modes.push(mode);
            Ok(1)
        }

        fn read_scan_page(
            &self,
            _scan_id: u64,
            _maximum_events: u16,
        ) -> Result<ScanPage, PlatformError> {
            self.state
                .lock()
                .expect("state")
                .pages
                .pop_front()
                .ok_or_else(|| {
                    PlatformError::new(PlatformErrorKind::Internal, "fake", "page missing")
                })
        }

        fn cancel_scan(&self, _scan_id: u64) -> Result<(), PlatformError> {
            Ok(())
        }

        fn read_changes(
            &self,
            _checkpoint: &ProviderCheckpoint,
            _maximum_events: u32,
        ) -> Result<(Vec<FilesystemEvent>, ChangeBatch), PlatformError> {
            self.state
                .lock()
                .expect("state")
                .changes
                .pop_front()
                .ok_or_else(|| {
                    PlatformError::new(PlatformErrorKind::Internal, "fake", "change missing")
                })?
        }
    }

    #[test]
    fn mount_root_selection_resolves_case_and_trailing_separator() {
        let selection = ObservationSelection::VolumesAndMountPoints {
            volume_ids: BTreeSet::new(),
            mount_points: BTreeSet::from(["c:".to_owned()]),
        };
        let mut discovered = descriptor();
        discovered.mount_points = vec![r"C:\".to_owned()];
        assert_eq!(selected_volumes(&[discovered], &selection).len(), 1);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end controller lifecycle remains visible in one deterministic test"
    )]
    fn controller_bootstraps_polls_and_turns_history_gap_into_reconciliation() {
        let state = Arc::new(Mutex::new(FakeState {
            pages: VecDeque::from([
                ScanPage {
                    events: initial_events(),
                    checkpoint: None,
                    complete: false,
                    has_more: true,
                },
                ScanPage {
                    events: Vec::new(),
                    checkpoint: Some(checkpoint(1)),
                    complete: true,
                    has_more: false,
                },
            ]),
            changes: VecDeque::from([
                Ok((
                    vec![FilesystemEvent::ObjectObserved { object: object(99) }],
                    ChangeBatch {
                        checkpoint: checkpoint(2),
                        emitted_events: 1,
                        has_more: false,
                    },
                )),
                Err(PlatformError::new(
                    PlatformErrorKind::SourceHistoryGap,
                    "fake",
                    "history gap",
                )),
            ]),
            started_modes: Vec::new(),
        }));
        let source = FakeSource {
            state: Arc::clone(&state),
        };
        let temp = tempfile::tempdir().expect("temp");
        let graph_path = temp.path().join("graph.sqlite3");
        let mut controller = BrokerObservationController::new(
            source,
            &graph_path,
            ObservationSelection::AllLocalNtfs,
        )
        .expect("controller");

        assert!(matches!(
            controller.step(1).expect("prepare"),
            ObservationStep::ScanPrepared { .. }
        ));
        assert!(matches!(
            controller.step(1).expect("start"),
            ObservationStep::ScanStarted { .. }
        ));
        assert!(matches!(
            controller.step(2).expect("events"),
            ObservationStep::ScanPage {
                events: 2,
                complete: false,
                ..
            }
        ));
        assert!(matches!(
            controller.step(2).expect("terminal"),
            ObservationStep::ScanPage { complete: true, .. }
        ));
        assert!(matches!(
            controller.step(2).expect("link sweep"),
            ObservationStep::Finalized {
                complete: false,
                ..
            }
        ));
        assert!(matches!(
            controller.step(2).expect("object sweep"),
            ObservationStep::Finalized { complete: true, .. }
        ));
        assert!(matches!(
            controller.step(2).expect("incremental"),
            ObservationStep::Incremental { events: 1, .. }
        ));
        let graph = FilesystemGraph::open_read_only(&graph_path).expect("graph");
        assert_eq!(
            graph.checkpoint(volume()).expect("checkpoint"),
            Some(checkpoint(2))
        );
        assert_eq!(
            graph.desired_catalog_documents().expect("documents")[0]
                .metadata
                .size,
            99
        );
        drop(graph);
        assert!(matches!(
            controller.step(2).expect("gap"),
            ObservationStep::ReconciliationRequired { .. }
        ));
        let graph = FilesystemGraph::open_read_only(&graph_path).expect("graph");
        assert_eq!(
            graph.volume_state(volume()).expect("state"),
            Some(VolumeState::NeedsReconciliation)
        );
        assert!(
            graph
                .observation_session(volume())
                .expect("session")
                .is_none()
        );
        drop(graph);
        assert!(matches!(
            controller.step(2).expect("prepare reconciliation"),
            ObservationStep::ScanPrepared { .. }
        ));
        assert!(matches!(
            controller.step(2).expect("start reconciliation"),
            ObservationStep::ScanStarted { .. }
        ));
        assert_eq!(
            state.lock().expect("state").started_modes,
            vec![ScanMode::Initial, ScanMode::Reconcile]
        );
    }
}
