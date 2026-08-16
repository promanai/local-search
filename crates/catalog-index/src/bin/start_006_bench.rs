use std::{
    env,
    error::Error,
    ffi::OsStr,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use localsearch_benchmark_data::{CatalogGenerator, DATASET_NAME, DATASET_VERSION};
use localsearch_catalog_index::{
    CATALOG_SCHEMA_ID, CatalogFingerprint, ProjectionWorker, ProjectionWorkerOptions, RecoveryKind,
};
use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, VolumeId,
};
use localsearch_filesystem_graph::{
    FilesystemGraph, GRAPH_SCHEMA_VERSION, GraphMutation, GraphMutationBatch,
};
use localsearch_platform_core::{ProviderCheckpoint, VolumeDescriptor};
use serde::Serialize;

const DEFAULT_RECORDS: u64 = 1_000_000;
const DEFAULT_SEED: u64 = 20_260_814;
const INGEST_BATCH_RECORDS: u64 = 5_000;
const INCREMENTAL_RECORDS: u64 = 10_000;
const START_005_BASELINE_BYTES: u64 = 411_889_664;
const START_005_BASELINE_SECONDS: f64 = 50.026;

#[derive(Debug)]
struct Arguments {
    records: u64,
    seed: u64,
    database: PathBuf,
    index_root: PathBuf,
    output_directory: PathBuf,
}

#[derive(Serialize)]
struct DatasetReport {
    name: &'static str,
    version: u32,
    seed: u64,
    records: u64,
    workload: &'static str,
}

#[derive(Serialize)]
struct EnvironmentReport {
    os: &'static str,
    arch: &'static str,
    rustc: String,
    profile: &'static str,
    logical_cpus: usize,
}

#[derive(Serialize)]
struct Measurement {
    name: &'static str,
    unit: &'static str,
    value: f64,
}

#[derive(Serialize)]
struct Artifact {
    kind: &'static str,
    path: String,
}

#[derive(Serialize)]
struct BenchmarkReport {
    report_version: u32,
    run_id: String,
    spike: &'static str,
    timestamp_utc: String,
    commit_sha: String,
    dirty_tree: bool,
    dataset: DatasetReport,
    environment: EnvironmentReport,
    parameters: serde_json::Value,
    measurements: Vec<Measurement>,
    artifacts: Vec<Artifact>,
    notes: Vec<String>,
}

#[allow(
    clippy::too_many_lines,
    reason = "the benchmark keeps one auditable end-to-end recovery protocol"
)]
fn main() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    validate_paths(&arguments)?;
    fs::create_dir_all(&arguments.output_directory)?;
    let commit_sha = command_output("git", ["rev-parse", "HEAD"])?;
    let dirty_tree = !command_output("git", ["status", "--porcelain"])?.is_empty();
    let timestamp = powershell_timestamp()?;
    let run_id = format!("start-006-{}-{}", arguments.records, unix_seconds()?);
    let volume_id = VolumeId::from_u128(6);
    let root_key = FileKey::new(volume_id, FileId128::from_u128(1));
    let generator = CatalogGenerator::new(arguments.seed);

    let ingest_started = Instant::now();
    let mut graph = FilesystemGraph::open(&arguments.database)?;
    graph.apply_rebuildable_batch(&GraphMutationBatch {
        volume_id,
        checkpoint: checkpoint(volume_id, 0),
        mutations: vec![
            GraphMutation::UpsertVolume {
                descriptor: volume_descriptor(volume_id),
            },
            GraphMutation::UpsertObject {
                object: object_snapshot(root_key, FileKind::Directory, 0, 0),
            },
            GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(1),
                    object_key: root_key,
                    parent_key: None,
                    name: "catalog".to_owned(),
                },
                traversal_boundary: false,
            },
        ],
    })?;
    let mut start = 0_u64;
    while start < arguments.records {
        let end = start
            .saturating_add(INGEST_BATCH_RECORDS)
            .min(arguments.records);
        let mut mutations = Vec::with_capacity(usize::try_from((end - start) * 2)?);
        for ordinal in start..end {
            let record = generator.record(ordinal);
            let identity = u128::from(ordinal) + 2;
            let key = FileKey::new(volume_id, FileId128::from_u128(identity));
            mutations.push(GraphMutation::UpsertObject {
                object: object_snapshot(
                    key,
                    FileKind::File,
                    record.size,
                    record.modified_at_unix_ms,
                ),
            });
            mutations.push(GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(identity),
                    object_key: key,
                    parent_key: Some(root_key),
                    name: format!("{ordinal:08}-{}", record.name),
                },
                traversal_boundary: false,
            });
        }
        graph.apply_rebuildable_batch(&GraphMutationBatch {
            volume_id,
            checkpoint: checkpoint(volume_id, end),
            mutations,
        })?;
        start = end;
    }
    let ingest_elapsed = ingest_started.elapsed();
    graph.prepare_size_measurement()?;
    let sqlite_bytes_before_projection = fs::metadata(&arguments.database)?.len();
    let outbox_sequence_after_ingest = graph.latest_outbox_sequence()?.0;

    let worker = ProjectionWorker::new(
        &arguments.index_root,
        ProjectionWorkerOptions {
            maximum_batch_mutations: 20_000,
            maximum_batches: 10_000,
            maximum_run_time: Duration::from_hours(1),
            writer_heap_bytes: 256 * 1_024 * 1_024,
            rebuild_page_size: 10_000,
        },
    );
    let projection_started = Instant::now();
    let initial_projection = worker.run(&graph)?;
    let projection_elapsed = projection_started.elapsed();
    if initial_projection.recovery != RecoveryKind::RebuiltGeneration
        || initial_projection.backlog_remaining
    {
        return Err("initial projection did not complete as one rebuild generation".into());
    }
    let index_bytes = directory_size(&arguments.index_root)?;

    let startup_started = Instant::now();
    let startup = worker.run(&graph)?;
    let startup_elapsed = startup_started.elapsed();
    if startup.recovery != RecoveryKind::ExistingGeneration || startup.backlog_remaining {
        return Err("steady-state startup did not open the active generation".into());
    }

    let incremental_count = arguments.records.min(INCREMENTAL_RECORDS);
    let incoming_started = Instant::now();
    let mut incremental_start = 0_u64;
    while incremental_start < incremental_count {
        let end = incremental_start
            .saturating_add(INGEST_BATCH_RECORDS)
            .min(incremental_count);
        let mut mutations = Vec::with_capacity(usize::try_from(end - incremental_start)?);
        for ordinal in incremental_start..end {
            let record = generator.record(ordinal);
            let identity = u128::from(ordinal) + 2;
            mutations.push(GraphMutation::UpsertObject {
                object: object_snapshot(
                    FileKey::new(volume_id, FileId128::from_u128(identity)),
                    FileKind::File,
                    record.size.saturating_add(1),
                    record.modified_at_unix_ms,
                ),
            });
        }
        graph.apply_batch(&GraphMutationBatch {
            volume_id,
            checkpoint: checkpoint(volume_id, arguments.records + end),
            mutations,
        })?;
        incremental_start = end;
    }
    let incoming_elapsed = incoming_started.elapsed();
    let backlog_started = Instant::now();
    let backlog = worker.run(&graph)?;
    let backlog_elapsed = backlog_started.elapsed();
    if backlog.backlog_remaining || backlog.applied_mutations != incremental_count {
        return Err("incremental backlog did not converge".into());
    }
    let pruned_rows = graph.prune_consumed_outbox()?;
    graph.prepare_size_measurement()?;
    let sqlite_bytes_after_prune = fs::metadata(&arguments.database)?.len();

    let active_before_loss = arguments
        .index_root
        .join(format!("generation-{:020}", backlog.index_generation));
    fs::remove_dir_all(&active_before_loss)?;
    let rebuild_started = Instant::now();
    let rebuilt = worker.run(&graph)?;
    let rebuild_elapsed = rebuild_started.elapsed();
    if rebuilt.recovery != RecoveryKind::RebuiltGeneration {
        return Err("lost index did not select full rebuild".into());
    }

    let desired_fingerprint = desired_fingerprint(&graph)?;
    let index_fingerprint = worker.active_index(&graph)?.reader()?.fingerprint()?;
    let fingerprints_match = desired_fingerprint == index_fingerprint;
    if !fingerprints_match {
        return Err(format!(
            "convergence fingerprint mismatch: desired={desired_fingerprint:?}, index={index_fingerprint:?}"
        )
        .into());
    }

    let ingest_seconds = ingest_elapsed.as_secs_f64();
    let projection_seconds = projection_elapsed.as_secs_f64();
    let backlog_seconds = backlog_elapsed.as_secs_f64();
    let incoming_rate = f64_from_u64(incremental_count)? / incoming_elapsed.as_secs_f64();
    let recovery_rate = f64_from_u64(incremental_count)? / backlog_seconds;
    let measurements = vec![
        measurement("graph_rebuildable_ingest", "seconds", ingest_seconds),
        measurement(
            "graph_rebuildable_ingest",
            "source_records_per_second",
            f64_from_u64(arguments.records)? / ingest_seconds,
        ),
        measurement(
            "rebuildable_ingest_vs_start005",
            "ratio",
            ingest_seconds / START_005_BASELINE_SECONDS,
        ),
        measurement(
            "sqlite_with_compact_desired_state",
            "bytes",
            f64_from_u64(sqlite_bytes_before_projection)?,
        ),
        measurement(
            "sqlite_growth_vs_start005",
            "bytes",
            f64_from_u64(sqlite_bytes_before_projection.saturating_sub(START_005_BASELINE_BYTES))?,
        ),
        measurement("initial_projection", "seconds", projection_seconds),
        measurement(
            "initial_projection",
            "documents_per_second",
            f64_from_u64(arguments.records + 1)? / projection_seconds,
        ),
        measurement("tantivy_index", "bytes", f64_from_u64(index_bytes)?),
        measurement(
            "steady_state_startup_recovery",
            "milliseconds",
            startup_elapsed.as_secs_f64() * 1_000.0,
        ),
        measurement(
            "incremental_incoming",
            "mutations_per_second",
            incoming_rate,
        ),
        measurement("backlog_recovery", "mutations_per_second", recovery_rate),
        measurement("recovery_headroom", "ratio", recovery_rate / incoming_rate),
        measurement(
            "tantivy_commit_average",
            "milliseconds",
            backlog_seconds * 1_000.0 / f64::from(backlog.committed_batches.max(1)),
        ),
        measurement(
            "full_lost_index_rebuild",
            "seconds",
            rebuild_elapsed.as_secs_f64(),
        ),
        measurement("outbox_rows_pruned", "rows", f64_from_u64(pruned_rows)?),
        measurement(
            "sqlite_after_outbox_prune",
            "bytes",
            f64_from_u64(sqlite_bytes_after_prune)?,
        ),
        measurement(
            "duplicate_searchable_documents",
            "documents",
            f64_from_u64(index_fingerprint.duplicate_documents())?,
        ),
        measurement("lost_documents", "documents", 0.0),
        measurement("stale_documents", "documents", 0.0),
    ];

    let stem = format!("start-006-{}-records", arguments.records);
    let json_path = arguments.output_directory.join(format!("{stem}.json"));
    let csv_path = arguments.output_directory.join(format!("{stem}.csv"));
    let markdown_path = arguments.output_directory.join(format!("{stem}.md"));
    let report = BenchmarkReport {
        report_version: 1,
        run_id,
        spike: "START-006",
        timestamp_utc: timestamp,
        commit_sha,
        dirty_tree,
        dataset: DatasetReport {
            name: DATASET_NAME,
            version: DATASET_VERSION,
            seed: arguments.seed,
            records: arguments.records,
            workload: "compact-rebuildable-projection-v2",
        },
        environment: EnvironmentReport {
            os: env::consts::OS,
            arch: env::consts::ARCH,
            rustc: command_output("rustc", ["--version"] )?,
            profile: "release",
            logical_cpus: std::thread::available_parallelism()?.get(),
        },
        parameters: serde_json::json!({
            "graph_schema_version": GRAPH_SCHEMA_VERSION,
            "catalog_schema_id": CATALOG_SCHEMA_ID,
            "ingest_batch_records": INGEST_BATCH_RECORDS,
            "incremental_records": incremental_count,
            "initial_outbox_high_watermark": outbox_sequence_after_ingest,
            "final_index_generation": rebuilt.index_generation,
            "fingerprints_match": fingerprints_match,
        }),
        measurements,
        artifacts: vec![
            artifact("json", &json_path),
            artifact("csv", &csv_path),
            artifact("markdown", &markdown_path),
        ],
        notes: vec![
            "Initial graph and provider checkpoint commit atomically with compact desired state; rebuildable outbox JSON is suppressed until the first consumer exists."
                .to_owned(),
            "Initial and lost-index builds enumerate desired SQLite state rather than trusting retained outbox history."
                .to_owned(),
            "Convergence compares document count, exact unique IDs, and two order-independent payload hashes."
                .to_owned(),
            "Temporary database and index generations are retained for explicit post-run cleanup."
                .to_owned(),
        ],
    };
    fs::write(&json_path, serde_json::to_vec_pretty(&report)?)?;
    write_csv(&csv_path, &report.measurements)?;
    write_markdown(&markdown_path, &report)?;
    println!("START-006 report: {}", json_path.display());
    println!(
        "compact graph {:.1}s; initial projection {:.1}s; lost-index rebuild {:.1}s; convergence PASS",
        ingest_seconds,
        projection_seconds,
        rebuild_elapsed.as_secs_f64()
    );
    Ok(())
}

fn validate_paths(arguments: &Arguments) -> Result<(), Box<dyn Error>> {
    if arguments.records == 0 {
        return Err("--records must be greater than zero".into());
    }
    if arguments.database.exists() {
        return Err(format!("database already exists: {}", arguments.database.display()).into());
    }
    if arguments.index_root.exists() {
        return Err(format!(
            "index root already exists: {}",
            arguments.index_root.display()
        )
        .into());
    }
    Ok(())
}

fn parse_arguments() -> Result<Arguments, Box<dyn Error>> {
    let mut records = DEFAULT_RECORDS;
    let mut seed = DEFAULT_SEED;
    let mut database = PathBuf::from(".lab/start-006.sqlite3");
    let mut index_root = PathBuf::from(".lab/start-006-index");
    let mut output_directory = PathBuf::from("reports/benchmarks/start-006");
    let mut arguments = env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        let value = arguments
            .next()
            .ok_or("every benchmark option requires a value")?;
        match argument.to_str() {
            Some("--records") => records = value.to_string_lossy().parse()?,
            Some("--seed") => seed = value.to_string_lossy().parse()?,
            Some("--database") => database = value.into(),
            Some("--index-root") => index_root = value.into(),
            Some("--output") => output_directory = value.into(),
            _ => return Err(format!("unknown option: {}", argument.to_string_lossy()).into()),
        }
    }
    Ok(Arguments {
        records,
        seed,
        database,
        index_root,
        output_directory,
    })
}

fn volume_descriptor(volume_id: VolumeId) -> VolumeDescriptor {
    VolumeDescriptor {
        volume_id,
        display_name: Some("START-006 synthetic volume".to_owned()),
        mount_points: vec!["synthetic-root".to_owned()],
        filesystem: Some("synthetic".to_owned()),
        removable: false,
        local: true,
    }
}

fn checkpoint(volume_id: VolumeId, sequence: u64) -> ProviderCheckpoint {
    ProviderCheckpoint {
        provider_id: "start-006-benchmark".to_owned(),
        format_version: 1,
        volume_id,
        opaque: sequence.to_be_bytes().to_vec(),
    }
}

fn object_snapshot(
    object_key: FileKey,
    kind: FileKind,
    size: u64,
    modified_at_unix_ms: i64,
) -> FileObjectSnapshot {
    FileObjectSnapshot {
        object_key,
        metadata: FileMetadata {
            kind,
            size,
            created_at_unix_ms: None,
            modified_at_unix_ms: Some(modified_at_unix_ms),
            hidden: false,
            availability: Availability::Online,
        },
    }
}

fn desired_fingerprint(graph: &FilesystemGraph) -> Result<CatalogFingerprint, Box<dyn Error>> {
    let mut fingerprint = CatalogFingerprint::default();
    let mut after = None;
    loop {
        let documents = graph.desired_catalog_page(after, 10_000)?;
        if documents.is_empty() {
            break;
        }
        for document in &documents {
            fingerprint.add_desired(document)?;
        }
        after = documents
            .last()
            .map(|document| document.identity.document_id);
        if documents.len() < 10_000 {
            break;
        }
    }
    Ok(fingerprint)
}

fn directory_size(path: &Path) -> Result<u64, Box<dyn Error>> {
    let mut total = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

const fn measurement(name: &'static str, unit: &'static str, value: f64) -> Measurement {
    Measurement { name, unit, value }
}

fn artifact(kind: &'static str, path: &Path) -> Artifact {
    Artifact {
        kind,
        path: path.to_string_lossy().replace('\\', "/"),
    }
}

fn f64_from_u64(value: u64) -> Result<f64, Box<dyn Error>> {
    value.to_string().parse().map_err(Into::into)
}

fn unix_seconds() -> Result<u64, Box<dyn Error>> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

fn command_output<I, S>(program: &str, arguments: I) -> Result<String, Box<dyn Error>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program).args(arguments).output()?;
    if !output.status.success() {
        return Err(format!("{program} failed with status {}", output.status).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn powershell_timestamp() -> Result<String, Box<dyn Error>> {
    command_output(
        "powershell.exe",
        [
            "-NoProfile",
            "-Command",
            "[DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ss.fffffffZ')",
        ],
    )
}

fn write_csv(path: &Path, measurements: &[Measurement]) -> Result<(), Box<dyn Error>> {
    let mut output = String::from("name,unit,value\n");
    for item in measurements {
        writeln!(output, "{},{},{:.6}", item.name, item.unit, item.value)?;
    }
    fs::write(path, output)?;
    Ok(())
}

fn write_markdown(path: &Path, report: &BenchmarkReport) -> Result<(), Box<dyn Error>> {
    let mut output = format!(
        "# START-006 Durable Projection benchmark\n\nDataset: {} records, seed {}.\n\n| Measurement | Unit | Value |\n|---|---:|---:|\n",
        report.dataset.records, report.dataset.seed
    );
    for item in &report.measurements {
        writeln!(
            output,
            "| `{}` | {} | {:.3} |",
            item.name, item.unit, item.value
        )?;
    }
    write!(
        output,
        "\nCommit: `{}`. Dirty before run: `{}`. Convergence: `PASS`.\n",
        report.commit_sha, report.dirty_tree
    )?;
    fs::write(path, output)?;
    Ok(())
}
