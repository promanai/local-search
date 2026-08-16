use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use chrono::{SecondsFormat, Utc};
use localsearch_benchmark_data::{
    CatalogGenerator, DATASET_NAME, DATASET_VERSION, QueryKind, RETRIEVAL_WORKLOAD_VERSION,
    SyntheticQuery,
};
use localsearch_catalog_spike::{CatalogIndex, EXPERIMENTAL_SCHEMA_ID};
use serde::Serialize;
use serde_json::{Value, json};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

const REPORT_VERSION: u32 = 1;

#[derive(Debug)]
struct Arguments {
    records: Vec<u64>,
    seed: u64,
    queries_per_kind: usize,
    warm_repetitions: usize,
    top_k: usize,
    writer_heap_bytes: usize,
    index_root: PathBuf,
    output_root: PathBuf,
    storage: Option<String>,
    power: Option<String>,
}

#[derive(Serialize)]
struct Report {
    #[serde(rename = "report_version")]
    format_version: u32,
    run_id: String,
    spike: &'static str,
    timestamp_utc: String,
    commit_sha: String,
    dirty_tree: bool,
    dataset: Dataset,
    environment: Environment,
    parameters: BTreeMap<String, Value>,
    measurements: Vec<Measurement>,
    artifacts: Vec<Artifact>,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct Dataset {
    name: &'static str,
    version: u32,
    seed: u64,
    records: u64,
    workload: String,
}

#[derive(Clone, Serialize)]
struct Environment {
    os: String,
    arch: String,
    rustc: String,
    target: String,
    profile: &'static str,
    logical_cpus: usize,
    memory_bytes: Option<u64>,
    storage: Option<String>,
    power: Option<String>,
}

#[derive(Serialize)]
struct Measurement {
    name: String,
    unit: String,
    value: f64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    dimensions: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    samples: Option<Vec<f64>>,
}

#[derive(Serialize)]
struct Artifact {
    kind: &'static str,
    path: String,
}

struct LatencySeries {
    mode: &'static str,
    kind: QueryKind,
    samples_ms: Vec<f64>,
    errors: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments(env::args().skip(1))?;
    let commit_sha = command_output("git", &["rev-parse", "HEAD"])?;
    let dirty_tree = !command_output("git", &["status", "--porcelain"])?.is_empty();
    let environment = environment(&arguments)?;
    let invocation = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    fs::create_dir_all(&arguments.output_root)?;
    fs::create_dir_all(&arguments.index_root)?;

    for records in &arguments.records {
        run_scale(
            *records,
            &invocation,
            &arguments,
            &commit_sha,
            dirty_tree,
            &environment,
        )?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the runner keeps one scale's lifecycle and evidence assembly visibly contiguous"
)]
fn run_scale(
    records: u64,
    invocation: &str,
    arguments: &Arguments,
    commit_sha: &str,
    dirty_tree: bool,
    environment: &Environment,
) -> Result<(), Box<dyn Error>> {
    let run_id = format!("start002-{records}-{invocation}");
    let index_path = arguments.index_root.join(&run_id);
    let json_path = arguments.output_root.join(format!("{run_id}.json"));
    let csv_path = arguments.output_root.join(format!("{run_id}.csv"));
    let markdown_path = arguments.output_root.join(format!("{run_id}.md"));
    if index_path.exists() || json_path.exists() || csv_path.exists() || markdown_path.exists() {
        return Err(format!("refusing to overwrite an existing run: {run_id}").into());
    }

    println!(
        "START-002: building {records} records at {}",
        index_path.display()
    );
    let generator = CatalogGenerator::new(arguments.seed);
    let index = CatalogIndex::create(&index_path)?;
    let memory_sampler = MemorySampler::start()?;
    let build_started = Instant::now();
    let mut writer = index.writer(arguments.writer_heap_bytes)?;
    for record in generator.records(records) {
        writer.add(&record)?;
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    let build_duration = build_started.elapsed();
    let peak_working_set = memory_sampler.finish()?;
    let index_bytes = directory_bytes(&index_path)?;

    let queries = generator.retrieval_queries(records, arguments.queries_per_kind);
    let reader = index.reader()?;
    reader.reload()?;
    for query in &queries {
        let _ = reader.search(query, arguments.top_k)?;
    }
    let warm = measure_warm(
        &reader,
        &queries,
        arguments.warm_repetitions,
        arguments.top_k,
    );
    drop(reader);
    drop(index);
    let cold = measure_reader_cold(&index_path, &queries, arguments.top_k);

    let build_ms = build_duration.as_secs_f64() * 1_000.0;
    let docs_per_second = measured_number(records) / build_duration.as_secs_f64();
    let mut measurements = vec![
        scalar("build_time", "ms", build_ms),
        scalar("build_throughput", "documents/second", docs_per_second),
        scalar("index_size", "bytes", measured_number(index_bytes)),
        scalar(
            "index_bytes_per_document",
            "bytes/document",
            measured_number(index_bytes) / measured_number(records),
        ),
        scalar(
            "peak_working_set",
            "bytes",
            measured_number(peak_working_set),
        ),
    ];
    for series in warm.iter().chain(cold.iter()) {
        append_latency_measurements(&mut measurements, series);
    }

    let artifact_path = |path: &Path| path.to_string_lossy().replace('\\', "/");
    let mut parameters = BTreeMap::new();
    parameters.insert(
        "experimental_schema_id".to_owned(),
        json!(EXPERIMENTAL_SCHEMA_ID),
    );
    parameters.insert(
        "writer_heap_bytes".to_owned(),
        json!(arguments.writer_heap_bytes),
    );
    parameters.insert(
        "queries_per_kind".to_owned(),
        json!(arguments.queries_per_kind),
    );
    parameters.insert(
        "warm_repetitions".to_owned(),
        json!(arguments.warm_repetitions),
    );
    parameters.insert("top_k".to_owned(), json!(arguments.top_k));
    parameters.insert("cold_cache_flush".to_owned(), json!(false));
    parameters.insert(
        "cold_definition".to_owned(),
        json!("fresh Index + IndexReader per query; operating-system cache uncontrolled"),
    );
    let report = Report {
        format_version: REPORT_VERSION,
        run_id: run_id.clone(),
        spike: "START-002",
        timestamp_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        commit_sha: commit_sha.to_owned(),
        dirty_tree,
        dataset: Dataset {
            name: DATASET_NAME,
            version: DATASET_VERSION,
            seed: arguments.seed,
            records,
            workload: format!("catalog-retrieval-v{RETRIEVAL_WORKLOAD_VERSION}"),
        },
        environment: environment.clone(),
        parameters,
        measurements,
        artifacts: vec![
            Artifact { kind: "json", path: artifact_path(&json_path) },
            Artifact { kind: "csv", path: artifact_path(&csv_path) },
            Artifact { kind: "markdown", path: artifact_path(&markdown_path) },
        ],
        notes: vec![
            "Experimental schema only; this run does not select TANTIVY-SCHEMA-v1.".to_owned(),
            "Cold measurements recreate Tantivy readers but do not flush the Windows filesystem cache.".to_owned(),
            "Peak working set is sampled every 20 ms and may miss shorter transients.".to_owned(),
            "No substring, fuzzy matching, or product ranker is present in START-002.".to_owned(),
        ],
    };
    write_json(&json_path, &report)?;
    write_csv(&csv_path, &report)?;
    write_markdown(&markdown_path, &report)?;
    fs::remove_dir_all(&index_path)?;
    println!("START-002: wrote {}", json_path.display());
    Ok(())
}

fn measure_warm(
    reader: &localsearch_catalog_spike::CatalogReader,
    queries: &[SyntheticQuery],
    repetitions: usize,
    top_k: usize,
) -> Vec<LatencySeries> {
    let mut series = empty_series("warm");
    for _ in 0..repetitions {
        for query in queries {
            measure_one(&mut series, query, || reader.search(query, top_k));
        }
    }
    series
}

fn measure_reader_cold(
    path: &Path,
    queries: &[SyntheticQuery],
    top_k: usize,
) -> Vec<LatencySeries> {
    let mut series = empty_series("reader_cold");
    for query in queries {
        let started = Instant::now();
        let result = CatalogIndex::open(path)
            .and_then(|index| index.reader())
            .and_then(|reader| reader.search(query, top_k));
        let sample = started.elapsed().as_secs_f64() * 1_000.0;
        let target = series_for_kind(&mut series, query.kind);
        target.samples_ms.push(sample);
        match result {
            Ok(hits) if !hits.is_empty() => {}
            Ok(_) | Err(_) => target.errors += 1,
        }
    }
    series
}

fn measure_one<F>(series: &mut [LatencySeries], query: &SyntheticQuery, operation: F)
where
    F: FnOnce() -> localsearch_catalog_spike::CatalogResult<Vec<u64>>,
{
    let started = Instant::now();
    let result = operation();
    let sample = started.elapsed().as_secs_f64() * 1_000.0;
    let target = series_for_kind(series, query.kind);
    target.samples_ms.push(sample);
    match result {
        Ok(hits) if !hits.is_empty() => {}
        Ok(_) | Err(_) => target.errors += 1,
    }
}

fn empty_series(mode: &'static str) -> Vec<LatencySeries> {
    [QueryKind::Exact, QueryKind::Token, QueryKind::Prefix]
        .into_iter()
        .map(|kind| LatencySeries {
            mode,
            kind,
            samples_ms: Vec::new(),
            errors: 0,
        })
        .collect()
}

fn series_for_kind(series: &mut [LatencySeries], kind: QueryKind) -> &mut LatencySeries {
    let index = match kind {
        QueryKind::Exact => 0,
        QueryKind::Token => 1,
        QueryKind::Prefix => 2,
    };
    &mut series[index]
}

fn append_latency_measurements(measurements: &mut Vec<Measurement>, series: &LatencySeries) {
    let mut sorted = series.samples_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let dimensions = dimensions(series);
    for (statistic, percentile) in [("p50", 50), ("p95", 95), ("p99", 99), ("max", 100)] {
        measurements.push(Measurement {
            name: format!("query_latency_{statistic}"),
            unit: "ms".to_owned(),
            value: percentile_nearest_rank(&sorted, percentile),
            dimensions: dimensions.clone(),
            samples: if statistic == "p50" {
                Some(series.samples_ms.clone())
            } else {
                None
            },
        });
    }
    measurements.push(Measurement {
        name: "query_errors".to_owned(),
        unit: "count".to_owned(),
        value: measured_number(series.errors),
        dimensions,
        samples: None,
    });
}

fn dimensions(series: &LatencySeries) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("cache_state".to_owned(), json!(series.mode)),
        ("query_kind".to_owned(), json!(kind_name(series.kind))),
    ])
}

fn kind_name(kind: QueryKind) -> &'static str {
    match kind {
        QueryKind::Exact => "exact",
        QueryKind::Token => "token",
        QueryKind::Prefix => "prefix",
    }
}

fn percentile_nearest_rank(sorted: &[f64], percentile: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = percentile.saturating_mul(sorted.len()).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn scalar(name: &str, unit: &str, value: f64) -> Measurement {
    Measurement {
        name: name.to_owned(),
        unit: unit.to_owned(),
        value,
        dimensions: BTreeMap::new(),
        samples: None,
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the report schema requires JSON numbers; measured values remain far below 2^53"
)]
fn measured_number(value: u64) -> f64 {
    value as f64
}

fn write_json(path: &Path, report: &Report) -> Result<(), Box<dyn Error>> {
    let mut output = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut output, report)?;
    writeln!(output)?;
    Ok(())
}

fn write_csv(path: &Path, report: &Report) -> Result<(), Box<dyn Error>> {
    let mut output = BufWriter::new(File::create(path)?);
    writeln!(
        output,
        "name,unit,value,cache_state,query_kind,sample_index,sample_value"
    )?;
    for measurement in &report.measurements {
        let cache = dimension_text(measurement, "cache_state");
        let kind = dimension_text(measurement, "query_kind");
        writeln!(
            output,
            "{},{},{},{cache},{kind},,",
            measurement.name, measurement.unit, measurement.value
        )?;
        if let Some(samples) = &measurement.samples {
            for (index, sample) in samples.iter().enumerate() {
                writeln!(
                    output,
                    "{},{},,{cache},{kind},{index},{sample}",
                    measurement.name, measurement.unit
                )?;
            }
        }
    }
    Ok(())
}

fn dimension_text<'a>(measurement: &'a Measurement, key: &str) -> &'a str {
    measurement
        .dimensions
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn write_markdown(path: &Path, report: &Report) -> Result<(), Box<dyn Error>> {
    let mut output = BufWriter::new(File::create(path)?);
    writeln!(output, "# START-002: {} records\n", report.dataset.records)?;
    writeln!(output, "- Run: `{}`", report.run_id)?;
    writeln!(
        output,
        "- Commit: `{}` (dirty: `{}`)",
        report.commit_sha, report.dirty_tree
    )?;
    writeln!(
        output,
        "- Dataset: v{}, seed `{}`",
        report.dataset.version, report.dataset.seed
    )?;
    writeln!(
        output,
        "- Schema: `{EXPERIMENTAL_SCHEMA_ID}` (experimental)\n"
    )?;
    writeln!(output, "| Measurement | Value |")?;
    writeln!(output, "|---|---:|")?;
    for name in [
        "build_time",
        "build_throughput",
        "index_size",
        "index_bytes_per_document",
        "peak_working_set",
    ] {
        if let Some(measurement) = report
            .measurements
            .iter()
            .find(|measurement| measurement.name == name)
        {
            writeln!(
                output,
                "| {} | {:.3} {} |",
                measurement.name, measurement.value, measurement.unit
            )?;
        }
    }
    writeln!(
        output,
        "\n| Mode | Query | p50 ms | p95 ms | p99 ms | max ms | errors |"
    )?;
    writeln!(output, "|---|---|---:|---:|---:|---:|---:|")?;
    for mode in ["warm", "reader_cold"] {
        for kind in ["exact", "token", "prefix"] {
            let value = |name: &str| find_dimensioned(report, name, mode, kind);
            writeln!(
                output,
                "| {mode} | {kind} | {:.3} | {:.3} | {:.3} | {:.3} | {:.0} |",
                value("query_latency_p50"),
                value("query_latency_p95"),
                value("query_latency_p99"),
                value("query_latency_max"),
                value("query_errors")
            )?;
        }
    }
    writeln!(output, "\n## Limitations\n")?;
    for note in &report.notes {
        writeln!(output, "- {note}")?;
    }
    Ok(())
}

fn find_dimensioned(report: &Report, name: &str, mode: &str, kind: &str) -> f64 {
    report
        .measurements
        .iter()
        .find(|measurement| {
            measurement.name == name
                && dimension_text(measurement, "cache_state") == mode
                && dimension_text(measurement, "query_kind") == kind
        })
        .map_or(0.0, |measurement| measurement.value)
}

fn directory_bytes(path: &Path) -> Result<u64, Box<dyn Error>> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        total = total.saturating_add(if metadata.is_dir() {
            directory_bytes(&entry.path())?
        } else {
            metadata.len()
        });
    }
    Ok(total)
}

fn environment(arguments: &Arguments) -> Result<Environment, Box<dyn Error>> {
    let rustc = command_output("rustc", &["--version"])?;
    let verbose_rustc = command_output("rustc", &["-vV"])?;
    let target = verbose_rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap_or("unknown")
        .to_owned();
    let mut system = System::new_all();
    system.refresh_memory();
    Ok(Environment {
        os: format!(
            "{} {}",
            System::name().unwrap_or_else(|| env::consts::OS.to_owned()),
            System::os_version().unwrap_or_else(|| "unknown".to_owned())
        ),
        arch: env::consts::ARCH.to_owned(),
        rustc,
        target,
        profile: "release",
        logical_cpus: thread::available_parallelism()?.get(),
        memory_bytes: Some(system.total_memory()),
        storage: arguments.storage.clone(),
        power: arguments.power.clone(),
    })
}

struct MemorySampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    handle: thread::JoinHandle<()>,
}

impl MemorySampler {
    fn start() -> Result<Self, Box<dyn Error>> {
        let pid = sysinfo::get_current_pid()?;
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let thread_stop = Arc::clone(&stop);
        let thread_peak = Arc::clone(&peak);
        let handle = thread::spawn(move || {
            let mut system = System::new();
            while !thread_stop.load(Ordering::Relaxed) {
                system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[pid]),
                    false,
                    ProcessRefreshKind::nothing().with_memory(),
                );
                if let Some(process) = system.process(pid) {
                    thread_peak.fetch_max(process.memory(), Ordering::Relaxed);
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok(Self { stop, peak, handle })
    }

    fn finish(self) -> Result<u64, Box<dyn Error>> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().map_err(|_| "memory sampler panicked")?;
        Ok(self.peak.load(Ordering::Relaxed))
    }
}

fn command_output(program: &str, arguments: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new(program).args(arguments).output()?;
    if !output.status.success() {
        return Err(format!("{program} exited with {}", output.status).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Arguments, Box<dyn Error>> {
    let mut records = vec![100_000];
    let mut seed = 20_260_814;
    let mut queries_per_kind = 30;
    let mut warm_repetitions = 3;
    let mut top_k = 20;
    let mut writer_heap_bytes = 256 * 1_024 * 1_024;
    let mut index_root = PathBuf::from("target/start002-indexes");
    let mut output_root = PathBuf::from("reports/benchmarks");
    let mut storage = None;
    let mut power = None;
    let values: Vec<_> = arguments.collect();
    let mut index = 0;
    while index < values.len() {
        let flag = &values[index];
        let value = values
            .get(index + 1)
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--records" => records = value.split(',').map(str::parse).collect::<Result<_, _>>()?,
            "--seed" => seed = value.parse()?,
            "--queries-per-kind" => queries_per_kind = value.parse()?,
            "--warm-repetitions" => warm_repetitions = value.parse()?,
            "--top-k" => top_k = value.parse()?,
            "--writer-heap-mb" => {
                writer_heap_bytes = value.parse::<usize>()?.saturating_mul(1_024 * 1_024);
            }
            "--index-root" => index_root = PathBuf::from(value),
            "--output-root" => output_root = PathBuf::from(value),
            "--storage" => storage = Some(value.clone()),
            "--power" => power = Some(value.clone()),
            _ => return Err(format!("unknown argument: {flag}").into()),
        }
        index += 2;
    }
    if records.is_empty()
        || records.contains(&0)
        || queries_per_kind == 0
        || warm_repetitions == 0
        || top_k == 0
    {
        return Err("record counts and query parameters must be positive".into());
    }
    Ok(Arguments {
        records,
        seed,
        queries_per_kind,
        warm_repetitions,
        top_k,
        writer_heap_bytes,
        index_root,
        output_root,
        storage,
        power,
    })
}
