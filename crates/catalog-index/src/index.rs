use std::{
    collections::{HashSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::Path,
};

use localsearch_core::{CatalogDocument, DocumentId, IndexMutation};
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term,
    collector::{Count, TopDocs},
    directory::MmapDirectory,
    query::{AllQuery, PhraseQuery, Query, TermQuery},
    schema::{Field, IndexRecordOption, STORED, STRING, Schema, TEXT, Value},
};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

/// Frozen production catalog schema selected by the engineering gates.
pub const CATALOG_SCHEMA_ID: &str = "TANTIVY-SCHEMA-v1";
const SCHEMA_MARKER: &str = "LOCALSEARCH_SCHEMA";

/// Failure while creating, opening, or mutating the materialized catalog.
#[derive(Debug, Error)]
pub enum CatalogIndexError {
    /// Tantivy operation failed.
    #[error("Tantivy catalog operation failed: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    /// Filesystem operation failed.
    #[error("catalog filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// Stored canonical document payload is invalid.
    #[error("catalog document serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    /// The directory contains another catalog schema.
    #[error("catalog schema marker mismatch: expected {expected}, found {found}")]
    SchemaMismatch {
        /// Required schema identifier.
        expected: &'static str,
        /// Observed marker.
        found: String,
    },
    /// A stored document omitted a required field.
    #[error("stored catalog document is missing {0}")]
    MissingField(&'static str),
    /// A mutation carries inconsistent canonical identity.
    #[error("poison projection mutation: {0}")]
    Poison(String),
    /// A stored document ID is not a canonical `DocumentId`.
    #[error("stored catalog document ID is invalid: {0}")]
    InvalidDocumentId(#[from] localsearch_core::IdParseError),
    /// Product substring contract requires at least three characters.
    #[error("substring query requires at least three normalized characters")]
    ShortSubstring,
}

/// Result type for the materialized catalog.
pub type CatalogIndexResult<T> = Result<T, CatalogIndexError>;

/// Candidate retrieval path implemented by Catalog Schema v1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogQueryMode {
    /// Full normalized filename.
    Exact,
    /// Complete normalized filename token.
    Token,
    /// Leading normalized filename characters.
    Prefix,
    /// Positional normalized filename trigram sequence.
    Substring,
    /// Complete normalized path token.
    Path,
}

/// Order-independent convergence fingerprint plus exact duplicate-ID count.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CatalogFingerprint {
    /// Total live documents observed.
    pub documents: u64,
    /// Distinct stable document IDs observed.
    pub unique_documents: u64,
    /// XOR of canonical payload hashes.
    pub payload_hash_xor: u64,
    /// Wrapping sum of canonical payload hashes.
    pub payload_hash_sum: u64,
}

impl CatalogFingerprint {
    /// Adds one canonical document known to be unique in durable desired state.
    ///
    /// # Errors
    ///
    /// Returns an error when the canonical document cannot be serialized.
    pub fn add_desired(&mut self, document: &CatalogDocument) -> CatalogIndexResult<()> {
        let hash = document_hash(document)?;
        self.documents = self.documents.saturating_add(1);
        self.unique_documents = self.unique_documents.saturating_add(1);
        self.payload_hash_xor ^= hash;
        self.payload_hash_sum = self.payload_hash_sum.wrapping_add(hash);
        Ok(())
    }

    /// Returns the number of duplicate searchable document IDs.
    #[must_use]
    pub const fn duplicate_documents(self) -> u64 {
        self.documents.saturating_sub(self.unique_documents)
    }
}

#[derive(Clone, Copy)]
struct Fields {
    document_id: Field,
    document_version: Field,
    payload: Field,
    exact_name: Field,
    name_tokens: Field,
    name_prefixes: Field,
    name_trigrams: Field,
    path_tokens: Field,
}

/// Open catalog generation with immutable production schema handles.
pub struct CatalogIndex {
    index: Index,
    fields: Fields,
}

impl CatalogIndex {
    /// Creates a new empty catalog generation.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory exists, cannot be initialized, or Tantivy rejects the
    /// production schema.
    pub fn create(path: &Path) -> CatalogIndexResult<Self> {
        if path.exists() {
            return Err(CatalogIndexError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("catalog generation already exists: {}", path.display()),
            )));
        }
        fs::create_dir_all(path)?;
        fs::write(path.join(SCHEMA_MARKER), CATALOG_SCHEMA_ID)?;
        let (schema, fields) = build_schema();
        let directory = MmapDirectory::open(path).map_err(tantivy::TantivyError::from)?;
        let index = Index::create(directory, schema, tantivy::IndexSettings::default())?;
        Ok(Self { index, fields })
    }

    /// Opens and validates an existing catalog generation.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing/corrupt directory or incompatible schema marker.
    pub fn open(path: &Path) -> CatalogIndexResult<Self> {
        let found = fs::read_to_string(path.join(SCHEMA_MARKER))?;
        if found != CATALOG_SCHEMA_ID {
            return Err(CatalogIndexError::SchemaMismatch {
                expected: CATALOG_SCHEMA_ID,
                found,
            });
        }
        let directory = MmapDirectory::open(path).map_err(tantivy::TantivyError::from)?;
        let index = Index::open(directory)?;
        let schema = index.schema();
        let fields = Fields {
            document_id: schema.get_field("document_id")?,
            document_version: schema.get_field("document_version")?,
            payload: schema.get_field("payload")?,
            exact_name: schema.get_field("exact_name")?,
            name_tokens: schema.get_field("name_tokens")?,
            name_prefixes: schema.get_field("name_prefixes")?,
            name_trigrams: schema.get_field("name_trigrams")?,
            path_tokens: schema.get_field("path_tokens")?,
        };
        Ok(Self { index, fields })
    }

    /// Acquires the sole logical mutation writer for this generation.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot acquire its writer lock.
    pub fn writer(&self, heap_bytes: usize) -> CatalogIndexResult<CatalogWriter> {
        Ok(CatalogWriter {
            writer: self.index.writer(heap_bytes)?,
            fields: self.fields,
        })
    }

    /// Creates a manually reloaded reader.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot create the reader.
    pub fn reader(&self) -> CatalogIndexResult<CatalogReader> {
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
}

/// Exclusive mutation owner for one catalog generation.
pub struct CatalogWriter {
    writer: IndexWriter,
    fields: Fields,
}

impl CatalogWriter {
    /// Applies one backend-neutral canonical mutation idempotently.
    ///
    /// # Errors
    ///
    /// Returns an error for poisoned identity, serialization failure, or Tantivy rejection.
    pub fn apply(&mut self, mutation: &IndexMutation) -> CatalogIndexResult<()> {
        match mutation {
            IndexMutation::Upsert { document } => {
                if document.identity.document_id != document_id_from_link(document) {
                    return Err(CatalogIndexError::Poison(
                        "document ID must be derived from the file-link ID".to_owned(),
                    ));
                }
                self.delete(document.identity.document_id);
                self.writer
                    .add_document(index_document(self.fields, document)?)?;
            }
            IndexMutation::Delete { document_id, .. } => self.delete(*document_id),
        }
        Ok(())
    }

    /// Adds current desired state during full rebuild.
    ///
    /// # Errors
    ///
    /// Returns an error for poisoned identity, serialization failure, or Tantivy rejection.
    pub fn add_current(&mut self, document: &CatalogDocument) -> CatalogIndexResult<()> {
        if document.identity.document_id != document_id_from_link(document) {
            return Err(CatalogIndexError::Poison(
                "document ID must be derived from the file-link ID".to_owned(),
            ));
        }
        self.writer
            .add_document(index_document(self.fields, document)?)?;
        Ok(())
    }

    fn delete(&mut self, document_id: DocumentId) {
        self.writer.delete_term(Term::from_field_text(
            self.fields.document_id,
            &document_id.to_string(),
        ));
    }

    /// Commits the whole batch as one Tantivy generation.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot make the generation durable.
    pub fn commit(&mut self) -> CatalogIndexResult<u64> {
        Ok(self.writer.commit()?)
    }

    /// Waits for merge threads and consumes the writer.
    ///
    /// # Errors
    ///
    /// Returns an error when a background merge failed.
    pub fn wait_merging_threads(self) -> CatalogIndexResult<()> {
        self.writer.wait_merging_threads()?;
        Ok(())
    }
}

/// Stable reader over one committed catalog snapshot.
#[derive(Clone)]
pub struct CatalogReader {
    reader: IndexReader,
    fields: Fields,
}

impl CatalogReader {
    /// Reloads the latest committed snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot reload metadata.
    pub fn reload(&self) -> CatalogIndexResult<()> {
        self.reader.reload()?;
        Ok(())
    }

    /// Counts live searchable documents.
    ///
    /// # Errors
    ///
    /// Returns an error when the count query fails.
    pub fn document_count(&self) -> CatalogIndexResult<usize> {
        Ok(self.reader.searcher().search(&AllQuery, &Count)?)
    }

    /// Loads one canonical document by stable document identity.
    ///
    /// # Errors
    ///
    /// Returns an error when query or stored-document decoding fails.
    pub fn document(&self, document_id: DocumentId) -> CatalogIndexResult<Option<CatalogDocument>> {
        let searcher = self.reader.searcher();
        let query = TermQuery::new(
            Term::from_field_text(self.fields.document_id, &document_id.to_string()),
            IndexRecordOption::Basic,
        );
        let Some((_, address)) = searcher
            .search(&query, &TopDocs::with_limit(1).order_by_score())?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let stored: TantivyDocument = searcher.doc(address)?;
        let payload = stored
            .get_first(self.fields.payload)
            .and_then(|value| value.as_str())
            .ok_or(CatalogIndexError::MissingField("payload"))?;
        Ok(Some(serde_json::from_str(payload)?))
    }

    /// Retrieves bounded stable document IDs using Catalog Schema v1 fields.
    ///
    /// Candidate results still require deterministic verification and product ranking by the
    /// caller. Substring queries shorter than three normalized characters are rejected.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported short substring, Tantivy failure, or malformed stored
    /// canonical identity.
    pub fn search_candidates(
        &self,
        query: &str,
        mode: CatalogQueryMode,
        limit: usize,
    ) -> CatalogIndexResult<Vec<DocumentId>> {
        let (searcher, addresses) = self.candidate_addresses(query, mode, limit)?;
        addresses
            .into_iter()
            .map(|address| {
                let stored: TantivyDocument = searcher.doc(address)?;
                let value = stored
                    .get_first(self.fields.document_id)
                    .and_then(|value| value.as_str())
                    .ok_or(CatalogIndexError::MissingField("document_id"))?;
                value.parse().map_err(CatalogIndexError::from)
            })
            .collect()
    }

    /// Retrieves bounded canonical candidate payloads from the same committed snapshot used for
    /// matching.
    ///
    /// Returning stored payloads together with the match avoids an N+1 lookup through another
    /// durable store and guarantees that verification/ranking sees one coherent index generation.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported short substring, Tantivy failure, or malformed stored
    /// canonical payload.
    pub fn search_candidate_documents(
        &self,
        query: &str,
        mode: CatalogQueryMode,
        limit: usize,
    ) -> CatalogIndexResult<Vec<CatalogDocument>> {
        let (searcher, addresses) = self.candidate_addresses(query, mode, limit)?;
        addresses
            .into_iter()
            .map(|address| {
                let stored: TantivyDocument = searcher.doc(address)?;
                let payload = stored
                    .get_first(self.fields.payload)
                    .and_then(|value| value.as_str())
                    .ok_or(CatalogIndexError::MissingField("payload"))?;
                serde_json::from_str(payload).map_err(CatalogIndexError::from)
            })
            .collect()
    }

    fn candidate_addresses(
        &self,
        query: &str,
        mode: CatalogQueryMode,
        limit: usize,
    ) -> CatalogIndexResult<(tantivy::Searcher, Vec<tantivy::DocAddress>)> {
        let normalized = normalize(query);
        let query: Box<dyn Query> = match mode {
            CatalogQueryMode::Exact => Box::new(TermQuery::new(
                Term::from_field_text(self.fields.exact_name, &normalized),
                IndexRecordOption::Basic,
            )),
            CatalogQueryMode::Token => Box::new(TermQuery::new(
                Term::from_field_text(self.fields.name_tokens, &normalized),
                IndexRecordOption::Basic,
            )),
            CatalogQueryMode::Prefix => Box::new(TermQuery::new(
                Term::from_field_text(self.fields.name_prefixes, &normalized),
                IndexRecordOption::Basic,
            )),
            CatalogQueryMode::Substring => {
                let terms = grams(&normalized, 3)
                    .into_iter()
                    .map(|gram| Term::from_field_text(self.fields.name_trigrams, &gram))
                    .collect::<Vec<_>>();
                if terms.is_empty() {
                    return Err(CatalogIndexError::ShortSubstring);
                }
                if terms.len() == 1 {
                    Box::new(TermQuery::new(
                        terms[0].clone(),
                        IndexRecordOption::WithFreqsAndPositions,
                    ))
                } else {
                    Box::new(PhraseQuery::new(terms))
                }
            }
            CatalogQueryMode::Path => Box::new(TermQuery::new(
                Term::from_field_text(self.fields.path_tokens, &normalized),
                IndexRecordOption::Basic,
            )),
        };
        let searcher = self.reader.searcher();
        let addresses =
            searcher.search(query.as_ref(), &TopDocs::with_limit(limit).order_by_score())?;
        Ok((
            searcher,
            addresses.into_iter().map(|(_, address)| address).collect(),
        ))
    }

    /// Loads every live canonical document for convergence verification.
    ///
    /// # Errors
    ///
    /// Returns an error when query or stored-document decoding fails.
    pub fn documents(&self) -> CatalogIndexResult<Vec<CatalogDocument>> {
        let count = self.document_count()?;
        let searcher = self.reader.searcher();
        let addresses = searcher.search(&AllQuery, &TopDocs::with_limit(count).order_by_score())?;
        addresses
            .into_iter()
            .map(|(_, address)| {
                let stored: TantivyDocument = searcher.doc(address)?;
                let payload = stored
                    .get_first(self.fields.payload)
                    .and_then(|value| value.as_str())
                    .ok_or(CatalogIndexError::MissingField("payload"))?;
                serde_json::from_str(payload).map_err(CatalogIndexError::from)
            })
            .collect()
    }

    /// Streams stored documents to an order-independent convergence fingerprint.
    ///
    /// # Errors
    ///
    /// Returns an error when stored-document access or canonical decoding fails.
    pub fn fingerprint(&self) -> CatalogIndexResult<CatalogFingerprint> {
        let searcher = self.reader.searcher();
        let mut result = CatalogFingerprint::default();
        let mut identities = HashSet::new();
        for segment in searcher.segment_readers() {
            let store = segment.get_store_reader(4)?;
            for doc_id in segment.doc_ids_alive() {
                let stored: TantivyDocument = store.get(doc_id)?;
                let payload = stored
                    .get_first(self.fields.payload)
                    .and_then(|value| value.as_str())
                    .ok_or(CatalogIndexError::MissingField("payload"))?;
                let document: CatalogDocument = serde_json::from_str(payload)?;
                let hash = document_hash(&document)?;
                result.documents = result.documents.saturating_add(1);
                if identities.insert(document.identity.document_id) {
                    result.unique_documents = result.unique_documents.saturating_add(1);
                }
                result.payload_hash_xor ^= hash;
                result.payload_hash_sum = result.payload_hash_sum.wrapping_add(hash);
            }
        }
        Ok(result)
    }
}

fn build_schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let document_id = builder.add_text_field("document_id", STRING | STORED);
    let document_version = builder.add_u64_field("document_version", STORED);
    let payload = builder.add_text_field("payload", STORED);
    let exact_name = builder.add_text_field("exact_name", STRING);
    let name_tokens = builder.add_text_field("name_tokens", TEXT);
    let name_prefixes = builder.add_text_field("name_prefixes", TEXT);
    let name_trigrams = builder.add_text_field("name_trigrams", TEXT);
    let path_tokens = builder.add_text_field("path_tokens", TEXT);
    let schema = builder.build();
    (
        schema,
        Fields {
            document_id,
            document_version,
            payload,
            exact_name,
            name_tokens,
            name_prefixes,
            name_trigrams,
            path_tokens,
        },
    )
}

fn index_document(
    fields: Fields,
    document: &CatalogDocument,
) -> CatalogIndexResult<TantivyDocument> {
    let normalized_name = normalize(&document.name);
    let normalized_path = normalize(&document.resolved_path);
    let tokens = tokens(&normalized_name);
    let prefixes = tokens
        .iter()
        .flat_map(|token| prefixes(token, 32))
        .collect::<Vec<_>>()
        .join(" ");
    let trigrams = grams(&normalized_name, 3).join(" ");
    let mut indexed = TantivyDocument::default();
    indexed.add_text(
        fields.document_id,
        document.identity.document_id.to_string(),
    );
    indexed.add_u64(fields.document_version, document.document_version.0);
    indexed.add_text(fields.payload, serde_json::to_string(document)?);
    indexed.add_text(fields.exact_name, &normalized_name);
    indexed.add_text(fields.name_tokens, tokens.join(" "));
    indexed.add_text(fields.name_prefixes, prefixes);
    indexed.add_text(fields.name_trigrams, trigrams);
    indexed.add_text(fields.path_tokens, normalized_path);
    Ok(indexed)
}

fn document_id_from_link(document: &CatalogDocument) -> DocumentId {
    DocumentId::from_bytes(document.identity.file_link_id.into_bytes())
}

fn document_hash(document: &CatalogDocument) -> CatalogIndexResult<u64> {
    let payload = serde_json::to_vec(document)?;
    let mut hasher = DefaultHasher::new();
    payload.hash(&mut hasher);
    Ok(hasher.finish())
}

fn normalize(value: &str) -> String {
    value.nfkc().flat_map(char::to_lowercase).collect()
}

fn tokens(value: &str) -> Vec<&str> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect()
}

fn prefixes(value: &str, maximum_length: usize) -> Vec<String> {
    let characters = value.chars().collect::<Vec<_>>();
    (1..=characters.len().min(maximum_length))
        .map(|length| characters[..length].iter().collect())
        .collect()
}

fn grams(value: &str, width: usize) -> Vec<String> {
    let characters = value.chars().collect::<Vec<_>>();
    characters
        .windows(width)
        .map(|window| window.iter().collect())
        .collect()
}
