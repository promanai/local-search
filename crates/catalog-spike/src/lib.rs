//! Experimental Tantivy catalog retrieval implementation for START-002.
//!
//! This crate deliberately does not define the product's final Tantivy schema.

use std::{path::Path, sync::Arc};

use localsearch_benchmark_data::{QueryKind, SyntheticQuery, SyntheticRecord};
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term,
    collector::TopDocs,
    directory::MmapDirectory,
    query::{Query, TermQuery},
    schema::{
        Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, Value,
    },
    tokenizer::{LowerCaser, NgramTokenizer, RemoveLongFilter, SimpleTokenizer, TextAnalyzer},
};
use thiserror::Error;

/// Explicitly experimental schema identifier; this is not `TANTIVY-SCHEMA-v1`.
pub const EXPERIMENTAL_SCHEMA_ID: &str = "start002-name-exact-token-prefix-e1";
const TOKENIZER_NAME: &str = "start002_filename_tokens";
const PREFIX_TOKENIZER_NAME: &str = "start002_filename_prefixes";

/// START-002 catalog failure.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// Tantivy operation failed.
    #[error(transparent)]
    Tantivy(#[from] tantivy::TantivyError),
    /// The requested n-gram tokenizer configuration was invalid.
    #[error("invalid experimental tokenizer configuration")]
    InvalidTokenizer,
    /// A retrieved document omitted its ordinal.
    #[error("indexed document omitted its ordinal")]
    MissingOrdinal,
}

/// Result type for the experimental catalog.
pub type CatalogResult<T> = Result<T, CatalogError>;

#[derive(Clone, Copy)]
struct Fields {
    ordinal: Field,
    name_exact: Field,
    name_tokens: Field,
    name_prefix: Field,
}

/// Open experimental index plus its immutable schema handles.
pub struct CatalogIndex {
    index: Index,
    fields: Fields,
}

impl CatalogIndex {
    /// Creates a new on-disk experimental index.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory or Tantivy index cannot be created.
    pub fn create(path: &Path) -> CatalogResult<Self> {
        std::fs::create_dir_all(path).map_err(tantivy::TantivyError::from)?;
        let fields = build_schema();
        let directory = MmapDirectory::open(path).map_err(tantivy::TantivyError::from)?;
        let index = Index::create(
            directory,
            fields.0.clone(),
            tantivy::IndexSettings::default(),
        )?;
        register_tokenizers(&index)?;
        Ok(Self {
            index,
            fields: fields.1,
        })
    }

    /// Opens an existing index created by this experimental schema.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot open the index or the schema differs.
    pub fn open(path: &Path) -> CatalogResult<Self> {
        let directory = MmapDirectory::open(path).map_err(tantivy::TantivyError::from)?;
        let index = Index::open(directory)?;
        register_tokenizers(&index)?;
        let schema = index.schema();
        let fields = Fields {
            ordinal: schema.get_field("ordinal")?,
            name_exact: schema.get_field("name_exact")?,
            name_tokens: schema.get_field("name_tokens")?,
            name_prefix: schema.get_field("name_prefix")?,
        };
        Ok(Self { index, fields })
    }

    /// Acquires the only mutation owner used by this process.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot acquire the writer lock.
    pub fn writer(&self, heap_bytes: usize) -> CatalogResult<CatalogWriter> {
        Ok(CatalogWriter {
            writer: self.index.writer(heap_bytes)?,
            fields: self.fields,
        })
    }

    /// Creates a reusable reader with explicit reload behavior.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot build the reader.
    pub fn reader(&self) -> CatalogResult<CatalogReader> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(CatalogReader {
            reader,
            fields: self.fields,
        })
    }

    /// Gives read-only access to the underlying experimental Tantivy index.
    #[must_use]
    pub const fn tantivy_index(&self) -> &Index {
        &self.index
    }
}

/// Exclusive index mutation owner. Tantivy itself enforces the process lock.
pub struct CatalogWriter {
    writer: IndexWriter,
    fields: Fields,
}

impl CatalogWriter {
    /// Adds a generated record.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy rejects the document.
    pub fn add(&mut self, record: &SyntheticRecord) -> CatalogResult<()> {
        let normalized_name = normalize(&record.name);
        let mut document = TantivyDocument::default();
        document.add_u64(self.fields.ordinal, record.ordinal);
        document.add_text(self.fields.name_exact, &normalized_name);
        document.add_text(self.fields.name_tokens, &record.name);
        document.add_text(self.fields.name_prefix, &normalized_name);
        self.writer.add_document(document)?;
        Ok(())
    }

    /// Makes all pending records durable and visible after reader reload.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot commit.
    pub fn commit(&mut self) -> CatalogResult<u64> {
        Ok(self.writer.commit()?)
    }

    /// Waits for merge workers and consumes this sole writer owner.
    ///
    /// # Errors
    ///
    /// Returns an error when a background merge failed.
    pub fn wait_merging_threads(self) -> CatalogResult<()> {
        self.writer.wait_merging_threads()?;
        Ok(())
    }
}

/// Reusable catalog reader. Its `IndexReader` owns reusable search resources.
pub struct CatalogReader {
    reader: IndexReader,
    fields: Fields,
}

impl CatalogReader {
    /// Reloads the searcher snapshot after a committed writer update.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot load the current metadata.
    pub fn reload(&self) -> CatalogResult<()> {
        self.reader.reload()?;
        Ok(())
    }

    /// Executes exact, token, or prefix retrieval and returns stable ordinals.
    ///
    /// # Errors
    ///
    /// Returns an error when the query fails or a result lacks its ordinal.
    pub fn search(&self, query: &SyntheticQuery, limit: usize) -> CatalogResult<Vec<u64>> {
        let searcher = self.reader.searcher();
        let field = match query.kind {
            QueryKind::Exact => self.fields.name_exact,
            QueryKind::Token => self.fields.name_tokens,
            QueryKind::Prefix => self.fields.name_prefix,
        };
        let term_text = normalize(&query.text);
        let term = Term::from_field_text(field, &term_text);
        let term_query: Arc<dyn Query> = Arc::new(TermQuery::new(term, IndexRecordOption::Basic));
        let matches = searcher.search(
            term_query.as_ref(),
            &TopDocs::with_limit(limit).order_by_score(),
        )?;
        matches
            .into_iter()
            .map(|(_, address)| {
                let document: TantivyDocument = searcher.doc(address)?;
                document
                    .get_first(self.fields.ordinal)
                    .and_then(|value| value.as_u64())
                    .ok_or(CatalogError::MissingOrdinal)
            })
            .collect()
    }
}

/// Catalog normalization used by both indexing and retrieval in the spike.
#[must_use]
pub fn normalize(input: &str) -> String {
    input.to_lowercase()
}

fn build_schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let ordinal_options = NumericOptions::default().set_stored();
    let ordinal = builder.add_u64_field("ordinal", ordinal_options);
    let exact_options = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("raw")
            .set_index_option(IndexRecordOption::Basic),
    );
    let name_exact = builder.add_text_field("name_exact", exact_options);
    let token_options = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::Basic),
    );
    let name_tokens = builder.add_text_field("name_tokens", token_options);
    let prefix_options = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(PREFIX_TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::Basic),
    );
    let name_prefix = builder.add_text_field("name_prefix", prefix_options);
    let schema = builder.build();
    (
        schema,
        Fields {
            ordinal,
            name_exact,
            name_tokens,
            name_prefix,
        },
    )
}

fn register_tokenizers(index: &Index) -> CatalogResult<()> {
    let tokens = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(80))
        .filter(LowerCaser)
        .build();
    index.tokenizers().register(TOKENIZER_NAME, tokens);
    let prefix = NgramTokenizer::new(1, 32, true).map_err(|_| CatalogError::InvalidTokenizer)?;
    let prefixes = TextAnalyzer::builder(prefix).filter(LowerCaser).build();
    index.tokenizers().register(PREFIX_TOKENIZER_NAME, prefixes);
    Ok(())
}
