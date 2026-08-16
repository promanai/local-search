use std::{fs, path::Path};

use localsearch_content_index::{
    ContentIndex, ContentIndexError, ContentIndexPolicy, DEFAULT_MAX_FILE_BYTES,
};
use localsearch_core::{
    Availability, CatalogDocument, CatalogIdentity, DocumentId, DocumentVersion, FileId128,
    FileKey, FileKind, FileLinkId, FileMetadata, VolumeId,
};
use tempfile::TempDir;

fn document(path: &Path, ordinal: u128) -> CatalogDocument {
    let metadata = fs::metadata(path).expect("fixture metadata");
    CatalogDocument {
        identity: CatalogIdentity::new(
            FileKey::new(VolumeId::from_u128(1), FileId128::from_u128(ordinal)),
            FileLinkId::from_u128(ordinal),
            DocumentId::from_u128(ordinal),
        ),
        document_version: DocumentVersion(1),
        name: path
            .file_name()
            .expect("fixture filename")
            .to_string_lossy()
            .into_owned(),
        resolved_path: path.to_string_lossy().into_owned(),
        extension: path
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned()),
        metadata: FileMetadata {
            kind: FileKind::File,
            size: metadata.len(),
            created_at_unix_ms: None,
            modified_at_unix_ms: None,
            hidden: false,
            availability: Availability::Online,
        },
    }
}

#[test]
fn explicit_root_build_indexes_utf8_and_returns_only_catalog_metadata() {
    let workspace = TempDir::new().expect("workspace");
    let root = workspace.path().join("allowed");
    fs::create_dir(&root).expect("allowed root");
    let source = root.join("meeting-notes.md");
    fs::write(
        &source,
        "The launch codeword is ultramarine-constellation and is not in the filename.",
    )
    .expect("write source");
    let destination = workspace.path().join("content-index");
    let policy = ContentIndexPolicy::new([root], DEFAULT_MAX_FILE_BYTES).expect("policy");

    let summary = ContentIndex::build(&destination, [document(&source, 7)], &policy)
        .expect("build content index");
    assert_eq!(summary.catalog_documents, 1);
    assert_eq!(summary.indexed_documents, 1);

    let reader = ContentIndex::open(&destination).expect("open content index");
    assert_eq!(reader.document_count().expect("count"), 1);
    let hits = reader
        .search("ultramarine", 10)
        .expect("search indexed text");
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].document.identity.document_id,
        DocumentId::from_u128(7)
    );
    assert_eq!(hits[0].document.name, "meeting-notes.md");
    let encoded = serde_json::to_string(&hits).expect("serialize hits");
    assert!(!encoded.contains("constellation"));
    assert_eq!(
        reader
            .search("ultram", 10)
            .expect("incremental prefix search")
            .len(),
        1
    );
    assert!(
        reader
            .search("ult", 10)
            .expect("bounded short prefix")
            .is_empty()
    );
}

#[test]
fn policy_skips_outside_binary_unsupported_and_oversized_sources() {
    let workspace = TempDir::new().expect("workspace");
    let root = workspace.path().join("allowed");
    fs::create_dir(&root).expect("allowed root");
    let outside = workspace.path().join("outside.md");
    let binary = root.join("binary.txt");
    let unsupported = root.join("image.png");
    let oversized = root.join("oversized.md");
    fs::write(&outside, "out").expect("outside source");
    fs::write(&binary, b"a\0b").expect("binary source");
    fs::write(&unsupported, "image").expect("unsupported source");
    fs::write(&oversized, "this text exceeds the limit").expect("oversized source");
    let policy = ContentIndexPolicy::new([root], 16).expect("policy");

    let summary = ContentIndex::build(
        &workspace.path().join("content-index"),
        [
            document(&outside, 1),
            document(&binary, 2),
            document(&unsupported, 3),
            document(&oversized, 4),
        ],
        &policy,
    )
    .expect("build bounded index");

    assert_eq!(summary.indexed_documents, 0);
    assert_eq!(summary.skipped_outside_roots, 1);
    assert_eq!(summary.skipped_non_text, 1);
    assert_eq!(summary.skipped_extension, 1);
    assert_eq!(summary.skipped_too_large, 1);
}

#[test]
fn build_never_overwrites_an_existing_generation() {
    let workspace = TempDir::new().expect("workspace");
    let root = workspace.path().join("allowed");
    let destination = workspace.path().join("content-index");
    fs::create_dir(&root).expect("allowed root");
    fs::create_dir(&destination).expect("existing destination");
    let policy = ContentIndexPolicy::new([root], DEFAULT_MAX_FILE_BYTES).expect("policy");

    let error = ContentIndex::build(&destination, [], &policy).expect_err("must refuse overwrite");
    assert!(
        matches!(error, ContentIndexError::Io(ref io) if io.kind() == std::io::ErrorKind::AlreadyExists)
    );
}

#[test]
fn invalid_policy_and_query_bounds_are_rejected() {
    let workspace = TempDir::new().expect("workspace");
    assert!(matches!(
        ContentIndexPolicy::new(Vec::<std::path::PathBuf>::new(), DEFAULT_MAX_FILE_BYTES),
        Err(ContentIndexError::InvalidPolicy)
    ));

    let root = workspace.path().join("allowed");
    fs::create_dir(&root).expect("allowed root");
    let nested = root.join("nested");
    fs::create_dir(&nested).expect("nested root");
    assert!(matches!(
        ContentIndexPolicy::new([root.clone(), nested], DEFAULT_MAX_FILE_BYTES),
        Err(ContentIndexError::InvalidPolicy)
    ));
    let policy = ContentIndexPolicy::new([root], DEFAULT_MAX_FILE_BYTES).expect("policy");
    let destination = workspace.path().join("content-index");
    ContentIndex::build(&destination, [], &policy).expect("empty generation");
    let reader = ContentIndex::open(&destination).expect("reader");
    assert!(matches!(
        reader.search("", 10),
        Err(ContentIndexError::Query)
    ));
    assert!(matches!(
        reader.search("valid", 0),
        Err(ContentIndexError::Query)
    ));
}

#[test]
fn delta_updates_adds_and_deletes_only_named_identities() {
    let workspace = TempDir::new().expect("workspace");
    let root = workspace.path().join("allowed");
    fs::create_dir(&root).expect("allowed root");
    let changed = root.join("changed.md");
    let removed = root.join("removed.txt");
    let added = root.join("added.ts");
    fs::write(&changed, "old heliotrope delta").expect("changed source");
    fs::write(&removed, "old vermilion delta").expect("removed source");
    let destination = workspace.path().join("content-index");
    let policy = ContentIndexPolicy::new([root], DEFAULT_MAX_FILE_BYTES).expect("policy");
    ContentIndex::build(
        &destination,
        [document(&changed, 1), document(&removed, 2)],
        &policy,
    )
    .expect("initial generation");
    let reader = ContentIndex::open(&destination).expect("reader");

    fs::write(&changed, "new cerulean delta").expect("replace source");
    fs::write(&added, "const chartreuse = 'delta';").expect("typescript source");
    let mut changed_document = document(&changed, 1);
    changed_document.document_version = DocumentVersion(2);
    let summary = ContentIndex::apply_delta(
        &destination,
        [changed_document, document(&added, 3)],
        [DocumentId::from_u128(2)],
        &policy,
    )
    .expect("bounded delta");

    assert_eq!(summary.catalog_documents, 2);
    assert_eq!(summary.updated_documents, 1);
    assert_eq!(summary.added_documents, 1);
    assert_eq!(summary.removed_documents, 1);
    assert!(
        reader
            .search("heliotrope", 10)
            .expect("old term")
            .is_empty()
    );
    assert!(
        reader
            .search("vermilion", 10)
            .expect("removed term")
            .is_empty()
    );
    assert_eq!(
        reader.search("cerulean", 10).expect("updated term").len(),
        1
    );
    assert_eq!(reader.search("chartreuse", 10).expect("ts term").len(), 1);
}

#[test]
fn delta_skips_metadata_only_and_unchanged_hash_rebuilds() {
    let workspace = TempDir::new().expect("workspace");
    let root = workspace.path().join("allowed");
    fs::create_dir(&root).expect("allowed root");
    let metadata_only = root.join("metadata.md");
    let same_hash = root.join("same-hash.md");
    fs::write(&metadata_only, "persistent metadata-only content").expect("metadata fixture");
    fs::write(&same_hash, "persistent hash content").expect("hash fixture");
    let mut metadata_document = document(&metadata_only, 1);
    metadata_document.metadata.modified_at_unix_ms = Some(100);
    let destination = workspace.path().join("content-index");
    let policy = ContentIndexPolicy::new([root], DEFAULT_MAX_FILE_BYTES).expect("policy");
    ContentIndex::build(
        &destination,
        [metadata_document.clone(), document(&same_hash, 2)],
        &policy,
    )
    .expect("initial generation");

    metadata_document.document_version = DocumentVersion(2);
    metadata_document.name = "renamed-metadata.md".to_owned();
    let mut hash_document = document(&same_hash, 2);
    hash_document.document_version = DocumentVersion(2);
    let summary = ContentIndex::apply_delta(
        &destination,
        [metadata_document, hash_document],
        [],
        &policy,
    )
    .expect("content-aware delta");

    assert_eq!(summary.metadata_only_documents, 1);
    assert_eq!(summary.unchanged_hash_documents, 1);
    assert_eq!(summary.updated_documents, 0);
    assert_eq!(
        ContentIndex::open(&destination)
            .expect("reader")
            .document_count()
            .expect("count"),
        2
    );
}

#[test]
fn sync_adds_updates_removes_and_refreshes_an_open_reader() {
    let workspace = TempDir::new().expect("workspace");
    let root = workspace.path().join("allowed");
    fs::create_dir(&root).expect("allowed root");
    let changed = root.join("changed.md");
    let removed = root.join("removed.txt");
    let added = root.join("added.rs");
    fs::write(&changed, "old heliotrope content").expect("changed source");
    fs::write(&removed, "retired vermilion content").expect("removed source");
    let destination = workspace.path().join("content-index");
    let policy = ContentIndexPolicy::new([root], DEFAULT_MAX_FILE_BYTES).expect("policy");
    ContentIndex::build(
        &destination,
        [document(&changed, 1), document(&removed, 2)],
        &policy,
    )
    .expect("initial generation");
    let reader = ContentIndex::open(&destination).expect("open before sync");

    fs::write(&changed, "new cerulean content").expect("replace source");
    fs::write(&added, "added chartreuse content").expect("added source");
    let mut changed_document = document(&changed, 1);
    changed_document.document_version = DocumentVersion(2);
    let summary = ContentIndex::sync(
        &destination,
        [changed_document.clone(), document(&added, 3)],
        &policy,
    )
    .expect("incremental sync");

    assert_eq!(summary.catalog_documents, 2);
    assert_eq!(summary.updated_documents, 1);
    assert_eq!(summary.added_documents, 1);
    assert_eq!(summary.removed_documents, 1);
    assert_eq!(reader.document_count().expect("refreshed count"), 2);
    assert!(
        reader
            .search("heliotrope", 10)
            .expect("old term")
            .is_empty()
    );
    assert!(
        reader
            .search("vermilion", 10)
            .expect("removed term")
            .is_empty()
    );
    assert_eq!(reader.search("cerulean", 10).expect("new term").len(), 1);
    assert_eq!(
        reader.search("chartreuse", 10).expect("added term").len(),
        1
    );

    let unchanged = ContentIndex::sync(
        &destination,
        [changed_document, document(&added, 3)],
        &policy,
    )
    .expect("no-op sync");
    assert_eq!(unchanged.unchanged_documents, 2);
    assert_eq!(unchanged.added_documents, 0);
    assert_eq!(unchanged.updated_documents, 0);
}

#[test]
fn sync_evicts_ineligible_content_and_failed_batch_is_not_published() {
    let workspace = TempDir::new().expect("workspace");
    let root = workspace.path().join("allowed");
    fs::create_dir(&root).expect("allowed root");
    let source = root.join("notes.md");
    fs::write(&source, "original saffron content").expect("source");
    let destination = workspace.path().join("content-index");
    let policy = ContentIndexPolicy::new([root], DEFAULT_MAX_FILE_BYTES).expect("policy");
    ContentIndex::build(&destination, [document(&source, 1)], &policy).expect("build");
    let reader = ContentIndex::open(&destination).expect("reader");

    fs::write(&source, "replacement indigo content").expect("replace source");
    let mut updated = document(&source, 1);
    updated.document_version = DocumentVersion(2);
    let error = ContentIndex::sync(&destination, [updated.clone(), updated], &policy)
        .expect_err("duplicate identity must abort the batch");
    assert!(matches!(error, ContentIndexError::DuplicateDocument));
    assert_eq!(reader.search("saffron", 10).expect("old commit").len(), 1);
    assert!(
        reader
            .search("indigo", 10)
            .expect("uncommitted term")
            .is_empty()
    );

    fs::remove_file(&source).expect("remove source");
    let evicted = ContentIndex::sync(&destination, [document_without_io(&source, 1)], &policy)
        .expect("evict missing source");
    assert_eq!(evicted.evicted_documents, 1);
    assert_eq!(evicted.skipped_io, 1);
    assert_eq!(reader.document_count().expect("empty after eviction"), 0);
}

fn document_without_io(path: &Path, ordinal: u128) -> CatalogDocument {
    CatalogDocument {
        identity: CatalogIdentity::new(
            FileKey::new(VolumeId::from_u128(1), FileId128::from_u128(ordinal)),
            FileLinkId::from_u128(ordinal),
            DocumentId::from_u128(ordinal),
        ),
        document_version: DocumentVersion(3),
        name: path
            .file_name()
            .expect("filename")
            .to_string_lossy()
            .into_owned(),
        resolved_path: path.to_string_lossy().into_owned(),
        extension: path
            .extension()
            .map(|value| value.to_string_lossy().into_owned()),
        metadata: FileMetadata {
            kind: FileKind::File,
            size: 10,
            created_at_unix_ms: None,
            modified_at_unix_ms: None,
            hidden: false,
            availability: Availability::Online,
        },
    }
}

#[cfg(windows)]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the real-process contract keeps scan, reconciliation, durable outbox, and content projection in one end-to-end fixture"
)]
fn folder_sync_indexes_a_real_tree_and_reconciles_changes() {
    use std::process::Command;

    let temp = TempDir::new().expect("workspace");
    let selected = temp.path().join("selected");
    let state = temp.path().join("state");
    fs::create_dir(&selected).expect("selected root");
    let original = selected.join("original.md");
    let removed = selected.join("removed.txt");
    let ignored = selected.join("ignored.png");
    let excluded = selected.join("node_modules");
    fs::create_dir(&excluded).expect("excluded directory");
    fs::write(&original, "heliotrope folder onboarding").expect("original");
    fs::write(&removed, "quasar removal contract").expect("removed");
    fs::write(&ignored, b"\x89PNG\0binary").expect("ignored");
    fs::write(excluded.join("private.md"), "excludedprivacyprobe").expect("excluded source");

    let first = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "folder-sync",
            "--workspace",
            state.to_str().expect("state path"),
            "--root",
            selected.to_str().expect("selected path"),
            "--scan-batch-size",
            "2",
        ])
        .output()
        .expect("first folder sync");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_summary: serde_json::Value = serde_json::from_slice(&first.stdout).expect("summary");
    assert_eq!(first_summary["content_mode"], "generation-resume");
    assert_eq!(first_summary["content_complete"], true);
    assert_eq!(first_summary["scan_complete"], true);
    assert_eq!(first_summary["graph_limit_reached"], false);
    assert_eq!(first_summary["graph_outbox_mode"], "rebuildable-initial");
    assert_eq!(first_summary["graph_stats"]["live_files"], 3);
    assert_eq!(first_summary["graph_compaction"]["deleted_rows"], 0);
    assert_eq!(
        first_summary["graph_compaction"]["storage_after"]["incremental_vacuum"],
        true
    );
    let compacted_graph =
        localsearch_filesystem_graph::FilesystemGraph::open_read_only(state.join("graph.sqlite3"))
            .expect("compacted graph");
    assert!(
        compacted_graph
            .read_outbox(None, 10)
            .expect("compacted initial outbox")
            .mutations
            .is_empty()
    );
    drop(compacted_graph);
    let reader = ContentIndex::open(&state.join("content-index-v1")).expect("content reader");
    assert_eq!(reader.search("heliotrope", 10).expect("first hit").len(), 1);
    assert!(
        reader
            .search("binary", 10)
            .expect("ignored content")
            .is_empty()
    );
    assert!(
        reader
            .search("excludedprivacyprobe", 10)
            .expect("excluded content")
            .is_empty()
    );

    fs::write(&original, "indigo updated onboarding").expect("update original");
    fs::remove_file(&removed).expect("remove file");
    fs::write(selected.join("added.json"), "{\"term\":\"chartreuse\"}").expect("add file");
    let second = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "folder-sync",
            "--workspace",
            state.to_str().expect("state path"),
            "--root",
            selected.to_str().expect("selected path"),
            "--scan-batch-size",
            "2",
        ])
        .output()
        .expect("second folder sync");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_summary: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("second summary");
    assert_eq!(second_summary["content_mode"], "project");
    assert_eq!(second_summary["graph_outbox_mode"], "durable");
    assert!(
        second_summary["graph_compaction"]["deleted_rows"]
            .as_u64()
            .is_some_and(|rows| rows > 0)
    );
    assert!(
        reader
            .search("heliotrope", 10)
            .expect("old term")
            .is_empty()
    );
    assert!(
        reader
            .search("quasar", 10)
            .expect("removed term")
            .is_empty()
    );
    assert_eq!(reader.search("indigo", 10).expect("updated term").len(), 1);
    assert_eq!(
        reader.search("chartreuse", 10).expect("added term").len(),
        1
    );
    let graph =
        localsearch_filesystem_graph::FilesystemGraph::open_read_only(state.join("graph.sqlite3"))
            .expect("graph");
    let documents = graph.desired_catalog_documents().expect("documents");
    let names = documents
        .iter()
        .map(|document| document.name.clone())
        .collect::<Vec<_>>();
    assert!(names.iter().any(|name| name == "added.json"));
    assert!(!names.iter().any(|name| name == "removed.txt"));

    let original_document = documents
        .iter()
        .find(|document| document.name == "original.md")
        .expect("original document")
        .clone();
    drop(graph);

    let reset_graph =
        localsearch_filesystem_graph::FilesystemGraph::open(state.join("graph.sqlite3"))
            .expect("reset graph");
    assert!(
        reset_graph
            .reset_projection_consumer(localsearch_content_index::CONTENT_SCHEMA_ID)
            .expect("reset content consumer")
    );
    drop(reset_graph);
    fs::write(&original, "amber reset recovery update").expect("reset recovery source");
    let recovery = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "folder-sync",
            "--workspace",
            state.to_str().expect("state path"),
            "--root",
            selected.to_str().expect("selected path"),
            "--scan-batch-size",
            "2",
        ])
        .output()
        .expect("reset recovery folder sync");
    assert!(
        recovery.status.success(),
        "{}",
        String::from_utf8_lossy(&recovery.stderr)
    );
    let recovery_summary: serde_json::Value =
        serde_json::from_slice(&recovery.stdout).expect("recovery summary");
    assert_eq!(recovery_summary["graph_outbox_mode"], "durable");
    assert_eq!(reader.search("amber", 10).expect("recovery term").len(), 1);

    fs::write(&original, "violet projected content update").expect("projected update");
    let mut metadata = original_document.metadata.clone();
    metadata.size = fs::metadata(&original).expect("updated metadata").len();
    let volume_id = original_document.identity.object_key.volume_id;
    let mut graph =
        localsearch_filesystem_graph::FilesystemGraph::open(state.join("graph.sqlite3"))
            .expect("writable graph");
    graph
        .apply_batch(&localsearch_filesystem_graph::GraphMutationBatch {
            volume_id,
            checkpoint: localsearch_platform_core::ProviderCheckpoint {
                provider_id: "content-project-test".to_owned(),
                format_version: 1,
                volume_id,
                opaque: vec![1],
            },
            mutations: vec![localsearch_filesystem_graph::GraphMutation::UpsertObject {
                object: localsearch_core::FileObjectSnapshot {
                    object_key: original_document.identity.object_key,
                    metadata,
                },
            }],
        })
        .expect("graph content mutation");
    drop(graph);

    let projected = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "watch",
            "--workspace",
            state.to_str().expect("workspace path"),
            "--watch-iterations",
            "1",
            "--watch-interval-ms",
            "100",
        ])
        .output()
        .expect("content projection");
    assert!(
        projected.status.success(),
        "{}",
        String::from_utf8_lossy(&projected.stderr)
    );
    let projected_summary: serde_json::Value =
        serde_json::from_slice(&projected.stdout).expect("projection summary");
    assert_eq!(projected_summary["projected_mutations"], 1);
    assert!(
        reader
            .search("indigo", 10)
            .expect("pre-projection term removed")
            .is_empty()
    );
    assert_eq!(
        reader.search("violet", 10).expect("projected term").len(),
        1
    );
}

#[cfg(windows)]
#[test]
fn folder_sync_stops_before_scanning_when_graph_budget_is_already_exhausted() {
    use std::process::Command;

    let temp = TempDir::new().expect("workspace");
    let selected = temp.path().join("selected");
    let state = temp.path().join("state");
    fs::create_dir(&selected).expect("selected root");
    fs::write(selected.join("must-not-be-scanned.txt"), "bounded graph").expect("source");

    let output = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "folder-sync",
            "--workspace",
            state.to_str().expect("state path"),
            "--root",
            selected.to_str().expect("selected path"),
            "--max-graph-bytes",
            "1",
        ])
        .output()
        .expect("bounded folder sync");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&output.stdout).expect("summary");
    assert_eq!(summary["scan_complete"], false);
    assert_eq!(summary["graph_limit_reached"], true);
    assert_eq!(summary["observed_events"], 0);
    assert_eq!(summary["graph_stats"]["live_files"], 0);
    assert_eq!(summary["content_complete"], true);
    assert_eq!(
        summary["content_summary"]["generation"]["documents_projected"],
        0
    );
}

#[cfg(windows)]
#[test]
fn initial_generation_resumes_after_commit_checkpoint_crashes_at_10_50_and_90_percent() {
    use std::process::Command;

    for (label, crash_after_commits) in [("10", "1"), ("50", "5"), ("90", "9")] {
        let temp = TempDir::new().expect("workspace");
        let selected = temp.path().join(format!("selected-{label}"));
        let state = temp.path().join(format!("state-{label}"));
        fs::create_dir(&selected).expect("selected root");
        for ordinal in 0..100_u32 {
            fs::write(
                selected.join(format!("document-{ordinal:03}.md")),
                format!("restartable generation restarttoken{label}x{ordinal:03}"),
            )
            .expect("content fixture");
        }

        let crashed = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
            .args([
                "folder-sync",
                "--workspace",
                state.to_str().expect("state path"),
                "--root",
                selected.to_str().expect("selected path"),
                "--content-batch-documents",
                "10",
                "--content-maximum-batches",
                "20",
                "--min-free-disk-percent",
                "0",
                "--min-free-disk-bytes",
                "1",
            ])
            .env(
                "LOCALSEARCH_TEST_CRASH_AFTER_CONTENT_COMMITS",
                crash_after_commits,
            )
            .output()
            .expect("crashing generation process");
        assert!(!crashed.status.success(), "{label}% failpoint did not fire");
        assert!(
            String::from_utf8_lossy(&crashed.stderr).contains("simulated crash"),
            "{}",
            String::from_utf8_lossy(&crashed.stderr)
        );
        assert!(!state.join("content-index-v1").join("active.json").is_file());
        let generations = state.join("content-index-v1").join("generations");
        let generation_id = fs::read_dir(&generations)
            .expect("generation directory")
            .next()
            .expect("building generation")
            .expect("generation entry")
            .file_name();

        let resumed = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
            .args([
                "folder-sync",
                "--workspace",
                state.to_str().expect("state path"),
                "--root",
                selected.to_str().expect("selected path"),
                "--content-batch-documents",
                "10",
                "--content-maximum-batches",
                "20",
                "--min-free-disk-percent",
                "0",
                "--min-free-disk-bytes",
                "1",
            ])
            .output()
            .expect("resume generation process");
        assert!(
            resumed.status.success(),
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let summary: serde_json::Value =
            serde_json::from_slice(&resumed.stdout).expect("resume summary");
        assert_eq!(summary["scan_deferred_for_content_resume"], true);
        assert_eq!(summary["content_complete"], true);
        assert_eq!(
            summary["content_summary"]["generation"]["generation_id"],
            generation_id.to_string_lossy().as_ref()
        );
        let reader = ContentIndex::open(&state.join("content-index-v1")).expect("active reader");
        assert_eq!(reader.document_count().expect("content count"), 100);
        assert_eq!(
            reader
                .search(&format!("restarttoken{label}x099"), 10)
                .expect("resumed search")
                .len(),
            1
        );
    }
}

#[cfg(windows)]
#[test]
fn generation_activation_refreshes_open_reader_gc_retains_rollback_and_reset_erases_content() {
    use std::process::Command;

    let temp = TempDir::new().expect("workspace");
    let selected = temp.path().join("selected");
    let state = temp.path().join("state");
    fs::create_dir(&selected).expect("selected root");
    let source = selected.join("generation.md");
    fs::write(&source, "generation alpha").expect("first content");
    let initial = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "folder-sync",
            "--workspace",
            state.to_str().expect("state path"),
            "--root",
            selected.to_str().expect("selected path"),
            "--min-free-disk-percent",
            "0",
            "--min-free-disk-bytes",
            "1",
        ])
        .output()
        .expect("initial generation");
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let reader = ContentIndex::open(&state.join("content-index-v1")).expect("managed reader");
    assert_eq!(reader.search("alpha", 10).expect("first active").len(), 1);

    fs::write(&source, "generation bravo").expect("second content");
    let second = run_generation_rebuild(&state);
    assert!(second["complete"].as_bool().unwrap_or(false));
    assert!(reader.search("alpha", 10).expect("retired term").is_empty());
    assert_eq!(reader.search("bravo", 10).expect("new active").len(), 1);

    fs::write(&source, "generation cider").expect("third content");
    let third = run_generation_rebuild(&state);
    assert!(third["complete"].as_bool().unwrap_or(false));
    assert_eq!(reader.search("cider", 10).expect("third active").len(), 1);
    let generation_root = state.join("content-index-v1").join("generations");
    assert_eq!(
        fs::read_dir(&generation_root)
            .expect("generation root")
            .count(),
        2,
        "automatic GC must retain active plus one rollback generation"
    );

    let gc = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "gc",
            "--workspace",
            state.to_str().expect("state path"),
            "--retain-retired-generations",
            "0",
        ])
        .output()
        .expect("generation gc");
    assert!(
        gc.status.success(),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    let gc_summary: serde_json::Value = serde_json::from_slice(&gc.stdout).expect("gc summary");
    assert_eq!(
        gc_summary["removed_generations"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        fs::read_dir(&generation_root)
            .expect("generation root after gc")
            .count(),
        1
    );

    let reset = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "reset-content",
            "--workspace",
            state.to_str().expect("state path"),
        ])
        .output()
        .expect("content reset");
    assert!(
        reset.status.success(),
        "{}",
        String::from_utf8_lossy(&reset.stderr)
    );
    let reset_summary: serde_json::Value =
        serde_json::from_slice(&reset.stdout).expect("reset summary");
    assert_eq!(reset_summary["removed"], true);
    assert_eq!(reset_summary["checkpoint_removed"], true);
    assert!(!state.join("content-index-v1").exists());
    assert!(ContentIndex::open(&state.join("content-index-v1")).is_err());
}

#[cfg(windows)]
fn run_generation_rebuild(state: &Path) -> serde_json::Value {
    use std::process::Command;

    let output = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "generation-rebuild",
            "--workspace",
            state.to_str().expect("state path"),
            "--min-free-disk-percent",
            "0",
            "--min-free-disk-bytes",
            "1",
        ])
        .output()
        .expect("generation rebuild");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("generation summary")
}

#[cfg(windows)]
#[test]
fn capacity_limits_pause_with_explicit_partial_reasons_without_activation() {
    use std::process::Command;

    for (label, arguments, expected) in [
        (
            "content-bytes",
            vec!["--max-content-index-bytes", "1"],
            "CONTENT_INDEX_BYTES",
        ),
        (
            "documents",
            vec!["--max-content-documents", "1"],
            "DOCUMENTS",
        ),
        (
            "free-disk",
            vec!["--min-free-disk-percent", "50"],
            "FREE_DISK",
        ),
    ] {
        let temp = TempDir::new().expect("workspace");
        let selected = temp.path().join(format!("selected-{label}"));
        let state = temp.path().join(format!("state-{label}"));
        fs::create_dir(&selected).expect("selected root");
        for ordinal in 0..4_u8 {
            fs::write(
                selected.join(format!("capacity-{ordinal}.md")),
                format!("capacity fixture {ordinal}"),
            )
            .expect("capacity fixture");
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"));
        command.args([
            "folder-sync",
            "--workspace",
            state.to_str().expect("state path"),
            "--root",
            selected.to_str().expect("selected path"),
            "--min-free-disk-bytes",
            "1",
        ]);
        command.args(arguments);
        let output = command.output().expect("capacity run");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let summary: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("capacity summary");
        assert_eq!(summary["content_complete"], false, "{label}");
        assert_eq!(
            summary["content_summary"]["capacity_limited"], true,
            "{label}"
        );
        assert_eq!(
            summary["content_summary"]["generation"]["capacity_limit"], expected,
            "{label}"
        );
        assert_eq!(
            summary["content_summary"]["generation"]["state"], "BUILDING",
            "{label}"
        );
        assert!(ContentIndex::open(&state.join("content-index-v1")).is_err());
    }
}

#[cfg(windows)]
#[test]
fn removing_an_explicit_root_revokes_graph_and_content_projection() {
    use std::process::Command;

    let temp = TempDir::new().expect("workspace");
    let first_root = temp.path().join("first-root");
    let revoked_root = temp.path().join("revoked-root");
    let state = temp.path().join("state");
    fs::create_dir(&first_root).expect("first root");
    fs::create_dir(&revoked_root).expect("revoked root");
    fs::write(first_root.join("kept.md"), "keptscopeprobe").expect("kept source");
    fs::write(revoked_root.join("revoked.md"), "revokedscopeprobe").expect("revoked source");

    let initial = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "folder-sync",
            "--workspace",
            state.to_str().expect("state path"),
            "--root",
            first_root.to_str().expect("first root path"),
            "--root",
            revoked_root.to_str().expect("revoked root path"),
            "--min-free-disk-percent",
            "0",
            "--min-free-disk-bytes",
            "1",
        ])
        .output()
        .expect("initial roots");
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let reader = ContentIndex::open(&state.join("content-index-v1")).expect("reader");
    assert_eq!(reader.search("keptscopeprobe", 10).expect("kept").len(), 1);
    assert_eq!(
        reader
            .search("revokedscopeprobe", 10)
            .expect("before revocation")
            .len(),
        1
    );

    let revoked = Command::new(env!("CARGO_BIN_EXE_localsearch-content-index"))
        .args([
            "folder-sync",
            "--workspace",
            state.to_str().expect("state path"),
            "--root",
            first_root.to_str().expect("first root path"),
            "--min-free-disk-percent",
            "0",
            "--min-free-disk-bytes",
            "1",
        ])
        .output()
        .expect("scope revocation");
    assert!(
        revoked.status.success(),
        "{}",
        String::from_utf8_lossy(&revoked.stderr)
    );
    assert_eq!(reader.search("keptscopeprobe", 10).expect("kept").len(), 1);
    assert!(
        reader
            .search("revokedscopeprobe", 10)
            .expect("after revocation")
            .is_empty()
    );
    let graph =
        localsearch_filesystem_graph::FilesystemGraph::open_read_only(state.join("graph.sqlite3"))
            .expect("graph");
    assert!(
        graph
            .desired_catalog_documents()
            .expect("catalog records")
            .iter()
            .all(|document| !document.resolved_path.contains("revoked-root"))
    );
}
