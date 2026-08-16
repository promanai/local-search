#![allow(
    clippy::cast_precision_loss,
    reason = "benchmark counters are bounded far below f64's exact integer range"
)]

use crate::{
    CandidatePolicy, CatalogDataset, CatalogRecord, ExperimentError, ExperimentResult, QueryMode,
    QueryPlan, RankedHit, normalize_search_text, rank_verified, tokenize,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, STORED, STRING, Schema, TEXT, TantivyDocument, Value,
};
use tantivy::{DocAddress, Index, IndexReader, ReloadPolicy, Term, doc};
use tempfile::TempDir;

/// Candidate strategy compared by START-003. No variant is a selected default.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// All overlapping filename trigrams.
    Trigram,
    /// Exact/token/prefix first, then a capped trigram fallback.
    TokenPrefixLimitedTrigram,
    /// Less aggressive four-gram index; shorter queries use lexical fallback.
    BoundedFourgram,
}

impl Strategy {
    /// Stable report label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Trigram => "trigram",
            Self::TokenPrefixLimitedTrigram => "token_prefix_limited_trigram",
            Self::BoundedFourgram => "bounded_fourgram",
        }
    }

    /// All strategies in the engineering comparison.
    pub const ALL: [Self; 3] = [
        Self::Trigram,
        Self::TokenPrefixLimitedTrigram,
        Self::BoundedFourgram,
    ];
}

/// Build evidence for one strategy and dataset size.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BuildMetrics {
    /// Dataset documents written.
    pub documents: u64,
    /// Build and commit wall time.
    pub wall_ns: u64,
    /// Observed throughput.
    pub documents_per_second: f64,
    /// Total Tantivy directory size after commit.
    pub index_bytes: u64,
    /// Index bytes divided by document count.
    pub bytes_per_document: f64,
    /// Raw UTF-8 bytes of generated name and path input.
    pub source_text_bytes: u64,
    /// Total index bytes divided by raw name/path bytes.
    pub index_amplification: f64,
    /// Best-effort process physical-memory high-water sample.
    pub peak_ram_bytes: u64,
}

/// Per-query two-stage timing and cardinality measurements.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CandidateMetrics {
    /// Candidate retrieval wall time.
    pub retrieval_ns: u64,
    /// Exact normalized verification wall time.
    pub verification_ns: u64,
    /// Deterministic ranker wall time.
    pub ranker_ns: u64,
    /// End-to-end plan/retrieve/verify/rank wall time.
    pub end_to_end_ns: u64,
    /// Documents returned by bounded Tantivy retrieval.
    pub candidate_count: usize,
    /// Candidates surviving exact normalized verification.
    pub verified_count: usize,
    /// Fraction rejected as retrieval false positives.
    pub verification_rejection_ratio: f64,
}

/// One completed experimental search.
#[derive(Clone, Debug)]
pub struct SearchOutcome {
    /// Deterministically ranked verified results.
    pub hits: Vec<RankedHit>,
    /// Phase timing and cardinality evidence.
    pub metrics: CandidateMetrics,
}

#[derive(Clone, Copy)]
struct Fields {
    ordinal: Field,
    name: Field,
    path: Field,
    exact: Field,
    token: Field,
    prefix: Field,
    gram: Field,
}

/// On-disk Tantivy experiment for one strategy.
pub struct ExperimentalIndex {
    _directory: TempDir,
    reader: IndexReader,
    fields: Fields,
    strategy: Strategy,
    build_metrics: BuildMetrics,
}

impl ExperimentalIndex {
    /// Builds and commits one strategy index from the shared START-002 dataset.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem, Tantivy, or numeric conversion failure.
    pub fn build(
        dataset: &CatalogDataset,
        strategy: Strategy,
        writer_heap_bytes: usize,
    ) -> ExperimentResult<Self> {
        Self::build_records(
            dataset.records(),
            dataset.descriptor().records,
            strategy,
            writer_heap_bytes,
        )
    }

    /// Builds the shared synthetic catalog plus deterministic controlled records.
    ///
    /// START-003-R uses this path to measure candidate recall against labelled
    /// ground truth without changing the shared generator or its ordinal range.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem, Tantivy, or numeric conversion failure.
    pub fn build_augmented(
        dataset: &CatalogDataset,
        additions: &[CatalogRecord],
        strategy: Strategy,
        writer_heap_bytes: usize,
    ) -> ExperimentResult<Self> {
        let documents = dataset
            .descriptor()
            .records
            .checked_add(u64::try_from(additions.len()).map_err(|_| ExperimentError::NumericRange)?)
            .ok_or(ExperimentError::NumericRange)?;
        Self::build_records(
            dataset.records().chain(additions.iter().cloned()),
            documents,
            strategy,
            writer_heap_bytes,
        )
    }

    fn build_records(
        records: impl Iterator<Item = CatalogRecord>,
        documents: u64,
        strategy: Strategy,
        writer_heap_bytes: usize,
    ) -> ExperimentResult<Self> {
        let started = Instant::now();
        let directory = tempfile::tempdir()?;
        let (schema, fields) = schema();
        let index = Index::create_in_dir(directory.path(), schema)?;
        let mut writer = index.writer(writer_heap_bytes)?;
        let mut peak_ram_bytes = current_physical_memory();
        let mut source_text_bytes = 0_u64;

        for (position, record) in records.enumerate() {
            source_text_bytes = source_text_bytes.saturating_add(
                u64::try_from(record.name.len().saturating_add(record.path.len()))
                    .map_err(|_| ExperimentError::NumericRange)?,
            );
            let normalized_name = normalize_search_text(&record.name);
            let token_text = tokenize(&normalized_name).join(" ");
            let prefix_text = prefixes(&normalized_name, 16).join(" ");
            let gram_width = match strategy {
                Strategy::Trigram | Strategy::TokenPrefixLimitedTrigram => 3,
                Strategy::BoundedFourgram => 4,
            };
            let gram_text = grams(&normalized_name, gram_width).join(" ");
            writer.add_document(doc!(
                fields.ordinal => record.ordinal,
                fields.name => record.name,
                fields.path => record.path,
                fields.exact => normalized_name,
                fields.token => token_text,
                fields.prefix => prefix_text,
                fields.gram => gram_text,
            ))?;
            if position.is_multiple_of(4_096) {
                peak_ram_bytes = peak_ram_bytes.max(current_physical_memory());
            }
        }
        writer.commit()?;
        writer.wait_merging_threads()?;
        peak_ram_bytes = peak_ram_bytes.max(current_physical_memory());

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        reader.reload()?;
        let wall_ns = nanos_u64(started.elapsed().as_nanos())?;
        let index_bytes = directory_size(directory.path())?;
        let seconds = wall_ns as f64 / 1_000_000_000.0;
        let documents_per_second = if seconds > 0.0 {
            documents as f64 / seconds
        } else {
            0.0
        };
        let bytes_per_document = if documents == 0 {
            0.0
        } else {
            index_bytes as f64 / documents as f64
        };
        let index_amplification = if source_text_bytes == 0 {
            0.0
        } else {
            index_bytes as f64 / source_text_bytes as f64
        };

        Ok(Self {
            _directory: directory,
            reader,
            fields,
            strategy,
            build_metrics: BuildMetrics {
                documents,
                wall_ns,
                documents_per_second,
                index_bytes,
                bytes_per_document,
                source_text_bytes,
                index_amplification,
                peak_ram_bytes,
            },
        })
    }

    /// Returns immutable build evidence.
    #[must_use]
    pub const fn build_metrics(&self) -> &BuildMetrics {
        &self.build_metrics
    }

    /// Executes bounded candidate retrieval, mandatory verification, and ranking.
    ///
    /// # Errors
    ///
    /// Returns typed policy failures before retrieval and structured backend
    /// failures for Tantivy/document decoding errors.
    pub fn search(
        &self,
        query: &str,
        mode: QueryMode,
        policy: CandidatePolicy,
    ) -> ExperimentResult<SearchOutcome> {
        let end_to_end_started = Instant::now();
        let plan = policy.plan(query, mode)?;

        let retrieval_started = Instant::now();
        let addresses = self.retrieve_addresses(&plan)?;
        let searcher = self.reader.searcher();
        let candidates: Vec<CatalogRecord> = addresses
            .into_iter()
            .map(|address| self.read_record(&searcher, address))
            .collect::<ExperimentResult<_>>()?;
        let retrieval_ns = nanos_u64(retrieval_started.elapsed().as_nanos())?;

        let candidate_count = candidates.len();
        let verification_started = Instant::now();
        let verified: Vec<_> = candidates
            .into_iter()
            .filter(|record| {
                normalize_search_text(&record.name).contains(&plan.normalized_query)
                    || normalize_search_text(&record.path).contains(&plan.normalized_query)
            })
            .collect();
        let verification_ns = nanos_u64(verification_started.elapsed().as_nanos())?;
        let verified_count = verified.len();

        let ranker_started = Instant::now();
        let hits = rank_verified(verified, &plan.normalized_query);
        let ranker_ns = nanos_u64(ranker_started.elapsed().as_nanos())?;
        let end_to_end_ns = nanos_u64(end_to_end_started.elapsed().as_nanos())?;
        let verification_rejection_ratio = if candidate_count == 0 {
            0.0
        } else {
            (candidate_count - verified_count) as f64 / candidate_count as f64
        };

        Ok(SearchOutcome {
            hits,
            metrics: CandidateMetrics {
                retrieval_ns,
                verification_ns,
                ranker_ns,
                end_to_end_ns,
                candidate_count,
                verified_count,
                verification_rejection_ratio,
            },
        })
    }

    fn retrieve_addresses(&self, plan: &QueryPlan) -> ExperimentResult<Vec<DocAddress>> {
        let searcher = self.reader.searcher();
        let mut addresses = Vec::with_capacity(plan.candidate_limit);
        let mut seen = HashSet::with_capacity(plan.candidate_limit);

        let lexical = lexical_query(self.fields, &plan.normalized_query);
        let gram_width = match self.strategy {
            Strategy::Trigram | Strategy::TokenPrefixLimitedTrigram => 3,
            Strategy::BoundedFourgram => 4,
        };
        let query_length = plan.normalized_query.chars().count();

        match self.strategy {
            Strategy::Trigram if plan.allow_ngram => {
                let gram_query = gram_query(self.fields.gram, &plan.normalized_query, gram_width);
                append_top_docs(
                    &searcher,
                    gram_query.as_ref(),
                    plan.candidate_limit,
                    &mut seen,
                    &mut addresses,
                )?;
            }
            Strategy::TokenPrefixLimitedTrigram => {
                append_top_docs(
                    &searcher,
                    lexical.as_ref(),
                    plan.candidate_limit,
                    &mut seen,
                    &mut addresses,
                )?;
                if plan.allow_ngram && addresses.len() < plan.candidate_limit {
                    let gram_query =
                        gram_query(self.fields.gram, &plan.normalized_query, gram_width);
                    append_top_docs(
                        &searcher,
                        gram_query.as_ref(),
                        plan.candidate_limit,
                        &mut seen,
                        &mut addresses,
                    )?;
                }
            }
            Strategy::BoundedFourgram if plan.allow_ngram && query_length >= gram_width => {
                let gram_query = gram_query(self.fields.gram, &plan.normalized_query, gram_width);
                append_top_docs(
                    &searcher,
                    gram_query.as_ref(),
                    plan.candidate_limit,
                    &mut seen,
                    &mut addresses,
                )?;
            }
            Strategy::Trigram | Strategy::BoundedFourgram => {
                append_top_docs(
                    &searcher,
                    lexical.as_ref(),
                    plan.candidate_limit,
                    &mut seen,
                    &mut addresses,
                )?;
            }
        }
        addresses.truncate(plan.candidate_limit);
        Ok(addresses)
    }

    fn read_record(
        &self,
        searcher: &tantivy::Searcher,
        address: DocAddress,
    ) -> ExperimentResult<CatalogRecord> {
        let document: TantivyDocument = searcher.doc(address)?;
        let ordinal = document
            .get_first(self.fields.ordinal)
            .and_then(|value| value.as_u64())
            .ok_or(ExperimentError::InvalidDocument("ordinal"))?;
        let name = document
            .get_first(self.fields.name)
            .and_then(|value| value.as_str())
            .ok_or(ExperimentError::InvalidDocument("name"))?
            .to_owned();
        let path = document
            .get_first(self.fields.path)
            .and_then(|value| value.as_str())
            .ok_or(ExperimentError::InvalidDocument("path"))?
            .to_owned();
        Ok(CatalogRecord {
            ordinal,
            name,
            path,
            extension: String::new(),
            size: 0,
            modified_at_unix_ms: 0,
        })
    }
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let ordinal_options = NumericOptions::default().set_stored().set_indexed();
    let ordinal = builder.add_u64_field("ordinal", ordinal_options);
    let name = builder.add_text_field("name", STORED);
    let path = builder.add_text_field("path", STORED);
    let exact = builder.add_text_field("exact", STRING);
    let token = builder.add_text_field("token", TEXT);
    let prefix = builder.add_text_field("prefix", TEXT);
    let gram = builder.add_text_field("gram", TEXT);
    (
        builder.build(),
        Fields {
            ordinal,
            name,
            path,
            exact,
            token,
            prefix,
            gram,
        },
    )
}

fn lexical_query(fields: Fields, query: &str) -> Box<dyn Query> {
    let queries: Vec<(Occur, Box<dyn Query>)> = vec![
        (
            Occur::Should,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.exact, query),
                IndexRecordOption::Basic,
            )),
        ),
        (
            Occur::Should,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.token, query),
                IndexRecordOption::Basic,
            )),
        ),
        (
            Occur::Should,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.prefix, query),
                IndexRecordOption::Basic,
            )),
        ),
    ];
    Box::new(BooleanQuery::new(queries))
}

fn gram_query(field: Field, query: &str, width: usize) -> Box<dyn Query> {
    let terms: Vec<Term> = grams(query, width)
        .into_iter()
        .map(|gram| Term::from_field_text(field, &gram))
        .collect();
    if terms.len() == 1 {
        Box::new(TermQuery::new(terms[0].clone(), IndexRecordOption::Basic))
    } else {
        Box::new(PhraseQuery::new(terms))
    }
}

fn append_top_docs(
    searcher: &tantivy::Searcher,
    query: &dyn Query,
    limit: usize,
    seen: &mut HashSet<DocAddress>,
    addresses: &mut Vec<DocAddress>,
) -> ExperimentResult<()> {
    let remaining = limit.saturating_sub(addresses.len());
    if remaining == 0 {
        return Ok(());
    }
    for (_, address) in searcher.search(query, &TopDocs::with_limit(remaining).order_by_score())? {
        if seen.insert(address) {
            addresses.push(address);
        }
    }
    Ok(())
}

fn prefixes(value: &str, maximum_length: usize) -> Vec<String> {
    let characters: Vec<char> = value.chars().collect();
    let end = characters.len().min(maximum_length);
    (1..=end)
        .map(|length| characters[..length].iter().collect())
        .collect()
}

fn grams(value: &str, width: usize) -> Vec<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let characters: Vec<char> = value.chars().collect();
    if characters.len() < width {
        return Vec::new();
    }
    characters
        .windows(width)
        .map(|window| {
            let literal: String = window.iter().collect();
            let mut encoded = String::with_capacity(literal.len().saturating_mul(2));
            for byte in literal.as_bytes() {
                encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
            encoded
        })
        .collect()
}

fn directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn current_physical_memory() -> u64 {
    memory_stats::memory_stats()
        .and_then(|stats| u64::try_from(stats.physical_mem).ok())
        .unwrap_or(0)
}

fn nanos_u64(value: u128) -> ExperimentResult<u64> {
    u64::try_from(value).map_err(|_| ExperimentError::NumericRange)
}

#[cfg(test)]
mod tests {
    use super::{ExperimentalIndex, Strategy};
    use crate::{CandidatePolicy, CatalogDataset, CatalogRecord, QueryMode};

    fn record(ordinal: u64, name: &str) -> CatalogRecord {
        CatalogRecord {
            ordinal,
            name: name.to_owned(),
            path: format!("/z/{name}"),
            extension: "txt".to_owned(),
            size: 0,
            modified_at_unix_ms: 0,
        }
    }

    #[test]
    fn all_strategies_enforce_candidate_cap() -> crate::ExperimentResult<()> {
        let dataset = CatalogDataset::new(2_000, 42);
        for strategy in Strategy::ALL {
            let index = ExperimentalIndex::build(&dataset, strategy, 20_000_000)?;
            let outcome = index.search(
                "report",
                QueryMode::ProductSearch,
                CandidatePolicy {
                    candidate_limit: 17,
                },
            )?;
            assert!(outcome.metrics.candidate_count <= 17);
        }
        Ok(())
    }

    #[test]
    fn positional_grams_reject_reordered_false_candidate() -> crate::ExperimentResult<()> {
        let records = vec![record(1, "abc-then-bcd.txt"), record(2, "unrelated.txt")];
        let index = ExperimentalIndex::build_records(
            records.into_iter(),
            2,
            Strategy::Trigram,
            20_000_000,
        )?;
        let outcome = index.search(
            "abcd",
            QueryMode::SubstringOnly,
            CandidatePolicy {
                candidate_limit: 20,
            },
        )?;
        assert_eq!(outcome.metrics.candidate_count, 0);
        assert_eq!(outcome.metrics.verified_count, 0);
        assert!(outcome.hits.is_empty());
        Ok(())
    }
}
