use std::{fs, time::Duration};

use localsearch_catalog_index::{
    CATALOG_SCHEMA_ID, CatalogIndex, CatalogQueryMode, ProjectionWorker, ProjectionWorkerOptions,
    RecoveryKind,
};
use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, MutationSeq, VolumeId,
};
use localsearch_filesystem_graph::{FilesystemGraph, GraphMutation, GraphMutationBatch};
use localsearch_platform_core::{ProviderCheckpoint, VolumeDescriptor};

fn volume() -> VolumeId {
    VolumeId::from_u128(600)
}

fn key(value: u128) -> FileKey {
    FileKey::new(volume(), FileId128::from_u128(value))
}

fn object(value: u128, size: u64) -> FileObjectSnapshot {
    FileObjectSnapshot {
        object_key: key(value),
        metadata: FileMetadata {
            kind: if value == 1 {
                FileKind::Directory
            } else {
                FileKind::File
            },
            size,
            created_at_unix_ms: None,
            modified_at_unix_ms: Some(1),
            hidden: false,
            availability: Availability::Online,
        },
    }
}

fn link(value: u128, object: FileKey, name: &str) -> FileLinkSnapshot {
    FileLinkSnapshot {
        file_link_id: FileLinkId::from_u128(value),
        object_key: object,
        parent_key: (value != 101).then(|| key(1)),
        name: name.to_owned(),
    }
}

fn checkpoint(value: u64) -> ProviderCheckpoint {
    ProviderCheckpoint {
        provider_id: "backend-test".to_owned(),
        format_version: 1,
        volume_id: volume(),
        opaque: value.to_be_bytes().to_vec(),
    }
}

fn descriptor() -> VolumeDescriptor {
    VolumeDescriptor {
        volume_id: volume(),
        display_name: Some("backend".to_owned()),
        mount_points: vec!["root".to_owned()],
        filesystem: Some("testfs".to_owned()),
        removable: false,
        local: true,
    }
}

fn graph_with_files(count: u128) -> FilesystemGraph {
    let mut graph = FilesystemGraph::open_in_memory().expect("graph");
    let mut events = vec![
        FilesystemEvent::ObjectObserved {
            object: object(1, 0),
        },
        FilesystemEvent::LinkObserved {
            link: link(101, key(1), "root"),
        },
    ];
    for value in 2..count + 2 {
        events.push(FilesystemEvent::ObjectObserved {
            object: object(value, u64::try_from(value).expect("small identity")),
        });
        events.push(FilesystemEvent::LinkObserved {
            link: link(value + 100, key(value), &format!("file-{value}.txt")),
        });
    }
    graph
        .ingest_snapshot(descriptor(), checkpoint(1), events)
        .expect("snapshot");
    graph
}

fn options() -> ProjectionWorkerOptions {
    ProjectionWorkerOptions {
        maximum_batch_mutations: 2,
        maximum_batches: 100,
        maximum_run_time: Duration::from_secs(10),
        writer_heap_bytes: 20_000_000,
        rebuild_page_size: 2,
    }
}

fn drain_two_document_volume_refresh(graph: &mut FilesystemGraph) {
    let first = graph
        .refresh_volume_projection(1)
        .expect("first reconciliation page");
    let second = graph
        .refresh_volume_projection(1)
        .expect("second reconciliation page");
    let terminal = graph
        .refresh_volume_projection(1)
        .expect("terminal reconciliation page");
    assert_eq!(first.links_scanned, 1);
    assert_eq!(second.links_scanned, 1);
    assert_eq!(terminal.links_scanned, 0);
    assert!(terminal.job_completed);
}

#[test]
fn clean_rebuild_incremental_rename_and_delete_converge() {
    let root = tempfile::tempdir().expect("index root");
    let mut graph = graph_with_files(2);
    let worker = ProjectionWorker::new(root.path(), options());
    let initial = worker.run(&graph).expect("initial rebuild");
    assert_eq!(initial.recovery, RecoveryKind::RebuiltGeneration);
    assert_eq!(initial.index_generation, 1);
    assert!(!initial.backlog_remaining);
    let index = worker.active_index(&graph).expect("active index");
    assert_eq!(
        index
            .reader()
            .expect("reader")
            .document_count()
            .expect("count"),
        3
    );

    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(2),
            mutations: vec![GraphMutation::UpsertLink {
                link: link(102, key(2), "renamed.txt"),
                traversal_boundary: false,
            }],
        })
        .expect("rename");
    let incremental = worker.run(&graph).expect("incremental projection");
    assert_eq!(incremental.recovery, RecoveryKind::ExistingGeneration);
    assert_eq!(incremental.applied_mutations, 1);
    let renamed = worker
        .active_index(&graph)
        .expect("active")
        .reader()
        .expect("reader")
        .document(localsearch_core::DocumentId::from_u128(102))
        .expect("lookup")
        .expect("document");
    assert_eq!(renamed.name, "renamed.txt");
    let reader = worker
        .active_index(&graph)
        .expect("active")
        .reader()
        .expect("reader");
    assert_eq!(
        reader
            .search_candidates("renamed.txt", CatalogQueryMode::Exact, 10)
            .expect("exact search"),
        vec![localsearch_core::DocumentId::from_u128(102)]
    );
    assert_eq!(
        reader
            .search_candidates("named", CatalogQueryMode::Substring, 10)
            .expect("substring candidates"),
        vec![localsearch_core::DocumentId::from_u128(102)]
    );
    assert!(
        reader
            .search_candidates("re", CatalogQueryMode::Substring, 10)
            .is_err()
    );

    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(3),
            mutations: vec![GraphMutation::RemoveLink {
                file_link_id: FileLinkId::from_u128(103),
                object_key: key(3),
            }],
        })
        .expect("delete");
    worker.run(&graph).expect("delete projection");
    assert_eq!(
        worker
            .active_index(&graph)
            .expect("active")
            .reader()
            .expect("reader")
            .document_count()
            .expect("count"),
        2
    );
}

#[test]
fn crash_after_tantivy_commit_before_ack_replays_without_duplicates() {
    let root = tempfile::tempdir().expect("index root");
    let mut graph = graph_with_files(1);
    let worker = ProjectionWorker::new(root.path(), options());
    worker.run(&graph).expect("initial rebuild");
    let acknowledged = graph
        .projector_checkpoint(CATALOG_SCHEMA_ID)
        .expect("checkpoint")
        .expect("present")
        .last_sequence;

    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(2),
            mutations: vec![GraphMutation::UpsertObject {
                object: object(2, 999),
            }],
        })
        .expect("SQLite commit");
    let pending = graph
        .read_outbox(Some(MutationSeq(acknowledged)), 10)
        .expect("pending batch");
    let active = worker.active_index(&graph).expect("active index");
    let mut writer = active.writer(20_000_000).expect("writer");
    for mutation in &pending.mutations {
        writer.apply(&mutation.mutation).expect("manual apply");
    }
    writer.commit().expect("Tantivy commit");
    writer.wait_merging_threads().expect("merges");
    assert_eq!(
        graph
            .projector_checkpoint(CATALOG_SCHEMA_ID)
            .expect("checkpoint")
            .expect("present")
            .last_sequence,
        acknowledged,
        "simulated crash occurs before ACK"
    );

    let replay = worker.run(&graph).expect("restart replay");
    assert_eq!(replay.applied_mutations, 1);
    let reader = worker
        .active_index(&graph)
        .expect("active")
        .reader()
        .expect("reader");
    assert_eq!(reader.document_count().expect("count"), 2);
    let document = reader
        .document(localsearch_core::DocumentId::from_u128(102))
        .expect("lookup")
        .expect("document");
    assert_eq!(document.metadata.size, 999);
}

#[test]
fn deleted_active_index_and_interrupted_rebuild_create_new_generation() {
    let root = tempfile::tempdir().expect("index root");
    let graph = graph_with_files(3);
    let worker = ProjectionWorker::new(root.path(), options());
    worker.run(&graph).expect("initial rebuild");
    let active_one = root.path().join("generation-00000000000000000001");
    fs::remove_dir_all(&active_one).expect("simulate lost index");
    let interrupted = root.path().join("generation-00000000000000000002");
    fs::create_dir(&interrupted).expect("simulate interrupted rebuild");
    fs::write(interrupted.join("partial"), b"not an index").expect("partial generation");

    let recovery = worker.run(&graph).expect("rebuild after restart");
    assert_eq!(recovery.recovery, RecoveryKind::RebuiltGeneration);
    assert_eq!(recovery.index_generation, 3);
    assert_eq!(
        worker
            .active_index(&graph)
            .expect("active")
            .reader()
            .expect("reader")
            .document_count()
            .expect("count"),
        4
    );
}

#[test]
fn bounded_worker_reports_backlog_and_catches_up() {
    let root = tempfile::tempdir().expect("index root");
    let mut graph = graph_with_files(3);
    let worker = ProjectionWorker::new(root.path(), options());
    worker.run(&graph).expect("initial rebuild");
    for value in 2..5 {
        graph
            .apply_batch(&GraphMutationBatch {
                volume_id: volume(),
                checkpoint: checkpoint(u64::try_from(value).expect("small")),
                mutations: vec![GraphMutation::UpsertObject {
                    object: object(value, 1_000 + u64::try_from(value).expect("small")),
                }],
            })
            .expect("metadata update");
    }
    let bounded = ProjectionWorker::new(
        root.path(),
        ProjectionWorkerOptions {
            maximum_batch_mutations: 1,
            maximum_batches: 1,
            ..options()
        },
    );
    let first = bounded.run(&graph).expect("bounded run");
    assert_eq!(first.applied_mutations, 1);
    assert!(first.backlog_remaining);
    let final_run = worker.run(&graph).expect("catch up");
    assert!(!final_run.backlog_remaining);
    assert_eq!(
        worker
            .active_index(&graph)
            .expect("active")
            .reader()
            .expect("reader")
            .document_count()
            .expect("count"),
        4
    );
}

#[test]
fn poison_identity_is_rejected_before_commit() {
    let directory = tempfile::tempdir().expect("root");
    let path = directory.path().join("index");
    let index = CatalogIndex::create(&path).expect("index");
    let graph = graph_with_files(1);
    let mut document = graph
        .desired_catalog_documents()
        .expect("documents")
        .into_iter()
        .find(|document| document.name != "root")
        .expect("file document");
    document.identity.document_id = localsearch_core::DocumentId::from_u128(999);
    let mut writer = index.writer(20_000_000).expect("writer");
    assert!(writer.add_current(&document).is_err());
}

#[test]
fn hard_links_directory_refresh_and_reconciliation_preserve_convergence() {
    let root = tempfile::tempdir().expect("index root");
    let mut graph = graph_with_files(1);
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(2),
            mutations: vec![GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(202),
                    object_key: key(2),
                    parent_key: Some(key(1)),
                    name: "hard-link.txt".to_owned(),
                },
                traversal_boundary: false,
            }],
        })
        .expect("hard link");
    let worker = ProjectionWorker::new(root.path(), options());
    worker.run(&graph).expect("initial projection");
    assert_eq!(
        worker
            .active_index(&graph)
            .expect("active")
            .reader()
            .expect("reader")
            .document_count()
            .expect("count"),
        3
    );
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(3),
            mutations: vec![GraphMutation::RemoveLink {
                file_link_id: FileLinkId::from_u128(102),
                object_key: key(2),
            }],
        })
        .expect("remove one hard link");
    worker.run(&graph).expect("hard-link projection");
    let reader = worker
        .active_index(&graph)
        .expect("active")
        .reader()
        .expect("reader");
    assert_eq!(reader.document_count().expect("count"), 2);
    assert!(
        reader
            .document(localsearch_core::DocumentId::from_u128(202))
            .expect("lookup")
            .is_some()
    );

    let before = graph.latest_outbox_sequence().expect("sequence");
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(4),
            mutations: vec![GraphMutation::RequireReconciliation {
                volume_id: volume(),
                reason: localsearch_core::ReconciliationReason::ProviderRequested,
            }],
        })
        .expect("reconciliation marker");
    assert_eq!(graph.latest_outbox_sequence().expect("sequence"), before);
    assert!(
        graph
            .projection_refresh_maintenance_pending()
            .expect("pending reconciliation refresh")
    );
    drain_two_document_volume_refresh(&mut graph);
    assert_eq!(
        graph.latest_outbox_sequence().expect("sequence"),
        MutationSeq(before.0 + 2)
    );
    assert_eq!(
        worker
            .run(&graph)
            .expect("availability projection")
            .applied_mutations,
        2
    );
    let reconciled = worker
        .active_index(&graph)
        .expect("active")
        .reader()
        .expect("reader")
        .document(localsearch_core::DocumentId::from_u128(202))
        .expect("lookup")
        .expect("hard link remains searchable");
    assert_eq!(
        reconciled.metadata.availability,
        localsearch_core::Availability::Unknown
    );
}

#[test]
fn crash_before_tantivy_commit_leaves_outbox_for_restart() {
    let root = tempfile::tempdir().expect("index root");
    let mut graph = graph_with_files(1);
    let worker = ProjectionWorker::new(root.path(), options());
    worker.run(&graph).expect("initial projection");
    let acknowledged = graph
        .projector_checkpoint(CATALOG_SCHEMA_ID)
        .expect("checkpoint")
        .expect("present")
        .last_sequence;
    graph
        .apply_batch(&GraphMutationBatch {
            volume_id: volume(),
            checkpoint: checkpoint(2),
            mutations: vec![GraphMutation::UpsertObject {
                object: object(2, 777),
            }],
        })
        .expect("SQLite commit");
    let pending = graph
        .read_outbox(Some(MutationSeq(acknowledged)), 10)
        .expect("pending");
    {
        let active = worker.active_index(&graph).expect("active");
        let mut writer = active.writer(20_000_000).expect("writer");
        writer
            .apply(&pending.mutations[0].mutation)
            .expect("apply before crash");
        // Dropping without commit simulates termination before the Tantivy generation is durable.
    }
    assert_eq!(
        graph
            .projector_checkpoint(CATALOG_SCHEMA_ID)
            .expect("checkpoint")
            .expect("present")
            .last_sequence,
        acknowledged
    );
    worker.run(&graph).expect("restart");
    let document = worker
        .active_index(&graph)
        .expect("active")
        .reader()
        .expect("reader")
        .document(localsearch_core::DocumentId::from_u128(102))
        .expect("lookup")
        .expect("document");
    assert_eq!(document.metadata.size, 777);
}
