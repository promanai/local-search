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
const BATCH_RECORDS: u64 = 5_000;
const LATENCY_SAMPLES: usize = 100;
const WARM_RESOLVE_SAMPLES: usize = 1_000;
const COLD_RESOLVE_SAMPLES: usize = 30;

#[derive(Debug)]
struct Arguments {
    records: u64,
    seed: u64,
    database: PathBuf,
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
    reason = "the benchmark keeps one visible end-to-end measurement protocol"
)]
fn main() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    if arguments.records == 0 {
        return Err("--records must be greater than zero".into());
    }
    if arguments.database.exists() {
        return Err(format!(
            "benchmark database already exists: {}",
            arguments.database.display()
        )
        .into());
    }
    fs::create_dir_all(&arguments.output_directory)?;

    let commit_sha = git_output(["rev-parse", "HEAD"])?;
    let dirty_tree = !git_output(["status", "--porcelain"])?.is_empty();
    let timestamp = powershell_timestamp()?;
    let run_id = format!("start-005-{}-{}", arguments.records, unix_seconds()?);
    let volume_id = VolumeId::from_u128(5);
    let root_key = FileKey::new(volume_id, FileId128::from_u128(1));

    let started = Instant::now();
    let mut graph = FilesystemGraph::open(&arguments.database)?;
    graph.ingest_snapshot(
        volume_descriptor(volume_id),
        checkpoint(volume_id, 0),
        [
            localsearch_core::FilesystemEvent::ObjectObserved {
                object: object_snapshot(root_key, FileKind::Directory, 0, 0),
            },
            localsearch_core::FilesystemEvent::LinkObserved {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(1),
                    object_key: root_key,
                    parent_key: None,
                    name: "catalog".to_owned(),
                },
            },
        ],
    )?;

    let generator = CatalogGenerator::new(arguments.seed);
    let mut start = 0_u64;
    while start < arguments.records {
        let end = start.saturating_add(BATCH_RECORDS).min(arguments.records);
        let capacity = usize::try_from((end - start).saturating_mul(2))?;
        let mut mutations = Vec::with_capacity(capacity);
        for ordinal in start..end {
            let record = generator.record(ordinal);
            let identity = u128::from(ordinal) + 2;
            let object_key = FileKey::new(volume_id, FileId128::from_u128(identity));
            mutations.push(GraphMutation::UpsertObject {
                object: object_snapshot(
                    object_key,
                    FileKind::File,
                    record.size,
                    record.modified_at_unix_ms,
                ),
            });
            mutations.push(GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(identity),
                    object_key,
                    parent_key: Some(root_key),
                    name: format!("{:08}-{}", ordinal, record.name),
                },
                traversal_boundary: false,
            });
        }
        graph.apply_batch(&GraphMutationBatch {
            volume_id,
            checkpoint: checkpoint(volume_id, end),
            mutations,
        })?;
        start = end;
    }
    let ingest_elapsed = started.elapsed();
    let stats = graph.stats()?;
    if stats.live_objects != arguments.records + 1 || stats.links != arguments.records + 1 {
        return Err("graph counts do not match the generated dataset".into());
    }

    let target_identity = u128::from(arguments.records) + 1;
    let target_key = FileKey::new(volume_id, FileId128::from_u128(target_identity));
    let target_link = FileLinkId::from_u128(target_identity);
    let target_record = generator.record(arguments.records - 1);

    let mut single_apply = Vec::with_capacity(LATENCY_SAMPLES);
    for sample in 0..LATENCY_SAMPLES {
        let sample_u64 = u64::try_from(sample)?;
        let before = Instant::now();
        graph.apply_batch(&GraphMutationBatch {
            volume_id,
            checkpoint: checkpoint(volume_id, arguments.records + sample_u64 + 1),
            mutations: vec![GraphMutation::UpsertObject {
                object: object_snapshot(
                    target_key,
                    FileKind::File,
                    target_record.size + sample_u64,
                    target_record.modified_at_unix_ms,
                ),
            }],
        })?;
        single_apply.push(before.elapsed());
    }

    let mut rename_apply = Vec::with_capacity(LATENCY_SAMPLES);
    for sample in 0..LATENCY_SAMPLES {
        let sample_u64 = u64::try_from(sample)?;
        let before = Instant::now();
        graph.apply_batch(&GraphMutationBatch {
            volume_id,
            checkpoint: checkpoint(
                volume_id,
                arguments.records + u64::try_from(LATENCY_SAMPLES)? + sample_u64 + 1,
            ),
            mutations: vec![GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: target_link,
                    object_key: target_key,
                    parent_key: Some(root_key),
                    name: format!("renamed-{sample:03}.txt"),
                },
                traversal_boundary: false,
            }],
        })?;
        rename_apply.push(before.elapsed());
    }

    let mut warm_resolve = Vec::with_capacity(WARM_RESOLVE_SAMPLES);
    for _ in 0..WARM_RESOLVE_SAMPLES {
        let before = Instant::now();
        let resolved = graph.resolve_path(target_link, 32)?;
        if resolved.components.len() != 2 {
            return Err("resolved path has an unexpected depth".into());
        }
        warm_resolve.push(before.elapsed());
    }

    let directory_identity = u128::from(arguments.records) + 2;
    let directory_key = FileKey::new(volume_id, FileId128::from_u128(directory_identity));
    let directory_link = FileLinkId::from_u128(directory_identity);
    graph.apply_batch(&GraphMutationBatch {
        volume_id,
        checkpoint: checkpoint(volume_id, arguments.records + 10_000),
        mutations: vec![
            GraphMutation::UpsertObject {
                object: object_snapshot(directory_key, FileKind::Directory, 0, 0),
            },
            GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: directory_link,
                    object_key: directory_key,
                    parent_key: Some(root_key),
                    name: "directory-before".to_owned(),
                },
                traversal_boundary: false,
            },
        ],
    })?;
    let mut directory_rename = Vec::with_capacity(LATENCY_SAMPLES);
    for sample in 0..LATENCY_SAMPLES {
        let before = Instant::now();
        let summary = graph.apply_batch(&GraphMutationBatch {
            volume_id,
            checkpoint: checkpoint(
                volume_id,
                arguments.records + 10_001 + u64::try_from(sample)?,
            ),
            mutations: vec![GraphMutation::UpsertLink {
                link: FileLinkSnapshot {
                    file_link_id: directory_link,
                    object_key: directory_key,
                    parent_key: Some(root_key),
                    name: format!("directory-{sample:03}"),
                },
                traversal_boundary: false,
            }],
        })?;
        directory_rename.push(before.elapsed());
        if summary.refresh_jobs_enqueued != 1 {
            return Err("directory rename did not enqueue exactly one bounded refresh".into());
        }
        let job = graph
            .pending_refresh_jobs(1)?
            .into_iter()
            .next()
            .ok_or("directory rename refresh job is missing")?;
        if !graph.complete_refresh_job(job.job_id)? {
            return Err("directory rename refresh job could not be completed".into());
        }
    }
    graph.prepare_size_measurement()?;
    drop(graph);

    let database_bytes = fs::metadata(&arguments.database)?.len();
    let mut cold_resolve = Vec::with_capacity(COLD_RESOLVE_SAMPLES);
    for _ in 0..COLD_RESOLVE_SAMPLES {
        let before = Instant::now();
        let cold_graph = FilesystemGraph::open(&arguments.database)?;
        let _resolved = cold_graph.resolve_path(target_link, 32)?;
        cold_resolve.push(before.elapsed());
    }

    let ingest_seconds = ingest_elapsed.as_secs_f64();
    let measurements = vec![
        measurement("initial_ingest", "seconds", ingest_seconds),
        measurement(
            "initial_ingest",
            "objects_per_second",
            f64_from_u64(arguments.records)? / ingest_seconds,
        ),
        measurement("database_size", "bytes", f64_from_u64(database_bytes)?),
        measurement(
            "database_size",
            "bytes_per_record",
            f64_from_u64(database_bytes)? / f64_from_u64(arguments.records)?,
        ),
        measurement(
            "single_event_apply_p50",
            "milliseconds",
            percentile_ms(&single_apply, 50),
        ),
        measurement(
            "single_event_apply_p95",
            "milliseconds",
            percentile_ms(&single_apply, 95),
        ),
        measurement(
            "single_event_apply_p99",
            "milliseconds",
            percentile_ms(&single_apply, 99),
        ),
        measurement(
            "rename_move_apply_p95",
            "milliseconds",
            percentile_ms(&rename_apply, 95),
        ),
        measurement(
            "directory_rename_enqueue_p95",
            "milliseconds",
            percentile_ms(&directory_rename, 95),
        ),
        measurement(
            "path_resolve_warm_p95",
            "milliseconds",
            percentile_ms(&warm_resolve, 95),
        ),
        measurement(
            "path_resolve_warm_p99",
            "milliseconds",
            percentile_ms(&warm_resolve, 99),
        ),
        measurement(
            "path_resolve_cold_p95",
            "milliseconds",
            percentile_ms(&cold_resolve, 95),
        ),
        measurement(
            "path_resolve_cold_p99",
            "milliseconds",
            percentile_ms(&cold_resolve, 99),
        ),
    ];

    let stem = format!("start-005-{}-records", arguments.records);
    let json_path = arguments.output_directory.join(format!("{stem}.json"));
    let csv_path = arguments.output_directory.join(format!("{stem}.csv"));
    let markdown_path = arguments.output_directory.join(format!("{stem}.md"));
    let report = BenchmarkReport {
        report_version: 1,
        run_id,
        spike: "START-005",
        timestamp_utc: timestamp,
        commit_sha,
        dirty_tree,
        dataset: DatasetReport {
            name: DATASET_NAME,
            version: DATASET_VERSION,
            seed: arguments.seed,
            records: arguments.records,
            workload: "filesystem-graph-v1",
        },
        environment: EnvironmentReport {
            os: env::consts::OS,
            arch: env::consts::ARCH,
            rustc: command_output("rustc", ["--version"])?,
            profile: "release",
            logical_cpus: std::thread::available_parallelism()?.get(),
        },
        parameters: serde_json::json!({
            "schema_version": GRAPH_SCHEMA_VERSION,
            "batch_records": BATCH_RECORDS,
            "sqlite_journal": "wal",
            "sqlite_synchronous": "normal"
        }),
        measurements,
        artifacts: vec![
            artifact("json", &json_path),
            artifact("csv", &csv_path),
            artifact("markdown", &markdown_path),
        ],
        notes: vec![
            "ResolvedPath is derived from parent-object relationships; no full paths are stored."
                .to_owned(),
            "Cold path samples reopen SQLite but do not flush the operating-system page cache."
                .to_owned(),
            "The benchmark database is retained for explicit cleanup after evidence capture."
                .to_owned(),
        ],
    };
    fs::write(&json_path, serde_json::to_vec_pretty(&report)?)?;
    write_csv(&csv_path, &report.measurements)?;
    write_markdown(&markdown_path, &report, database_bytes)?;
    println!("START-005 report: {}", json_path.display());
    println!(
        "ingest: {:.3}s ({:.0} objects/s), database: {:.2} MiB",
        ingest_seconds,
        f64_from_u64(arguments.records)? / ingest_seconds,
        f64_from_u64(database_bytes)? / (1024.0 * 1024.0)
    );
    Ok(())
}

fn parse_arguments() -> Result<Arguments, Box<dyn Error>> {
    let mut records = DEFAULT_RECORDS;
    let mut seed = DEFAULT_SEED;
    let mut database = PathBuf::from(".lab/start-005-graph.sqlite3");
    let mut output_directory = PathBuf::from("reports/benchmarks/start-005");
    let mut arguments = env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        let value = arguments
            .next()
            .ok_or("every benchmark option requires a value")?;
        match argument.to_str() {
            Some("--records") => records = value.to_string_lossy().parse()?,
            Some("--seed") => seed = value.to_string_lossy().parse()?,
            Some("--database") => database = PathBuf::from(value),
            Some("--output") => output_directory = PathBuf::from(value),
            _ => {
                return Err(
                    format!("unknown benchmark option: {}", argument.to_string_lossy()).into(),
                );
            }
        }
    }
    Ok(Arguments {
        records,
        seed,
        database,
        output_directory,
    })
}

fn volume_descriptor(volume_id: VolumeId) -> VolumeDescriptor {
    VolumeDescriptor {
        volume_id,
        display_name: Some("START-005 synthetic volume".to_owned()),
        mount_points: vec!["synthetic-root".to_owned()],
        filesystem: Some("synthetic".to_owned()),
        removable: false,
        local: true,
    }
}

fn checkpoint(volume_id: VolumeId, sequence: u64) -> ProviderCheckpoint {
    ProviderCheckpoint {
        provider_id: "start-005-benchmark".to_owned(),
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

const fn measurement(name: &'static str, unit: &'static str, value: f64) -> Measurement {
    Measurement { name, unit, value }
}

fn artifact(kind: &'static str, path: &Path) -> Artifact {
    Artifact {
        kind,
        path: path.to_string_lossy().replace('\\', "/"),
    }
}

fn percentile_ms(samples: &[Duration], percentile: usize) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
    sorted[rank.saturating_sub(1)].as_secs_f64() * 1_000.0
}

fn f64_from_u64(value: u64) -> Result<f64, Box<dyn Error>> {
    value.to_string().parse().map_err(Into::into)
}

fn unix_seconds() -> Result<u64, Box<dyn Error>> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

fn git_output<const N: usize>(arguments: [&str; N]) -> Result<String, Box<dyn Error>> {
    command_output("git", arguments)
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

fn write_markdown(
    path: &Path,
    report: &BenchmarkReport,
    database_bytes: u64,
) -> Result<(), Box<dyn Error>> {
    let mut output = format!(
        "# START-005 Filesystem Graph benchmark\n\nDataset: {} records, seed {}, schema v{}.\n\n| Measurement | Unit | Value |\n|---|---:|---:|\n",
        report.dataset.records, report.dataset.seed, GRAPH_SCHEMA_VERSION
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
        "\nDatabase size: {:.2} MiB. Commit: `{}`. Dirty before run: `{}`.\n",
        f64_from_u64(database_bytes)? / (1024.0 * 1024.0),
        report.commit_sha,
        report.dirty_tree
    )?;
    fs::write(path, output)?;
    Ok(())
}
