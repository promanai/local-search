#![allow(
    clippy::cast_precision_loss,
    reason = "benchmark counters are bounded far below f64's exact integer range"
)]

use crate::{
    CandidatePolicy, CatalogDataset, CatalogRecord, ExperimentError, ExperimentResult,
    ExperimentalIndex, QueryMode, Strategy, normalize_search_text,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const REPORT_VERSION: u32 = 1;
const WORKLOAD_VERSION: u32 = 1;

/// One labelled substring query in the controlled recall workload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallCase {
    /// Stable case identifier.
    pub id: &'static str,
    /// Coverage dimension represented by this case.
    pub category: &'static str,
    /// Frequency class exercised by this case.
    pub frequency: &'static str,
    /// Original query; normalization is performed by the production experiment path.
    pub query: String,
}

/// CLI/programmatic controls for START-003-R.
#[derive(Clone, Debug)]
pub struct RecallBenchmarkOptions {
    /// Caller-provided unique invocation identifier.
    pub run_id: String,
    /// Shared synthetic records preceding controlled additions.
    pub records: u64,
    /// Shared deterministic generator seed.
    pub seed: u64,
    /// Measured repetitions per controlled query after warm-up.
    pub samples_per_case: usize,
    /// Tantivy writer heap budget.
    pub writer_heap_bytes: usize,
    /// Maximum documents passed to exact verification.
    pub candidate_limit: usize,
    /// Number of deliberate reordered-gram collision documents.
    pub pressure_false_candidates: usize,
    /// Base catalog index bytes at the same scale, or zero when unavailable.
    pub baseline_index_bytes: u64,
    /// Output directory for JSON/CSV/Markdown evidence.
    pub output_directory: PathBuf,
    /// Strategies to evaluate; empty means all experimental strategies.
    pub strategies: Vec<Strategy>,
    /// Total physical memory recorded for the host.
    pub memory_bytes: u64,
    /// Explicit storage description.
    pub storage: String,
    /// Explicit power/profile description.
    pub power: String,
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

#[derive(Clone)]
struct CaseResult {
    case: RecallCase,
    ground_truth: usize,
    returned: usize,
    candidate_count: usize,
    within_candidate_budget: bool,
    recall: f64,
    precision: f64,
    samples_ms: Vec<f64>,
}

/// Returns deterministic labelled records and cases covering the accepted
/// filename-shape and candidate-pressure matrix.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the literal controlled matrix stays together so coverage is reviewable"
)]
pub fn controlled_recall_workload(
    first_ordinal: u64,
    pressure_false_candidates: usize,
) -> (Vec<CatalogRecord>, Vec<RecallCase>) {
    let definitions = [
        (
            "beginning",
            "position_beginning",
            "ZetaStartMarker-document.txt",
            "zetastart",
        ),
        (
            "middle",
            "position_middle",
            "prefix-MidNeedle-suffix.txt",
            "midneedle",
        ),
        ("end", "position_end", "archive-FinalMarker", "finalmarker"),
        (
            "space",
            "separator_space",
            "budget Qzv Report 2026.txt",
            "qzv report",
        ),
        (
            "underscore",
            "separator_underscore",
            "under_QzvScore_Marker.txt",
            "qzvscore_mar",
        ),
        (
            "dash",
            "separator_dash",
            "dash-QzvSegment-Marker.txt",
            "qzvsegment-mar",
        ),
        (
            "dot",
            "separator_dot",
            "release.qzvnotes.final",
            "qzvnotes.fin",
        ),
        (
            "parentheses",
            "separator_parentheses",
            "draft (QzvApproved) copy.txt",
            "(qzvapproved)",
        ),
        ("digits", "digits", "invoice-QZV60814-final.txt", "qzv60814"),
        (
            "repeated",
            "repeated_sequence",
            "qzvabababab-target.txt",
            "ababab",
        ),
        (
            "latin",
            "script_latin",
            "RareQzvLatinNeedle.txt",
            "qzvlatin",
        ),
        (
            "cyrillic",
            "script_cyrillic",
            "Проект_КюзОтчёт_Финал.txt",
            "кюзотчёт",
        ),
        (
            "case",
            "case_normalization",
            "QzV-MiXeDCaSeMarker.txt",
            "qzv-mixedcase",
        ),
        (
            "nfc",
            "unicode_nfc_query",
            "Qzv-Cafe\u{301}-Resume\u{301}.txt",
            "qzv-café",
        ),
        (
            "nfd",
            "unicode_nfd_query",
            "Qzv-Café-Résumé.txt",
            "qzv-cafe\u{301}",
        ),
        (
            "three_char",
            "minimum_length",
            "prefixaqv7bsuffix.txt",
            "qv7",
        ),
    ];
    let mut records = Vec::with_capacity(definitions.len() + pressure_false_candidates + 2);
    let mut cases = Vec::with_capacity(definitions.len() + 2);
    for (offset, (id, category, name, query)) in definitions.into_iter().enumerate() {
        records.push(record(
            first_ordinal + u64::try_from(offset).unwrap_or(0),
            name.to_owned(),
        ));
        cases.push(RecallCase {
            id,
            category,
            frequency: "rare",
            query: query.to_owned(),
        });
    }

    let long_marker = "qzv-long-middle-marker";
    records.push(record(
        first_ordinal + u64::try_from(records.len()).unwrap_or(0),
        format!("{}{}{}.txt", "a".repeat(96), long_marker, "b".repeat(96)),
    ));
    cases.push(RecallCase {
        id: "long_name",
        category: "long_name",
        frequency: "rare",
        query: long_marker.to_owned(),
    });

    let common_query = "qzvcommon";
    for index in 0..250 {
        records.push(record(
            first_ordinal + u64::try_from(records.len()).unwrap_or(0),
            format!("common-{index:03}-{common_query}-document.txt"),
        ));
    }
    cases.push(RecallCase {
        id: "common_within_budget",
        category: "frequency_common",
        frequency: "common",
        query: common_query.to_owned(),
    });

    let pressure_query = "qxzmarker";
    let collision = "qxz|xzm|zma|mar|ark|rke|ker";
    for index in 0..pressure_false_candidates {
        records.push(record(
            first_ordinal + u64::try_from(records.len()).unwrap_or(0),
            format!("collision-{index:04}-{collision}.txt"),
        ));
    }
    records.push(record(
        first_ordinal + u64::try_from(records.len()).unwrap_or(0),
        format!("target-{pressure_query}-after-collisions.txt"),
    ));
    cases.push(RecallCase {
        id: "candidate_pressure",
        category: "candidate_limit_pressure",
        frequency: "worst_case",
        query: pressure_query.to_owned(),
    });
    (records, cases)
}

fn record(ordinal: u64, name: String) -> CatalogRecord {
    CatalogRecord {
        ordinal,
        path: format!("/controlled/{name}"),
        name,
        extension: "txt".to_owned(),
        size: 1,
        modified_at_unix_ms: 0,
    }
}

/// Runs START-003-R and writes one report triplet per strategy.
///
/// # Errors
///
/// Returns an error for invalid controls, indexing/query failures, provenance
/// collection, or artifact writes.
#[allow(
    clippy::too_many_lines,
    reason = "linear evidence orchestration is auditable"
)]
pub fn run_recall_benchmark(options: &RecallBenchmarkOptions) -> ExperimentResult<Vec<PathBuf>> {
    validate_options(options)?;
    let provenance = provenance()?;
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| ExperimentError::InvalidDocument("timestamp"))?;
    let dataset = CatalogDataset::new(options.records, options.seed);
    let (additions, cases) =
        controlled_recall_workload(options.records, options.pressure_false_candidates);
    let truth = ground_truth(&dataset, &additions, &cases);
    fs::create_dir_all(&options.output_directory)?;
    let strategies = if options.strategies.is_empty() {
        Strategy::ALL.to_vec()
    } else {
        options.strategies.clone()
    };
    let mut json_paths = Vec::with_capacity(strategies.len());

    for strategy in strategies {
        let index = ExperimentalIndex::build_augmented(
            &dataset,
            &additions,
            strategy,
            options.writer_heap_bytes,
        )?;
        let results = measure_cases(&index, &cases, &truth, options)?;
        let stem = format!(
            "start-003-r-{}-{}-{}",
            strategy.label(),
            options.records,
            options.run_id
        );
        let json_path = options.output_directory.join(format!("{stem}.json"));
        let csv_path = options.output_directory.join(format!("{stem}.csv"));
        let markdown_path = options.output_directory.join(format!("{stem}.md"));
        refuse_overwrite([&json_path, &csv_path, &markdown_path])?;
        write_csv(&csv_path, &results)?;
        write_markdown(
            &markdown_path,
            strategy,
            &results,
            index.build_metrics().index_bytes,
            options,
        )?;

        let measurements = measurements(
            strategy,
            &results,
            index.build_metrics().index_bytes,
            options,
        );
        let report = json!({
            "report_version": REPORT_VERSION,
            "run_id": options.run_id,
            "spike": "START-003-R",
            "timestamp_utc": timestamp,
            "commit_sha": provenance.0,
            "dirty_tree": provenance.1,
            "dataset": {
                "name": "synthetic-catalog-plus-controlled-recall",
                "version": WORKLOAD_VERSION,
                "seed": options.seed,
                "records": options.records + u64::try_from(additions.len()).map_err(|_| ExperimentError::NumericRange)?,
                "workload": "controlled-substring-recall-v1"
            },
            "environment": environment(options)?,
            "parameters": {
                "strategy": strategy.label(),
                "base_records": options.records,
                "controlled_records": additions.len(),
                "controlled_cases": cases.len(),
                "candidate_limit": options.candidate_limit,
                "pressure_false_candidates": options.pressure_false_candidates,
                "samples_per_case": options.samples_per_case,
                "writer_heap_bytes": options.writer_heap_bytes,
                "baseline_index_bytes": options.baseline_index_bytes,
                "query_contract": "normalized filename substring; minimum 3 characters"
            },
            "measurements": measurements,
            "artifacts": [
                Artifact { kind: "json", path: artifact_path(&json_path), sha256: None },
                artifact("csv", &csv_path)?,
                artifact("markdown", &markdown_path)?
            ],
            "notes": [
                "Ground truth is computed by scanning every normalized filename in the combined dataset.",
                "Recall acceptance applies only when the ground-truth cardinality fits the configured candidate budget.",
                "One/two-character queries are excluded from the substring contract.",
                "Exact normalized verification remains mandatory after positional candidate retrieval."
            ]
        });
        write_new(&json_path, &serde_json::to_vec_pretty(&report)?)?;
        json_paths.push(json_path);
    }
    Ok(json_paths)
}

fn validate_options(options: &RecallBenchmarkOptions) -> ExperimentResult<()> {
    if options.samples_per_case == 0
        || options.memory_bytes == 0
        || options.storage.is_empty()
        || options.power.is_empty()
        || options.run_id.is_empty()
        || !options
            .run_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ExperimentError::InvalidDocument("recall benchmark options"));
    }
    CandidatePolicy {
        candidate_limit: options.candidate_limit,
    }
    .plan("abc", QueryMode::SubstringOnly)?;
    Ok(())
}

fn ground_truth(
    dataset: &CatalogDataset,
    additions: &[CatalogRecord],
    cases: &[RecallCase],
) -> Vec<BTreeSet<u64>> {
    let queries: Vec<_> = cases
        .iter()
        .map(|case| normalize_search_text(&case.query))
        .collect();
    let mut truth = vec![BTreeSet::new(); cases.len()];
    for record in dataset.records().chain(additions.iter().cloned()) {
        let name = normalize_search_text(&record.name);
        for (position, query) in queries.iter().enumerate() {
            if name.contains(query) {
                truth[position].insert(record.ordinal);
            }
        }
    }
    truth
}

fn measure_cases(
    index: &ExperimentalIndex,
    cases: &[RecallCase],
    truth: &[BTreeSet<u64>],
    options: &RecallBenchmarkOptions,
) -> ExperimentResult<Vec<CaseResult>> {
    let policy = CandidatePolicy {
        candidate_limit: options.candidate_limit,
    };
    let mut results = Vec::with_capacity(cases.len());
    for (case, expected) in cases.iter().zip(truth) {
        for _ in 0..3 {
            let _ = index.search(&case.query, QueryMode::SubstringOnly, policy)?;
        }
        let mut samples_ms = Vec::with_capacity(options.samples_per_case);
        let mut returned = 0;
        let mut candidate_count = 0;
        let mut recall = 1.0_f64;
        let mut precision = 1.0_f64;
        let normalized_query = normalize_search_text(&case.query);
        for _ in 0..options.samples_per_case {
            let outcome = index.search(&case.query, QueryMode::SubstringOnly, policy)?;
            samples_ms.push(outcome.metrics.end_to_end_ns as f64 / 1_000_000.0);
            candidate_count = candidate_count.max(outcome.metrics.candidate_count);
            returned = returned.max(outcome.hits.len());
            let ordinals: BTreeSet<_> = outcome.hits.iter().map(|hit| hit.ordinal).collect();
            let found = expected.intersection(&ordinals).count();
            let sample_recall = if expected.is_empty() {
                1.0
            } else {
                found as f64 / expected.len() as f64
            };
            let verified_names = outcome
                .hits
                .iter()
                .filter(|hit| normalize_search_text(&hit.name).contains(&normalized_query))
                .count();
            let sample_precision = if outcome.hits.is_empty() {
                if expected.is_empty() { 1.0 } else { 0.0 }
            } else {
                verified_names as f64 / outcome.hits.len() as f64
            };
            recall = recall.min(sample_recall);
            precision = precision.min(sample_precision);
        }
        results.push(CaseResult {
            case: case.clone(),
            ground_truth: expected.len(),
            returned,
            candidate_count,
            within_candidate_budget: expected.len() <= options.candidate_limit,
            recall,
            precision,
            samples_ms,
        });
    }
    Ok(results)
}

#[allow(
    clippy::too_many_lines,
    reason = "all acceptance metrics are emitted together for schema review"
)]
fn measurements(
    strategy: Strategy,
    results: &[CaseResult],
    index_bytes: u64,
    options: &RecallBenchmarkOptions,
) -> Vec<Measurement> {
    let mut values = vec![measurement(
        "index_size",
        "bytes",
        index_bytes as f64,
        strategy,
        None,
    )];
    if options.baseline_index_bytes > 0 {
        values.push(measurement(
            "catalog_amplification",
            "ratio",
            index_bytes as f64 / options.baseline_index_bytes as f64,
            strategy,
            None,
        ));
    }
    let minimum_recall = results
        .iter()
        .filter(|result| result.within_candidate_budget)
        .map(|result| result.recall)
        .fold(1.0, f64::min);
    let minimum_precision = results
        .iter()
        .map(|result| result.precision)
        .fold(1.0, f64::min);
    let all_samples: Vec<_> = results
        .iter()
        .flat_map(|result| result.samples_ms.iter().copied())
        .collect();
    let mut p95 = measurement(
        "end_to_end_p95",
        "milliseconds",
        percentile(&all_samples, 95),
        strategy,
        None,
    );
    p95.samples = Some(all_samples.clone());
    values.extend([
        measurement(
            "minimum_candidate_recall",
            "ratio",
            minimum_recall,
            strategy,
            None,
        ),
        measurement(
            "minimum_final_precision",
            "ratio",
            minimum_precision,
            strategy,
            None,
        ),
        p95,
        measurement(
            "end_to_end_p99",
            "milliseconds",
            percentile(&all_samples, 99),
            strategy,
            None,
        ),
    ]);
    for result in results {
        let mut case_dimensions: Map<String, Value> = (&result.case).into();
        case_dimensions.insert(
            "within_candidate_budget".to_owned(),
            Value::Bool(result.within_candidate_budget),
        );
        let dimensions = Some(case_dimensions);
        values.extend([
            measurement(
                "candidate_recall",
                "ratio",
                result.recall,
                strategy,
                dimensions.clone(),
            ),
            measurement(
                "final_precision",
                "ratio",
                result.precision,
                strategy,
                dimensions.clone(),
            ),
            measurement(
                "ground_truth",
                "count",
                result.ground_truth as f64,
                strategy,
                dimensions.clone(),
            ),
            measurement(
                "returned",
                "count",
                result.returned as f64,
                strategy,
                dimensions.clone(),
            ),
            measurement(
                "candidate_count",
                "count",
                result.candidate_count as f64,
                strategy,
                dimensions.clone(),
            ),
            measurement(
                "case_end_to_end_p95",
                "milliseconds",
                percentile(&result.samples_ms, 95),
                strategy,
                dimensions.clone(),
            ),
            measurement(
                "case_end_to_end_p99",
                "milliseconds",
                percentile(&result.samples_ms, 99),
                strategy,
                dimensions,
            ),
        ]);
    }
    values
}

impl From<&RecallCase> for Map<String, Value> {
    fn from(case: &RecallCase) -> Self {
        let mut dimensions = Map::new();
        dimensions.insert("case".to_owned(), Value::String(case.id.to_owned()));
        dimensions.insert(
            "category".to_owned(),
            Value::String(case.category.to_owned()),
        );
        dimensions.insert(
            "frequency".to_owned(),
            Value::String(case.frequency.to_owned()),
        );
        dimensions.insert("query".to_owned(), Value::String(case.query.clone()));
        dimensions
    }
}

fn measurement(
    name: &str,
    unit: &str,
    value: f64,
    strategy: Strategy,
    dimensions: Option<Map<String, Value>>,
) -> Measurement {
    let mut dimensions = dimensions.unwrap_or_default();
    dimensions.insert(
        "strategy".to_owned(),
        Value::String(strategy.label().to_owned()),
    );
    Measurement {
        name: name.to_owned(),
        unit: unit.to_owned(),
        value,
        dimensions,
        samples: None,
    }
}

fn percentile(samples: &[f64], percentile: usize) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    let index = (ordered.len() - 1) * percentile / 100;
    ordered[index]
}

fn write_csv(path: &Path, results: &[CaseResult]) -> ExperimentResult<()> {
    let mut writer = BufWriter::new(create_new(path)?);
    writeln!(
        writer,
        "case,category,frequency,query,within_candidate_budget,ground_truth,returned,candidate_count,recall,precision,p95_ms,p99_ms"
    )?;
    for result in results {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{:.6},{:.6},{:.6},{:.6}",
            result.case.id,
            result.case.category,
            result.case.frequency,
            result.case.query.replace(',', "_"),
            result.within_candidate_budget,
            result.ground_truth,
            result.returned,
            result.candidate_count,
            result.recall,
            result.precision,
            percentile(&result.samples_ms, 95),
            percentile(&result.samples_ms, 99),
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn write_markdown(
    path: &Path,
    strategy: Strategy,
    results: &[CaseResult],
    index_bytes: u64,
    options: &RecallBenchmarkOptions,
) -> ExperimentResult<()> {
    let mut writer = BufWriter::new(create_new(path)?);
    let minimum_recall = results
        .iter()
        .filter(|result| result.within_candidate_budget)
        .map(|result| result.recall)
        .fold(1.0, f64::min);
    let minimum_precision = results
        .iter()
        .map(|result| result.precision)
        .fold(1.0, f64::min);
    let samples: Vec<_> = results
        .iter()
        .flat_map(|result| result.samples_ms.iter().copied())
        .collect();
    writeln!(writer, "# START-003-R {}", strategy.label())?;
    writeln!(writer)?;
    writeln!(writer, "- minimum candidate recall: `{minimum_recall:.6}`")?;
    writeln!(
        writer,
        "- minimum final precision: `{minimum_precision:.6}`"
    )?;
    writeln!(
        writer,
        "- end-to-end p95/p99: `{:.3}/{:.3} ms`",
        percentile(&samples, 95),
        percentile(&samples, 99)
    )?;
    writeln!(writer, "- index: `{index_bytes}` bytes")?;
    if options.baseline_index_bytes > 0 {
        writeln!(
            writer,
            "- catalog amplification: `{:.3}x`",
            index_bytes as f64 / options.baseline_index_bytes as f64
        )?;
    }
    writeln!(writer)?;
    writeln!(
        writer,
        "| Case | Budget | Ground truth | Returned | Candidates | Recall | Precision | p95 ms | p99 ms |"
    )?;
    writeln!(
        writer,
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    )?;
    for result in results {
        writeln!(
            writer,
            "| {} | {} | {} | {} | {} | {:.3} | {:.3} | {:.3} | {:.3} |",
            result.case.id,
            if result.within_candidate_budget {
                "supported"
            } else {
                "diagnostic"
            },
            result.ground_truth,
            result.returned,
            result.candidate_count,
            result.recall,
            result.precision,
            percentile(&result.samples_ms, 95),
            percentile(&result.samples_ms, 99)
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn environment(options: &RecallBenchmarkOptions) -> ExperimentResult<Value> {
    let rustc = Command::new("rustc").arg("--version").output()?;
    let rustc = String::from_utf8_lossy(&rustc.stdout).trim().to_owned();
    Ok(json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "rustc": rustc,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "logical_cpus": std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        "memory_bytes": options.memory_bytes,
        "storage": options.storage,
        "power": options.power
    }))
}

fn provenance() -> ExperimentResult<(String, bool)> {
    let sha = Command::new("git").args(["rev-parse", "HEAD"]).output()?;
    if !sha.status.success() {
        return Err(ExperimentError::InvalidDocument("git commit"));
    }
    let status = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude)reports/spikes/start-003-r/**",
        ])
        .output()?;
    Ok((
        String::from_utf8_lossy(&sha.stdout).trim().to_owned(),
        !status.stdout.is_empty(),
    ))
}

fn artifact(kind: &'static str, path: &Path) -> ExperimentResult<Artifact> {
    let digest = Sha256::digest(fs::read(path)?);
    Ok(Artifact {
        kind,
        path: artifact_path(path),
        sha256: Some(format!("{digest:x}")),
    })
}

fn artifact_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn refuse_overwrite<const N: usize>(paths: [&Path; N]) -> ExperimentResult<()> {
    if paths.into_iter().any(Path::exists) {
        return Err(ExperimentError::InvalidDocument("artifact already exists"));
    }
    Ok(())
}

fn create_new(path: &Path) -> ExperimentResult<File> {
    Ok(OpenOptions::new().write(true).create_new(true).open(path)?)
}

fn write_new(path: &Path, bytes: &[u8]) -> ExperimentResult<()> {
    let mut file = create_new(path)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workload_covers_required_shapes_and_pressure() {
        let (records, cases) = controlled_recall_workload(1_000, 350);
        for category in [
            "position_beginning",
            "position_middle",
            "position_end",
            "separator_space",
            "separator_underscore",
            "separator_dash",
            "separator_dot",
            "separator_parentheses",
            "digits",
            "long_name",
            "repeated_sequence",
            "script_latin",
            "script_cyrillic",
            "case_normalization",
            "unicode_nfc_query",
            "unicode_nfd_query",
            "frequency_common",
            "candidate_limit_pressure",
        ] {
            assert!(
                cases.iter().any(|case| case.category == category),
                "missing {category}"
            );
        }
        assert!(
            cases
                .iter()
                .all(|case| normalize_search_text(&case.query).chars().count() >= 3)
        );
        assert!(cases.iter().any(|case| case.frequency == "common"));
        assert!(cases.iter().any(|case| case.frequency == "rare"));
        assert!(records.len() > 350);
    }

    #[test]
    fn positional_trigram_survives_false_candidate_pressure() -> ExperimentResult<()> {
        let dataset = CatalogDataset::new(100, 20_260_814);
        let (additions, cases) = controlled_recall_workload(100, 25);
        let index = ExperimentalIndex::build_augmented(
            &dataset,
            &additions,
            Strategy::Trigram,
            20_000_000,
        )?;
        let pressure = cases
            .iter()
            .find(|case| case.id == "candidate_pressure")
            .expect("case exists");
        let outcome = index.search(
            &pressure.query,
            QueryMode::SubstringOnly,
            CandidatePolicy {
                candidate_limit: 10,
            },
        )?;
        assert_eq!(outcome.hits.len(), 1);
        assert!(
            normalize_search_text(&outcome.hits[0].name)
                .contains(&normalize_search_text(&pressure.query))
        );
        assert!(outcome.metrics.candidate_count <= 10);
        Ok(())
    }
}
