//! Experimental substring candidate retrieval and deterministic product ranking.
//!
//! This crate is intentionally not a production schema. It exists to generate
//! comparable START-003 evidence before `TANTIVY-SCHEMA-v1` is selected.

mod dataset;
mod error;
mod index;
mod normalize;
mod policy;
mod ranking;
mod recall;
mod report;

pub use dataset::{CatalogDataset, CatalogRecord, DatasetDescriptor};
pub use error::{ExperimentError, ExperimentResult};
pub use index::{CandidateMetrics, ExperimentalIndex, SearchOutcome, Strategy};
pub use normalize::{normalize_search_text, tokenize};
pub use policy::{CandidatePolicy, PolicyError, QueryMode, QueryPlan};
pub use ranking::{MatchClass, RankedHit, classify_match, rank_verified};
pub use recall::{
    RecallBenchmarkOptions, RecallCase, controlled_recall_workload, run_recall_benchmark,
};
pub use report::{BenchmarkOptions, run_benchmark};
