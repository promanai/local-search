#![allow(
    clippy::cast_precision_loss,
    reason = "reported counters are bounded by the 5M experiment and exactly representable"
)]

use crate::{
    CandidatePolicy, CatalogDataset, ExperimentError, ExperimentResult, ExperimentalIndex,
    QueryMode, Strategy,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const REPORT_VERSION: u32 = 1;

/// CLI/programmatic benchmark controls.
#[derive(Clone, Debug)]
pub struct BenchmarkOptions {
    /// Caller-provided unique invocation/trial identifier.
    pub run_id: String,
    /// Shared dataset ordinal count.
    pub records: u64,
    /// Shared deterministic seed.
    pub seed: u64,
    /// Repetitions for each workload cell after warm-up.
    pub samples_per_cell: usize,
    /// Writer heap budget passed to Tantivy.
    pub writer_heap_bytes: usize,
    /// Candidate cap applied to every query.
    pub candidate_limit: usize,
    /// Output directory for JSON/CSV/Markdown artifacts.
    pub output_directory: PathBuf,
    /// Optional subset; empty means all three strategies.
    pub strategies: Vec<Strategy>,
    /// Total physical memory recorded for the benchmark host.
    pub memory_bytes: u64,
    /// Explicit storage device/class description.
    pub storage: String,
    /// Explicit power source/profile description.
    pub power: String,
}

#[derive(Serialize)]
#[allow(
    clippy::struct_field_names,
    reason = "report_version is fixed by the external shared JSON schema"
)]
struct Report {
    report_version: u32,
    run_id: String,
    spike: &'static str,
    timestamp_utc: String,
    commit_sha: String,
    dirty_tree: bool,
    dataset: DatasetReport,
    environment: EnvironmentReport,
    parameters: Value,
    measurements: Vec<Measurement>,
    artifacts: Vec<Artifact>,
    notes: Vec<String>,
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
    target: Option<String>,
    profile: &'static str,
    logical_cpus: usize,
    memory_bytes: Option<u64>,
    storage: Option<String>,
    power: Option<String>,
}

#[derive(Clone, Serialize)]
struct Measurement {
    name: String,
    unit: String,
    value: f64,
    #[serde(skip_serializing_if = "Map::is_empty")]
    dimensions: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    samples: Option<Vec<f64>>,
}

#[derive(Serialize)]
struct Artifact {
    kind: &'static str,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
}

#[derive(Clone, Copy)]
struct WorkloadCell {
    length_band: &'static str,
    frequency: &'static str,
    query: &'static str,
}

const WORKLOAD: [WorkloadCell; 12] = [
    WorkloadCell {
        length_band: "1",
        frequency: "rare",
        query: "q",
    },
    WorkloadCell {
        length_band: "1",
        frequency: "common",
        query: "r",
    },
    WorkloadCell {
        length_band: "1",
        frequency: "worst_case",
        query: "0",
    },
    WorkloadCell {
        length_band: "2",
        frequency: "rare",
        query: "qx",
    },
    WorkloadCell {
        length_band: "2",
        frequency: "common",
        query: "re",
    },
    WorkloadCell {
        length_band: "2",
        frequency: "worst_case",
        query: "00",
    },
    WorkloadCell {
        length_band: "3-5",
        frequency: "rare",
        query: "qzx",
    },
    WorkloadCell {
        length_band: "3-5",
        frequency: "common",
        query: "rep",
    },
    WorkloadCell {
        length_band: "3-5",
        frequency: "worst_case",
        query: "000",
    },
    WorkloadCell {
        length_band: "6+",
        frequency: "rare",
        query: "qxnever",
    },
    WorkloadCell {
        length_band: "6+",
        frequency: "common",
        query: "report",
    },
    WorkloadCell {
        length_band: "6+",
        frequency: "worst_case",
        query: "000000",
    },
];

/// Runs the START-003 strategy comparison and writes JSON/CSV/Markdown evidence.
///
/// # Errors
///
/// Returns an error when indexing, querying, provenance collection, or artifact
/// serialization/writes fail.
#[allow(
    clippy::too_many_lines,
    reason = "the benchmark orchestration remains linear to keep phase ordering auditable"
)]
pub fn run_benchmark(options: &BenchmarkOptions) -> ExperimentResult<Vec<PathBuf>> {
    if options.samples_per_cell == 0 {
        return Err(ExperimentError::InvalidDocument("samples_per_cell"));
    }
    if options.run_id.is_empty()
        || !options
            .run_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ExperimentError::InvalidDocument("run_id"));
    }
    if options.memory_bytes == 0 || options.storage.is_empty() || options.power.is_empty() {
        return Err(ExperimentError::InvalidDocument("machine provenance"));
    }
    fs::create_dir_all(&options.output_directory)?;
    let dataset = CatalogDataset::new(options.records, options.seed);
    let strategies: Vec<_> = if options.strategies.is_empty() {
        Strategy::ALL.to_vec()
    } else {
        options.strategies.clone()
    };
    let provenance = provenance()?;
    let environment = environment(options)?;
    let mut reports = Vec::with_capacity(strategies.len());

    for strategy in strategies {
        let index = ExperimentalIndex::build(&dataset, strategy, options.writer_heap_bytes)?;
        let mut measurements = build_measurements(index.build_metrics(), strategy);
        for cell in WORKLOAD {
            for _ in 0..3 {
                let _ = index.search(
                    cell.query,
                    QueryMode::ProductSearch,
                    CandidatePolicy {
                        candidate_limit: options.candidate_limit,
                    },
                )?;
            }

            let mut retrieval = Vec::with_capacity(options.samples_per_cell);
            let mut verification = Vec::with_capacity(options.samples_per_cell);
            let mut ranker = Vec::with_capacity(options.samples_per_cell);
            let mut end_to_end = Vec::with_capacity(options.samples_per_cell);
            let mut candidate_count = Vec::with_capacity(options.samples_per_cell);
            let mut verified_count = Vec::with_capacity(options.samples_per_cell);
            let mut rejection_ratio = Vec::with_capacity(options.samples_per_cell);

            for _ in 0..options.samples_per_cell {
                let outcome = index.search(
                    cell.query,
                    QueryMode::ProductSearch,
                    CandidatePolicy {
                        candidate_limit: options.candidate_limit,
                    },
                )?;
                retrieval.push(outcome.metrics.retrieval_ns as f64 / 1_000_000.0);
                verification.push(outcome.metrics.verification_ns as f64 / 1_000_000.0);
                ranker.push(outcome.metrics.ranker_ns as f64 / 1_000_000.0);
                end_to_end.push(outcome.metrics.end_to_end_ns as f64 / 1_000_000.0);
                candidate_count.push(outcome.metrics.candidate_count as f64);
                verified_count.push(outcome.metrics.verified_count as f64);
                rejection_ratio.push(outcome.metrics.verification_rejection_ratio);
            }

            for (phase, samples) in [
                ("candidate_retrieval", retrieval),
                ("verification", verification),
                ("ranker", ranker),
                ("end_to_end", end_to_end),
            ] {
                add_distribution(&mut measurements, phase, "ms", samples, strategy, cell);
            }
            add_distribution(
                &mut measurements,
                "candidate_count",
                "documents",
                candidate_count,
                strategy,
                cell,
            );
            add_distribution(
                &mut measurements,
                "verified_count",
                "documents",
                verified_count,
                strategy,
                cell,
            );
            add_distribution(
                &mut measurements,
                "verification_rejection_ratio",
                "ratio",
                rejection_ratio,
                strategy,
                cell,
            );
        }

        // Required typed policy gate is measured separately from the safe short-query fallback.
        for query in ["q", "qx"] {
            let rejected = index
                .search(
                    query,
                    QueryMode::SubstringOnly,
                    CandidatePolicy {
                        candidate_limit: options.candidate_limit,
                    },
                )
                .is_err();
            measurements.push(Measurement {
                name: "short_substring_policy_rejected".to_owned(),
                unit: "boolean".to_owned(),
                value: f64::from(u8::from(rejected)),
                dimensions: dimensions(strategy, length_label(query), "policy_probe", None),
                samples: None,
            });
        }

        let stem = format!(
            "start-003-{}-{}-seed{}-{}",
            strategy.label(),
            options.records,
            options.seed,
            options.run_id
        );
        let csv_path = options.output_directory.join(format!("{stem}.csv"));
        let markdown_path = options.output_directory.join(format!("{stem}.md"));
        let json_path = options.output_directory.join(format!("{stem}.json"));
        refuse_overwrite([&csv_path, &markdown_path, &json_path])?;
        write_csv(&csv_path, &measurements)?;
        write_markdown(&markdown_path, strategy, options, &measurements)?;

        let descriptor = dataset.descriptor();
        let report = Report {
            report_version: REPORT_VERSION,
            run_id: stem,
            spike: "START-003",
            timestamp_utc: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|_| ExperimentError::InvalidDocument("timestamp"))?,
            commit_sha: provenance.0.clone(),
            dirty_tree: provenance.1,
            dataset: DatasetReport {
                name: descriptor.name,
                version: descriptor.version,
                seed: descriptor.seed,
                records: descriptor.records,
                workload: descriptor.workload,
            },
            environment: environment.clone(),
            parameters: json!({
                "strategy": strategy.label(),
                "candidate_limit": options.candidate_limit,
                "writer_heap_bytes": options.writer_heap_bytes,
                "samples_per_cell": options.samples_per_cell,
                "warmup_per_cell": 3,
                "measurement_state": "warm",
                "short_query_behavior": "product lexical fallback; substring-only typed rejection"
            }),
            measurements,
            artifacts: vec![
                Artifact {
                    kind: "json",
                    path: artifact_path(&json_path),
                    sha256: None,
                },
                artifact("csv", &csv_path)?,
                artifact("markdown", &markdown_path)?,
            ],
            notes: vec![
                "No strategy is selected by this report; ENGINEERING-GATE-001 review owns the decision.".to_owned(),
                "Dataset records come unchanged from localsearch-benchmark-data DATASET_VERSION=1.".to_owned(),
                "Rare/common/worst-case labels are deterministic workload classes, not estimates from private filenames.".to_owned(),
                "Peak RAM is a best-effort sampled process physical-memory high-water mark.".to_owned(),
                "Bounded fourgram intentionally uses lexical fallback for three-character queries; recall impact is a reviewed tradeoff.".to_owned(),
            ],
        };
        let file = create_new(&json_path)?;
        serde_json::to_writer_pretty(BufWriter::new(file), &report)?;
        reports.push(json_path);
    }

    Ok(reports)
}

impl Clone for EnvironmentReport {
    fn clone(&self) -> Self {
        Self {
            os: self.os,
            arch: self.arch,
            rustc: self.rustc.clone(),
            target: self.target.clone(),
            profile: self.profile,
            logical_cpus: self.logical_cpus,
            memory_bytes: self.memory_bytes,
            storage: self.storage.clone(),
            power: self.power.clone(),
        }
    }
}

fn build_measurements(build: &crate::index::BuildMetrics, strategy: Strategy) -> Vec<Measurement> {
    let dimensions = dimensions(strategy, "all", "all", None);
    [
        ("build_wall", "ms", build.wall_ns as f64 / 1_000_000.0),
        (
            "build_throughput",
            "documents_per_second",
            build.documents_per_second,
        ),
        ("index_bytes", "bytes", build.index_bytes as f64),
        (
            "bytes_per_document",
            "bytes_per_document",
            build.bytes_per_document,
        ),
        ("source_text_bytes", "bytes", build.source_text_bytes as f64),
        ("index_amplification", "ratio", build.index_amplification),
        ("peak_ram", "bytes", build.peak_ram_bytes as f64),
    ]
    .into_iter()
    .map(|(name, unit, value)| Measurement {
        name: name.to_owned(),
        unit: unit.to_owned(),
        value,
        dimensions: dimensions.clone(),
        samples: None,
    })
    .collect()
}

fn add_distribution(
    output: &mut Vec<Measurement>,
    name: &str,
    unit: &str,
    mut samples: Vec<f64>,
    strategy: Strategy,
    cell: WorkloadCell,
) {
    samples.sort_by(f64::total_cmp);
    let dimensions = dimensions(strategy, cell.length_band, cell.frequency, Some(cell.query));
    for (percentile_name, percentile) in [("p50", 50), ("p95", 95), ("p99", 99)] {
        output.push(Measurement {
            name: format!("{name}_{percentile_name}"),
            unit: unit.to_owned(),
            value: percentile_value(&samples, percentile),
            dimensions: dimensions.clone(),
            samples: None,
        });
    }
    output.push(Measurement {
        name: format!("{name}_max"),
        unit: unit.to_owned(),
        value: samples.last().copied().unwrap_or(0.0),
        dimensions,
        samples: Some(samples),
    });
}

fn percentile_value(samples: &[f64], percentile: usize) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let index = samples
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1)
        .min(samples.len().saturating_sub(1));
    samples[index]
}

fn dimensions(
    strategy: Strategy,
    length_band: &str,
    frequency: &str,
    query: Option<&str>,
) -> Map<String, Value> {
    let mut values = Map::new();
    values.insert("strategy".to_owned(), json!(strategy.label()));
    values.insert("query_length_band".to_owned(), json!(length_band));
    values.insert("frequency".to_owned(), json!(frequency));
    if let Some(query) = query {
        values.insert("query".to_owned(), json!(query));
    }
    values
}

fn write_csv(path: &Path, measurements: &[Measurement]) -> ExperimentResult<()> {
    let mut writer = BufWriter::new(create_new(path)?);
    writeln!(
        writer,
        "name,unit,value,strategy,query_length_band,frequency,query"
    )?;
    for measurement in measurements {
        writeln!(
            writer,
            "{},{},{},{},{},{},{}",
            csv_cell(&measurement.name),
            csv_cell(&measurement.unit),
            measurement.value,
            csv_dimension(&measurement.dimensions, "strategy"),
            csv_dimension(&measurement.dimensions, "query_length_band"),
            csv_dimension(&measurement.dimensions, "frequency"),
            csv_dimension(&measurement.dimensions, "query"),
        )?;
    }
    Ok(())
}

fn csv_dimension(dimensions: &Map<String, Value>, name: &str) -> String {
    dimensions
        .get(name)
        .and_then(Value::as_str)
        .map(csv_cell)
        .unwrap_or_default()
}

fn csv_cell(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn write_markdown(
    path: &Path,
    strategy: Strategy,
    options: &BenchmarkOptions,
    measurements: &[Measurement],
) -> ExperimentResult<()> {
    let mut writer = BufWriter::new(create_new(path)?);
    writeln!(writer, "# START-003 diagnostic: {}", strategy.label())?;
    writeln!(writer)?;
    writeln!(
        writer,
        "Dataset: {} records, seed {}.",
        options.records, options.seed
    )?;
    writeln!(writer)?;
    writeln!(
        writer,
        "This artifact records evidence only. It does not select a winning strategy."
    )?;
    writeln!(writer)?;
    writeln!(writer, "| Measurement | Value | Unit |")?;
    writeln!(writer, "|---|---:|---|")?;
    for measurement in measurements.iter().filter(|measurement| {
        measurement
            .dimensions
            .get("query_length_band")
            .and_then(Value::as_str)
            == Some("all")
    }) {
        writeln!(
            writer,
            "| {} | {:.3} | {} |",
            measurement.name, measurement.value, measurement.unit
        )?;
    }
    writeln!(writer)?;
    writeln!(
        writer,
        "See the sibling JSON for provenance, raw samples, and every workload cell; see CSV for tabular analysis."
    )?;
    Ok(())
}

fn provenance() -> ExperimentResult<(String, bool)> {
    let sha = command_output("git", &["rev-parse", "HEAD"])?;
    let status = command_output(
        "git",
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude)reports/spikes/start-003/**",
        ],
    )?;
    Ok((sha.trim().to_owned(), !status.trim().is_empty()))
}

fn environment(options: &BenchmarkOptions) -> ExperimentResult<EnvironmentReport> {
    let rustc = command_output("rustc", &["--version"])?;
    let verbose = command_output("rustc", &["-vV"])?;
    let target = verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned);
    let logical_cpus = std::thread::available_parallelism().map_or(1, usize::from);
    Ok(EnvironmentReport {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        rustc: rustc.trim().to_owned(),
        target,
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        logical_cpus,
        memory_bytes: Some(options.memory_bytes),
        storage: Some(options.storage.clone()),
        power: Some(options.power.clone()),
    })
}

fn command_output(program: &str, arguments: &[&str]) -> ExperimentResult<String> {
    let output = Command::new(program).args(arguments).output()?;
    if !output.status.success() {
        return Err(ExperimentError::InvalidDocument("provenance command"));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| ExperimentError::InvalidDocument("provenance encoding"))
}

fn artifact(kind: &'static str, path: &Path) -> ExperimentResult<Artifact> {
    let bytes = fs::read(path)?;
    let digest = Sha256::digest(bytes);
    Ok(Artifact {
        kind,
        path: artifact_path(path),
        sha256: Some(format!("{digest:x}")),
    })
}

fn artifact_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn refuse_overwrite<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> ExperimentResult<()> {
    if paths.into_iter().any(|path| path.exists()) {
        return Err(ExperimentError::InvalidDocument("artifact already exists"));
    }
    Ok(())
}

fn create_new(path: &Path) -> ExperimentResult<File> {
    Ok(OpenOptions::new().write(true).create_new(true).open(path)?)
}

fn length_label(query: &str) -> &'static str {
    match query.chars().count() {
        1 => "1",
        2 => "2",
        3..=5 => "3-5",
        _ => "6+",
    }
}
