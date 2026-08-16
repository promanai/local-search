use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use localsearch_content_index::{CONTENT_SCHEMA_ID, ContentIndex};
use localsearch_core::{
    Availability, CatalogDocument, CatalogIdentity, DocumentId, DocumentVersion, FileId128,
    FileKey, FileKind, FileLinkId, FileMetadata, VolumeId,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tantivy::{
    Index, IndexSettings, TantivyDocument,
    directory::MmapDirectory,
    schema::{STORED, STRING, Schema, TEXT},
};

const DEFAULT_DOCUMENTS: u64 = 500_000;
const DEFAULT_SAMPLES: usize = 100;
const WRITER_HEAP_BYTES: usize = 256 * 1024 * 1024;
const COMMIT_DOCUMENTS: u64 = 250_000;
const BENCHMARK_MARKER: &str = "LOCALSEARCH_CONTENT_BENCHMARK";
const BENCHMARK_MARKER_VALUE: &str = "CONTENT-PRODUCTION-GATE-001\n";

#[derive(Debug)]
struct Arguments {
    documents: u64,
    samples: usize,
    index: PathBuf,
    output: PathBuf,
    rebuild: bool,
}

#[derive(Clone, Serialize)]
struct QueryMeasurement {
    name: &'static str,
    query: &'static str,
    cold_ms: f64,
    warm_p50_ms: f64,
    warm_p95_ms: f64,
    warm_p99_ms: f64,
    hits: usize,
    warm_sla_pass: bool,
}

#[derive(Serialize)]
struct ProcessEvidence {
    peak_memory_bytes: u64,
    cpu_time_millis: u64,
    disk_read_bytes: u64,
    disk_written_bytes: u64,
}

#[derive(Serialize)]
struct BenchmarkReport {
    report_version: u32,
    gate: &'static str,
    documents: u64,
    index_bytes: u64,
    build_seconds: f64,
    query_samples: usize,
    cold_definition: &'static str,
    measurements: Vec<QueryMeasurement>,
    concurrent_projection_search_p95_ms: f64,
    content_warm_sla_pass: bool,
    process: ProcessEvidence,
    filename_mode_touched: bool,
}

#[allow(
    clippy::too_many_lines,
    reason = "the benchmark keeps measurement order and process sampling in one auditable flow"
)]
fn main() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    if arguments.documents == 0 || arguments.samples < 10 || arguments.samples > 10_000 {
        return Err("benchmark bounds are invalid".into());
    }
    if arguments.rebuild && arguments.index.exists() {
        let marker = arguments.index.join(BENCHMARK_MARKER);
        if !matches!(
            fs::read_to_string(&marker),
            Ok(value) if value == BENCHMARK_MARKER_VALUE
        ) {
            return Err(format!(
                "refusing to remove an index without the benchmark ownership marker: {}",
                arguments.index.display()
            )
            .into());
        }
        fs::remove_dir_all(&arguments.index)?;
    }
    let sampler = ProcessSampler::start()?;
    let build_started = Instant::now();
    if !arguments.index.exists() {
        build_index(&arguments.index, arguments.documents)?;
    }
    let build_seconds = build_started.elapsed().as_secs_f64();
    let reader = ContentIndex::open(&arguments.index)?;
    let count = u64::try_from(reader.document_count()?)?;
    if count < arguments.documents {
        return Err(format!(
            "index has {count} documents, expected at least {}",
            arguments.documents
        )
        .into());
    }

    let workload = [
        ("rare_word", "productionraretoken"),
        ("common_word", "commonword"),
        ("two_terms", "twoterm alpha"),
        ("phrase", "\"exact phrase\""),
        ("cyrillic", "кириллица"),
        ("latin", "latinword"),
        ("code_identifier", "code_identifier_00042"),
        ("very_common_token", "document"),
        ("worst_case_prefix", "comm"),
    ];
    let mut measurements = Vec::new();
    for (name, query) in workload {
        let cold_started = Instant::now();
        let cold_hits = ContentIndex::open(&arguments.index)?.search(query, 20)?;
        let cold_ms = cold_started.elapsed().as_secs_f64() * 1_000.0;
        if cold_hits.is_empty() {
            return Err(format!("benchmark query {name} returned no hits").into());
        }
        for _ in 0..5 {
            let _ = reader.search(query, 20)?;
        }
        let mut latencies = Vec::with_capacity(arguments.samples);
        for _ in 0..arguments.samples {
            let started = Instant::now();
            let hits = reader.search(query, 20)?;
            if hits.is_empty() {
                return Err(format!("warm benchmark query {name} returned no hits").into());
            }
            latencies.push(started.elapsed().as_secs_f64() * 1_000.0);
        }
        latencies.sort_by(f64::total_cmp);
        let p50 = percentile(&latencies, 50);
        let p95 = percentile(&latencies, 95);
        let p99 = percentile(&latencies, 99);
        measurements.push(QueryMeasurement {
            name,
            query,
            cold_ms,
            warm_p50_ms: p50,
            warm_p95_ms: p95,
            warm_p99_ms: p99,
            hits: cold_hits.len(),
            warm_sla_pass: p95 <= 150.0 && p99 <= 300.0,
        });
    }
    let concurrent_projection_search_p95_ms =
        concurrent_projection_search(&arguments.index, arguments.documents, arguments.samples)?;
    let process = sampler.finish()?;
    let index_bytes = directory_bytes(&arguments.index)?;
    let content_warm_sla_pass = measurements
        .iter()
        .all(|measurement| measurement.warm_sla_pass)
        && concurrent_projection_search_p95_ms <= 300.0;
    let report = BenchmarkReport {
        report_version: 1,
        gate: "CONTENT-PRODUCTION-GATE-001",
        documents: arguments.documents,
        index_bytes,
        build_seconds,
        query_samples: arguments.samples,
        cold_definition: "first query after opening a new reader; operating-system cache is not flushed",
        measurements,
        concurrent_projection_search_p95_ms,
        content_warm_sla_pass,
        process,
        filename_mode_touched: false,
    };
    if let Some(parent) = arguments.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&arguments.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !content_warm_sla_pass {
        return Err("content-search SLA failed".into());
    }
    Ok(())
}

fn build_index(path: &Path, documents: u64) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(path)?;
    fs::write(path.join(BENCHMARK_MARKER), BENCHMARK_MARKER_VALUE)?;
    fs::write(path.join("LOCALSEARCH_CONTENT_SCHEMA"), CONTENT_SCHEMA_ID)?;
    let mut builder = Schema::builder();
    let document_id = builder.add_text_field("document_id", STRING | STORED);
    let payload = builder.add_text_field("payload", STORED);
    let content = builder.add_text_field("content", TEXT);
    let content_hash = builder.add_text_field("content_hash", STRING | STORED);
    let schema = builder.build();
    let directory = MmapDirectory::open(path)?;
    let index = Index::create(directory, schema, IndexSettings::default())?;
    let mut writer = index.writer(WRITER_HEAP_BYTES)?;
    for ordinal in 0..documents {
        let text = synthetic_content(ordinal, documents);
        let catalog = synthetic_document(ordinal);
        let mut document = TantivyDocument::default();
        document.add_text(document_id, catalog.identity.document_id.to_string());
        document.add_text(payload, serde_json::to_string(&catalog)?);
        document.add_text(content, &text);
        document.add_text(
            content_hash,
            format!("{:x}", Sha256::digest(text.as_bytes())),
        );
        writer.add_document(document)?;
        if ordinal > 0 && ordinal.is_multiple_of(COMMIT_DOCUMENTS) {
            writer.commit()?;
            eprintln!("content-benchmark indexed_documents={ordinal}");
        }
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    Ok(())
}

fn synthetic_content(ordinal: u64, documents: u64) -> String {
    let mut content = format!(
        "document commonword latinword exact phrase code_identifier_{:05}",
        ordinal % 1_000
    );
    if ordinal.is_multiple_of(2) {
        content.push_str(" twoterm alpha");
    }
    if ordinal.is_multiple_of(100) {
        content.push_str(" кириллица проект");
    }
    if ordinal.saturating_add(1) == documents {
        content.push_str(" productionraretoken");
    }
    content
}

fn synthetic_document(ordinal: u64) -> CatalogDocument {
    let identity = u128::from(ordinal).saturating_add(1);
    CatalogDocument {
        identity: CatalogIdentity::new(
            FileKey::new(VolumeId::from_u128(1), FileId128::from_u128(identity)),
            FileLinkId::from_u128(identity),
            DocumentId::from_u128(identity),
        ),
        document_version: DocumentVersion(1),
        name: format!("document-{ordinal:010}.txt"),
        resolved_path: format!("C:\\controlled-content-benchmark\\document-{ordinal:010}.txt"),
        extension: Some("txt".to_owned()),
        metadata: FileMetadata {
            kind: FileKind::File,
            size: 128,
            created_at_unix_ms: Some(0),
            modified_at_unix_ms: Some(0),
            hidden: false,
            availability: Availability::Online,
        },
    }
}

fn concurrent_projection_search(
    path: &Path,
    documents: u64,
    samples: usize,
) -> Result<f64, Box<dyn Error>> {
    let running = Arc::new(AtomicBool::new(true));
    let writer_running = Arc::clone(&running);
    let writer_path = path.to_path_buf();
    let handle = thread::spawn(move || -> Result<(), String> {
        let directory = MmapDirectory::open(&writer_path).map_err(|error| error.to_string())?;
        let index = Index::open(directory).map_err(|error| error.to_string())?;
        let schema = index.schema();
        let document_id = schema
            .get_field("document_id")
            .map_err(|error| error.to_string())?;
        let payload = schema
            .get_field("payload")
            .map_err(|error| error.to_string())?;
        let content = schema
            .get_field("content")
            .map_err(|error| error.to_string())?;
        let content_hash = schema
            .get_field("content_hash")
            .map_err(|error| error.to_string())?;
        let mut writer = index
            .writer(32 * 1024 * 1024)
            .map_err(|error| error.to_string())?;
        for offset in 0..20_u64 {
            if !writer_running.load(Ordering::Acquire) {
                break;
            }
            let ordinal = documents.saturating_add(offset);
            let catalog = synthetic_document(ordinal);
            let text = format!("document commonword concurrentprojection {offset}");
            let mut document = TantivyDocument::default();
            document.add_text(document_id, catalog.identity.document_id.to_string());
            document.add_text(
                payload,
                serde_json::to_string(&catalog).map_err(|error| error.to_string())?,
            );
            document.add_text(content, &text);
            document.add_text(
                content_hash,
                format!("{:x}", Sha256::digest(text.as_bytes())),
            );
            writer
                .add_document(document)
                .map_err(|error| error.to_string())?;
            writer.commit().map_err(|error| error.to_string())?;
            thread::sleep(Duration::from_millis(5));
        }
        writer
            .wait_merging_threads()
            .map_err(|error| error.to_string())?;
        Ok(())
    });
    let reader = ContentIndex::open(path)?;
    let mut latencies = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let hits = reader.search("commonword", 20)?;
        if hits.is_empty() {
            return Err("concurrent query returned no hits".into());
        }
        latencies.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    running.store(false, Ordering::Release);
    handle
        .join()
        .map_err(|_| "concurrent projection thread panicked")?
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    latencies.sort_by(f64::total_cmp);
    Ok(percentile(&latencies, 95))
}

fn percentile(sorted: &[f64], percentile: usize) -> f64 {
    let index = sorted
        .len()
        .saturating_mul(percentile)
        .saturating_add(99)
        .saturating_div(100)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted.get(index).copied().unwrap_or(0.0)
}

fn directory_bytes(path: &Path) -> std::io::Result<u64> {
    fs::read_dir(path)?.try_fold(0_u64, |total, entry| {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let bytes = if metadata.is_dir() {
            directory_bytes(&entry.path())?
        } else {
            metadata.len()
        };
        Ok(total.saturating_add(bytes))
    })
}

struct ProcessSampler {
    stop: Arc<AtomicBool>,
    handle: thread::JoinHandle<ProcessEvidence>,
}

impl ProcessSampler {
    fn start() -> Result<Self, Box<dyn Error>> {
        let pid = sysinfo::get_current_pid()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let mut system = System::new();
            let mut peak = 0_u64;
            let mut cpu = 0_u64;
            let mut read = 0_u64;
            let mut written = 0_u64;
            while !thread_stop.load(Ordering::Acquire) {
                system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[pid]),
                    false,
                    ProcessRefreshKind::everything(),
                );
                if let Some(process) = system.process(pid) {
                    peak = peak.max(process.memory());
                    cpu = process.accumulated_cpu_time();
                    let disk = process.disk_usage();
                    read = disk.total_read_bytes;
                    written = disk.total_written_bytes;
                }
                thread::sleep(Duration::from_millis(20));
            }
            ProcessEvidence {
                peak_memory_bytes: peak,
                cpu_time_millis: cpu,
                disk_read_bytes: read,
                disk_written_bytes: written,
            }
        });
        Ok(Self { stop, handle })
    }

    fn finish(self) -> Result<ProcessEvidence, Box<dyn Error>> {
        self.stop.store(true, Ordering::Release);
        self.handle
            .join()
            .map_err(|_| "sampler thread panicked".into())
    }
}

fn parse_arguments() -> Result<Arguments, Box<dyn Error>> {
    let mut documents = DEFAULT_DOCUMENTS;
    let mut samples = DEFAULT_SAMPLES;
    let mut index = PathBuf::from("reports/benchmarks/content-production/index-500k");
    let mut output = PathBuf::from("reports/benchmarks/content-production/report-500k.json");
    let mut rebuild = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--documents" => documents = arguments.next().ok_or("missing documents")?.parse()?,
            "--samples" => samples = arguments.next().ok_or("missing samples")?.parse()?,
            "--index" => index = arguments.next().ok_or("missing index")?.into(),
            "--output" => output = arguments.next().ok_or("missing output")?.into(),
            "--rebuild" => rebuild = true,
            _ => return Err(format!("unknown benchmark argument: {argument}").into()),
        }
    }
    Ok(Arguments {
        documents,
        samples,
        index,
        output,
        rebuild,
    })
}
