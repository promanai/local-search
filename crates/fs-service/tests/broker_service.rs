use std::sync::Arc;

use localsearch_agent::{AgentService, ClientAuthorization};
use localsearch_agent_api::{
    AGENT_API_VERSION, AGENT_CODEC_VERSION, AgentRequest, RequestOperation, ResponsePayload,
};
use localsearch_broker_api::{
    BROKER_CODEC_VERSION, BROKER_PROTOCOL_VERSION, BrokerErrorCode, BrokerOperation, BrokerPayload,
    BrokerRequest, BrokerResponse, ScanMode,
};
use localsearch_broker_client::{BrokerFilesystemProvider, BrokerTransport, BrokerTransportError};
use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, VolumeId,
};
use localsearch_filesystem_graph::FilesystemGraph;
use localsearch_fs_service::BrokerService;
use localsearch_platform_core::{
    ChangeBatch, ChangeTrackingMode, FilesystemEventSink, FilesystemProvider, InitialScanMode,
    PlatformCapabilities, PlatformFamily, PlatformResult, PrivilegeModel, ProviderCheckpoint,
    ScanSummary, VolumeDescriptor,
};

#[derive(Clone)]
struct FakeProvider {
    volume: VolumeDescriptor,
    events: Arc<Vec<FilesystemEvent>>,
}

impl FakeProvider {
    fn new(event_count: usize) -> Self {
        let volume_id = VolumeId::from_u128(44);
        let events = (0..event_count)
            .map(|ordinal| {
                let identity = u128::try_from(ordinal + 1).expect("fixture identity");
                FilesystemEvent::ObjectObserved {
                    object: FileObjectSnapshot {
                        object_key: FileKey::new(volume_id, FileId128::from_u128(identity)),
                        metadata: metadata(),
                    },
                }
            })
            .collect();
        Self {
            volume: VolumeDescriptor {
                volume_id,
                display_name: Some("broker-fixture".to_owned()),
                mount_points: vec!["fixture-root".to_owned()],
                filesystem: Some("ntfs".to_owned()),
                removable: false,
                local: true,
            },
            events: Arc::new(events),
        }
    }

    fn checkpoint(&self, cursor: u8) -> ProviderCheckpoint {
        ProviderCheckpoint {
            provider_id: "fake-broker".to_owned(),
            format_version: 1,
            volume_id: self.volume.volume_id,
            opaque: vec![cursor],
        }
    }
}

impl FilesystemProvider for FakeProvider {
    fn capabilities(&self) -> PlatformCapabilities {
        PlatformCapabilities {
            platform: PlatformFamily::Windows,
            initial_scan: InitialScanMode::FastMetadataEnumeration,
            change_tracking: ChangeTrackingMode::PersistentObjectJournal,
            privilege_model: PrivilegeModel::OptionalBroker,
            stable_object_ids: true,
            hard_links: true,
            persistent_history: true,
        }
    }

    fn discover_volumes(&self) -> PlatformResult<Vec<VolumeDescriptor>> {
        Ok(vec![self.volume.clone()])
    }

    fn initial_scan(
        &self,
        _volume: &VolumeDescriptor,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        for event in self.events.iter().cloned() {
            sink.emit(event)?;
        }
        Ok(ScanSummary {
            checkpoint: self.checkpoint(1),
            emitted_events: u64::try_from(self.events.len()).expect("event count"),
        })
    }

    fn read_changes(
        &self,
        _checkpoint: &ProviderCheckpoint,
        maximum_events: u32,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ChangeBatch> {
        let emitted = usize::try_from(maximum_events)
            .unwrap_or(usize::MAX)
            .min(self.events.len());
        for event in self.events[..emitted].iter().cloned() {
            sink.emit(event)?;
        }
        Ok(ChangeBatch {
            checkpoint: self.checkpoint(2),
            emitted_events: u32::try_from(emitted).expect("emitted"),
            has_more: emitted < self.events.len(),
        })
    }

    fn reconcile(
        &self,
        volume: &VolumeDescriptor,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        self.initial_scan(volume, sink)
    }
}

fn metadata() -> FileMetadata {
    FileMetadata {
        kind: FileKind::File,
        size: 1,
        created_at_unix_ms: None,
        modified_at_unix_ms: None,
        hidden: false,
        availability: Availability::Online,
    }
}

struct InProcessTransport {
    service: Arc<BrokerService<FakeProvider>>,
}

impl BrokerTransport for InProcessTransport {
    fn invoke(&self, request: &BrokerRequest) -> Result<BrokerResponse, BrokerTransportError> {
        Ok(self.service.dispatch(request.clone(), &|| false))
    }
}

fn request(id: &str, operation: BrokerOperation) -> BrokerRequest {
    BrokerRequest {
        protocol_version: BROKER_PROTOCOL_VERSION,
        codec_version: BROKER_CODEC_VERSION,
        request_id: id.to_owned(),
        deadline_ms: 5_000,
        operation,
    }
}

#[test]
fn bounded_queue_streams_more_than_capacity_through_portable_provider() {
    let native = Arc::new(FakeProvider::new(600));
    let volume = native.volume.clone();
    let service = Arc::new(BrokerService::new(native));
    let provider = BrokerFilesystemProvider::connect(InProcessTransport {
        service: Arc::clone(&service),
    })
    .expect("connect broker provider");
    assert!(provider.capabilities().persistent_history);
    let discovered = provider.discover_volumes().expect("volumes");
    assert_eq!(discovered.len(), 1);
    assert_eq!(&discovered[0], &volume);

    let mut events = Vec::new();
    let summary = provider
        .initial_scan(&volume, &mut |event| {
            events.push(event);
            Ok(())
        })
        .expect("stream scan");
    assert_eq!(events.len(), 600);
    assert_eq!(summary.emitted_events, 600);
    assert_eq!(summary.checkpoint.opaque, [1]);

    let mut changes = Vec::new();
    let batch = provider
        .read_changes(&summary.checkpoint, 10, &mut |event| {
            changes.push(event);
            Ok(())
        })
        .expect("changes");
    assert_eq!(changes.len(), 10);
    assert_eq!(batch.emitted_events, 10);
    assert!(batch.has_more);
}

#[test]
fn replay_unknown_version_and_missing_scan_fail_closed() {
    let service = BrokerService::new(Arc::new(FakeProvider::new(1)));
    let capabilities = request("replay-1", BrokerOperation::BrokerGetCapabilities);
    let first = service.dispatch(capabilities.clone(), &|| false);
    assert!(matches!(first.result, Some(BrokerPayload::Capabilities(_))));
    let replay = service.dispatch(capabilities, &|| false);
    assert_eq!(
        replay.error.as_ref().map(|error| error.code),
        Some(BrokerErrorCode::ReplayRejected)
    );

    let mut unknown = request("version-1", BrokerOperation::DiscoverVolumes);
    unknown.protocol_version += 1;
    let response = service.dispatch(unknown, &|| false);
    assert_eq!(
        response.error.as_ref().map(|error| error.code),
        Some(BrokerErrorCode::UnsupportedProtocolVersion)
    );

    let response = service.dispatch(
        request(
            "missing-1",
            BrokerOperation::ReadScanPage {
                scan_id: 99,
                maximum_events: 1,
            },
        ),
        &|| false,
    );
    assert_eq!(
        response.error.as_ref().map(|error| error.code),
        Some(BrokerErrorCode::NotFound)
    );
}

#[test]
fn dropping_service_cancels_a_producer_blocked_by_backpressure() {
    let service = BrokerService::new(Arc::new(FakeProvider::new(10_000)));
    let response = service.dispatch(
        request(
            "drop-1",
            BrokerOperation::StartScan {
                volume_id: VolumeId::from_u128(44),
                mode: ScanMode::Initial,
            },
        ),
        &|| false,
    );
    assert!(matches!(
        response.result,
        Some(BrokerPayload::ScanStarted { .. })
    ));
    drop(service);
}

#[test]
fn broker_contract_has_no_content_or_path_operation() {
    let service = BrokerService::new(Arc::new(FakeProvider::new(1)));
    let response = service.dispatch(
        request("caps-1", BrokerOperation::BrokerGetCapabilities),
        &|| false,
    );
    let Some(BrokerPayload::Capabilities(capabilities)) = response.result else {
        panic!("capabilities")
    };
    let encoded = serde_json::to_string(&capabilities.allowed_operations).expect("allowlist JSON");
    for forbidden in ["content", "path", "write", "execute", "admin", "search"] {
        assert!(!encoded.contains(forbidden));
    }

    // Metadata events carry canonical names but never file bytes.
    let link = FilesystemEvent::LinkObserved {
        link: FileLinkSnapshot {
            file_link_id: FileLinkId::from_u128(1),
            object_key: FileKey::new(VolumeId::from_u128(44), FileId128::from_u128(1)),
            parent_key: None,
            name: "metadata-only".to_owned(),
        },
    };
    let serialized = serde_json::to_string(&link).expect("event JSON");
    assert!(!serialized.contains("content"));
}

#[test]
fn broker_snapshot_becomes_searchable_and_survives_agent_restart() {
    let volume_id = VolumeId::from_u128(44);
    let root = FileKey::new(volume_id, FileId128::from_u128(1));
    let file = FileKey::new(volume_id, FileId128::from_u128(2));
    let mut native = FakeProvider::new(0);
    native.events = Arc::new(vec![
        FilesystemEvent::ObjectObserved {
            object: FileObjectSnapshot {
                object_key: root,
                metadata: FileMetadata {
                    kind: FileKind::Directory,
                    ..metadata()
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
                metadata: metadata(),
            },
        },
        FilesystemEvent::LinkObserved {
            link: FileLinkSnapshot {
                file_link_id: FileLinkId::from_u128(2),
                object_key: file,
                parent_key: Some(root),
                name: "broker-architecture.md".to_owned(),
            },
        },
    ]);
    let volume = native.volume.clone();
    let service = Arc::new(BrokerService::new(Arc::new(native)));
    let provider = BrokerFilesystemProvider::connect(InProcessTransport { service })
        .expect("connect broker provider");
    let mut events = Vec::new();
    let summary = provider
        .initial_scan(&volume, &mut |event| {
            events.push(event);
            Ok(())
        })
        .expect("broker snapshot");

    let temp = tempfile::tempdir().expect("temp");
    let graph_path = temp.path().join("graph.sqlite3");
    let index_path = temp.path().join("catalog");
    let mut graph = FilesystemGraph::open(&graph_path).expect("graph");
    graph
        .ingest_snapshot(volume, summary.checkpoint, events)
        .expect("durable snapshot");
    drop(graph);

    let first = AgentService::open(
        &graph_path,
        &index_path,
        ClientAuthorization::v0_1_metadata(),
    )
    .expect("first Agent");
    let first_hit = search_name(&first, "broker-before");
    drop(first);
    let restarted =
        AgentService::open(graph_path, index_path, ClientAuthorization::v0_1_metadata())
            .expect("restarted Agent");
    let restarted_hit = search_name(&restarted, "broker-after");
    assert_eq!(first_hit, "broker-architecture.md");
    assert_eq!(restarted_hit, first_hit);
}

fn search_name(service: &AgentService, request_id: &str) -> String {
    let response = service.dispatch(AgentRequest {
        protocol_version: AGENT_API_VERSION,
        codec_version: AGENT_CODEC_VERSION,
        request_id: request_id.to_owned(),
        deadline_ms: 2_000,
        operation: RequestOperation::CatalogSearch(localsearch_core::SearchRequest {
            query: "architecture".to_owned(),
            scope: localsearch_core::SearchScope::All,
            filters: localsearch_core::SearchFilter::default(),
            top_k: 10,
        }),
    });
    let Some(ResponsePayload::Search(search)) = response.result else {
        panic!("search response: {:?}", response.error)
    };
    search.hits[0].name.clone()
}
