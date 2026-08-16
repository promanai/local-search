use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, IndexMutation, MutationSeq, ReconciliationReason,
    VolumeId,
};
use localsearch_filesystem_graph::{
    FilesystemGraph, GraphMutation, GraphMutationBatch, ObservationScanMode, ObservationScanPhase,
    VolumeState,
};
use localsearch_platform_core::{ProviderCheckpoint, VolumeDescriptor};

fn volume() -> VolumeId {
    VolumeId::from_u128(60)
}

fn key(value: u128) -> FileKey {
    FileKey::new(volume(), FileId128::from_u128(value))
}

fn object(value: u128, kind: FileKind, size: u64) -> FileObjectSnapshot {
    FileObjectSnapshot {
        object_key: key(value),
        metadata: FileMetadata {
            kind,
            size,
            created_at_unix_ms: None,
            modified_at_unix_ms: Some(10),
            hidden: false,
            availability: Availability::Online,
        },
    }
}

fn link(value: u128, object: FileKey, parent: Option<FileKey>, name: &str) -> FileLinkSnapshot {
    FileLinkSnapshot {
        file_link_id: FileLinkId::from_u128(value),
        object_key: object,
        parent_key: parent,
        name: name.to_owned(),
    }
}

fn checkpoint(value: u8) -> ProviderCheckpoint {
    ProviderCheckpoint {
        provider_id: "projection-test".to_owned(),
        format_version: 1,
        volume_id: volume(),
        opaque: vec![value],
    }
}

fn descriptor() -> VolumeDescriptor {
    VolumeDescriptor {
        volume_id: volume(),
        display_name: Some("projection".to_owned()),
        mount_points: vec!["root".to_owned()],
        filesystem: Some("testfs".to_owned()),
        removable: false,
        local: true,
    }
}

fn initial_events() -> Vec<FilesystemEvent> {
    vec![
        FilesystemEvent::ObjectObserved {
            object: object(1, FileKind::Directory, 0),
        },
        FilesystemEvent::LinkObserved {
            link: link(101, key(1), None, "root"),
        },
        FilesystemEvent::ObjectObserved {
            object: object(2, FileKind::File, 20),
        },
        FilesystemEvent::LinkObserved {
            link: link(102, key(2), Some(key(1)), "note.txt"),
        },
    ]
}

fn initial_batch(checkpoint_value: u8) -> GraphMutationBatch {
    let mut mutations = vec![GraphMutation::UpsertVolume {
        descriptor: descriptor(),
    }];
    mutations.extend(initial_events().into_iter().map(GraphMutation::from));
    GraphMutationBatch {
        volume_id: volume(),
        checkpoint: checkpoint(checkpoint_value),
        mutations,
    }
}

#[test]
fn rebuildable_initial_batch_preserves_desired_state_without_outbox() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    assert!(!graph.has_projection_consumers().expect("consumers"));

    let initial = graph
        .apply_rebuildable_batch(&initial_batch(1))
        .expect("rebuildable initial batch");
    assert_eq!(initial.outbox_mutations_appended, 0);
    assert_eq!(
        graph.latest_outbox_sequence().expect("sequence"),
        MutationSeq(0)
    );
    assert!(
        graph
            .read_outbox(None, 10)
            .expect("outbox")
            .mutations
            .is_empty()
    );
    let documents = graph.desired_catalog_documents().expect("documents");
    assert_eq!(documents.len(), 2);
    assert!(documents.iter().any(|document| document.name == "note.txt"));

    graph
        .acknowledge_projection("content-v1", MutationSeq(0), 1)
        .expect("register consumer after full rebuild");
    assert!(graph.has_projection_consumers().expect("consumers"));
    let changed = graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(2),
            mutations: vec![GraphMutation::UpsertObject {
                object: object(2, FileKind::File, 21),
            }],
        })
        .expect("durable change");
    assert_eq!(changed.outbox_mutations_appended, 1);
    assert_eq!(
        graph.latest_outbox_sequence().expect("sequence"),
        MutationSeq(1)
    );
}

#[test]
fn rebuildable_batch_is_rejected_after_consumer_registration() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .apply_rebuildable_batch(&initial_batch(1))
        .expect("rebuildable initial batch");
    graph
        .acknowledge_projection("content-v1", MutationSeq(0), 1)
        .expect("consumer");

    let rejected = graph.apply_rebuildable_batch(&GraphMutationBatch {
        volume_id: volume(),
        checkpoint: checkpoint(2),
        mutations: vec![GraphMutation::UpsertObject {
            object: object(2, FileKind::File, 99),
        }],
    });
    assert!(rejected.is_err());
    assert_eq!(
        graph.checkpoint(volume()).expect("checkpoint"),
        Some(checkpoint(1))
    );
    assert!(
        graph
            .read_outbox(None, 10)
            .expect("outbox")
            .mutations
            .is_empty()
    );
    assert_eq!(
        graph
            .desired_catalog_documents()
            .expect("documents")
            .into_iter()
            .find(|document| document.name == "note.txt")
            .expect("note")
            .metadata
            .size,
        20
    );
}

#[test]
fn volume_state_projection_fans_out_through_a_bounded_durable_cursor() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    graph
        .acknowledge_projection("catalog-v1", MutationSeq(2), 1)
        .expect("consumer");

    let transition = graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(2),
            mutations: vec![GraphMutation::SetVolumeState {
                volume_id: volume(),
                state: VolumeState::Offline,
            }],
        })
        .expect("offline transition");
    assert_eq!(transition.outbox_mutations_appended, 0);
    assert_eq!(transition.refresh_jobs_enqueued, 1);
    assert_eq!(
        graph.latest_outbox_sequence().expect("sequence"),
        MutationSeq(2)
    );
    assert!(
        graph
            .projection_refresh_maintenance_pending()
            .expect("pending")
    );
    assert_eq!(
        graph.stats().expect("pending stats").pending_refresh_jobs,
        1
    );
    assert!(
        graph
            .desired_catalog_documents()
            .expect("desired")
            .iter()
            .all(|document| document.metadata.availability == Availability::Offline)
    );

    let first = graph.refresh_volume_projection(1).expect("first page");
    assert_eq!(first.links_scanned, 1);
    assert_eq!(first.outbox_mutations_appended, 1);
    assert!(!first.job_completed);
    let second = graph.refresh_volume_projection(1).expect("second page");
    assert_eq!(second.links_scanned, 1);
    assert_eq!(second.outbox_mutations_appended, 1);
    assert!(!second.job_completed);
    let terminal = graph.refresh_volume_projection(1).expect("terminal page");
    assert_eq!(terminal.links_scanned, 0);
    assert!(terminal.job_completed);
    assert!(
        !graph
            .projection_refresh_maintenance_pending()
            .expect("complete")
    );
    assert_eq!(
        graph.stats().expect("complete stats").pending_refresh_jobs,
        0
    );
    assert_eq!(
        graph.latest_outbox_sequence().expect("sequence"),
        MutationSeq(4)
    );
    let updates = graph
        .read_outbox(Some(MutationSeq(2)), 10)
        .expect("availability updates");
    assert_eq!(updates.mutations.len(), 2);
    assert!(updates.mutations.iter().all(|mutation| {
        matches!(
            &mutation.mutation,
            IndexMutation::Upsert { document }
                if document.metadata.availability == Availability::Offline
        )
    }));
}

#[test]
fn authoritative_scan_restart_sweeps_stale_rows_before_activating_checkpoint() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    let first = graph
        .begin_observation_scan(
            &descriptor(),
            ObservationScanMode::Reconcile,
            ReconciliationReason::SourceHistoryUnavailable,
        )
        .expect("first session");
    graph
        .apply_observation_scan_page(
            volume(),
            vec![FilesystemEvent::ObjectObserved {
                object: object(3, FileKind::File, 30),
            }],
        )
        .expect("partial page before crash");

    let restarted = graph
        .begin_observation_scan(
            &descriptor(),
            ObservationScanMode::Reconcile,
            ReconciliationReason::SourceHistoryUnavailable,
        )
        .expect("restarted session");
    assert!(restarted.scan_generation > first.scan_generation);
    graph
        .apply_observation_scan_page(volume(), initial_events()[..2].to_vec())
        .expect("authoritative root page");
    graph
        .stage_observation_checkpoint(volume(), &checkpoint(9))
        .expect("stage final checkpoint");
    assert_eq!(
        graph
            .observation_session(volume())
            .expect("session")
            .expect("present")
            .phase,
        ObservationScanPhase::SweepingLinks
    );
    assert_eq!(
        graph.volume_state(volume()).expect("volume state"),
        Some(VolumeState::NeedsReconciliation)
    );

    let first_sweep = graph
        .finalize_observation_scan(volume(), 1)
        .expect("first bounded sweep");
    assert_eq!(first_sweep.stale_links_removed, 1);
    assert!(!first_sweep.completed);
    let phase_change = graph
        .finalize_observation_scan(volume(), 1)
        .expect("link terminal page");
    assert_eq!(phase_change.stale_links_removed, 0);
    assert!(!phase_change.completed);
    let object_sweep = graph
        .finalize_observation_scan(volume(), 1)
        .expect("bounded object sweep");
    assert_eq!(object_sweep.stale_objects_tombstoned, 1);
    assert!(!object_sweep.completed);
    let completed = graph
        .finalize_observation_scan(volume(), 1)
        .expect("object terminal page");
    assert!(completed.completed);
    assert!(
        graph
            .observation_session(volume())
            .expect("session")
            .is_none()
    );
    assert_eq!(
        graph.checkpoint(volume()).expect("checkpoint"),
        Some(checkpoint(9))
    );
    assert_eq!(
        graph.volume_state(volume()).expect("volume state"),
        Some(VolumeState::Online)
    );
    let documents = graph.desired_catalog_documents().expect("documents");
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].name, "root");
    let stats = graph.stats().expect("stats");
    assert_eq!(stats.links, 1);
    assert_eq!(stats.live_objects, 1);
    assert_eq!(stats.tombstoned_objects, 2);
}

#[test]
fn authoritative_scan_replaces_a_stale_same_name_identity_atomically() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    graph
        .begin_observation_scan(
            &descriptor(),
            ObservationScanMode::Reconcile,
            ReconciliationReason::SourceHistoryUnavailable,
        )
        .expect("session");
    let mut replacement = initial_events()[..2].to_vec();
    replacement.extend([
        FilesystemEvent::ObjectObserved {
            object: object(3, FileKind::File, 40),
        },
        FilesystemEvent::LinkObserved {
            link: link(103, key(3), Some(key(1)), "note.txt"),
        },
    ]);
    graph
        .apply_observation_scan_page(volume(), replacement)
        .expect("same-name replacement page");
    let documents = graph.desired_catalog_documents().expect("documents");
    assert_eq!(documents.len(), 2);
    let replacement = documents
        .iter()
        .find(|document| document.name == "note.txt")
        .expect("replacement");
    assert_eq!(
        replacement.identity.file_link_id,
        FileLinkId::from_u128(103)
    );
    assert_eq!(replacement.identity.object_key, key(3));
    assert_eq!(replacement.metadata.size, 40);
}

#[test]
fn ingestion_and_projection_ack_do_not_fail_with_busy_snapshot() {
    let temp = tempfile::tempdir().expect("temp");
    let path = temp.path().join("concurrent.sqlite3");
    let mut seed = FilesystemGraph::open(&path).expect("seed graph");
    seed.ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    drop(seed);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let reader_barrier = std::sync::Arc::clone(&barrier);
    let reader_path = path.clone();
    let acknowledger = std::thread::spawn(move || {
        let graph = FilesystemGraph::open(reader_path).expect("projection connection");
        reader_barrier.wait();
        for _ in 0..200 {
            let latest = graph.latest_outbox_sequence().expect("latest sequence");
            graph
                .acknowledge_projection("concurrent-catalog", latest, 1)
                .expect("projection ACK");
            std::thread::yield_now();
        }
    });

    let mut ingestion = FilesystemGraph::open(path).expect("ingestion connection");
    barrier.wait();
    for generation in 2_u8..=101 {
        ingestion
            .apply_batch(&GraphMutationBatch {
                volume_id: volume(),
                checkpoint: checkpoint(generation),
                mutations: vec![GraphMutation::UpsertObject {
                    object: object(2, FileKind::File, u64::from(generation)),
                }],
            })
            .expect("concurrent ingestion");
        std::thread::yield_now();
    }
    acknowledger.join().expect("acknowledger");
}

#[test]
fn graph_state_checkpoint_desired_documents_and_outbox_commit_together() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    let summary = graph
        .ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    assert_eq!(summary.outbox_mutations_appended, 2);
    assert_eq!(
        graph.latest_outbox_sequence().expect("sequence"),
        MutationSeq(2)
    );
    let batch = graph.read_outbox(None, 10).expect("outbox");
    batch.validate().expect("consecutive outbox");
    assert_eq!(batch.mutations.len(), 2);
    assert!(
        batch
            .mutations
            .iter()
            .all(|item| matches!(item.mutation, IndexMutation::Upsert { .. }))
    );
    let documents = graph.desired_catalog_documents().expect("documents");
    assert_eq!(documents.len(), 2);
    assert!(documents.iter().any(|document| {
        document.name == "note.txt" && document.resolved_path == "root/note.txt"
    }));

    let failed = graph.apply_batch(&GraphMutationBatch {
        volume_id: volume(),
        checkpoint: checkpoint(2),
        mutations: vec![GraphMutation::TombstoneObject { object_key: key(2) }],
    });
    assert!(failed.is_err());
    assert_eq!(
        graph.latest_outbox_sequence().expect("sequence"),
        MutationSeq(2)
    );
    assert_eq!(
        graph.checkpoint(volume()).expect("checkpoint"),
        Some(checkpoint(1))
    );
}

#[test]
fn semantic_noop_is_suppressed_and_link_delete_is_durable() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    let no_op = graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(2),
            mutations: vec![GraphMutation::UpsertObject {
                object: object(2, FileKind::File, 20),
            }],
        })
        .expect("no-op observation");
    assert_eq!(no_op.outbox_mutations_appended, 0);
    assert_eq!(
        graph.latest_outbox_sequence().expect("sequence"),
        MutationSeq(2)
    );

    let changed = graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(3),
            mutations: vec![GraphMutation::UpsertObject {
                object: object(2, FileKind::File, 21),
            }],
        })
        .expect("metadata change");
    assert_eq!(changed.outbox_mutations_appended, 1);

    let removed = graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(4),
            mutations: vec![GraphMutation::RemoveLink {
                file_link_id: FileLinkId::from_u128(102),
                object_key: key(2),
            }],
        })
        .expect("link delete");
    assert_eq!(removed.outbox_mutations_appended, 1);
    let tail = graph.read_outbox(Some(MutationSeq(3)), 10).expect("tail");
    assert!(matches!(
        tail.mutations[0].mutation,
        IndexMutation::Delete { .. }
    ));
    assert_eq!(
        graph.desired_catalog_documents().expect("documents").len(),
        1
    );
}

#[test]
fn directory_descendants_converge_through_bounded_restart_safe_refresh() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    let events = vec![
        FilesystemEvent::ObjectObserved {
            object: object(1, FileKind::Directory, 0),
        },
        FilesystemEvent::LinkObserved {
            link: link(101, key(1), None, "root"),
        },
        FilesystemEvent::ObjectObserved {
            object: object(2, FileKind::Directory, 0),
        },
        FilesystemEvent::LinkObserved {
            link: link(102, key(2), Some(key(1)), "before"),
        },
        FilesystemEvent::ObjectObserved {
            object: object(3, FileKind::File, 1),
        },
        FilesystemEvent::LinkObserved {
            link: link(103, key(3), Some(key(2)), "child.txt"),
        },
    ];
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), events)
        .expect("snapshot");
    graph
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
        .expect("directory rename");
    let before = graph.desired_catalog_documents().expect("documents");
    assert!(before.iter().any(|document| {
        document.name == "child.txt" && document.resolved_path == "root/before/child.txt"
    }));

    let mut appended = 0;
    loop {
        let result = graph.refresh_projection_paths(2).expect("refresh step");
        appended += result.outbox_mutations_appended;
        if result.job_completed {
            break;
        }
    }
    assert_eq!(appended, 1);
    let after = graph.desired_catalog_documents().expect("documents");
    assert!(after.iter().any(|document| {
        document.name == "child.txt" && document.resolved_path == "root/after/child.txt"
    }));
}

#[test]
fn projector_checkpoint_is_monotonic_and_bounded_by_outbox() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    graph
        .acknowledge_projection("catalog-v1", MutationSeq(2), 1)
        .expect("acknowledge");
    assert_eq!(
        graph
            .projector_checkpoint("catalog-v1")
            .expect("checkpoint")
            .expect("present")
            .last_sequence,
        2
    );
    assert!(
        graph
            .acknowledge_projection("catalog-v1", MutationSeq(1), 2)
            .is_err()
    );
    assert!(
        graph
            .acknowledge_projection("catalog-v1", MutationSeq(3), 2)
            .is_err()
    );
    assert_eq!(graph.prune_consumed_outbox().expect("prune"), 2);
    assert!(
        graph
            .read_outbox(None, 10)
            .expect("outbox")
            .mutations
            .is_empty()
    );
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(4),
            mutations: vec![GraphMutation::UpsertObject {
                object: object(2, FileKind::File, 99),
            }],
        })
        .expect("post-prune mutation");
    let resumed = graph
        .read_outbox(Some(MutationSeq(2)), 10)
        .expect("post-prune outbox");
    assert_eq!(resumed.first_sequence(), Some(MutationSeq(3)));
    assert_eq!(
        graph.volume_state(volume()).expect("volume"),
        Some(VolumeState::Online)
    );
}

#[test]
fn bounded_outbox_maintenance_respects_slowest_consumer_and_preserves_sequence() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    graph
        .acknowledge_projection("catalog-v1", MutationSeq(2), 1)
        .expect("catalog checkpoint");
    graph
        .acknowledge_projection("content-v1", MutationSeq(1), 1)
        .expect("content checkpoint");
    assert!(
        graph
            .consumed_outbox_maintenance_pending()
            .expect("maintenance pending")
    );

    let first = graph
        .prune_rebuildable_outbox_bounded(100, false)
        .expect("bounded prune");
    assert_eq!(first.safe_through_sequence, Some(1));
    assert_eq!(first.deleted_rows, 1);
    assert!(!first.backlog_remaining);
    assert!(
        !graph
            .consumed_outbox_maintenance_pending()
            .expect("maintenance drained")
    );
    assert_eq!(
        graph.latest_outbox_sequence().expect("latest"),
        MutationSeq(2)
    );
    assert_eq!(
        graph
            .read_outbox(Some(MutationSeq(1)), 10)
            .expect("remaining outbox")
            .first_sequence(),
        Some(MutationSeq(2))
    );
}

#[test]
fn explicit_rebuildable_prune_without_consumers_is_bounded_and_keeps_desired_state() {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    let latest = graph.latest_outbox_sequence().expect("latest");
    let retained = graph
        .prune_rebuildable_outbox_bounded(1, false)
        .expect("retained without consumer");
    assert_eq!(retained.deleted_rows, 0);

    let first = graph
        .prune_rebuildable_outbox_bounded(1, true)
        .expect("first explicit prune");
    assert_eq!(first.deleted_rows, 1);
    assert!(first.backlog_remaining);
    let second = graph
        .prune_rebuildable_outbox_bounded(1, true)
        .expect("second explicit prune");
    assert_eq!(second.deleted_rows, 1);
    assert!(!second.backlog_remaining);
    assert_eq!(
        graph.latest_outbox_sequence().expect("latest retained"),
        latest
    );
    assert_eq!(graph.desired_catalog_documents().expect("desired").len(), 2);
}

#[test]
fn new_durable_graphs_enable_incremental_page_reclaim() {
    let temp = tempfile::tempdir().expect("temp");
    let mut graph = FilesystemGraph::open(temp.path().join("graph.sqlite3")).expect("graph");
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), initial_events())
        .expect("snapshot");
    graph
        .prune_rebuildable_outbox_bounded(100, true)
        .expect("prune");
    let storage = graph.storage_stats().expect("storage stats");
    assert!(storage.incremental_vacuum);
    assert_eq!(
        storage.allocated_bytes,
        storage
            .page_size_bytes
            .saturating_mul(storage.allocated_pages)
    );
    assert_eq!(
        storage.reusable_bytes,
        storage
            .page_size_bytes
            .saturating_mul(storage.reusable_pages)
    );
    let reclaimed = graph.reclaim_reusable_pages(4_096).expect("reclaim");
    let after = graph.storage_stats().expect("storage after reclaim");
    assert!(reclaimed <= storage.reusable_pages);
    assert!(after.allocated_pages <= storage.allocated_pages);
    assert!(after.reusable_pages <= storage.reusable_pages);
}
