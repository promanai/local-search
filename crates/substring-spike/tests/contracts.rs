use localsearch_benchmark_data::{DATASET_NAME, DATASET_VERSION};
#[cfg(not(target_arch = "aarch64"))]
use localsearch_substring_spike::{BenchmarkOptions, Strategy, run_benchmark};
use localsearch_substring_spike::{
    CandidatePolicy, CatalogDataset, CatalogRecord, MatchClass, PolicyError, QueryMode,
    normalize_search_text, rank_verified,
};
#[cfg(not(target_arch = "aarch64"))]
use localsearch_substring_spike::{RecallBenchmarkOptions, run_recall_benchmark};
#[cfg(not(target_arch = "aarch64"))]
use serde_json::Value;
#[cfg(not(target_arch = "aarch64"))]
use std::fs;

fn record(ordinal: u64, name: &str, path: &str) -> CatalogRecord {
    CatalogRecord {
        ordinal,
        name: name.to_owned(),
        path: path.to_owned(),
        extension: "txt".to_owned(),
        size: 1,
        modified_at_unix_ms: 0,
    }
}

#[test]
fn shared_dataset_identity_is_not_forked() {
    let dataset = CatalogDataset::new(1_000, 20_260_814);
    assert_eq!(dataset.descriptor().name, DATASET_NAME);
    assert_eq!(dataset.descriptor().version, DATASET_VERSION);
    assert_eq!(
        dataset.record(417),
        dataset.records().nth(417).expect("ordinal exists")
    );
}

#[test]
fn primary_order_and_ties_are_deterministic() {
    let candidates = vec![
        record(5, "z-alpha-q.txt", "/z/z-alpha-q.txt"),
        record(4, "zalphaq.txt", "/z/zalphaq.txt"),
        record(3, "x alpha q.txt", "/z/x alpha q.txt"),
        record(2, "alphabet.txt", "/z/alphabet.txt"),
        record(1, "alpha", "/z/alpha"),
        record(9, "other.txt", "/alpha/other.txt"),
    ];
    let first = rank_verified(candidates.clone(), "alpha");
    let second = rank_verified(candidates, "alpha");
    assert_eq!(first, second);
    assert_eq!(
        first.iter().map(|hit| hit.match_class).collect::<Vec<_>>(),
        vec![
            MatchClass::ExactName,
            MatchClass::PrefixName,
            MatchClass::TokenName,
            MatchClass::TokenName,
            MatchClass::SubstringName,
            MatchClass::Path,
        ]
    );
}

#[test]
fn verification_rejects_candidate_with_matching_grams_but_no_substring() {
    let query = normalize_search_text("abcd");
    let false_candidate = record(1, "abc-then-bcd.txt", "/z/abc-then-bcd.txt");
    assert!(rank_verified(vec![false_candidate], &query).is_empty());
}

#[test]
fn expensive_short_shape_is_a_typed_policy_error() {
    let error = CandidatePolicy::default()
        .plan("xy", QueryMode::SubstringOnly)
        .expect_err("forced short substring must fail");
    assert_eq!(error, PolicyError::ExpensiveShortSubstring { length: 2 });
}

#[test]
#[cfg(not(target_arch = "aarch64"))]
fn machine_report_conforms_to_shared_schema() -> Result<(), Box<dyn std::error::Error>> {
    let output = tempfile::tempdir()?;
    let options = BenchmarkOptions {
        run_id: "contract-test".to_owned(),
        records: 100,
        seed: 20_260_814,
        samples_per_cell: 2,
        writer_heap_bytes: 20_000_000,
        candidate_limit: 20,
        output_directory: output.path().to_path_buf(),
        strategies: vec![Strategy::Trigram],
        memory_bytes: 1,
        storage: "test storage".to_owned(),
        power: "test power".to_owned(),
    };
    let paths = run_benchmark(&options)?;
    let report: Value = serde_json::from_slice(&fs::read(&paths[0])?)?;
    let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../benchmarks/report.schema.json");
    let schema: Value = serde_json::from_slice(&fs::read(schema_path)?)?;
    let validator = jsonschema::validator_for(&schema)?;
    let validation = validator.validate(&report);
    assert!(validation.is_ok(), "schema error: {validation:?}");
    assert_eq!(report["artifacts"][0]["kind"], "json");
    assert_eq!(report["environment"]["memory_bytes"], 1);
    assert!(
        run_benchmark(&options).is_err(),
        "artifacts must not be overwritten"
    );
    Ok(())
}

#[test]
#[cfg(not(target_arch = "aarch64"))]
fn recall_report_conforms_to_shared_schema() -> Result<(), Box<dyn std::error::Error>> {
    let output = tempfile::tempdir()?;
    let options = RecallBenchmarkOptions {
        run_id: "recall-contract-test".to_owned(),
        records: 100,
        seed: 20_260_814,
        samples_per_case: 2,
        writer_heap_bytes: 20_000_000,
        candidate_limit: 20,
        pressure_false_candidates: 25,
        baseline_index_bytes: 1,
        output_directory: output.path().to_path_buf(),
        strategies: vec![Strategy::Trigram],
        memory_bytes: 1,
        storage: "test storage".to_owned(),
        power: "test power".to_owned(),
    };
    let paths = run_recall_benchmark(&options)?;
    let report: Value = serde_json::from_slice(&fs::read(&paths[0])?)?;
    let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../benchmarks/report.schema.json");
    let schema: Value = serde_json::from_slice(&fs::read(schema_path)?)?;
    let validator = jsonschema::validator_for(&schema)?;
    assert!(validator.validate(&report).is_ok());
    assert_eq!(report["spike"], "START-003-R");
    assert_eq!(report["parameters"]["candidate_limit"], 20);
    Ok(())
}
