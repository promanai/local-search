use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, ReconciliationReason, VolumeId,
};
use localsearch_filesystem_graph::{
    FilesystemGraph, GraphError, GraphIntegrityIssue, GraphMutation, GraphMutationBatch,
    VolumeState,
};
use localsearch_platform_core::{ProviderCheckpoint, VolumeDescriptor};
use rusqlite::Connection;

fn volume() -> VolumeId {
    VolumeId::from_u128(7)
}

fn key(value: u128) -> FileKey {
    FileKey::new(volume(), FileId128::from_u128(value))
}

fn link(value: u128, object: FileKey, parent: Option<FileKey>, name: &str) -> FileLinkSnapshot {
    FileLinkSnapshot {
        file_link_id: FileLinkId::from_u128(value),
        object_key: object,
        parent_key: parent,
        name: name.to_owned(),
    }
}

fn object(value: u128, kind: FileKind) -> FileObjectSnapshot {
    FileObjectSnapshot {
        object_key: key(value),
        metadata: FileMetadata {
            kind,
            size: 10,
            created_at_unix_ms: Some(100),
            modified_at_unix_ms: Some(200),
            hidden: false,
            availability: Availability::Online,
        },
    }
}

fn descriptor() -> VolumeDescriptor {
    VolumeDescriptor {
        volume_id: volume(),
        display_name: Some("Graph test".to_owned()),
        mount_points: vec!["portable-root".to_owned()],
        filesystem: Some("testfs".to_owned()),
        removable: false,
        local: true,
    }
}

fn checkpoint(byte: u8) -> ProviderCheckpoint {
    ProviderCheckpoint {
        provider_id: "test-provider".to_owned(),
        format_version: 1,
        volume_id: volume(),
        opaque: vec![byte],
    }
}

fn base_events() -> Vec<FilesystemEvent> {
    vec![
        FilesystemEvent::ObjectObserved {
            object: object(1, FileKind::Directory),
        },
        FilesystemEvent::LinkObserved {
            link: link(101, key(1), None, "root"),
        },
    ]
}

fn apply(graph: &mut FilesystemGraph, marker: u8, mutations: Vec<GraphMutation>) {
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(marker),
            mutations,
        })
        .expect("test mutation batch should apply");
}

#[test]
fn read_only_api_path_never_waits_for_or_mutates_the_graph_writer() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("graph.sqlite3");
    let mut graph = FilesystemGraph::open(&database).expect("open graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), base_events())
        .expect("snapshot");
    drop(graph);

    let writer = Connection::open(&database).expect("writer connection");
    writer
        .execute_batch("PRAGMA journal_mode = WAL; BEGIN IMMEDIATE;")
        .expect("hold writer transaction");

    let started = Instant::now();
    let mut reader = FilesystemGraph::open_read_only(&database).expect("read-only graph");
    assert_eq!(reader.stats().expect("read stats").volumes, 1);
    assert!(started.elapsed() < Duration::from_millis(500));
    let mutation = reader.apply_batch(&GraphMutationBatch {
        volume_id: volume(),
        checkpoint: checkpoint(2),
        mutations: Vec::new(),
    });
    assert!(matches!(mutation, Err(GraphError::Sqlite(_))));

    writer.execute_batch("ROLLBACK;").expect("release writer");
}

#[test]
fn snapshot_checkpoint_and_paths_survive_restart() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("graph.sqlite3");
    {
        let mut graph = FilesystemGraph::open(&database).expect("open graph");
        let mut events = base_events();
        events.extend([
            FilesystemEvent::ObjectObserved {
                object: object(2, FileKind::File),
            },
            FilesystemEvent::LinkObserved {
                link: link(102, key(2), Some(key(1)), "document.txt"),
            },
        ]);
        graph
            .ingest_snapshot(descriptor(), checkpoint(1), events)
            .expect("ingest snapshot");
    }

    let graph = FilesystemGraph::open(&database).expect("reopen graph");
    assert_eq!(
        graph.schema_version(),
        localsearch_filesystem_graph::GRAPH_SCHEMA_VERSION
    );
    assert_eq!(
        graph.checkpoint(volume()).expect("checkpoint"),
        Some(checkpoint(1))
    );
    assert_eq!(
        graph
            .resolve_path(FileLinkId::from_u128(102), 32)
            .expect("resolve")
            .display(),
        "root/document.txt"
    );
    let stats = graph.stats().expect("stats");
    assert_eq!(stats.live_objects, 2);
    assert_eq!(stats.links, 2);
}

#[test]
fn hard_links_keep_object_alive_until_final_link_is_removed() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    let mut events = base_events();
    events.extend([
        FilesystemEvent::ObjectObserved {
            object: object(2, FileKind::File),
        },
        FilesystemEvent::LinkObserved {
            link: link(102, key(2), Some(key(1)), "a.txt"),
        },
        FilesystemEvent::LinkObserved {
            link: link(103, key(2), Some(key(1)), "b.txt"),
        },
    ]);
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), events)
        .expect("snapshot");

    apply(
        &mut graph,
        2,
        vec![GraphMutation::RemoveLink {
            file_link_id: FileLinkId::from_u128(102),
            object_key: key(2),
        }],
    );
    assert_eq!(graph.stats().expect("stats").live_objects, 2);

    apply(
        &mut graph,
        3,
        vec![GraphMutation::RemoveLink {
            file_link_id: FileLinkId::from_u128(103),
            object_key: key(2),
        }],
    );
    let stats = graph.stats().expect("stats");
    assert_eq!(stats.live_objects, 1);
    assert_eq!(stats.tombstoned_objects, 1);
}

#[test]
fn failed_batch_rolls_back_graph_and_checkpoint_together() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), base_events())
        .expect("snapshot");
    let result = graph.apply_batch(&GraphMutationBatch {
        volume_id: volume(),
        checkpoint: checkpoint(2),
        mutations: vec![GraphMutation::TombstoneObject { object_key: key(1) }],
    });
    assert!(matches!(result, Err(GraphError::Invariant(_))));
    assert_eq!(
        graph.checkpoint(volume()).expect("checkpoint"),
        Some(checkpoint(1))
    );
    assert_eq!(graph.stats().expect("stats").live_objects, 1);
}

#[test]
fn directory_rename_enqueues_one_bounded_refresh_and_preserves_descendants() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    let mut events = base_events();
    events.extend([
        FilesystemEvent::ObjectObserved {
            object: object(2, FileKind::Directory),
        },
        FilesystemEvent::LinkObserved {
            link: link(102, key(2), Some(key(1)), "before"),
        },
        FilesystemEvent::ObjectObserved {
            object: object(3, FileKind::File),
        },
        FilesystemEvent::LinkObserved {
            link: link(103, key(3), Some(key(2)), "child.txt"),
        },
    ]);
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), events)
        .expect("snapshot");

    let summary = graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(2),
            mutations: vec![
                GraphMutation::RemoveLink {
                    file_link_id: FileLinkId::from_u128(102),
                    object_key: key(2),
                },
                GraphMutation::UpsertLink {
                    link: link(202, key(2), Some(key(1)), "after"),
                    traversal_boundary: false,
                },
            ],
        })
        .expect("rename batch");
    assert_eq!(summary.refresh_jobs_enqueued, 1);
    assert_eq!(graph.pending_refresh_jobs(10).expect("jobs").len(), 1);
    assert_eq!(
        graph
            .resolve_path(FileLinkId::from_u128(103), 32)
            .expect("resolve child")
            .display(),
        "root/after/child.txt"
    );
}

#[test]
fn resolver_contains_corrupt_branches() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    let events = vec![
        FilesystemEvent::ObjectObserved {
            object: object(1, FileKind::Directory),
        },
        FilesystemEvent::ObjectObserved {
            object: object(2, FileKind::Directory),
        },
        FilesystemEvent::ObjectObserved {
            object: object(3, FileKind::File),
        },
        FilesystemEvent::LinkObserved {
            link: link(101, key(1), Some(key(2)), "a"),
        },
        FilesystemEvent::LinkObserved {
            link: link(102, key(2), Some(key(1)), "b"),
        },
        FilesystemEvent::LinkObserved {
            link: link(103, key(3), Some(key(1)), "child"),
        },
    ];
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), events)
        .expect("snapshot");
    assert!(matches!(
        graph.resolve_path(FileLinkId::from_u128(103), 32),
        Err(GraphError::ParentCycle(_))
    ));
}

#[test]
fn resolver_rejects_missing_ambiguous_and_boundary_parents() {
    let mut missing = FilesystemGraph::open_in_memory().expect("graph");
    missing
        .ingest_snapshot(
            descriptor(),
            checkpoint(1),
            [
                FilesystemEvent::ObjectObserved {
                    object: object(3, FileKind::File),
                },
                FilesystemEvent::LinkObserved {
                    link: link(103, key(3), Some(key(99)), "orphan"),
                },
            ],
        )
        .expect("snapshot");
    assert!(matches!(
        missing.resolve_path(FileLinkId::from_u128(103), 32),
        Err(GraphError::MissingParent(parent)) if parent == key(99)
    ));

    let mut ambiguous = FilesystemGraph::open_in_memory().expect("graph");
    ambiguous
        .ingest_snapshot(
            descriptor(),
            checkpoint(1),
            [
                FilesystemEvent::ObjectObserved {
                    object: object(1, FileKind::Directory),
                },
                FilesystemEvent::ObjectObserved {
                    object: object(2, FileKind::File),
                },
                FilesystemEvent::LinkObserved {
                    link: link(101, key(1), None, "root-a"),
                },
                FilesystemEvent::LinkObserved {
                    link: link(102, key(1), None, "root-b"),
                },
                FilesystemEvent::LinkObserved {
                    link: link(103, key(2), Some(key(1)), "child"),
                },
            ],
        )
        .expect("snapshot");
    assert!(matches!(
        ambiguous.resolve_path(FileLinkId::from_u128(103), 32),
        Err(GraphError::AmbiguousParent(parent)) if parent == key(1)
    ));

    let mut boundary = FilesystemGraph::open_in_memory().expect("graph");
    boundary
        .ingest_snapshot(
            descriptor(),
            checkpoint(1),
            [
                FilesystemEvent::ObjectObserved {
                    object: object(1, FileKind::Directory),
                },
                FilesystemEvent::ObjectObserved {
                    object: object(2, FileKind::Directory),
                },
                FilesystemEvent::ObjectObserved {
                    object: object(3, FileKind::File),
                },
                FilesystemEvent::LinkObserved {
                    link: link(101, key(1), None, "root"),
                },
                FilesystemEvent::LinkObserved {
                    link: link(103, key(3), Some(key(2)), "child"),
                },
            ],
        )
        .expect("snapshot");
    apply(
        &mut boundary,
        2,
        vec![GraphMutation::UpsertLink {
            link: link(102, key(2), Some(key(1)), "boundary"),
            traversal_boundary: true,
        }],
    );
    assert!(matches!(
        boundary.resolve_path(FileLinkId::from_u128(103), 32),
        Err(GraphError::TraversalBoundary(id)) if id == FileLinkId::from_u128(102)
    ));
}

#[test]
fn reconciliation_and_offline_state_are_portable() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), base_events())
        .expect("snapshot");
    apply(
        &mut graph,
        2,
        vec![GraphMutation::SetVolumeState {
            volume_id: volume(),
            state: VolumeState::Offline,
        }],
    );
    assert_eq!(
        graph.volume_state(volume()).expect("state"),
        Some(VolumeState::Offline)
    );
    assert!(
        graph
            .desired_catalog_documents()
            .expect("offline catalog")
            .iter()
            .all(|document| document.metadata.availability == Availability::Offline)
    );
    apply(
        &mut graph,
        3,
        vec![GraphMutation::RequireReconciliation {
            volume_id: volume(),
            reason: ReconciliationReason::SourceHistoryUnavailable,
        }],
    );
    assert_eq!(
        graph.volume_state(volume()).expect("state"),
        Some(VolumeState::NeedsReconciliation)
    );
    assert!(
        graph
            .desired_catalog_documents()
            .expect("reconciling catalog")
            .iter()
            .all(|document| document.metadata.availability == Availability::Unknown)
    );
}

#[test]
fn empty_batch_can_advance_checkpoint_but_cannot_cross_volumes() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), base_events())
        .expect("snapshot");
    apply(&mut graph, 2, Vec::new());
    assert_eq!(
        graph.checkpoint(volume()).expect("checkpoint"),
        Some(checkpoint(2))
    );

    let mut wrong = checkpoint(3);
    wrong.volume_id = VolumeId::from_u128(999);
    let result = graph.apply_batch(&GraphMutationBatch {
        volume_id: volume(),
        checkpoint: wrong,
        mutations: Vec::new(),
    });
    assert!(matches!(result, Err(GraphError::InvalidBatch(_))));
}

#[test]
fn integrity_audit_contains_orphans_and_missing_parents() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(
            descriptor(),
            checkpoint(1),
            [
                FilesystemEvent::ObjectObserved {
                    object: object(1, FileKind::Directory),
                },
                FilesystemEvent::ObjectObserved {
                    object: object(2, FileKind::File),
                },
                FilesystemEvent::ObjectObserved {
                    object: object(3, FileKind::File),
                },
                FilesystemEvent::LinkObserved {
                    link: link(103, key(3), Some(key(99)), "missing-parent.txt"),
                },
            ],
        )
        .expect("snapshot");
    let issues = graph.audit_integrity(10).expect("integrity audit");
    assert!(issues.contains(&GraphIntegrityIssue::OrphanObject { object_key: key(1) }));
    assert!(issues.contains(&GraphIntegrityIssue::OrphanObject { object_key: key(2) }));
    assert!(issues.contains(&GraphIntegrityIssue::MissingParent {
        file_link_id: FileLinkId::from_u128(103),
        parent_key: key(99),
    }));
}

#[test]
fn exact_duplicate_namespace_links_are_rejected_atomically() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    let mut events = base_events();
    events.extend([
        FilesystemEvent::ObjectObserved {
            object: object(2, FileKind::File),
        },
        FilesystemEvent::ObjectObserved {
            object: object(3, FileKind::File),
        },
        FilesystemEvent::LinkObserved {
            link: link(102, key(2), Some(key(1)), "same.txt"),
        },
    ]);
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), events)
        .expect("snapshot");
    let result = graph.apply_batch(&GraphMutationBatch {
        volume_id: volume(),
        checkpoint: checkpoint(2),
        mutations: vec![GraphMutation::UpsertLink {
            link: link(103, key(3), Some(key(1)), "same.txt"),
            traversal_boundary: false,
        }],
    });
    assert!(result.is_err());
    assert_eq!(
        graph.checkpoint(volume()).expect("checkpoint"),
        Some(checkpoint(1))
    );
    assert_eq!(graph.stats().expect("stats").links, 2);
}
use std::time::{Duration, Instant};
