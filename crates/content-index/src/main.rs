#[cfg(windows)]
const DEFAULT_MAX_GRAPH_BYTES: u64 = 10 * 1024 * 1024 * 1024;
#[cfg(windows)]
const DEFAULT_MAX_CONTENT_INDEX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
#[cfg(windows)]
const DEFAULT_MAX_CONTENT_DOCUMENTS: u64 = 5_000_000;
#[cfg(windows)]
const DEFAULT_MIN_FREE_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
#[cfg(windows)]
const DEFAULT_MIN_FREE_DISK_PERCENT: u8 = 1;

#[cfg(windows)]
#[allow(
    clippy::too_many_lines,
    reason = "top-level CLI dispatch keeps every command's required arguments visible"
)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use localsearch_content_index::{ContentIndex, ContentIndexPolicy};
    use localsearch_filesystem_graph::FilesystemGraph;

    let CliArguments {
        command,
        graph,
        index,
        workspace,
        roots,
        query,
        top_k,
        max_file_bytes,
        include_generated,
        batch_size,
        maximum_batches,
        scan_batch_size,
        max_graph_bytes,
        max_content_index_bytes,
        max_content_documents,
        min_free_disk_bytes,
        min_free_disk_percent,
        content_batch_documents,
        content_maximum_batches,
        retain_retired_generations,
        watch_interval_ms,
        watch_iterations,
        compact_batch_rows,
        compact_maximum_batches,
        reclaim_pages,
        allow_without_consumers,
        custom_excluded_directories,
    } = parse_arguments()?;
    match command.as_str() {
        "folder-sync" => {
            let workspace = workspace.ok_or("folder-sync requires --workspace PATH")?;
            let summary = folder_sync(
                &workspace,
                roots,
                max_file_bytes,
                include_generated,
                scan_batch_size,
                max_graph_bytes,
                localsearch_content_index::ContentGenerationLimits {
                    max_content_index_bytes,
                    max_documents: max_content_documents,
                    min_free_disk_bytes,
                    min_free_disk_percent,
                    batch_documents: content_batch_documents,
                    maximum_batches: content_maximum_batches,
                },
                custom_excluded_directories,
            )?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        "build" => {
            let index = index.ok_or("build requires --index PATH")?;
            let graph = graph.ok_or("build requires --graph PATH")?;
            let policy = ContentIndexPolicy::new(roots, max_file_bytes)?;
            let graph = FilesystemGraph::open_read_only(graph)?;
            let summary = ContentIndex::build_from_graph(&index, &graph, &policy)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        "sync" => {
            let index = index.ok_or("sync requires --index PATH")?;
            let graph = graph.ok_or("sync requires --graph PATH")?;
            let policy = ContentIndexPolicy::new(roots, max_file_bytes)?;
            let graph = FilesystemGraph::open_read_only(graph)?;
            let summary = ContentIndex::sync_from_graph(&index, &graph, &policy)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        "project" => {
            let summary = if let Some(workspace) = workspace {
                if graph.is_some() || index.is_some() || !roots.is_empty() {
                    return Err(
                        "project --workspace cannot be combined with graph/index/root".into(),
                    );
                }
                project_workspace(&workspace, batch_size, maximum_batches)?
            } else {
                let index = index.ok_or("project requires --index PATH or --workspace PATH")?;
                let graph = graph.ok_or("project requires --graph PATH or --workspace PATH")?;
                project_content(
                    &graph,
                    &index,
                    roots,
                    max_file_bytes,
                    batch_size,
                    maximum_batches,
                )?
            };
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        "search" => {
            let index = index.ok_or("search requires --index PATH")?;
            let query = query.ok_or("search requires --query TEXT")?;
            let reader = ContentIndex::open(&index)?;
            let hits = reader.search(&query, top_k)?;
            println!("{}", serde_json::to_string_pretty(&hits)?);
        }
        "generation-rebuild" => {
            let workspace = workspace.ok_or("generation-rebuild requires --workspace PATH")?;
            let summary = rebuild_workspace(
                &workspace,
                localsearch_content_index::ContentGenerationLimits {
                    max_content_index_bytes,
                    max_documents: max_content_documents,
                    min_free_disk_bytes,
                    min_free_disk_percent,
                    batch_documents: content_batch_documents,
                    maximum_batches: content_maximum_batches,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        "gc" => {
            let workspace = workspace.ok_or("gc requires --workspace PATH")?;
            let summary = garbage_collect_workspace(&workspace, retain_retired_generations)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        "reset-content" => {
            let workspace = workspace.ok_or("reset-content requires --workspace PATH")?;
            let summary = reset_content_workspace(&workspace)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        "watch" => {
            let workspace = workspace.ok_or("watch requires --workspace PATH")?;
            watch_workspace(
                &workspace,
                batch_size,
                maximum_batches,
                watch_interval_ms,
                watch_iterations,
            )?;
        }
        "compact" => {
            let workspace = workspace.ok_or("compact requires --workspace PATH")?;
            let summary = compact_graph_workspace(
                &workspace,
                compact_batch_rows,
                compact_maximum_batches,
                reclaim_pages,
                allow_without_consumers,
            )?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        _ => {
            return Err(
                "expected folder-sync, build, sync, project, search, generation-rebuild, gc, reset-content, watch, or compact"
                    .into(),
            );
        }
    }
    Ok(())
}

#[cfg(windows)]
struct CliArguments {
    command: String,
    graph: Option<std::path::PathBuf>,
    index: Option<std::path::PathBuf>,
    workspace: Option<std::path::PathBuf>,
    roots: Vec<std::path::PathBuf>,
    query: Option<String>,
    top_k: usize,
    max_file_bytes: u64,
    include_generated: bool,
    batch_size: u32,
    maximum_batches: u32,
    scan_batch_size: usize,
    max_graph_bytes: u64,
    max_content_index_bytes: u64,
    max_content_documents: u64,
    min_free_disk_bytes: u64,
    min_free_disk_percent: u8,
    content_batch_documents: u32,
    content_maximum_batches: u32,
    retain_retired_generations: usize,
    watch_interval_ms: u64,
    watch_iterations: Option<u32>,
    compact_batch_rows: u32,
    compact_maximum_batches: u32,
    reclaim_pages: u32,
    allow_without_consumers: bool,
    custom_excluded_directories: Vec<String>,
}

#[cfg(windows)]
#[allow(
    clippy::too_many_lines,
    reason = "the dependency-free CLI parser validates one closed option vocabulary"
)]
fn parse_arguments() -> Result<CliArguments, Box<dyn std::error::Error>> {
    use std::path::PathBuf;

    let mut arguments = std::env::args().skip(1);
    let command = arguments
        .next()
        .ok_or("expected folder-sync, build, sync, project, or search")?;
    let mut parsed = CliArguments {
        command,
        graph: None,
        index: None,
        workspace: None,
        roots: Vec::new(),
        query: None,
        top_k: 20,
        max_file_bytes: localsearch_content_index::DEFAULT_MAX_FILE_BYTES,
        include_generated: false,
        batch_size: 1_024,
        maximum_batches: 64,
        scan_batch_size: 10_000,
        max_graph_bytes: DEFAULT_MAX_GRAPH_BYTES,
        max_content_index_bytes: DEFAULT_MAX_CONTENT_INDEX_BYTES,
        max_content_documents: DEFAULT_MAX_CONTENT_DOCUMENTS,
        min_free_disk_bytes: DEFAULT_MIN_FREE_DISK_BYTES,
        min_free_disk_percent: DEFAULT_MIN_FREE_DISK_PERCENT,
        content_batch_documents: 4_096,
        content_maximum_batches: 64,
        retain_retired_generations: 1,
        watch_interval_ms: 1_000,
        watch_iterations: None,
        compact_batch_rows: 50_000,
        compact_maximum_batches: 64,
        reclaim_pages: 4_096,
        allow_without_consumers: false,
        custom_excluded_directories: Vec::new(),
    };
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--graph" => parsed.graph = arguments.next().map(PathBuf::from),
            "--index" => parsed.index = arguments.next().map(PathBuf::from),
            "--workspace" => parsed.workspace = arguments.next().map(PathBuf::from),
            "--root" => parsed
                .roots
                .push(PathBuf::from(arguments.next().ok_or("missing root")?)),
            "--query" => parsed.query = arguments.next(),
            "--top-k" => parsed.top_k = arguments.next().ok_or("missing top-k")?.parse()?,
            "--max-file-bytes" => {
                parsed.max_file_bytes =
                    arguments.next().ok_or("missing max-file-bytes")?.parse()?;
            }
            "--include-generated" => parsed.include_generated = true,
            "--batch-size" => {
                parsed.batch_size = arguments.next().ok_or("missing batch-size")?.parse()?;
            }
            "--maximum-batches" => {
                parsed.maximum_batches =
                    arguments.next().ok_or("missing maximum-batches")?.parse()?;
            }
            "--scan-batch-size" => {
                parsed.scan_batch_size =
                    arguments.next().ok_or("missing scan-batch-size")?.parse()?;
            }
            "--max-graph-bytes" => {
                parsed.max_graph_bytes =
                    arguments.next().ok_or("missing max-graph-bytes")?.parse()?;
            }
            "--max-graph-gib" => {
                let gib = arguments
                    .next()
                    .ok_or("missing max-graph-gib")?
                    .parse::<u64>()?;
                parsed.max_graph_bytes = gib
                    .checked_mul(1024 * 1024 * 1024)
                    .ok_or("max-graph-gib is too large")?;
            }
            "--max-content-index-bytes" => {
                parsed.max_content_index_bytes = arguments
                    .next()
                    .ok_or("missing max-content-index-bytes")?
                    .parse()?;
            }
            "--max-content-index-gib" => {
                let gib = arguments
                    .next()
                    .ok_or("missing max-content-index-gib")?
                    .parse::<u64>()?;
                parsed.max_content_index_bytes = gib
                    .checked_mul(1024 * 1024 * 1024)
                    .ok_or("max-content-index-gib is too large")?;
            }
            "--max-content-documents" => {
                parsed.max_content_documents = arguments
                    .next()
                    .ok_or("missing max-content-documents")?
                    .parse()?;
            }
            "--min-free-disk-bytes" => {
                parsed.min_free_disk_bytes = arguments
                    .next()
                    .ok_or("missing min-free-disk-bytes")?
                    .parse()?;
            }
            "--min-free-disk-gib" => {
                let gib = arguments
                    .next()
                    .ok_or("missing min-free-disk-gib")?
                    .parse::<u64>()?;
                parsed.min_free_disk_bytes = gib
                    .checked_mul(1024 * 1024 * 1024)
                    .ok_or("min-free-disk-gib is too large")?;
            }
            "--min-free-disk-percent" => {
                parsed.min_free_disk_percent = arguments
                    .next()
                    .ok_or("missing min-free-disk-percent")?
                    .parse()?;
            }
            "--content-batch-documents" => {
                parsed.content_batch_documents = arguments
                    .next()
                    .ok_or("missing content-batch-documents")?
                    .parse()?;
            }
            "--content-maximum-batches" => {
                parsed.content_maximum_batches = arguments
                    .next()
                    .ok_or("missing content-maximum-batches")?
                    .parse()?;
            }
            "--retain-retired-generations" => {
                parsed.retain_retired_generations = arguments
                    .next()
                    .ok_or("missing retain-retired-generations")?
                    .parse()?;
            }
            "--watch-interval-ms" => {
                parsed.watch_interval_ms = arguments
                    .next()
                    .ok_or("missing watch-interval-ms")?
                    .parse()?;
            }
            "--watch-iterations" => {
                parsed.watch_iterations = Some(
                    arguments
                        .next()
                        .ok_or("missing watch-iterations")?
                        .parse()?,
                );
            }
            "--compact-batch-rows" => {
                parsed.compact_batch_rows = arguments
                    .next()
                    .ok_or("missing compact-batch-rows")?
                    .parse()?;
            }
            "--compact-maximum-batches" => {
                parsed.compact_maximum_batches = arguments
                    .next()
                    .ok_or("missing compact-maximum-batches")?
                    .parse()?;
            }
            "--reclaim-pages" => {
                parsed.reclaim_pages = arguments.next().ok_or("missing reclaim-pages")?.parse()?;
            }
            "--allow-without-consumers" => parsed.allow_without_consumers = true,
            "--exclude-dir" => parsed.custom_excluded_directories.push(
                arguments
                    .next()
                    .ok_or("missing exclude-dir directory name")?,
            ),
            _ => return Err(format!("unknown or incomplete argument: {argument}").into()),
        }
    }
    Ok(parsed)
}

#[cfg(windows)]
fn project_workspace(
    workspace: &std::path::Path,
    batch_size: u32,
    maximum_batches: u32,
) -> Result<ContentProjectionSummary, Box<dyn std::error::Error>> {
    let summary = localsearch_content_index::ContentProjectionWorker::from_workspace(workspace)?
        .project(localsearch_content_index::ContentProjectionOptions {
            batch_size,
            maximum_batches,
        })?;
    let _ = compact_graph_workspace(workspace, 10_000, 1, 4_096, false)?;
    Ok(summary)
}

#[cfg(windows)]
fn rebuild_workspace(
    workspace: &std::path::Path,
    limits: localsearch_content_index::ContentGenerationLimits,
) -> Result<localsearch_content_index::ContentGenerationSummary, Box<dyn std::error::Error>> {
    use localsearch_content_index::{
        CONTENT_SCHEMA_ID, ContentGenerationManager, ContentIndexPolicy,
    };
    use localsearch_filesystem_graph::FilesystemGraph;

    let manifest: ContentWorkspaceManifest =
        serde_json::from_slice(&std::fs::read(workspace.join("content-workspace.json"))?)?;
    if !matches!(manifest.version, 1 | 2) {
        return Err("unsupported content workspace manifest".into());
    }
    let policy = ContentIndexPolicy::new(
        manifest.roots.iter().map(std::path::PathBuf::from),
        manifest.max_file_bytes,
    )?;
    let graph = FilesystemGraph::open(workspace.join("graph.sqlite3"))?;
    let manager = ContentGenerationManager::open(workspace.join("content-index-v1"))?;
    let summary = manager.resume_initial_generation(&graph, &policy, limits)?;
    if summary.complete {
        graph.acknowledge_projection(
            CONTENT_SCHEMA_ID,
            localsearch_core::MutationSeq(summary.generation.target_sequence),
            summary.generation.index_generation,
        )?;
    }
    Ok(summary)
}

#[cfg(windows)]
fn garbage_collect_workspace(
    workspace: &std::path::Path,
    retain_retired: usize,
) -> Result<GarbageCollectionSummary, Box<dyn std::error::Error>> {
    if retain_retired > 8 {
        return Err("retired generation retention is outside product bounds".into());
    }
    let root = workspace.join("content-index-v1");
    let before = owned_directory_bytes(&root)?;
    let removed = localsearch_content_index::ContentGenerationManager::open(&root)?
        .collect_garbage(retain_retired)?;
    let after = owned_directory_bytes(&root)?;
    Ok(GarbageCollectionSummary {
        removed_generations: removed,
        reclaimed_bytes: before.saturating_sub(after),
        retained_retired_generations: retain_retired,
    })
}

#[cfg(windows)]
fn reset_content_workspace(
    workspace: &std::path::Path,
) -> Result<ResetContentSummary, Box<dyn std::error::Error>> {
    use localsearch_content_index::CONTENT_SCHEMA_ID;
    use localsearch_filesystem_graph::FilesystemGraph;

    let workspace = std::fs::canonicalize(workspace)?;
    let content = workspace.join("content-index-v1");
    if content.parent() != Some(workspace.as_path()) {
        return Err("refusing content reset outside workspace".into());
    }
    let removed_bytes = owned_directory_bytes(&content)?;
    if content.exists() {
        std::fs::remove_dir_all(&content)?;
    }
    let graph_path = workspace.join("graph.sqlite3");
    let checkpoint_removed = if graph_path.is_file() {
        FilesystemGraph::open(graph_path)?.reset_projection_consumer(CONTENT_SCHEMA_ID)?
    } else {
        false
    };
    Ok(ResetContentSummary {
        content_root: content.to_string_lossy().into_owned(),
        removed_bytes,
        checkpoint_removed,
        removed: !content.exists(),
    })
}

#[cfg(windows)]
fn compact_graph_workspace(
    workspace: &std::path::Path,
    batch_rows: u32,
    maximum_batches: u32,
    reclaim_pages: u32,
    allow_without_consumers: bool,
) -> Result<GraphCompactionSummary, Box<dyn std::error::Error>> {
    use localsearch_filesystem_graph::FilesystemGraph;

    if batch_rows == 0
        || batch_rows > 100_000
        || maximum_batches == 0
        || maximum_batches > 1_024
        || reclaim_pages == 0
        || reclaim_pages > 4_096
    {
        return Err("graph compaction bounds are invalid".into());
    }
    let workspace = std::fs::canonicalize(workspace)?;
    let graph_path = workspace.join("graph.sqlite3");
    if graph_path.parent() != Some(workspace.as_path()) || !graph_path.is_file() {
        return Err("workspace does not own a durable graph".into());
    }
    let mut graph = FilesystemGraph::open(&graph_path)?;
    graph.prepare_size_measurement()?;
    let storage_before = graph.storage_stats()?;
    let sequence_before = graph.latest_outbox_sequence()?.0;
    let file_bytes_before = graph_storage_bytes(&graph_path)?;
    let mut desired_payload_rows_rewritten = 0_u64;
    let mut desired_payload_backlog_remaining = false;
    let mut desired_payload_batches = 0_u32;
    for _ in 0..maximum_batches {
        let summary = graph.compact_legacy_desired_payloads_bounded(batch_rows)?;
        desired_payload_batches = desired_payload_batches.saturating_add(1);
        desired_payload_rows_rewritten =
            desired_payload_rows_rewritten.saturating_add(summary.rewritten_rows);
        desired_payload_backlog_remaining = summary.backlog_remaining;
        if !summary.backlog_remaining || summary.rewritten_rows == 0 {
            break;
        }
    }
    let mut deleted_rows = 0_u64;
    let mut safe_through_sequence = None;
    let mut backlog_remaining = false;
    let mut batches = 0_u32;
    for _ in 0..maximum_batches {
        let summary =
            graph.prune_rebuildable_outbox_bounded(batch_rows, allow_without_consumers)?;
        batches = batches.saturating_add(1);
        deleted_rows = deleted_rows.saturating_add(summary.deleted_rows);
        safe_through_sequence = summary.safe_through_sequence;
        backlog_remaining = summary.backlog_remaining;
        if !summary.backlog_remaining || summary.deleted_rows == 0 {
            break;
        }
    }
    let reclaimed_pages = graph.reclaim_reusable_pages(reclaim_pages)?;
    graph.prepare_size_measurement()?;
    let storage_after = graph.storage_stats()?;
    let sequence_after = graph.latest_outbox_sequence()?.0;
    if sequence_after != sequence_before {
        return Err("graph compaction changed the durable sequence high-water mark".into());
    }
    let file_bytes_after = graph_storage_bytes(&graph_path)?;
    Ok(GraphCompactionSummary {
        graph: graph_path.to_string_lossy().into_owned(),
        desired_payload_rows_rewritten,
        desired_payload_batches,
        desired_payload_backlog_remaining,
        safe_through_sequence,
        sequence_before,
        sequence_after,
        deleted_rows,
        batches,
        backlog_remaining,
        reclaimed_pages,
        file_bytes_before,
        file_bytes_after,
        storage_before,
        storage_after,
        allow_without_consumers,
    })
}

#[cfg(windows)]
fn watch_workspace(
    workspace: &std::path::Path,
    batch_size: u32,
    maximum_batches: u32,
    interval_ms: u64,
    iterations: Option<u32>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;

    if !(100..=60_000).contains(&interval_ms) || iterations == Some(0) {
        return Err("watch bounds are invalid".into());
    }
    let mut completed = 0_u32;
    loop {
        let mut last_error = None;
        let mut projected = None;
        for retry in 0..3_u32 {
            match project_workspace(workspace, batch_size, maximum_batches) {
                Ok(summary) => {
                    projected = Some(summary);
                    break;
                }
                Err(error) => {
                    last_error = Some(error);
                    std::thread::sleep(std::time::Duration::from_millis(
                        100_u64.saturating_mul(1_u64 << retry),
                    ));
                }
            }
        }
        let Some(summary) = projected else {
            return Err(last_error.ok_or("content watch retry failed")?);
        };
        println!("{}", serde_json::to_string(&summary)?);
        std::io::stdout().flush()?;
        completed = completed.saturating_add(1);
        if iterations.is_some_and(|maximum| completed >= maximum) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
    }
}

#[cfg(windows)]
fn owned_directory_bytes(path: &std::path::Path) -> std::io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    std::fs::read_dir(path)?.try_fold(0_u64, |total, entry| {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let bytes = if metadata.is_dir() && !metadata.file_type().is_symlink() {
            owned_directory_bytes(&entry.path())?
        } else if metadata.is_file() {
            metadata.len()
        } else {
            0
        };
        Ok(total.saturating_add(bytes))
    })
}

#[cfg(windows)]
#[derive(serde::Serialize)]
struct GarbageCollectionSummary {
    removed_generations: Vec<String>,
    reclaimed_bytes: u64,
    retained_retired_generations: usize,
}

#[cfg(windows)]
#[derive(serde::Serialize)]
struct ResetContentSummary {
    content_root: String,
    removed_bytes: u64,
    checkpoint_removed: bool,
    removed: bool,
}

#[cfg(windows)]
#[derive(serde::Serialize)]
struct GraphCompactionSummary {
    graph: String,
    desired_payload_rows_rewritten: u64,
    desired_payload_batches: u32,
    desired_payload_backlog_remaining: bool,
    safe_through_sequence: Option<u64>,
    sequence_before: u64,
    sequence_after: u64,
    deleted_rows: u64,
    batches: u32,
    backlog_remaining: bool,
    reclaimed_pages: u64,
    file_bytes_before: u64,
    file_bytes_after: u64,
    storage_before: localsearch_filesystem_graph::GraphStorageStats,
    storage_after: localsearch_filesystem_graph::GraphStorageStats,
    allow_without_consumers: bool,
}

#[cfg(windows)]
fn project_content(
    graph_path: &std::path::Path,
    content_path: &std::path::Path,
    roots: Vec<std::path::PathBuf>,
    max_file_bytes: u64,
    batch_size: u32,
    maximum_batches: u32,
) -> Result<ContentProjectionSummary, Box<dyn std::error::Error>> {
    use std::collections::BTreeMap;

    use localsearch_content_index::{CONTENT_SCHEMA_ID, ContentIndex, ContentIndexPolicy};
    use localsearch_core::{IndexMutation, MutationSeq};
    use localsearch_filesystem_graph::FilesystemGraph;

    if batch_size == 0 || batch_size > 10_000 || maximum_batches == 0 || maximum_batches > 1_024 {
        return Err("content projection bounds are invalid".into());
    }
    let policy = ContentIndexPolicy::new(roots, max_file_bytes)?;
    let graph = FilesystemGraph::open(graph_path)?;
    let checkpoint = graph
        .projector_checkpoint(CONTENT_SCHEMA_ID)?
        .ok_or("content projection is not initialized; run folder-sync first")?;
    let start_sequence = checkpoint.last_sequence;
    let mut after = MutationSeq(start_sequence);
    let mut commits = Vec::new();
    let mut projected_mutations = 0_u64;

    for _ in 0..maximum_batches {
        let batch = graph.read_outbox(Some(after), batch_size)?;
        if batch.mutations.is_empty() {
            break;
        }
        batch.validate()?;
        let mut coalesced = BTreeMap::new();
        for sequenced in &batch.mutations {
            match &sequenced.mutation {
                IndexMutation::Upsert { document } => {
                    coalesced.insert(document.identity.document_id, Some(document.clone()));
                }
                IndexMutation::Delete { document_id, .. } => {
                    coalesced.insert(*document_id, None);
                }
            }
        }
        let upserts = coalesced
            .values()
            .filter_map(Clone::clone)
            .collect::<Vec<_>>();
        let deletions = coalesced
            .into_iter()
            .filter_map(|(document_id, document)| document.is_none().then_some(document_id))
            .collect::<Vec<_>>();
        let summary = ContentIndex::apply_delta(content_path, upserts, deletions, &policy)?;
        let last = batch
            .last_sequence()
            .ok_or("non-empty content projection batch lost its tail")?;
        graph.acknowledge_projection(CONTENT_SCHEMA_ID, last, summary.generation)?;
        projected_mutations = projected_mutations
            .saturating_add(u64::try_from(batch.mutations.len()).unwrap_or(u64::MAX));
        after = last;
        commits.push(summary);
    }
    let latest = graph.latest_outbox_sequence()?.0;
    Ok(ContentProjectionSummary {
        start_sequence,
        applied_sequence: after.0,
        latest_sequence: latest,
        projected_mutations,
        backlog_remaining: after.0 < latest,
        commits,
    })
}

#[cfg(windows)]
type ContentProjectionSummary = localsearch_content_index::DurableContentProjectionSummary;

#[cfg(windows)]
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "folder sync keeps scan, projection, manifest, and one machine-readable summary in a single transaction-oriented orchestration path"
)]
fn folder_sync(
    workspace: &std::path::Path,
    roots: Vec<std::path::PathBuf>,
    max_file_bytes: u64,
    include_generated: bool,
    scan_batch_size: usize,
    max_graph_bytes: u64,
    content_limits: localsearch_content_index::ContentGenerationLimits,
    custom_excluded_directories: Vec<String>,
) -> Result<FolderSyncSummary, Box<dyn std::error::Error>> {
    use std::{fs, io};

    use localsearch_content_index::{CONTENT_SCHEMA_ID, ContentIndex, ContentIndexPolicy};
    use localsearch_filesystem_graph::FilesystemGraph;
    use localsearch_windows_fs::{ScopedScanOptions, WindowsFilesystemProvider};

    let policy = ContentIndexPolicy::new(roots, max_file_bytes)?;
    if scan_batch_size == 0 || scan_batch_size > 50_000 {
        return Err("scan batch size is outside product bounds".into());
    }
    if max_graph_bytes == 0 {
        return Err("graph byte limit must be positive".into());
    }
    fs::create_dir_all(workspace)?;
    let workspace = fs::canonicalize(workspace)?;
    if policy
        .roots()
        .iter()
        .any(|root| workspace.starts_with(root) || root.starts_with(&workspace))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "folder workspace and selected roots must not overlap",
        )
        .into());
    }

    let graph_path = workspace.join("graph.sqlite3");
    let catalog_path = workspace.join("catalog");
    let content_path = workspace.join("content-index-v1");
    let manifest_path = workspace.join("content-workspace.json");
    let previous_manifest = if manifest_path.is_file() {
        Some(serde_json::from_slice::<ContentWorkspaceManifest>(
            &fs::read(&manifest_path)?,
        )?)
    } else {
        None
    };
    let requested_roots = policy
        .roots()
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let legacy_content_index = content_path.join("LOCALSEARCH_CONTENT_SCHEMA").is_file();
    let generation_manager = if legacy_content_index {
        None
    } else {
        Some(localsearch_content_index::ContentGenerationManager::open(
            &content_path,
        )?)
    };
    let scan_deferred_for_content_resume = generation_manager
        .as_ref()
        .and_then(|manager| manager.building_generation_record().ok().flatten())
        .is_some_and(|record| record.root_ids == requested_roots);
    let mut graph = FilesystemGraph::open(&graph_path)?;
    // A missing checkpoint beside an already-published generation is a reset/recovery state, not
    // an initial build. Keep durable deltas in that case so the active index cannot miss changes.
    let active_generation_exists = generation_manager
        .as_ref()
        .is_some_and(localsearch_content_index::ContentGenerationManager::has_active_generation);
    let rebuildable_initial_ingest =
        !graph.has_projection_consumers()? && !active_generation_exists;
    let provider = WindowsFilesystemProvider::new();
    let scoped_roots = policy
        .roots()
        .iter()
        .map(|root| {
            provider
                .prepare_scan_root(root)
                .map(|prepared| ScopedRootBinding {
                    root: root.to_string_lossy().into_owned(),
                    volume_id: prepared.volume().volume_id,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut excluded_directories = if include_generated {
        Vec::new()
    } else {
        default_excluded_directories()
    };
    excluded_directories.extend(custom_excluded_directories);
    excluded_directories.sort_by_key(|value| value.to_lowercase());
    excluded_directories.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    let scan_options = ScopedScanOptions::excluding(excluded_directories.iter().cloned());
    let mut observed_events = 0_u64;
    let mut graph_mutations = 0_u64;
    if let Some(previous) = &previous_manifest {
        for removed in previous.scoped_roots.iter().filter(|previous_root| {
            !scoped_roots
                .iter()
                .any(|current| current.root == previous_root.root)
        }) {
            graph_mutations = graph_mutations.saturating_add(remove_scoped_root(
                &mut graph,
                removed.volume_id,
                scan_batch_size,
                rebuildable_initial_ingest,
            )?);
        }
    }
    let mut scan_complete = true;
    let mut graph_limit_reached =
        graph_storage_pressure_bytes(&graph_path, &graph)? >= graph_stop_threshold(max_graph_bytes);
    if scan_deferred_for_content_resume || graph_limit_reached {
        scan_complete = false;
    } else {
        for root in policy.roots() {
            let root_summary = sync_scoped_root(
                &mut graph,
                &provider,
                root,
                &scan_options,
                scan_batch_size,
                &graph_path,
                max_graph_bytes,
                rebuildable_initial_ingest,
            )?;
            observed_events = observed_events.saturating_add(root_summary.observed_events);
            graph_mutations = graph_mutations.saturating_add(root_summary.graph_mutations);
            if !root_summary.complete {
                scan_complete = false;
                graph_limit_reached = true;
                break;
            }
        }
    }

    graph.prepare_size_measurement()?;
    let graph_stats = graph.stats()?;

    let (content_mode, content_summary, content_sequence, content_complete) =
        if legacy_content_index {
            let summary = ContentIndex::sync_from_graph(&content_path, &graph, &policy)?;
            let sequence = graph.latest_outbox_sequence()?;
            graph.acknowledge_projection(CONTENT_SCHEMA_ID, sequence, summary.generation)?;
            (
                "legacy-sync",
                serde_json::to_value(&summary)?,
                sequence,
                true,
            )
        } else {
            let manager = generation_manager
                .as_ref()
                .ok_or("content generation manager is unavailable")?;
            if manager.has_building_generation()? || !manager.has_active_generation() {
                let summary = manager.resume_initial_generation(&graph, &policy, content_limits)?;
                let generation = summary.generation.index_generation;
                let sequence = if summary.complete {
                    let sequence =
                        localsearch_core::MutationSeq(summary.generation.target_sequence);
                    graph.acknowledge_projection(CONTENT_SCHEMA_ID, sequence, generation)?;
                    sequence
                } else {
                    graph
                        .projector_checkpoint(CONTENT_SCHEMA_ID)?
                        .map_or(localsearch_core::MutationSeq(0), |checkpoint| {
                            localsearch_core::MutationSeq(checkpoint.last_sequence)
                        })
                };
                (
                    "generation-resume",
                    serde_json::to_value(&summary)?,
                    sequence,
                    summary.complete,
                )
            } else {
                let active = manager.active_generation_record()?;
                if graph.projector_checkpoint(CONTENT_SCHEMA_ID)?.is_none() {
                    graph.acknowledge_projection(
                        CONTENT_SCHEMA_ID,
                        localsearch_core::MutationSeq(active.target_sequence),
                        active.index_generation,
                    )?;
                }
                let active_index = manager.active_index_path()?;
                drop(graph);
                let projected = project_content(
                    &graph_path,
                    &active_index,
                    policy.roots().to_vec(),
                    max_file_bytes,
                    1_024,
                    64,
                )?;
                (
                    "project",
                    serde_json::to_value(&projected)?,
                    localsearch_core::MutationSeq(projected.applied_sequence),
                    true,
                )
            }
        };
    let graph_compaction = content_complete
        .then(|| compact_graph_workspace(&workspace, 100_000, 64, 4_096, false))
        .transpose()?;
    let graph = FilesystemGraph::open(&graph_path)?;
    let graph_storage = graph.storage_stats()?;
    let graph_bytes = graph_storage_bytes(&graph_path)?;
    let graph_pressure_bytes = graph_storage_pressure_bytes(&graph_path, &graph)?;
    let roots = policy
        .roots()
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let manifest = ContentWorkspaceManifest {
        version: 2,
        roots: roots.clone(),
        max_file_bytes,
        include_generated,
        excluded_directories: excluded_directories.clone(),
        scan_batch_size,
        max_graph_bytes,
        max_content_index_bytes: content_limits.max_content_index_bytes,
        max_content_documents: content_limits.max_documents,
        min_free_disk_bytes: content_limits.min_free_disk_bytes,
        min_free_disk_percent: content_limits.min_free_disk_percent,
        content_batch_documents: content_limits.batch_documents,
        content_maximum_batches: content_limits.maximum_batches,
        scoped_roots,
    };
    fs::write(
        workspace.join("folder-roots.json"),
        serde_json::to_vec_pretty(&roots)?,
    )?;
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(FolderSyncSummary {
        workspace: workspace.to_string_lossy().into_owned(),
        roots,
        graph: graph_path.to_string_lossy().into_owned(),
        catalog: catalog_path.to_string_lossy().into_owned(),
        content_index: content_path.to_string_lossy().into_owned(),
        observed_events,
        graph_mutations,
        scan_complete,
        scan_deferred_for_content_resume,
        graph_limit_reached,
        max_graph_bytes,
        graph_bytes,
        graph_pressure_bytes,
        graph_reusable_bytes: graph_storage.reusable_bytes,
        graph_stats,
        graph_compaction,
        graph_outbox_mode: if rebuildable_initial_ingest {
            "rebuildable-initial"
        } else {
            "durable"
        },
        content_mode,
        content_summary,
        content_complete,
        max_content_index_bytes: content_limits.max_content_index_bytes,
        max_content_documents: content_limits.max_documents,
        min_free_disk_bytes: content_limits.min_free_disk_bytes,
        min_free_disk_percent: content_limits.min_free_disk_percent,
        excluded_directories,
        content_sequence: content_sequence.0,
        scan_batch_size,
        manifest: manifest_path.to_string_lossy().into_owned(),
    })
}

#[cfg(windows)]
fn default_excluded_directories() -> Vec<String> {
    [
        "$Recycle.Bin",
        ".cache",
        ".git",
        ".hg",
        ".svn",
        ".venv",
        "__pycache__",
        "build",
        "dist",
        "node_modules",
        "Program Files",
        "Program Files (x86)",
        "ProgramData",
        "System Volume Information",
        "target",
        "venv",
        "Windows",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[cfg(windows)]
fn remove_scoped_root(
    graph: &mut localsearch_filesystem_graph::FilesystemGraph,
    volume_id: localsearch_core::VolumeId,
    batch_size: usize,
    rebuildable_initial_ingest: bool,
) -> Result<u64, Box<dyn std::error::Error>> {
    use std::collections::BTreeSet;

    use localsearch_filesystem_graph::GraphMutation;
    use localsearch_platform_core::ProviderCheckpoint;

    let current = graph.desired_catalog_identities(volume_id)?;
    let checkpoint = ProviderCheckpoint {
        provider_id: "content-scope-revocation".to_owned(),
        format_version: 1,
        volume_id,
        opaque: Vec::new(),
    };
    let mut pending = Vec::with_capacity(batch_size);
    let mut mutations = 0_u64;
    for (file_link_id, object_key) in &current {
        pending.push(GraphMutation::RemoveLink {
            file_link_id: *file_link_id,
            object_key: *object_key,
        });
        if pending.len() >= batch_size {
            mutations = mutations.saturating_add(apply_root_removal_batch(
                graph,
                volume_id,
                &checkpoint,
                &mut pending,
                rebuildable_initial_ingest,
            )?);
        }
    }
    for object_key in current
        .into_iter()
        .map(|(_, object_key)| object_key)
        .collect::<BTreeSet<_>>()
    {
        pending.push(GraphMutation::TombstoneObject { object_key });
        if pending.len() >= batch_size {
            mutations = mutations.saturating_add(apply_root_removal_batch(
                graph,
                volume_id,
                &checkpoint,
                &mut pending,
                rebuildable_initial_ingest,
            )?);
        }
    }
    if !pending.is_empty() {
        mutations = mutations.saturating_add(apply_root_removal_batch(
            graph,
            volume_id,
            &checkpoint,
            &mut pending,
            rebuildable_initial_ingest,
        )?);
    }
    Ok(mutations)
}

#[cfg(windows)]
fn apply_root_removal_batch(
    graph: &mut localsearch_filesystem_graph::FilesystemGraph,
    volume_id: localsearch_core::VolumeId,
    checkpoint: &localsearch_platform_core::ProviderCheckpoint,
    pending: &mut Vec<localsearch_filesystem_graph::GraphMutation>,
    rebuildable_initial_ingest: bool,
) -> Result<u64, localsearch_filesystem_graph::GraphError> {
    let mutations = std::mem::take(pending);
    apply_folder_graph_batch(
        graph,
        &localsearch_filesystem_graph::GraphMutationBatch {
            volume_id,
            checkpoint: checkpoint.clone(),
            mutations,
        },
        rebuildable_initial_ingest,
    )
}

#[cfg(windows)]
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "scoped reconciliation keeps the crawl sink and its observed identity sets in one borrow scope"
)]
fn sync_scoped_root(
    graph: &mut localsearch_filesystem_graph::FilesystemGraph,
    provider: &localsearch_windows_fs::WindowsFilesystemProvider,
    root: &std::path::Path,
    scan_options: &localsearch_windows_fs::ScopedScanOptions,
    scan_batch_size: usize,
    graph_path: &std::path::Path,
    max_graph_bytes: u64,
    rebuildable_initial_ingest: bool,
) -> Result<ScopedRootSyncSummary, Box<dyn std::error::Error>> {
    use std::collections::BTreeSet;

    use localsearch_core::{FileKey, FileLinkId, FilesystemEvent};
    use localsearch_filesystem_graph::{GraphMutation, GraphMutationBatch};
    use localsearch_platform_core::{PlatformError, PlatformErrorKind, ProviderCheckpoint};

    let prepared = provider.prepare_scan_root(root)?;
    let descriptor = prepared.volume().clone();
    let volume_id = descriptor.volume_id;
    let incomplete = ProviderCheckpoint {
        provider_id: "windows-scoped-folder-incomplete".to_owned(),
        format_version: 1,
        volume_id,
        opaque: Vec::new(),
    };
    let mut pending = Vec::with_capacity(scan_batch_size);
    let mut observed_links = BTreeSet::<FileLinkId>::new();
    let mut observed_objects = BTreeSet::<FileKey>::new();
    let mut graph_mutations = 0_u64;
    let mut observed_events = 0_u64;
    let mut next_progress_event = 250_000_u64;
    let scan_result =
        provider.scan_prepared_root_with_options(&prepared, scan_options, &mut |event| {
            observed_events = observed_events.saturating_add(1);
            match &event {
                FilesystemEvent::LinkObserved { link } => {
                    observed_links.insert(link.file_link_id);
                }
                FilesystemEvent::ObjectObserved { object } => {
                    observed_objects.insert(object.object_key);
                }
                _ => {}
            }
            pending.push(GraphMutation::from(event));
            if pending.len() >= scan_batch_size {
                graph_mutations = graph_mutations.saturating_add(
                    apply_scoped_batch(
                        graph,
                        &descriptor,
                        &incomplete,
                        &mut pending,
                        rebuildable_initial_ingest,
                    )
                    .map_err(|_| {
                            PlatformError::new(
                                PlatformErrorKind::Internal,
                                "persist_folder_scan",
                                "durable graph rejected a bounded scan batch",
                            )
                        })?,
                );
                if graph_storage_pressure_bytes(graph_path, graph).map_err(|_| {
                    PlatformError::new(
                        PlatformErrorKind::Internal,
                        "measure_graph_pressure",
                        "could not measure durable graph storage pressure",
                    )
                })? >= graph_stop_threshold(max_graph_bytes)
                {
                    return Err(PlatformError::new(
                        PlatformErrorKind::ResourceExhausted,
                        "graph_size_limit",
                        "durable graph reached its configured byte budget",
                    ));
                }
            }
            if observed_events >= next_progress_event {
                eprintln!("folder-sync observed_events={observed_events} graph_mutations={graph_mutations}");
                next_progress_event = next_progress_event.saturating_add(250_000);
            }
            Ok(())
        });
    let scoped = match scan_result {
        Ok(scoped) => scoped,
        Err(error)
            if error.kind == PlatformErrorKind::ResourceExhausted
                && error.operation == "graph_size_limit" =>
        {
            return Ok(ScopedRootSyncSummary {
                observed_events,
                graph_mutations,
                complete: false,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if !pending.is_empty() {
        graph_mutations = graph_mutations.saturating_add(apply_scoped_batch(
            graph,
            &descriptor,
            &incomplete,
            &mut pending,
            rebuildable_initial_ingest,
        )?);
    }

    let current = graph.desired_catalog_identities(volume_id)?;
    for (file_link_id, object_key) in &current {
        if !observed_links.contains(file_link_id) {
            pending.push(GraphMutation::RemoveLink {
                file_link_id: *file_link_id,
                object_key: *object_key,
            });
            if pending.len() >= scan_batch_size {
                graph_mutations = graph_mutations.saturating_add(apply_scoped_batch(
                    graph,
                    &descriptor,
                    &incomplete,
                    &mut pending,
                    rebuildable_initial_ingest,
                )?);
            }
        }
    }
    let missing_objects = current
        .iter()
        .map(|(_, object_key)| *object_key)
        .filter(|object| !observed_objects.contains(object))
        .collect::<BTreeSet<_>>();
    for object_key in missing_objects {
        pending.push(GraphMutation::TombstoneObject { object_key });
        if pending.len() >= scan_batch_size {
            graph_mutations = graph_mutations.saturating_add(apply_scoped_batch(
                graph,
                &descriptor,
                &incomplete,
                &mut pending,
                rebuildable_initial_ingest,
            )?);
        }
    }
    if !pending.is_empty() {
        graph_mutations = graph_mutations.saturating_add(apply_scoped_batch(
            graph,
            &descriptor,
            &incomplete,
            &mut pending,
            rebuildable_initial_ingest,
        )?);
    }
    let final_batch = GraphMutationBatch {
        volume_id,
        checkpoint: scoped.scan.checkpoint,
        mutations: vec![GraphMutation::UpsertVolume { descriptor }],
    };
    graph_mutations = graph_mutations.saturating_add(apply_folder_graph_batch(
        graph,
        &final_batch,
        rebuildable_initial_ingest,
    )?);
    Ok(ScopedRootSyncSummary {
        observed_events: scoped.scan.emitted_events,
        graph_mutations,
        complete: true,
    })
}

#[cfg(windows)]
struct ScopedRootSyncSummary {
    observed_events: u64,
    graph_mutations: u64,
    complete: bool,
}

#[cfg(windows)]
fn graph_stop_threshold(max_graph_bytes: u64) -> u64 {
    let headroom = (max_graph_bytes / 40).min(256 * 1024 * 1024);
    max_graph_bytes.saturating_sub(headroom).max(1)
}

#[cfg(windows)]
fn graph_storage_bytes(graph_path: &std::path::Path) -> std::io::Result<u64> {
    ["", "-wal", "-shm"]
        .into_iter()
        .try_fold(0_u64, |total, suffix| {
            let mut path = graph_path.as_os_str().to_os_string();
            path.push(suffix);
            match std::fs::metadata(std::path::PathBuf::from(path)) {
                Ok(metadata) => Ok(total.saturating_add(metadata.len())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(total),
                Err(error) => Err(error),
            }
        })
}

#[cfg(windows)]
fn graph_storage_pressure_bytes(
    graph_path: &std::path::Path,
    graph: &localsearch_filesystem_graph::FilesystemGraph,
) -> Result<u64, Box<dyn std::error::Error>> {
    let storage = graph.storage_stats()?;
    let sidecars =
        ["-wal", "-shm"]
            .into_iter()
            .try_fold(0_u64, |total, suffix| -> std::io::Result<u64> {
                let path = std::path::PathBuf::from(format!("{}{suffix}", graph_path.display()));
                let bytes = match std::fs::metadata(path) {
                    Ok(metadata) => metadata.len(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                    Err(error) => return Err(error),
                };
                Ok(total.saturating_add(bytes))
            })?;
    Ok(storage
        .allocated_bytes
        .saturating_sub(storage.reusable_bytes)
        .saturating_add(sidecars))
}

#[cfg(windows)]
fn apply_scoped_batch(
    graph: &mut localsearch_filesystem_graph::FilesystemGraph,
    descriptor: &localsearch_platform_core::VolumeDescriptor,
    checkpoint: &localsearch_platform_core::ProviderCheckpoint,
    pending: &mut Vec<localsearch_filesystem_graph::GraphMutation>,
    rebuildable_initial_ingest: bool,
) -> Result<u64, localsearch_filesystem_graph::GraphError> {
    use localsearch_filesystem_graph::{GraphMutation, GraphMutationBatch};

    let mut mutations = Vec::with_capacity(pending.len().saturating_add(1));
    mutations.push(GraphMutation::UpsertVolume {
        descriptor: descriptor.clone(),
    });
    mutations.append(pending);
    apply_folder_graph_batch(
        graph,
        &GraphMutationBatch {
            volume_id: descriptor.volume_id,
            checkpoint: checkpoint.clone(),
            mutations,
        },
        rebuildable_initial_ingest,
    )
}

#[cfg(windows)]
fn apply_folder_graph_batch(
    graph: &mut localsearch_filesystem_graph::FilesystemGraph,
    batch: &localsearch_filesystem_graph::GraphMutationBatch,
    rebuildable_initial_ingest: bool,
) -> Result<u64, localsearch_filesystem_graph::GraphError> {
    let summary = if rebuildable_initial_ingest {
        graph.apply_rebuildable_batch(batch)?
    } else {
        graph.apply_batch(batch)?
    };
    Ok(summary.mutations)
}

#[cfg(windows)]
#[derive(serde::Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "machine-readable summary preserves independent completion and capacity facts"
)]
struct FolderSyncSummary {
    workspace: String,
    roots: Vec<String>,
    graph: String,
    catalog: String,
    content_index: String,
    observed_events: u64,
    graph_mutations: u64,
    scan_complete: bool,
    scan_deferred_for_content_resume: bool,
    graph_limit_reached: bool,
    max_graph_bytes: u64,
    graph_bytes: u64,
    graph_pressure_bytes: u64,
    graph_reusable_bytes: u64,
    graph_stats: localsearch_filesystem_graph::GraphStats,
    graph_compaction: Option<GraphCompactionSummary>,
    graph_outbox_mode: &'static str,
    content_mode: &'static str,
    content_summary: serde_json::Value,
    content_complete: bool,
    max_content_index_bytes: u64,
    max_content_documents: u64,
    min_free_disk_bytes: u64,
    min_free_disk_percent: u8,
    excluded_directories: Vec<String>,
    content_sequence: u64,
    scan_batch_size: usize,
    manifest: String,
}

#[cfg(windows)]
#[derive(serde::Deserialize, serde::Serialize)]
struct ContentWorkspaceManifest {
    version: u32,
    roots: Vec<String>,
    max_file_bytes: u64,
    include_generated: bool,
    excluded_directories: Vec<String>,
    scan_batch_size: usize,
    #[serde(default = "default_max_graph_bytes")]
    max_graph_bytes: u64,
    #[serde(default = "default_max_content_index_bytes")]
    max_content_index_bytes: u64,
    #[serde(default = "default_max_content_documents")]
    max_content_documents: u64,
    #[serde(default = "default_min_free_disk_bytes")]
    min_free_disk_bytes: u64,
    #[serde(default = "default_min_free_disk_percent")]
    min_free_disk_percent: u8,
    #[serde(default = "default_content_batch_documents")]
    content_batch_documents: u32,
    #[serde(default = "default_content_maximum_batches")]
    content_maximum_batches: u32,
    #[serde(default)]
    scoped_roots: Vec<ScopedRootBinding>,
}

#[cfg(windows)]
#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct ScopedRootBinding {
    root: String,
    volume_id: localsearch_core::VolumeId,
}

#[cfg(windows)]
const fn default_max_graph_bytes() -> u64 {
    DEFAULT_MAX_GRAPH_BYTES
}

#[cfg(windows)]
const fn default_max_content_index_bytes() -> u64 {
    DEFAULT_MAX_CONTENT_INDEX_BYTES
}

#[cfg(windows)]
const fn default_max_content_documents() -> u64 {
    DEFAULT_MAX_CONTENT_DOCUMENTS
}

#[cfg(windows)]
const fn default_min_free_disk_bytes() -> u64 {
    DEFAULT_MIN_FREE_DISK_BYTES
}

#[cfg(windows)]
const fn default_min_free_disk_percent() -> u8 {
    DEFAULT_MIN_FREE_DISK_PERCENT
}

#[cfg(windows)]
const fn default_content_batch_documents() -> u32 {
    4_096
}

#[cfg(windows)]
const fn default_content_maximum_batches() -> u32 {
    64
}

#[cfg(not(windows))]
fn main() {
    eprintln!("LocalSearch content indexing is currently enabled on Windows builds");
    std::process::exit(2);
}
