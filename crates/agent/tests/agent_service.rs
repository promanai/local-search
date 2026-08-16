use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use localsearch_agent::{AgentService, ClientAuthorization};
use localsearch_agent_api::{
    AGENT_API_VERSION, AGENT_CODEC_VERSION, AgentErrorCode, AgentRequest, Capability,
    ContentSearchRequest, IndexStatusPort, RequestOperation, ResponsePayload,
};
use localsearch_content_index::{
    CONTENT_SCHEMA_ID, ContentGenerationLimits, ContentGenerationManager, ContentIndex,
    ContentIndexPolicy, DEFAULT_MAX_FILE_BYTES,
};
use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, SearchFilter, SearchRequest, SearchScope, VolumeId,
};
use localsearch_filesystem_graph::{FilesystemGraph, GraphMutation, GraphMutationBatch};
use localsearch_platform_core::{PowerSource, ProviderCheckpoint, VolumeDescriptor};
use localsearch_resource_governor::{DecisionReason, GovernorMode, SystemPressure};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().expect("temp");
    let graph_path = temp.path().join("graph.sqlite3");
    let index_path = temp.path().join("catalog");
    let volume = VolumeId::from_u128(7);
    let root = FileKey::new(volume, FileId128::from_u128(1));
    let file = FileKey::new(volume, FileId128::from_u128(2));
    let mut graph = FilesystemGraph::open(&graph_path).expect("graph");
    graph
        .ingest_snapshot(
            VolumeDescriptor {
                volume_id: volume,
                display_name: Some("agent-test".to_owned()),
                mount_points: vec!["root".to_owned()],
                filesystem: Some("testfs".to_owned()),
                removable: false,
                local: true,
            },
            ProviderCheckpoint {
                provider_id: "agent-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![1],
            },
            [
                FilesystemEvent::ObjectObserved {
                    object: FileObjectSnapshot {
                        object_key: root,
                        metadata: metadata(FileKind::Directory, 0),
                    },
                },
                FilesystemEvent::LinkObserved {
                    link: FileLinkSnapshot {
                        file_link_id: FileLinkId::from_u128(101),
                        object_key: root,
                        parent_key: None,
                        name: "root".to_owned(),
                    },
                },
                FilesystemEvent::ObjectObserved {
                    object: FileObjectSnapshot {
                        object_key: file,
                        metadata: metadata(FileKind::File, 42),
                    },
                },
                FilesystemEvent::LinkObserved {
                    link: FileLinkSnapshot {
                        file_link_id: FileLinkId::from_u128(102),
                        object_key: file,
                        parent_key: Some(root),
                        name: "architecture-plan.md".to_owned(),
                    },
                },
            ],
        )
        .expect("snapshot");
    drop(graph);
    (temp, graph_path, index_path)
}

fn metadata(kind: FileKind, size: u64) -> FileMetadata {
    FileMetadata {
        kind,
        size,
        created_at_unix_ms: None,
        modified_at_unix_ms: Some(123),
        hidden: false,
        availability: Availability::Online,
    }
}

fn search_request(request_id: &str) -> AgentRequest {
    AgentRequest {
        protocol_version: AGENT_API_VERSION,
        codec_version: AGENT_CODEC_VERSION,
        request_id: request_id.to_owned(),
        deadline_ms: 2_000,
        operation: RequestOperation::CatalogSearch(SearchRequest {
            query: "architecture".to_owned(),
            scope: SearchScope::All,
            filters: SearchFilter::default(),
            top_k: 10,
        }),
    }
}

#[test]
fn service_recovers_projection_searches_and_preserves_three_identities() {
    let (_temp, graph, index) = fixture();
    let service =
        AgentService::open(graph, index, ClientAuthorization::v0_1_metadata()).expect("agent");
    let response = service.dispatch(search_request("service-1"));
    let Some(ResponsePayload::Search(search)) = response.result else {
        panic!("search response: {:?}", response.error)
    };
    assert_eq!(search.hits.len(), 1);
    let hit = &search.hits[0];
    assert_eq!(hit.name, "architecture-plan.md");
    assert_eq!(hit.rank, 1);
    assert_eq!(hit.ranking_version.get(), 1);
    assert_ne!(
        hit.document_id.to_string().split(':').next(),
        hit.file_link_id.to_string().split(':').next()
    );
    assert_eq!(hit.document_id.as_u128(), hit.file_link_id.as_u128());
    assert_eq!(hit.object_key.file_id.as_u128(), 2);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the end-to-end privacy contract keeps graph, source, content generation, Agent response, and stale-identity proof together"
)]
fn opt_in_content_search_re_resolves_current_metadata_without_returning_source_text() {
    let temp = tempfile::tempdir().expect("temp");
    let content_root = temp.path().join("documents");
    std::fs::create_dir(&content_root).expect("content root");
    let source = content_root.join("notes.md");
    std::fs::write(
        &source,
        "The internal project codeword is heliotrope-sundial.",
    )
    .expect("content source");
    let graph_path = temp.path().join("graph.sqlite3");
    let catalog_path = temp.path().join("catalog");
    let content_path = temp.path().join("content-index-v1");
    let volume = VolumeId::from_u128(17);
    let root = FileKey::new(volume, FileId128::from_u128(1));
    let file = FileKey::new(volume, FileId128::from_u128(2));
    let mut graph = FilesystemGraph::open(&graph_path).expect("graph");
    graph
        .ingest_snapshot(
            VolumeDescriptor {
                volume_id: volume,
                display_name: Some("content-test".to_owned()),
                mount_points: vec![content_root.to_string_lossy().into_owned()],
                filesystem: Some("testfs".to_owned()),
                removable: false,
                local: true,
            },
            ProviderCheckpoint {
                provider_id: "content-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![1],
            },
            [
                FilesystemEvent::ObjectObserved {
                    object: FileObjectSnapshot {
                        object_key: root,
                        metadata: metadata(FileKind::Directory, 0),
                    },
                },
                FilesystemEvent::LinkObserved {
                    link: FileLinkSnapshot {
                        file_link_id: FileLinkId::from_u128(201),
                        object_key: root,
                        parent_key: None,
                        name: content_root.to_string_lossy().into_owned(),
                    },
                },
                FilesystemEvent::ObjectObserved {
                    object: FileObjectSnapshot {
                        object_key: file,
                        metadata: metadata(
                            FileKind::File,
                            std::fs::metadata(&source).expect("source metadata").len(),
                        ),
                    },
                },
                FilesystemEvent::LinkObserved {
                    link: FileLinkSnapshot {
                        file_link_id: FileLinkId::from_u128(202),
                        object_key: file,
                        parent_key: Some(root),
                        name: "notes.md".to_owned(),
                    },
                },
            ],
        )
        .expect("snapshot");
    let policy = ContentIndexPolicy::new([content_root], DEFAULT_MAX_FILE_BYTES).expect("policy");
    let manager = ContentGenerationManager::open(&content_path).expect("generation manager");
    let generation = manager
        .resume_initial_generation(
            &graph,
            &policy,
            ContentGenerationLimits {
                max_content_index_bytes: 64 * 1024 * 1024,
                max_documents: 100,
                min_free_disk_bytes: 1,
                min_free_disk_percent: 0,
                batch_documents: 16,
                maximum_batches: 16,
            },
        )
        .expect("content generation");
    assert!(generation.complete);
    graph
        .acknowledge_projection(
            CONTENT_SCHEMA_ID,
            localsearch_core::MutationSeq(generation.generation.target_sequence),
            generation.generation.index_generation,
        )
        .expect("content checkpoint");
    std::fs::write(
        temp.path().join("content-workspace.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 2,
            "roots": [policy.roots()[0].to_string_lossy()],
            "max_file_bytes": DEFAULT_MAX_FILE_BYTES
        }))
        .expect("manifest JSON"),
    )
    .expect("content manifest");
    drop(graph);

    let service = AgentService::open_with_content(
        &graph_path,
        &catalog_path,
        Some(&content_path),
        ClientAuthorization::v0_2_with_content(),
    )
    .expect("content-enabled Agent");
    let response = service.dispatch(AgentRequest {
        protocol_version: AGENT_API_VERSION,
        codec_version: AGENT_CODEC_VERSION,
        request_id: "content-1".to_owned(),
        deadline_ms: 2_000,
        operation: RequestOperation::ContentSearch(ContentSearchRequest {
            query: "heliotrope".to_owned(),
            top_k: 10,
        }),
    });
    let Some(ResponsePayload::ContentSearch(search)) = response.result else {
        panic!("content response: {:?}", response.error)
    };
    assert_eq!(search.hits.len(), 1);
    assert_eq!(search.hits[0].item.name, "notes.md");
    let encoded = serde_json::to_string(&search).expect("response JSON");
    assert!(!encoded.contains("sundial"));

    let mut graph = FilesystemGraph::open(&graph_path).expect("graph writer");
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume,
            checkpoint: ProviderCheckpoint {
                provider_id: "content-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![2],
            },
            mutations: vec![GraphMutation::RemoveLink {
                file_link_id: FileLinkId::from_u128(202),
                object_key: file,
            }],
        })
        .expect("remove indexed identity");
    drop(graph);
    service
        .maintain_all_projections_scheduled()
        .expect("scheduled catalog and content projection");
    assert!(
        ContentIndex::open(&content_path)
            .expect("managed content reader")
            .search("heliotrope", 10)
            .expect("projected delete")
            .is_empty()
    );
    let stale_response = service.dispatch(AgentRequest {
        protocol_version: AGENT_API_VERSION,
        codec_version: AGENT_CODEC_VERSION,
        request_id: "content-stale".to_owned(),
        deadline_ms: 2_000,
        operation: RequestOperation::ContentSearch(ContentSearchRequest {
            query: "heliotrope".to_owned(),
            top_k: 10,
        }),
    });
    let Some(ResponsePayload::ContentSearch(stale_search)) = stale_response.result else {
        panic!("stale content response: {:?}", stale_response.error)
    };
    assert!(stale_search.hits.is_empty());
}

#[test]
fn agent_restart_reopens_durable_graph_and_active_generation() {
    let (_temp, graph, index) = fixture();
    let first = AgentService::open(&graph, &index, ClientAuthorization::v0_1_metadata())
        .expect("first Agent start");
    let first_response = first.dispatch(search_request("restart-before"));
    let Some(ResponsePayload::Search(first_search)) = first_response.result else {
        panic!("first search")
    };
    let generation = first_search.index_generation;
    drop(first);

    let restarted = AgentService::open(graph, index, ClientAuthorization::v0_1_metadata())
        .expect("restarted Agent");
    let second_response = restarted.dispatch(search_request("restart-after"));
    let Some(ResponsePayload::Search(second_search)) = second_response.result else {
        panic!("second search")
    };
    assert_eq!(second_search.index_generation, generation);
    assert_eq!(second_search.hits[0].name, "architecture-plan.md");
}

#[test]
fn capability_is_derived_from_service_grant_not_request_payload() {
    let (_temp, graph, index) = fixture();
    let service = AgentService::open(
        graph,
        index,
        ClientAuthorization::new([Capability::IndexStatus]),
    )
    .expect("agent");
    let response = service.dispatch(search_request("denied-1"));
    assert_eq!(
        response.error.as_ref().map(|error| error.code),
        Some(AgentErrorCode::Forbidden)
    );
}

#[test]
fn cancellation_is_checked_between_query_engine_units() {
    let (_temp, graph, index) = fixture();
    let service =
        AgentService::open(graph, index, ClientAuthorization::v0_1_metadata()).expect("agent");
    let checks = AtomicUsize::new(0);
    let response = service.dispatch_cancellable(search_request("cancel-1"), || {
        checks.fetch_add(1, Ordering::SeqCst) > 0
    });
    assert_eq!(
        response.error.as_ref().map(|error| error.code),
        Some(AgentErrorCode::Cancelled)
    );
    assert!(checks.load(Ordering::SeqCst) >= 2);
}

#[test]
fn concurrent_readers_pause_projection_then_recover_without_search_failure() {
    let (_temp, graph_path, index) = fixture();
    let service = Arc::new(
        AgentService::open(&graph_path, index, ClientAuthorization::v0_1_metadata())
            .expect("agent"),
    );
    let volume = VolumeId::from_u128(7);
    let root = FileKey::new(volume, FileId128::from_u128(1));
    let file = FileKey::new(volume, FileId128::from_u128(2));
    let mut graph = FilesystemGraph::open(&graph_path).expect("graph writer");
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume,
            checkpoint: ProviderCheckpoint {
                provider_id: "agent-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![2],
            },
            mutations: vec![GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(102),
                    object_key: file,
                    parent_key: Some(root),
                    name: "architecture-concurrent.md".to_owned(),
                },
                traversal_boundary: false,
            }],
        })
        .expect("rename creates outbox work");
    drop(graph);

    let workers = 8;
    let barrier = Arc::new(Barrier::new(workers + 1));
    let mut handles = Vec::new();
    for worker in 0..workers {
        let service = Arc::clone(&service);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for query in 0..10 {
                let response =
                    service.dispatch(search_request(&format!("reader-{worker}-{query}")));
                assert!(response.error.is_none(), "{:?}", response.error);
            }
        }));
    }
    barrier.wait();
    for handle in handles {
        handle.join().expect("reader thread");
    }

    let paused = service
        .maintain_projection()
        .expect("interactive maintenance pause");
    assert_eq!(paused.backlog_mutations, 1);
    assert!(
        service
            .governor_decision()
            .expect("governor decision")
            .budget
            .background_paused
    );

    let mut status = paused;
    for _ in 0..8 {
        status = service
            .maintain_projection()
            .expect("bounded projection recovery");
        if status.backlog_mutations == 0 {
            break;
        }
    }
    assert_eq!(status.backlog_mutations, 0);
    let response = service.dispatch(search_request("after-projection"));
    let Some(ResponsePayload::Search(search)) = response.result else {
        panic!("post projection search")
    };
    assert_eq!(search.hits[0].name, "architecture-concurrent.md");
}

#[test]
fn critical_pressure_pauses_projection_without_advancing_the_durable_cursor() {
    let (_temp, graph_path, index) = fixture();
    let service = AgentService::open(&graph_path, index, ClientAuthorization::v0_1_metadata())
        .expect("agent");
    let before = service.index_status().expect("initial status");
    let volume = VolumeId::from_u128(7);
    let root = FileKey::new(volume, FileId128::from_u128(1));
    let file = FileKey::new(volume, FileId128::from_u128(2));
    let mut graph = FilesystemGraph::open(&graph_path).expect("graph writer");
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume,
            checkpoint: ProviderCheckpoint {
                provider_id: "agent-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![3],
            },
            mutations: vec![GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(102),
                    object_key: file,
                    parent_key: Some(root),
                    name: "architecture-pressure.md".to_owned(),
                },
                traversal_boundary: false,
            }],
        })
        .expect("durable mutation");
    drop(graph);

    let decision = service
        .report_system_pressure(SystemPressure {
            available_memory_basis_points: Some(400),
            ..SystemPressure::default()
        })
        .expect("pressure report");
    assert_eq!(decision.mode, GovernorMode::Pressure);
    let paused = service.maintain_projection().expect("paused maintenance");
    assert_eq!(paused.applied_sequence, before.applied_sequence);
    assert_eq!(paused.backlog_mutations, 1);
    assert_eq!(
        service.governor_decision().expect("decision").mode,
        GovernorMode::Pressure
    );
    let unavailable = service
        .report_resource_unavailable()
        .expect("unavailable resource sample");
    assert_eq!(unavailable.mode, GovernorMode::Pressure);
    assert_eq!(
        unavailable.reason,
        DecisionReason::ResourceTelemetryUnavailable
    );
    assert!(unavailable.budget.background_paused);
}

#[test]
fn interactive_search_pauses_projection_without_advancing_the_durable_cursor() {
    let (_temp, graph_path, index) = fixture();
    let service = AgentService::open(&graph_path, index, ClientAuthorization::v0_1_metadata())
        .expect("agent");
    let before = service.index_status().expect("initial status");
    let volume = VolumeId::from_u128(7);
    let root = FileKey::new(volume, FileId128::from_u128(1));
    let file = FileKey::new(volume, FileId128::from_u128(2));
    let mut graph = FilesystemGraph::open(&graph_path).expect("graph writer");
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume,
            checkpoint: ProviderCheckpoint {
                provider_id: "agent-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![5],
            },
            mutations: vec![GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(102),
                    object_key: file,
                    parent_key: Some(root),
                    name: "architecture-active.md".to_owned(),
                },
                traversal_boundary: false,
            }],
        })
        .expect("durable mutation");
    drop(graph);

    let response = service.dispatch(search_request("architecture"));
    assert!(response.error.is_none(), "{:?}", response.error);
    let decision = service.governor_decision().expect("active decision");
    assert_eq!(decision.mode, GovernorMode::Active);
    assert!(decision.budget.background_paused);

    let paused = service
        .maintain_projection_scheduled()
        .expect("paused scheduled maintenance");
    assert_eq!(paused.applied_sequence, before.applied_sequence);
    assert_eq!(paused.backlog_mutations, 1);
}

#[test]
fn interactive_search_uses_committed_snapshot_without_reopening_sqlite() {
    let (_temp, graph_path, index) = fixture();
    let service = AgentService::open(&graph_path, &index, ClientAuthorization::v0_1_metadata())
        .expect("agent");
    let battery = service
        .report_system_pressure(SystemPressure {
            power_source: PowerSource::Battery,
            battery_percent: Some(24),
            ..SystemPressure::default()
        })
        .expect("battery observation");
    assert_eq!(battery.mode, GovernorMode::Battery);

    std::fs::remove_file(&graph_path).expect("remove disposable graph");
    let response = service.dispatch(search_request("interactive-snapshot"));
    let Some(ResponsePayload::Search(search)) = response.result else {
        panic!("cached snapshot search: {:?}", response.error)
    };
    assert_eq!(search.hits[0].name, "architecture-plan.md");
    let active = service.governor_decision().expect("active decision");
    assert_eq!(active.mode, GovernorMode::Active);
    assert_eq!(active.reason, DecisionReason::UserActive);
    assert!(active.budget.background_paused);
}

#[test]
fn search_uses_the_cached_snapshot_without_reopening_tantivy() {
    let (_temp, graph_path, index) = fixture();
    let service = AgentService::open(&graph_path, &index, ClientAuthorization::v0_1_metadata())
        .expect("agent");
    let marker = index
        .join("generation-00000000000000000001")
        .join("LOCALSEARCH_SCHEMA");
    std::fs::rename(&marker, marker.with_extension("disabled"))
        .expect("hide disposable schema marker");

    let response = service.dispatch(search_request("cached-reader"));
    let Some(ResponsePayload::Search(search)) = response.result else {
        panic!("cached search response: {:?}", response.error)
    };
    assert_eq!(search.hits[0].name, "architecture-plan.md");
}

#[test]
fn trusted_scheduler_observation_requires_backlog_and_new_input_cancels_boost() {
    let (_temp, graph_path, index) = fixture();
    let service = AgentService::open(&graph_path, index, ClientAuthorization::v0_1_metadata())
        .expect("agent");
    let volume = VolumeId::from_u128(7);
    let root = FileKey::new(volume, FileId128::from_u128(1));
    let file = FileKey::new(volume, FileId128::from_u128(2));
    let mut graph = FilesystemGraph::open(&graph_path).expect("graph writer");
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume,
            checkpoint: ProviderCheckpoint {
                provider_id: "agent-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![4],
            },
            mutations: vec![GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(102),
                    object_key: file,
                    parent_key: Some(root),
                    name: "architecture-idle.md".to_owned(),
                },
                traversal_boundary: false,
            }],
        })
        .expect("durable mutation");
    drop(graph);

    let healthy_ac = SystemPressure {
        available_memory_basis_points: Some(5_000),
        system_cpu_load_basis_points: Some(500),
        power_source: PowerSource::Ac,
        ..SystemPressure::default()
    };
    for _ in 0..5 {
        service
            .report_resource_observation(healthy_ac, Some(5 * 60 * 1_000), 1)
            .expect("trusted idle observation");
    }
    assert_eq!(
        service.governor_decision().expect("boost decision").mode,
        GovernorMode::IdleBoost
    );

    let after_input = service
        .report_resource_observation(healthy_ac, Some(0), 1)
        .expect("new input observation");
    assert_eq!(after_input.mode, GovernorMode::Balanced);
}

#[test]
fn scheduled_projection_compacts_only_after_catalog_ack() {
    let (_temp, graph_path, index) = fixture();
    let service = AgentService::open(&graph_path, &index, ClientAuthorization::v0_1_metadata())
        .expect("service");
    let volume = VolumeId::from_u128(7);
    let root = FileKey::new(volume, FileId128::from_u128(1));
    let file = FileKey::new(volume, FileId128::from_u128(2));
    FilesystemGraph::open(&graph_path)
        .expect("graph writer")
        .apply_batch(&GraphMutationBatch {
            volume_id: volume,
            checkpoint: ProviderCheckpoint {
                provider_id: "agent-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![4],
            },
            mutations: vec![GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(102),
                    object_key: file,
                    parent_key: Some(root),
                    name: "architecture-compacted.md".to_owned(),
                },
                traversal_boundary: false,
            }],
        })
        .expect("mutation");
    service
        .maintain_all_projections_scheduled()
        .expect("scheduled maintenance");
    assert!(
        FilesystemGraph::open_read_only(&graph_path)
            .expect("graph reader")
            .read_outbox(None, 10)
            .expect("outbox")
            .mutations
            .is_empty()
    );
}

#[test]
fn scheduled_projection_drains_bounded_volume_refresh_before_catalog_ack() {
    let (_temp, graph_path, index) = fixture();
    let service = AgentService::open(&graph_path, &index, ClientAuthorization::v0_1_metadata())
        .expect("service");
    let volume = VolumeId::from_u128(7);
    let transition = FilesystemGraph::open(&graph_path)
        .expect("graph writer")
        .apply_batch(&GraphMutationBatch {
            volume_id: volume,
            checkpoint: ProviderCheckpoint {
                provider_id: "agent-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![5],
            },
            mutations: vec![GraphMutation::SetVolumeState {
                volume_id: volume,
                state: localsearch_filesystem_graph::VolumeState::Offline,
            }],
        })
        .expect("offline transition");
    assert_eq!(transition.outbox_mutations_appended, 0);
    let pending = FilesystemGraph::open_read_only(&graph_path).expect("pending graph");
    assert!(
        pending
            .projection_refresh_maintenance_pending()
            .expect("pending refresh")
    );
    drop(pending);

    service
        .maintain_all_projections_scheduled()
        .expect("scheduled refresh and projection");
    let graph = FilesystemGraph::open_read_only(&graph_path).expect("completed graph");
    assert!(
        !graph
            .projection_refresh_maintenance_pending()
            .expect("completed refresh")
    );
    assert!(
        graph
            .read_outbox(None, 10)
            .expect("compacted outbox")
            .mutations
            .is_empty()
    );
    drop(graph);
    let response = service.dispatch(search_request("offline-volume-refresh"));
    let Some(ResponsePayload::Search(search)) = response.result else {
        panic!("search response: {:?}", response.error)
    };
    assert_eq!(search.hits.len(), 1);
    assert_eq!(search.hits[0].availability, Availability::Offline);
}

#[cfg(windows)]
#[test]
fn cli_shape_round_trip_over_authenticated_named_pipe_reaches_tantivy() {
    let (_temp, graph, index) = fixture();
    let service = Arc::new(
        AgentService::open(graph, index, ClientAuthorization::v0_1_metadata()).expect("agent"),
    );
    let pipe_name = format!(
        r"\\.\pipe\LocalSearch\Agent\v1\test-{}-{}",
        std::process::id(),
        7
    );
    let (ready_tx, ready_rx) = mpsc::channel();
    let server_name = pipe_name.clone();
    let server_service = Arc::clone(&service);
    let server = std::thread::spawn(move || {
        let server = localsearch_agent::windows_pipe::NamedPipeServer::bind(&server_name)
            .expect("secure bind");
        ready_tx.send(()).expect("ready");
        server
            .serve_one(
                |request, cancelled| server_service.dispatch_cancellable(request, cancelled),
                Duration::from_secs(5),
            )
            .expect("serve request");
    });
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("server ready");
    let response = localsearch_agent::windows_pipe::round_trip(
        &pipe_name,
        &search_request("pipe-1"),
        Duration::from_secs(5),
    )
    .expect("pipe request");
    server.join().expect("server thread");
    let Some(ResponsePayload::Search(search)) = response.result else {
        panic!("search response")
    };
    assert_eq!(search.hits[0].name, "architecture-plan.md");
}

#[cfg(windows)]
#[test]
fn real_cli_process_searches_through_real_agent_process() {
    let (_temp, graph, index) = fixture();
    let pipe_name = format!(
        r"\\.\pipe\LocalSearch\Agent\v1\process-test-{}",
        std::process::id()
    );
    let mut agent = Command::new(env!("CARGO_BIN_EXE_localsearch-agent"))
        .args([
            "--graph",
            graph.to_str().expect("graph path"),
            "--index",
            index.to_str().expect("index path"),
            "--pipe",
            &pipe_name,
            "--once",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agent");
    let stderr = agent.stderr.take().expect("agent stderr");
    let mut reader = BufReader::new(stderr);
    let mut ready = String::new();
    reader.read_line(&mut ready).expect("read readiness");
    assert!(ready.starts_with("LocalSearch Agent ready:"), "{ready}");

    let output = Command::new(env!("CARGO_BIN_EXE_localsearch-cli"))
        .args(["--pipe", &pipe_name, "search", "architecture"])
        .output()
        .expect("run CLI");
    assert!(
        output.status.success(),
        "CLI stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: localsearch_agent_api::AgentResponse =
        serde_json::from_slice(&output.stdout).expect("CLI JSON");
    let Some(ResponsePayload::Search(search)) = response.result else {
        panic!("CLI search response")
    };
    assert_eq!(search.hits[0].name, "architecture-plan.md");
    assert!(agent.wait().expect("agent exit").success());
}

#[cfg(windows)]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "real process coverage keeps startup, projection, search, and compaction in one lifecycle"
)]
fn real_agent_scheduler_projects_mutations_created_after_startup() {
    struct ChildGuard(std::process::Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let (_temp, graph_path, index) = fixture();
    let pipe_name = format!(
        r"\\.\pipe\LocalSearch\Agent\v1\scheduler-test-{}",
        std::process::id()
    );
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_localsearch-agent"))
            .args([
                "--graph",
                graph_path.to_str().expect("graph path"),
                "--index",
                index.to_str().expect("index path"),
                "--pipe",
                &pipe_name,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn scheduled agent"),
    );
    let stderr = child.0.stderr.take().expect("agent stderr");
    let mut reader = BufReader::new(stderr);
    let mut ready = false;
    for _ in 0..10 {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read readiness");
        if line.starts_with("LocalSearch Agent ready:") {
            ready = true;
            break;
        }
    }
    assert!(ready, "scheduled Agent did not become ready");

    let volume = VolumeId::from_u128(7);
    let root = FileKey::new(volume, FileId128::from_u128(1));
    let file = FileKey::new(volume, FileId128::from_u128(2));
    let mut graph = FilesystemGraph::open(&graph_path).expect("graph writer");
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume,
            checkpoint: ProviderCheckpoint {
                provider_id: "agent-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![4],
            },
            mutations: vec![GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(102),
                    object_key: file,
                    parent_key: Some(root),
                    name: "architecture-governed.md".to_owned(),
                },
                traversal_boundary: false,
            }],
        })
        .expect("post-start mutation");
    drop(graph);

    let mut projected = false;
    for attempt in 0..100 {
        let response = localsearch_agent::windows_pipe::round_trip(
            &pipe_name,
            &AgentRequest {
                protocol_version: AGENT_API_VERSION,
                codec_version: AGENT_CODEC_VERSION,
                request_id: format!("scheduler-status-{attempt}"),
                deadline_ms: 2_000,
                operation: RequestOperation::IndexGetStatus,
            },
            Duration::from_secs(2),
        )
        .expect("scheduled status");
        if response.result.is_some_and(
            |payload| matches!(payload, ResponsePayload::IndexStatus(status) if status.backlog_mutations == 0),
        ) {
            projected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        projected,
        "Agent scheduler did not project the durable mutation"
    );
    let mut compacted = false;
    for _ in 0..200 {
        let graph = FilesystemGraph::open_read_only(&graph_path).expect("compacted graph");
        if graph
            .read_outbox(None, 10)
            .expect("compacted outbox")
            .mutations
            .is_empty()
        {
            compacted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        compacted,
        "Agent scheduler did not compact acknowledged outbox"
    );
    let response = localsearch_agent::windows_pipe::round_trip(
        &pipe_name,
        &search_request("scheduler-search"),
        Duration::from_secs(2),
    )
    .expect("search projected mutation");
    assert!(response.result.is_some_and(|payload| {
        matches!(payload, ResponsePayload::Search(search) if search.hits.iter().any(|hit| hit.name == "architecture-governed.md"))
    }));
}
