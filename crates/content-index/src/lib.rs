#![forbid(unsafe_code)]

//! Explicitly opt-in, bounded plaintext indexing kept separate from the filename catalog.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use localsearch_core::{
    Availability, CatalogDocument, DocumentId, FileKind, IndexMutation, MutationSeq,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::{
    DocAddress, DocSet, Index, IndexReader, ReloadPolicy, Searcher, TERMINATED, TantivyDocument,
    Term,
    collector::{Count, TopDocs},
    directory::MmapDirectory,
    query::{
        AllQuery, BooleanQuery, EnableScoring, FuzzyTermQuery, Occur, Query, QueryParser, TermQuery,
    },
    schema::{Field, IndexRecordOption, STORED, STRING, Schema, TEXT, Value},
    tokenizer::TokenStream,
};
use thiserror::Error;

/// Independent content schema. Catalog Schema v1 remains unchanged and rebuildable on its own.
pub const CONTENT_SCHEMA_ID: &str = "CONTENT-SCHEMA-v1";
/// Default maximum bytes read from one explicitly eligible file.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1_048_576;
const MAX_CONFIGURED_FILE_BYTES: u64 = 16 * 1_048_576;
const SCHEMA_MARKER: &str = "LOCALSEARCH_CONTENT_SCHEMA";
const WRITER_HEAP_BYTES: usize = 32 * 1_048_576;
const MIN_CONTENT_PREFIX_CHARS: usize = 4;
const GENERATION_MANAGER_MARKER: &str = "LOCALSEARCH_CONTENT_GENERATIONS";
const GENERATION_MANAGER_VERSION: u32 = 1;
const GENERATION_STATE_FILE: &str = "generation.json";
const ACTIVE_GENERATION_FILE: &str = "active.json";

/// Content-index error without embedding source text.
#[derive(Debug, Error)]
pub enum ContentIndexError {
    /// Native file or directory operation failed.
    #[error("content index filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// Tantivy operation failed.
    #[error("content index operation failed: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    /// Query syntax was invalid.
    #[error("content query was invalid")]
    Query,
    /// Stored catalog metadata could not be decoded.
    #[error("stored content metadata was invalid: {0}")]
    Serialization(#[from] serde_json::Error),
    /// Policy is empty or outside hard product bounds.
    #[error("content indexing policy is invalid")]
    InvalidPolicy,
    /// Existing index uses another schema.
    #[error("content schema marker mismatch")]
    SchemaMismatch,
    /// Required stored metadata was absent.
    #[error("content index stored metadata is incomplete")]
    MissingPayload,
    /// A source snapshot contained the same stable document identity more than once.
    #[error("content source snapshot contains a duplicate document identity")]
    DuplicateDocument,
    /// Durable graph paging failed.
    #[error("content source graph operation failed: {0}")]
    Graph(#[from] localsearch_filesystem_graph::GraphError),
}

/// Result alias for content indexing.
pub type ContentIndexResult<T> = Result<T, ContentIndexError>;

/// Explicit allowlist and resource boundary for reading file contents.
#[derive(Clone, Debug)]
pub struct ContentIndexPolicy {
    roots: Vec<PathBuf>,
    extensions: BTreeSet<String>,
    max_file_bytes: u64,
}

impl ContentIndexPolicy {
    /// Creates a policy from explicit existing roots and the conservative built-in text allowlist.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty root set, an unavailable root, or an unsafe byte limit.
    pub fn new(
        roots: impl IntoIterator<Item = PathBuf>,
        max_file_bytes: u64,
    ) -> ContentIndexResult<Self> {
        if max_file_bytes == 0 || max_file_bytes > MAX_CONFIGURED_FILE_BYTES {
            return Err(ContentIndexError::InvalidPolicy);
        }
        let mut canonical_roots = roots
            .into_iter()
            .map(fs::canonicalize)
            .collect::<Result<Vec<_>, _>>()?;
        canonical_roots.sort();
        canonical_roots.dedup();
        let overlaps = canonical_roots.iter().enumerate().any(|(index, root)| {
            canonical_roots
                .iter()
                .skip(index.saturating_add(1))
                .any(|other| other.starts_with(root) || root.starts_with(other))
        });
        if canonical_roots.is_empty()
            || canonical_roots.iter().any(|root| !root.is_dir())
            || overlaps
        {
            return Err(ContentIndexError::InvalidPolicy);
        }
        let extensions = [
            "bat", "c", "cc", "cfg", "cmd", "conf", "cpp", "css", "csv", "cs", "cxx", "go", "h",
            "hpp", "htm", "html", "ini", "java", "js", "json", "jsx", "kt", "kts", "log",
            "markdown", "md", "php", "ps1", "py", "rb", "rs", "sh", "sql", "swift", "toml", "ts",
            "tsx", "txt", "vue", "xml", "yaml", "yml",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        Ok(Self {
            roots: canonical_roots,
            extensions,
            max_file_bytes,
        })
    }

    /// Returns canonical roots accepted by this policy.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Returns the hard per-file read limit.
    #[must_use]
    pub const fn max_file_bytes(&self) -> u64 {
        self.max_file_bytes
    }

    fn declared_eligible_bytes(&self, document: &CatalogDocument) -> u64 {
        let eligible = document.metadata.kind == FileKind::File
            && document.metadata.availability == Availability::Online
            && document.metadata.size <= self.max_file_bytes
            && document
                .extension
                .as_deref()
                .is_some_and(|extension| self.extensions.contains(&extension.to_lowercase()));
        if eligible { document.metadata.size } else { 0 }
    }
}

/// Machine-readable bounded rebuild accounting.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentBuildSummary {
    /// Catalog documents considered.
    pub catalog_documents: u64,
    /// Text documents committed to this content generation.
    pub indexed_documents: u64,
    /// Documents outside explicit roots.
    pub skipped_outside_roots: u64,
    /// Non-file, offline, or unavailable documents.
    pub skipped_unavailable: u64,
    /// Extensions outside the allowlist.
    pub skipped_extension: u64,
    /// Files larger than the configured read bound.
    pub skipped_too_large: u64,
    /// Binary, NUL-containing, or non-UTF-8 files.
    pub skipped_non_text: u64,
    /// Files that changed/disappeared or could not be opened during the bounded pass.
    pub skipped_io: u64,
    /// Tantivy commit generation.
    pub generation: u64,
}

/// Machine-readable accounting for one atomic incremental synchronization.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentSyncSummary {
    /// Current catalog documents considered.
    pub catalog_documents: u64,
    /// Eligible documents whose catalog projection did not change and whose contents were not read.
    pub unchanged_documents: u64,
    /// Metadata/path changed without a content-affecting source fingerprint change.
    #[serde(default)]
    pub metadata_only_documents: u64,
    /// Source was read, but its cryptographic content hash matched the indexed document.
    #[serde(default)]
    pub unchanged_hash_documents: u64,
    /// Newly indexed documents.
    pub added_documents: u64,
    /// Existing documents replaced after their catalog projection changed.
    pub updated_documents: u64,
    /// Previously indexed documents no longer present in the catalog snapshot.
    pub removed_documents: u64,
    /// Previously indexed documents removed because they are no longer eligible or readable.
    pub evicted_documents: u64,
    /// Documents outside explicit roots.
    pub skipped_outside_roots: u64,
    /// Non-file, offline, or unavailable documents.
    pub skipped_unavailable: u64,
    /// Extensions outside the allowlist.
    pub skipped_extension: u64,
    /// Files larger than the configured read bound.
    pub skipped_too_large: u64,
    /// Binary, NUL-containing, or non-UTF-8 files.
    pub skipped_non_text: u64,
    /// Files that changed/disappeared or could not be opened during the bounded pass.
    pub skipped_io: u64,
    /// Tantivy commit generation atomically containing the entire synchronization.
    pub generation: u64,
}

/// Lifecycle of one isolated content-index generation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContentGenerationState {
    /// Candidate accepts bounded, restartable projection commits.
    Building,
    /// Candidate finished projection and passed validation.
    Ready,
    /// Generation is selected by the atomic active pointer.
    Active,
    /// Previously active generation retained for rollback.
    Retired,
    /// Candidate cannot be resumed and is eligible for garbage collection.
    Failed,
}

/// Capacity boundary which safely paused a generation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContentCapacityLimit {
    /// Configured content-index byte ceiling would be exceeded.
    ContentIndexBytes,
    /// Configured live content-document ceiling was reached.
    Documents,
    /// Absolute or percentage free-disk reserve would be violated.
    FreeDisk,
}

/// Hard resource policy for a restartable initial content generation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentGenerationLimits {
    /// Maximum bytes occupied by the candidate Tantivy index.
    pub max_content_index_bytes: u64,
    /// Maximum live content documents in the candidate.
    pub max_documents: u64,
    /// Absolute disk reserve which must remain available.
    pub min_free_disk_bytes: u64,
    /// Percentage disk reserve which must remain available, in `0..=50`.
    pub min_free_disk_percent: u8,
    /// Maximum catalog records in one atomic projection commit.
    pub batch_documents: u32,
    /// Maximum commits performed by one resume invocation.
    pub maximum_batches: u32,
}

impl ContentGenerationLimits {
    fn validate(self) -> ContentIndexResult<Self> {
        if self.max_content_index_bytes == 0
            || self.max_documents == 0
            || self.min_free_disk_percent > 50
            || self.batch_documents == 0
            || self.batch_documents > 10_000
            || self.maximum_batches == 0
            || self.maximum_batches > 4_096
        {
            return Err(ContentIndexError::InvalidPolicy);
        }
        Ok(self)
    }
}

/// Durable record for one restartable generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentGenerationRecord {
    /// State format version.
    pub version: u32,
    /// Stable identifier and directory name.
    pub generation_id: String,
    /// Stable identifier for the source catalog scan resumed by this generation.
    #[serde(default)]
    pub scan_id: String,
    /// Current lifecycle state.
    pub state: ContentGenerationState,
    /// Graph outbox sequence represented by the source snapshot.
    pub target_sequence: u64,
    /// Canonical roots used to enforce extraction scope.
    pub root_ids: Vec<String>,
    /// Last durably committed catalog cursor.
    pub last_checkpoint: Option<DocumentId>,
    /// Catalog records traversed by committed batches.
    pub documents_seen: u64,
    /// Live text documents projected into the candidate.
    pub documents_projected: u64,
    /// Declared source bytes traversed by committed batches.
    pub bytes_processed: u64,
    /// Successful Tantivy commits performed for this candidate.
    pub commits: u64,
    /// Latest committed Tantivy generation identifier.
    pub index_generation: u64,
    /// Current on-disk bytes occupied by the candidate index.
    pub content_index_bytes: u64,
    /// Capacity reason for a safe partial stop, if any.
    pub capacity_limit: Option<ContentCapacityLimit>,
    /// Creation timestamp in Unix milliseconds.
    pub created_at_unix_ms: u64,
    /// Last durable state update in Unix milliseconds.
    pub updated_at_unix_ms: u64,
}

/// Machine-readable result of one bounded generation-resume invocation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentGenerationSummary {
    /// Latest durable generation record.
    pub generation: ContentGenerationRecord,
    /// Whether validation and atomic activation completed.
    pub complete: bool,
    /// Whether execution paused at a configured capacity boundary.
    pub capacity_limited: bool,
    /// Aggregated projection accounting for this invocation.
    pub projection: ContentSyncSummary,
    /// Available disk bytes after the last committed batch.
    pub available_disk_bytes: u64,
    /// Effective reserve: `max(absolute reserve, percentage reserve)`.
    pub required_disk_reserve_bytes: u64,
}

/// Bounds for one continuous durable content-projection pass.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentProjectionOptions {
    /// Maximum graph mutations loaded into one coalesced commit.
    pub batch_size: u32,
    /// Maximum commits admitted in one scheduler pass.
    pub maximum_batches: u32,
}

impl Default for ContentProjectionOptions {
    fn default() -> Self {
        Self {
            batch_size: 1_024,
            maximum_batches: 64,
        }
    }
}

/// Accounting for one bounded graph-outbox to content-index projection pass.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DurableContentProjectionSummary {
    /// Consumer sequence before the pass.
    pub start_sequence: u64,
    /// Last sequence committed and acknowledged.
    pub applied_sequence: u64,
    /// Latest durable graph sequence observed after the pass.
    pub latest_sequence: u64,
    /// Raw durable mutations consumed before per-document coalescing.
    pub projected_mutations: u64,
    /// Whether another bounded pass is required.
    pub backlog_remaining: bool,
    /// Tantivy commits made by this pass.
    pub commits: Vec<ContentSyncSummary>,
}

/// Restart-safe bounded content projector used by CLI and Agent scheduling.
#[derive(Clone)]
pub struct ContentProjectionWorker {
    graph_path: PathBuf,
    content_root: PathBuf,
    policy: ContentIndexPolicy,
}

#[derive(Deserialize)]
struct ProjectionWorkspaceManifest {
    version: u32,
    roots: Vec<String>,
    max_file_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ActiveGenerationPointer {
    version: u32,
    generation_id: String,
    activated_at_unix_ms: u64,
}

#[derive(Deserialize, Serialize)]
struct DurableJsonEnvelope {
    version: u32,
    sequence: u64,
    checksum: u64,
    payload: serde_json::Value,
}

/// One content match. Source text is deliberately absent from the response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentSearchHit {
    /// Canonical catalog projection stored with the matching content generation.
    pub document: CatalogDocument,
}

#[derive(Clone, Copy)]
struct Fields {
    document_id: Field,
    payload: Field,
    content: Field,
    content_hash: Option<Field>,
}

/// Builder for one new content generation.
pub struct ContentIndex;

impl ContentIndex {
    /// Builds a new generation from catalog documents and explicit filesystem policy.
    /// Existing paths are never overwritten.
    ///
    /// # Errors
    ///
    /// Returns an error for an existing destination or index-level failure. Individual source I/O
    /// and text-policy rejections are counted and skipped.
    pub fn build(
        path: &Path,
        documents: impl IntoIterator<Item = CatalogDocument>,
        policy: &ContentIndexPolicy,
    ) -> ContentIndexResult<ContentBuildSummary> {
        Self::build_fallible(path, documents.into_iter().map(Ok), policy)
    }

    /// Builds from bounded pages of durable graph state.
    ///
    /// # Errors
    ///
    /// Returns an error for graph paging, an existing destination, or index-level failure.
    pub fn build_from_graph(
        path: &Path,
        graph: &localsearch_filesystem_graph::FilesystemGraph,
        policy: &ContentIndexPolicy,
    ) -> ContentIndexResult<ContentBuildSummary> {
        Self::build_fallible(path, GraphCatalogIter::new(graph), policy)
    }

    fn build_fallible(
        path: &Path,
        documents: impl IntoIterator<Item = ContentIndexResult<CatalogDocument>>,
        policy: &ContentIndexPolicy,
    ) -> ContentIndexResult<ContentBuildSummary> {
        if path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "content index destination already exists",
            )
            .into());
        }
        fs::create_dir_all(path)?;
        fs::write(path.join(SCHEMA_MARKER), CONTENT_SCHEMA_ID)?;
        let (schema, fields) = schema();
        let directory = MmapDirectory::open(path).map_err(tantivy::TantivyError::from)?;
        let index = Index::create(directory, schema, tantivy::IndexSettings::default())?;
        let mut writer = index.writer(WRITER_HEAP_BYTES)?;
        let mut summary = ContentBuildSummary::default();
        for document in documents {
            let document = document?;
            summary.catalog_documents = summary.catalog_documents.saturating_add(1);
            match extract_text(&document, policy) {
                Extraction::Text(text) => {
                    let hash = content_hash(&text);
                    writer.add_document(index_document(fields, &document, &text, &hash)?)?;
                    summary.indexed_documents = summary.indexed_documents.saturating_add(1);
                }
                Extraction::Skip(reason) => reason.increment(&mut summary),
            }
        }
        summary.generation = writer.commit()?;
        writer.wait_merging_threads()?;
        Ok(summary)
    }

    /// Synchronizes an existing generation to a current catalog snapshot in one atomic commit.
    ///
    /// Eligible documents with an identical catalog projection are not read again. New and
    /// changed documents are extracted under the current policy, while missing, ineligible, or
    /// unreadable documents are deleted. A failure before `commit` leaves the last searchable
    /// generation intact.
    ///
    /// # Errors
    ///
    /// Returns an error for missing/corrupt state, duplicate source identities, or index-level
    /// failure. Individual source I/O and text-policy rejections are counted and skipped.
    pub fn sync(
        path: &Path,
        documents: impl IntoIterator<Item = CatalogDocument>,
        policy: &ContentIndexPolicy,
    ) -> ContentIndexResult<ContentSyncSummary> {
        Self::sync_fallible(path, documents.into_iter().map(Ok), policy)
    }

    /// Synchronizes from bounded pages of durable graph state.
    ///
    /// # Errors
    ///
    /// Returns an error for graph paging, corrupt index state, or index-level failure.
    pub fn sync_from_graph(
        path: &Path,
        graph: &localsearch_filesystem_graph::FilesystemGraph,
        policy: &ContentIndexPolicy,
    ) -> ContentIndexResult<ContentSyncSummary> {
        Self::sync_fallible(path, GraphCatalogIter::new(graph), policy)
    }

    fn sync_fallible(
        path: &Path,
        documents: impl IntoIterator<Item = ContentIndexResult<CatalogDocument>>,
        policy: &ContentIndexPolicy,
    ) -> ContentIndexResult<ContentSyncSummary> {
        let (index, fields) = open_index(path)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let mut previous = stored_documents(&reader, fields)?;
        let mut seen = BTreeSet::new();
        let mut writer = index.writer(WRITER_HEAP_BYTES)?;
        let mut summary = ContentSyncSummary::default();

        for document in documents {
            let document = document?;
            summary.catalog_documents = summary.catalog_documents.saturating_add(1);
            let document_id = document.identity.document_id;
            if !seen.insert(document_id) {
                return Err(ContentIndexError::DuplicateDocument);
            }
            let prior = previous.remove(&document_id);
            let prepared = match prepare_source(&document, policy) {
                Ok(prepared) => prepared,
                Err(reason) => {
                    reason.increment_sync(&mut summary);
                    if prior.is_some() {
                        writer.delete_term(document_id_term(fields, document_id));
                        summary.evicted_documents = summary.evicted_documents.saturating_add(1);
                    }
                    continue;
                }
            };
            if prior
                .as_ref()
                .is_some_and(|stored| stored.document == document)
            {
                summary.unchanged_documents = summary.unchanged_documents.saturating_add(1);
                continue;
            }
            if prior
                .as_ref()
                .is_some_and(|stored| content_source_unchanged(&stored.document, &document))
            {
                summary.metadata_only_documents = summary.metadata_only_documents.saturating_add(1);
                continue;
            }

            let text = match read_text(prepared, policy) {
                Ok(text) => text,
                Err(reason) => {
                    reason.increment_sync(&mut summary);
                    if prior.is_some() {
                        writer.delete_term(document_id_term(fields, document_id));
                        summary.evicted_documents = summary.evicted_documents.saturating_add(1);
                    }
                    continue;
                }
            };
            let hash = content_hash(&text);
            if prior
                .as_ref()
                .and_then(|stored| stored.content_hash.as_deref())
                == Some(hash.as_str())
            {
                summary.unchanged_hash_documents =
                    summary.unchanged_hash_documents.saturating_add(1);
                continue;
            }
            writer.delete_term(document_id_term(fields, document_id));
            writer.add_document(index_document(fields, &document, &text, &hash)?)?;
            if prior.is_some() {
                summary.updated_documents = summary.updated_documents.saturating_add(1);
            } else {
                summary.added_documents = summary.added_documents.saturating_add(1);
            }
        }

        for document_id in previous.keys().copied() {
            writer.delete_term(document_id_term(fields, document_id));
            summary.removed_documents = summary.removed_documents.saturating_add(1);
        }
        summary.generation = writer.commit()?;
        writer.wait_merging_threads()?;
        Ok(summary)
    }

    /// Applies one explicitly bounded document delta in a single idempotent commit.
    ///
    /// This method never enumerates the complete graph or complete content index. Callers must
    /// coalesce an ordered mutation batch so each `DocumentId` appears at most once across upserts
    /// and deletions.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate identities, corrupt index state, or index-level failure.
    pub fn apply_delta(
        path: &Path,
        upserts: impl IntoIterator<Item = CatalogDocument>,
        deletions: impl IntoIterator<Item = DocumentId>,
        policy: &ContentIndexPolicy,
    ) -> ContentIndexResult<ContentSyncSummary> {
        let (index, fields) = open_index(path)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        reader.reload()?;
        let searcher = reader.searcher();
        let mut seen = BTreeSet::new();
        let mut writer = index.writer(WRITER_HEAP_BYTES)?;
        let mut summary = ContentSyncSummary::default();

        for document in upserts {
            summary.catalog_documents = summary.catalog_documents.saturating_add(1);
            let document_id = document.identity.document_id;
            if !seen.insert(document_id) {
                return Err(ContentIndexError::DuplicateDocument);
            }
            let prior = stored_document(&searcher, fields, document_id)?;
            let prepared = match prepare_source(&document, policy) {
                Ok(prepared) => prepared,
                Err(reason) => {
                    reason.increment_sync(&mut summary);
                    if prior.is_some() {
                        writer.delete_term(document_id_term(fields, document_id));
                        summary.evicted_documents = summary.evicted_documents.saturating_add(1);
                    }
                    continue;
                }
            };
            if prior
                .as_ref()
                .is_some_and(|stored| stored.document == document)
            {
                summary.unchanged_documents = summary.unchanged_documents.saturating_add(1);
                continue;
            }
            if prior
                .as_ref()
                .is_some_and(|stored| content_source_unchanged(&stored.document, &document))
            {
                summary.metadata_only_documents = summary.metadata_only_documents.saturating_add(1);
                continue;
            }
            let text = match read_text(prepared, policy) {
                Ok(text) => text,
                Err(reason) => {
                    reason.increment_sync(&mut summary);
                    if prior.is_some() {
                        writer.delete_term(document_id_term(fields, document_id));
                        summary.evicted_documents = summary.evicted_documents.saturating_add(1);
                    }
                    continue;
                }
            };
            let hash = content_hash(&text);
            if prior
                .as_ref()
                .and_then(|stored| stored.content_hash.as_deref())
                == Some(hash.as_str())
            {
                summary.unchanged_hash_documents =
                    summary.unchanged_hash_documents.saturating_add(1);
                continue;
            }
            writer.delete_term(document_id_term(fields, document_id));
            writer.add_document(index_document(fields, &document, &text, &hash)?)?;
            if prior.is_some() {
                summary.updated_documents = summary.updated_documents.saturating_add(1);
            } else {
                summary.added_documents = summary.added_documents.saturating_add(1);
            }
        }

        for document_id in deletions {
            if !seen.insert(document_id) {
                return Err(ContentIndexError::DuplicateDocument);
            }
            if stored_document(&searcher, fields, document_id)?.is_some() {
                writer.delete_term(document_id_term(fields, document_id));
                summary.removed_documents = summary.removed_documents.saturating_add(1);
            }
        }
        summary.generation = writer.commit()?;
        writer.wait_merging_threads()?;
        Ok(summary)
    }

    /// Opens an immutable content reader.
    ///
    /// # Errors
    ///
    /// Returns an error for missing/corrupt state or another schema marker.
    pub fn open(path: &Path) -> ContentIndexResult<ContentReader> {
        let managed = path.join(GENERATION_MANAGER_MARKER).is_file();
        let resolved = resolve_active_index(path)?;
        let (index, fields) = open_index(&resolved)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(ContentReader {
            source: if managed {
                ContentReaderSource::Managed(path.to_path_buf())
            } else {
                ContentReaderSource::Direct
            },
            state: Arc::new(Mutex::new(ContentReaderState {
                path: resolved,
                index,
                reader,
                fields,
            })),
        })
    }
}

/// Manages isolated restartable generations beneath one content-index root.
pub struct ContentGenerationManager {
    root: PathBuf,
}

impl ContentGenerationManager {
    /// Opens or creates a generation manager. A legacy direct index remains readable through
    /// [`ContentIndex::open`] but cannot be silently converted in place.
    ///
    /// # Errors
    ///
    /// Returns an error when the root contains a legacy direct index or cannot be initialized.
    pub fn open(root: impl Into<PathBuf>) -> ContentIndexResult<Self> {
        let root = root.into();
        if root.join(SCHEMA_MARKER).exists() {
            return Err(ContentIndexError::SchemaMismatch);
        }
        fs::create_dir_all(root.join("generations"))?;
        let marker = root.join(GENERATION_MANAGER_MARKER);
        if marker.exists() {
            if fs::read_to_string(&marker)? != CONTENT_SCHEMA_ID {
                return Err(ContentIndexError::SchemaMismatch);
            }
        } else {
            fs::write(marker, CONTENT_SCHEMA_ID)?;
        }
        let manager = Self { root };
        manager.recover_lifecycle()?;
        Ok(manager)
    }

    /// Returns whether an atomically selected active generation exists.
    #[must_use]
    pub fn has_active_generation(&self) -> bool {
        self.active_index_path().is_ok()
    }

    /// Returns whether a restartable candidate is currently building.
    ///
    /// # Errors
    ///
    /// Returns an error when generation metadata cannot be read.
    pub fn has_building_generation(&self) -> ContentIndexResult<bool> {
        Ok(self.building_generation_record()?.is_some())
    }

    /// Loads the newest resumable building generation, if present.
    ///
    /// # Errors
    ///
    /// Returns an error when generation metadata cannot be read.
    pub fn building_generation_record(
        &self,
    ) -> ContentIndexResult<Option<ContentGenerationRecord>> {
        Ok(self
            .records()?
            .into_iter()
            .filter(|record| record.state == ContentGenerationState::Building)
            .max_by_key(|record| record.updated_at_unix_ms))
    }

    /// Loads the record selected by the active pointer.
    ///
    /// # Errors
    ///
    /// Returns an error when the pointer or generation record is unavailable.
    pub fn active_generation_record(&self) -> ContentIndexResult<ContentGenerationRecord> {
        let pointer = self.active_pointer()?;
        if !valid_generation_id(&pointer.generation_id) {
            return Err(ContentIndexError::SchemaMismatch);
        }
        read_json(
            &self
                .root
                .join("generations")
                .join(pointer.generation_id)
                .join(GENERATION_STATE_FILE),
        )
    }

    /// Resolves the active generation index selected by the durable pointer.
    ///
    /// # Errors
    ///
    /// Returns an error when no active generation exists or the pointer is invalid.
    pub fn active_index_path(&self) -> ContentIndexResult<PathBuf> {
        let pointer: ActiveGenerationPointer = read_json(&self.root.join(ACTIVE_GENERATION_FILE))?;
        if pointer.version != GENERATION_MANAGER_VERSION
            || !valid_generation_id(&pointer.generation_id)
        {
            return Err(ContentIndexError::SchemaMismatch);
        }
        let path = self
            .root
            .join("generations")
            .join(pointer.generation_id)
            .join("index");
        if !path.join(SCHEMA_MARKER).is_file() {
            return Err(ContentIndexError::SchemaMismatch);
        }
        Ok(path)
    }

    /// Continues one bounded initial generation from its last committed catalog cursor.
    ///
    /// Each page is a separate Tantivy commit followed by an atomic checkpoint write. Replaying a
    /// page after a commit-before-checkpoint crash is idempotent. The active pointer changes only
    /// after the complete candidate validates successfully.
    ///
    /// # Errors
    ///
    /// Returns an error for corrupt generation state, graph paging, extraction, or index failure.
    pub fn resume_initial_generation(
        &self,
        graph: &localsearch_filesystem_graph::FilesystemGraph,
        policy: &ContentIndexPolicy,
        limits: ContentGenerationLimits,
    ) -> ContentIndexResult<ContentGenerationSummary> {
        let limits = limits.validate()?;
        let roots = policy
            .roots()
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let mut record = self.recover_or_create_building(graph, policy, &roots)?;
        let generation_dir = self.root.join("generations").join(&record.generation_id);
        let index_path = generation_dir.join("index");
        let state_path = generation_dir.join(GENERATION_STATE_FILE);
        let mut projection = ContentSyncSummary::default();
        let mut complete = false;

        for _ in 0..limits.maximum_batches {
            record.capacity_limit = None;
            let disk = disk_space(&self.root)?;
            let reserve = required_disk_reserve(disk.total, limits);
            let current_bytes = directory_bytes(&index_path)?;
            if current_bytes >= limits.max_content_index_bytes {
                record.capacity_limit = Some(ContentCapacityLimit::ContentIndexBytes);
                break;
            }
            if record.documents_projected >= limits.max_documents {
                record.capacity_limit = Some(ContentCapacityLimit::Documents);
                break;
            }
            if disk.available <= reserve.saturating_add(WRITER_HEAP_BYTES as u64) {
                record.capacity_limit = Some(ContentCapacityLimit::FreeDisk);
                break;
            }

            let remaining = limits
                .max_documents
                .saturating_sub(record.documents_projected)
                .min(u64::from(limits.batch_documents));
            let page_limit = u32::try_from(remaining).unwrap_or(limits.batch_documents);
            let page = graph.desired_catalog_page(record.last_checkpoint, page_limit)?;
            if page.is_empty() {
                self.activate_validated(&mut record, &index_path, &state_path)?;
                complete = true;
                break;
            }

            let declared_bytes = page.iter().fold(0_u64, |total, document| {
                total.saturating_add(policy.declared_eligible_bytes(document))
            });
            let forecast = forecast_index_growth(declared_bytes, page.len());
            if current_bytes.saturating_add(forecast) > limits.max_content_index_bytes {
                record.capacity_limit = Some(ContentCapacityLimit::ContentIndexBytes);
                break;
            }
            if disk.available
                <= reserve
                    .saturating_add(forecast)
                    .saturating_add(WRITER_HEAP_BYTES as u64)
            {
                record.capacity_limit = Some(ContentCapacityLimit::FreeDisk);
                break;
            }

            let checkpoint = page
                .last()
                .map(|document| document.identity.document_id)
                .ok_or(ContentIndexError::MissingPayload)?;
            let page_len = u64::try_from(page.len()).unwrap_or(u64::MAX);
            let summary = ContentIndex::apply_delta(&index_path, page, [], policy)?;
            if test_crash_after_commit(record.commits.saturating_add(1)) {
                return Err(ContentIndexError::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "simulated crash after content commit",
                )));
            }
            add_sync_summary(&mut projection, &summary);
            record.last_checkpoint = Some(checkpoint);
            record.documents_seen = record.documents_seen.saturating_add(page_len);
            record.documents_projected = record
                .documents_projected
                .saturating_add(summary.added_documents)
                .saturating_add(summary.unchanged_documents);
            record.bytes_processed = record.bytes_processed.saturating_add(declared_bytes);
            record.commits = record.commits.saturating_add(1);
            record.index_generation = summary.generation;
            record.content_index_bytes = directory_bytes(&index_path)?;
            record.updated_at_unix_ms = unix_time_millis();
            write_json_atomic(&state_path, &record)?;
        }

        record.content_index_bytes = directory_bytes(&index_path)?;
        record.updated_at_unix_ms = unix_time_millis();
        write_json_atomic(&state_path, &record)?;
        let disk = disk_space(&self.root)?;
        Ok(ContentGenerationSummary {
            capacity_limited: record.capacity_limit.is_some(),
            generation: record,
            complete,
            projection,
            available_disk_bytes: disk.available,
            required_disk_reserve_bytes: required_disk_reserve(disk.total, limits),
        })
    }

    /// Deletes failed generations and retired generations beyond the rollback retention count.
    /// The active generation and any resumable building generation are never removed.
    ///
    /// # Errors
    ///
    /// Returns an error when generation metadata or an owned directory cannot be processed.
    pub fn collect_garbage(&self, retain_retired: usize) -> ContentIndexResult<Vec<String>> {
        let active = self
            .active_pointer()
            .ok()
            .map(|pointer| pointer.generation_id);
        let mut retired = Vec::new();
        let mut failed = Vec::new();
        for record in self.records()? {
            match record.state {
                ContentGenerationState::Retired => retired.push(record),
                ContentGenerationState::Failed => failed.push(record),
                ContentGenerationState::Building
                | ContentGenerationState::Ready
                | ContentGenerationState::Active => {}
            }
        }
        retired.sort_by_key(|record| std::cmp::Reverse(record.updated_at_unix_ms));
        let mut removable = failed;
        removable.extend(retired.into_iter().skip(retain_retired));
        let mut removed = Vec::new();
        for record in removable {
            if active.as_deref() == Some(record.generation_id.as_str())
                || !valid_generation_id(&record.generation_id)
            {
                continue;
            }
            let path = self.root.join("generations").join(&record.generation_id);
            if path.parent() != Some(self.root.join("generations").as_path()) {
                return Err(ContentIndexError::SchemaMismatch);
            }
            fs::remove_dir_all(path)?;
            removed.push(record.generation_id);
        }
        Ok(removed)
    }

    fn recover_or_create_building(
        &self,
        graph: &localsearch_filesystem_graph::FilesystemGraph,
        policy: &ContentIndexPolicy,
        roots: &[String],
    ) -> ContentIndexResult<ContentGenerationRecord> {
        let mut building = self
            .records()?
            .into_iter()
            .filter(|record| record.state == ContentGenerationState::Building)
            .collect::<Vec<_>>();
        building.sort_by_key(|record| std::cmp::Reverse(record.updated_at_unix_ms));
        for mut stale in building.iter().skip(1).cloned() {
            stale.state = ContentGenerationState::Failed;
            stale.updated_at_unix_ms = unix_time_millis();
            self.write_record(&stale)?;
        }
        if let Some(mut candidate) = building.into_iter().next() {
            if candidate.version == GENERATION_MANAGER_VERSION && candidate.root_ids == roots {
                candidate.capacity_limit = None;
                return Ok(candidate);
            }
            candidate.state = ContentGenerationState::Failed;
            candidate.updated_at_unix_ms = unix_time_millis();
            self.write_record(&candidate)?;
        }

        let target_sequence = graph.latest_outbox_sequence()?.0;
        let now = unix_time_millis();
        let generation_id = self.unique_generation_id(target_sequence, now)?;
        let generation_dir = self.root.join("generations").join(&generation_id);
        let index_path = generation_dir.join("index");
        fs::create_dir_all(&generation_dir)?;
        let initialized = ContentIndex::build(&index_path, std::iter::empty(), policy)?;
        let record = ContentGenerationRecord {
            version: GENERATION_MANAGER_VERSION,
            scan_id: format!("scan-{generation_id}"),
            generation_id,
            state: ContentGenerationState::Building,
            target_sequence,
            root_ids: roots.to_vec(),
            last_checkpoint: None,
            documents_seen: 0,
            documents_projected: 0,
            bytes_processed: 0,
            commits: 0,
            index_generation: initialized.generation,
            content_index_bytes: directory_bytes(&index_path)?,
            capacity_limit: None,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        };
        self.write_record(&record)?;
        Ok(record)
    }

    fn activate_validated(
        &self,
        record: &mut ContentGenerationRecord,
        index_path: &Path,
        state_path: &Path,
    ) -> ContentIndexResult<()> {
        let count =
            u64::try_from(ContentIndex::open(index_path)?.document_count()?).unwrap_or(u64::MAX);
        if count != record.documents_projected {
            record.state = ContentGenerationState::Failed;
            record.updated_at_unix_ms = unix_time_millis();
            write_json_atomic(state_path, record)?;
            return Err(ContentIndexError::MissingPayload);
        }
        record.state = ContentGenerationState::Ready;
        record.updated_at_unix_ms = unix_time_millis();
        write_json_atomic(state_path, record)?;
        let previous = self.active_pointer().ok();
        let pointer = ActiveGenerationPointer {
            version: GENERATION_MANAGER_VERSION,
            generation_id: record.generation_id.clone(),
            activated_at_unix_ms: unix_time_millis(),
        };
        write_json_atomic(&self.root.join(ACTIVE_GENERATION_FILE), &pointer)?;
        record.state = ContentGenerationState::Active;
        record.updated_at_unix_ms = pointer.activated_at_unix_ms;
        write_json_atomic(state_path, record)?;
        if let Some(previous) = previous
            && previous.generation_id != record.generation_id
            && valid_generation_id(&previous.generation_id)
        {
            let previous_path = self
                .root
                .join("generations")
                .join(previous.generation_id)
                .join(GENERATION_STATE_FILE);
            if let Ok(mut previous_record) = read_json::<ContentGenerationRecord>(&previous_path) {
                previous_record.state = ContentGenerationState::Retired;
                previous_record.updated_at_unix_ms = unix_time_millis();
                write_json_atomic(&previous_path, &previous_record)?;
            }
        }
        let _ = self.collect_garbage(1)?;
        Ok(())
    }

    fn active_pointer(&self) -> ContentIndexResult<ActiveGenerationPointer> {
        read_json(&self.root.join(ACTIVE_GENERATION_FILE))
    }

    fn recover_lifecycle(&self) -> ContentIndexResult<()> {
        let Ok(pointer) = self.active_pointer() else {
            return Ok(());
        };
        if !valid_generation_id(&pointer.generation_id) {
            return Err(ContentIndexError::SchemaMismatch);
        }
        for mut record in self.records()? {
            let desired = if record.generation_id == pointer.generation_id {
                Some(ContentGenerationState::Active)
            } else if record.state == ContentGenerationState::Active {
                Some(ContentGenerationState::Retired)
            } else {
                None
            };
            if let Some(desired) = desired
                && record.state != desired
            {
                record.state = desired;
                record.updated_at_unix_ms = unix_time_millis();
                self.write_record(&record)?;
            }
        }
        Ok(())
    }

    fn records(&self) -> ContentIndexResult<Vec<ContentGenerationRecord>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(self.root.join("generations"))? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !valid_generation_id(&name) {
                continue;
            }
            let state = entry.path().join(GENERATION_STATE_FILE);
            if durable_json_exists(&state) {
                records.push(read_json(&state)?);
            }
        }
        Ok(records)
    }

    fn write_record(&self, record: &ContentGenerationRecord) -> ContentIndexResult<()> {
        write_json_atomic(
            &self
                .root
                .join("generations")
                .join(&record.generation_id)
                .join(GENERATION_STATE_FILE),
            record,
        )
    }

    fn unique_generation_id(&self, sequence: u64, now: u64) -> ContentIndexResult<String> {
        (0_u32..=9_999)
            .map(|suffix| format!("generation-{sequence:020}-{now:020}-{suffix:04}"))
            .find(|candidate| !self.root.join("generations").join(candidate).exists())
            .ok_or_else(|| {
                ContentIndexError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "content generation identifier space is exhausted",
                ))
            })
    }
}

impl ContentProjectionWorker {
    /// Loads an explicitly configured content workspace without accepting caller-supplied roots.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing/unsupported manifest or invalid extraction policy.
    pub fn from_workspace(workspace: impl AsRef<Path>) -> ContentIndexResult<Self> {
        let workspace = workspace.as_ref();
        let manifest: ProjectionWorkspaceManifest =
            read_json(&workspace.join("content-workspace.json"))?;
        if !matches!(manifest.version, 1 | 2) {
            return Err(ContentIndexError::SchemaMismatch);
        }
        let policy = ContentIndexPolicy::new(
            manifest.roots.into_iter().map(PathBuf::from),
            manifest.max_file_bytes,
        )?;
        Ok(Self {
            graph_path: workspace.join("graph.sqlite3"),
            content_root: workspace.join("content-index-v1"),
            policy,
        })
    }

    /// Returns the durable content-consumer backlog.
    ///
    /// # Errors
    ///
    /// Returns an error when graph state is unavailable or initial projection is not complete.
    pub fn backlog(&self) -> ContentIndexResult<u64> {
        let graph =
            localsearch_filesystem_graph::FilesystemGraph::open_read_only(&self.graph_path)?;
        let applied = graph
            .projector_checkpoint(CONTENT_SCHEMA_ID)?
            .ok_or(ContentIndexError::MissingPayload)?
            .last_sequence;
        Ok(graph.latest_outbox_sequence()?.0.saturating_sub(applied))
    }

    /// Projects bounded graph outbox batches into the active content generation.
    ///
    /// Each batch is coalesced by stable `DocumentId`, committed to Tantivy, and only then
    /// acknowledged in `SQLite`. Commit-before-ACK replay remains idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bounds, unavailable active generation, or durable failures.
    pub fn project(
        &self,
        options: ContentProjectionOptions,
    ) -> ContentIndexResult<DurableContentProjectionSummary> {
        if options.batch_size == 0
            || options.batch_size > 10_000
            || options.maximum_batches == 0
            || options.maximum_batches > 1_024
        {
            return Err(ContentIndexError::InvalidPolicy);
        }
        let graph = localsearch_filesystem_graph::FilesystemGraph::open(&self.graph_path)?;
        let checkpoint = graph
            .projector_checkpoint(CONTENT_SCHEMA_ID)?
            .ok_or(ContentIndexError::MissingPayload)?;
        let content_path = if self.content_root.join(SCHEMA_MARKER).is_file() {
            self.content_root.clone()
        } else {
            ContentGenerationManager::open(&self.content_root)?.active_index_path()?
        };
        let start_sequence = checkpoint.last_sequence;
        let mut after = MutationSeq(start_sequence);
        let mut commits = Vec::new();
        let mut projected_mutations = 0_u64;

        for _ in 0..options.maximum_batches {
            let batch = graph.read_outbox(Some(after), options.batch_size)?;
            if batch.mutations.is_empty() {
                break;
            }
            batch
                .validate()
                .map_err(|error| ContentIndexError::Io(std::io::Error::other(error.to_string())))?;
            let mut coalesced = BTreeMap::new();
            for sequenced in &batch.mutations {
                match &sequenced.mutation {
                    IndexMutation::Upsert { document } => {
                        coalesced.insert(document.identity.document_id, Some(document.clone()));
                    }
                    IndexMutation::Delete { document_id, .. } => {
                        coalesced.insert(*document_id, None);
                    }
                }
            }
            let upserts = coalesced
                .values()
                .filter_map(Clone::clone)
                .collect::<Vec<_>>();
            let deletions = coalesced
                .into_iter()
                .filter_map(|(document_id, document)| document.is_none().then_some(document_id))
                .collect::<Vec<_>>();
            let summary =
                ContentIndex::apply_delta(&content_path, upserts, deletions, &self.policy)?;
            let last = batch
                .last_sequence()
                .ok_or(ContentIndexError::MissingPayload)?;
            graph.acknowledge_projection(CONTENT_SCHEMA_ID, last, summary.generation)?;
            projected_mutations = projected_mutations
                .saturating_add(u64::try_from(batch.mutations.len()).unwrap_or(u64::MAX));
            after = last;
            commits.push(summary);
        }
        let latest = graph.latest_outbox_sequence()?.0;
        Ok(DurableContentProjectionSummary {
            start_sequence,
            applied_sequence: after.0,
            latest_sequence: latest,
            projected_mutations,
            backlog_remaining: after.0 < latest,
            commits,
        })
    }
}

#[derive(Clone, Copy)]
struct DiskSpace {
    total: u64,
    available: u64,
}

fn resolve_active_index(path: &Path) -> ContentIndexResult<PathBuf> {
    if path.join(SCHEMA_MARKER).is_file() {
        return Ok(path.to_path_buf());
    }
    if fs::read_to_string(path.join(GENERATION_MANAGER_MARKER))? != CONTENT_SCHEMA_ID {
        return Err(ContentIndexError::SchemaMismatch);
    }
    let pointer: ActiveGenerationPointer = read_json(&path.join(ACTIVE_GENERATION_FILE))?;
    if pointer.version != GENERATION_MANAGER_VERSION || !valid_generation_id(&pointer.generation_id)
    {
        return Err(ContentIndexError::SchemaMismatch);
    }
    Ok(path
        .join("generations")
        .join(pointer.generation_id)
        .join("index"))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> ContentIndexResult<T> {
    let mut valid = durable_slots(path)
        .into_iter()
        .filter_map(|slot| {
            let bytes = fs::read(slot).ok()?;
            let envelope = serde_json::from_slice::<DurableJsonEnvelope>(&bytes).ok()?;
            (envelope.version == GENERATION_MANAGER_VERSION
                && envelope.checksum == json_checksum(&envelope.payload))
            .then_some(envelope)
        })
        .collect::<Vec<_>>();
    valid.sort_by_key(|envelope| std::cmp::Reverse(envelope.sequence));
    if let Some(envelope) = valid.into_iter().next() {
        return Ok(serde_json::from_value(envelope.payload)?);
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> ContentIndexResult<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "state path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let sequence = durable_slots(path)
        .into_iter()
        .filter_map(|slot| fs::read(slot).ok())
        .filter_map(|bytes| serde_json::from_slice::<DurableJsonEnvelope>(&bytes).ok())
        .filter(|envelope| envelope.checksum == json_checksum(&envelope.payload))
        .map(|envelope| envelope.sequence)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let payload = serde_json::to_value(value)?;
    let envelope = DurableJsonEnvelope {
        version: GENERATION_MANAGER_VERSION,
        sequence,
        checksum: json_checksum(&payload),
        payload,
    };
    let slots = durable_slots(path);
    let slot = &slots[usize::try_from(sequence % 2).unwrap_or(0)];
    let bytes = serde_json::to_vec_pretty(&envelope)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(slot)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn durable_slots(path: &Path) -> [PathBuf; 2] {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    [
        parent.join(format!("{name}.slot0")),
        parent.join(format!("{name}.slot1")),
    ]
}

fn durable_json_exists(path: &Path) -> bool {
    path.is_file() || durable_slots(path).iter().any(|slot| slot.is_file())
}

fn json_checksum(value: &serde_json::Value) -> u64 {
    serde_json::to_vec(value).map_or(0, |bytes| {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    })
}

fn valid_generation_id(value: &str) -> bool {
    value.starts_with("generation-")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn directory_bytes(path: &Path) -> ContentIndexResult<u64> {
    fn visit(path: &Path, total: &mut u64) -> std::io::Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                visit(&entry.path(), total)?;
            } else if metadata.is_file() {
                *total = total.saturating_add(metadata.len());
            }
        }
        Ok(())
    }
    let mut total = 0_u64;
    visit(path, &mut total)?;
    Ok(total)
}

fn disk_space(path: &Path) -> ContentIndexResult<DiskSpace> {
    let path = normalized_disk_path(path);
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter_map(|disk| {
            let mount = normalized_disk_path(disk.mount_point());
            path.starts_with(&mount).then_some((mount.len(), disk))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, disk)| DiskSpace {
            total: disk.total_space(),
            available: disk.available_space(),
        })
        .ok_or_else(|| {
            ContentIndexError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "content workspace disk could not be resolved",
            ))
        })
}

fn normalized_disk_path(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .to_lowercase()
}

fn required_disk_reserve(total: u64, limits: ContentGenerationLimits) -> u64 {
    let percentage = total
        .saturating_mul(u64::from(limits.min_free_disk_percent))
        .saturating_div(100);
    limits.min_free_disk_bytes.max(percentage)
}

fn forecast_index_growth(declared_source_bytes: u64, documents: usize) -> u64 {
    declared_source_bytes.saturating_mul(2).saturating_add(
        u64::try_from(documents)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_024),
    )
}

fn add_sync_summary(total: &mut ContentSyncSummary, value: &ContentSyncSummary) {
    total.catalog_documents = total
        .catalog_documents
        .saturating_add(value.catalog_documents);
    total.unchanged_documents = total
        .unchanged_documents
        .saturating_add(value.unchanged_documents);
    total.metadata_only_documents = total
        .metadata_only_documents
        .saturating_add(value.metadata_only_documents);
    total.unchanged_hash_documents = total
        .unchanged_hash_documents
        .saturating_add(value.unchanged_hash_documents);
    total.added_documents = total.added_documents.saturating_add(value.added_documents);
    total.updated_documents = total
        .updated_documents
        .saturating_add(value.updated_documents);
    total.removed_documents = total
        .removed_documents
        .saturating_add(value.removed_documents);
    total.evicted_documents = total
        .evicted_documents
        .saturating_add(value.evicted_documents);
    total.skipped_outside_roots = total
        .skipped_outside_roots
        .saturating_add(value.skipped_outside_roots);
    total.skipped_unavailable = total
        .skipped_unavailable
        .saturating_add(value.skipped_unavailable);
    total.skipped_extension = total
        .skipped_extension
        .saturating_add(value.skipped_extension);
    total.skipped_too_large = total
        .skipped_too_large
        .saturating_add(value.skipped_too_large);
    total.skipped_non_text = total
        .skipped_non_text
        .saturating_add(value.skipped_non_text);
    total.skipped_io = total.skipped_io.saturating_add(value.skipped_io);
    total.generation = value.generation;
}

fn test_crash_after_commit(commit: u64) -> bool {
    std::env::var("LOCALSEARCH_TEST_CRASH_AFTER_CONTENT_COMMITS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        == Some(commit)
}

struct GraphCatalogIter<'a> {
    graph: &'a localsearch_filesystem_graph::FilesystemGraph,
    after: Option<DocumentId>,
    page: std::vec::IntoIter<CatalogDocument>,
    done: bool,
}

impl<'a> GraphCatalogIter<'a> {
    fn new(graph: &'a localsearch_filesystem_graph::FilesystemGraph) -> Self {
        Self {
            graph,
            after: None,
            page: Vec::new().into_iter(),
            done: false,
        }
    }
}

impl Iterator for GraphCatalogIter<'_> {
    type Item = ContentIndexResult<CatalogDocument>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(document) = self.page.next() {
                self.after = Some(document.identity.document_id);
                return Some(Ok(document));
            }
            if self.done {
                return None;
            }
            match self.graph.desired_catalog_page(self.after, 4_096) {
                Ok(page) if page.is_empty() => self.done = true,
                Ok(page) => self.page = page.into_iter(),
                Err(error) => {
                    self.done = true;
                    return Some(Err(error.into()));
                }
            }
        }
    }
}

/// Reader over atomically committed opt-in content generations.
#[derive(Clone)]
pub struct ContentReader {
    source: ContentReaderSource,
    state: Arc<Mutex<ContentReaderState>>,
}

#[derive(Clone)]
enum ContentReaderSource {
    Direct,
    Managed(PathBuf),
}

struct ContentReaderState {
    path: PathBuf,
    index: Index,
    reader: IndexReader,
    fields: Fields,
}

impl ContentReader {
    /// Searches indexed content and returns only stored catalog metadata.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/oversized query, an invalid limit, or index corruption.
    pub fn search(&self, query: &str, limit: usize) -> ContentIndexResult<Vec<ContentSearchHit>> {
        let query = query.trim();
        if query.is_empty() || query.len() > 1_024 || !(1..=100).contains(&limit) {
            return Err(ContentIndexError::Query);
        }
        let state = self.current_state()?;
        let parser = QueryParser::for_index(&state.index, vec![state.fields.content]);
        let parsed = parser
            .parse_query(query)
            .map_err(|_| ContentIndexError::Query)?;
        let parsed = with_single_token_prefix(&state.index, state.fields, query, parsed)?;
        state.reader.reload()?;
        let searcher = state.reader.searcher();
        let addresses = bounded_unscored_addresses(&searcher, parsed.as_ref(), limit)?;
        addresses
            .into_iter()
            .map(|address| {
                let stored: TantivyDocument = searcher.doc(address)?;
                let payload = stored
                    .get_first(state.fields.payload)
                    .and_then(|value| value.as_str())
                    .ok_or(ContentIndexError::MissingPayload)?;
                let document = serde_json::from_str(payload)?;
                Ok(ContentSearchHit { document })
            })
            .collect()
    }

    /// Counts live content documents.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot read the index.
    pub fn document_count(&self) -> ContentIndexResult<usize> {
        let state = self.current_state()?;
        state.reader.reload()?;
        Ok(state.reader.searcher().search(&AllQuery, &Count)?)
    }

    fn current_state(&self) -> ContentIndexResult<MutexGuard<'_, ContentReaderState>> {
        let mut state = self.state.lock().map_err(|_| {
            ContentIndexError::Io(std::io::Error::other("content reader lock is unavailable"))
        })?;
        if let ContentReaderSource::Managed(root) = &self.source {
            let active = resolve_active_index(root)?;
            if active != state.path {
                let (index, fields) = open_index(&active)?;
                let reader = index
                    .reader_builder()
                    .reload_policy(ReloadPolicy::Manual)
                    .try_into()?;
                *state = ContentReaderState {
                    path: active,
                    index,
                    reader,
                    fields,
                };
            }
        }
        Ok(state)
    }
}

fn bounded_unscored_addresses(
    searcher: &Searcher,
    query: &dyn Query,
    limit: usize,
) -> ContentIndexResult<Vec<DocAddress>> {
    let weight = query.weight(EnableScoring::disabled_from_searcher(searcher))?;
    let mut addresses = Vec::with_capacity(limit);
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let mut scorer = weight.scorer(segment, 1.0)?;
        let mut doc = scorer.doc();
        while doc != TERMINATED && addresses.len() < limit {
            if segment
                .alive_bitset()
                .is_none_or(|alive| alive.is_alive(doc))
            {
                addresses.push(DocAddress::new(
                    u32::try_from(segment_ord).unwrap_or(u32::MAX),
                    doc,
                ));
            }
            doc = scorer.advance();
        }
        if addresses.len() == limit {
            break;
        }
    }
    Ok(addresses)
}

fn with_single_token_prefix(
    index: &Index,
    fields: Fields,
    query: &str,
    parsed: Box<dyn Query>,
) -> ContentIndexResult<Box<dyn Query>> {
    if query.chars().count() < MIN_CONTENT_PREFIX_CHARS
        || !query
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
    {
        return Ok(parsed);
    }
    let mut analyzer = index.tokenizer_for_field(fields.content)?;
    let mut stream = analyzer.token_stream(query);
    let mut token = None;
    while stream.advance() {
        if token.is_some() {
            return Ok(parsed);
        }
        token = Some(stream.token().text.clone());
    }
    let Some(token) = token else {
        return Ok(parsed);
    };
    let prefix = FuzzyTermQuery::new_prefix(Term::from_field_text(fields.content, &token), 0, true);
    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Should, parsed),
        (Occur::Should, Box::new(prefix)),
    ])))
}

fn open_index(path: &Path) -> ContentIndexResult<(Index, Fields)> {
    let marker = fs::read_to_string(path.join(SCHEMA_MARKER))?;
    if marker != CONTENT_SCHEMA_ID {
        return Err(ContentIndexError::SchemaMismatch);
    }
    let directory = MmapDirectory::open(path).map_err(tantivy::TantivyError::from)?;
    let index = Index::open(directory)?;
    let schema = index.schema();
    let fields = Fields {
        document_id: schema.get_field("document_id")?,
        payload: schema.get_field("payload")?,
        content: schema.get_field("content")?,
        content_hash: schema.get_field("content_hash").ok(),
    };
    Ok((index, fields))
}

struct StoredContent {
    document: CatalogDocument,
    content_hash: Option<String>,
}

fn stored_documents(
    reader: &IndexReader,
    fields: Fields,
) -> ContentIndexResult<BTreeMap<DocumentId, StoredContent>> {
    let searcher = reader.searcher();
    let mut documents = BTreeMap::new();
    for segment in searcher.segment_readers() {
        let store = segment.get_store_reader(4)?;
        for doc_id in segment.doc_ids_alive() {
            let stored: TantivyDocument = store.get(doc_id)?;
            let payload = stored
                .get_first(fields.payload)
                .and_then(|value| value.as_str())
                .ok_or(ContentIndexError::MissingPayload)?;
            let document: CatalogDocument = serde_json::from_str(payload)?;
            let content_hash = fields.content_hash.and_then(|field| {
                stored
                    .get_first(field)
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            });
            documents.insert(
                document.identity.document_id,
                StoredContent {
                    document,
                    content_hash,
                },
            );
        }
    }
    Ok(documents)
}

fn stored_document(
    searcher: &Searcher,
    fields: Fields,
    document_id: DocumentId,
) -> ContentIndexResult<Option<StoredContent>> {
    let query = TermQuery::new(
        document_id_term(fields, document_id),
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
        .get_first(fields.payload)
        .and_then(|value| value.as_str())
        .ok_or(ContentIndexError::MissingPayload)?;
    let document = serde_json::from_str(payload)?;
    let content_hash = fields.content_hash.and_then(|field| {
        stored
            .get_first(field)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    });
    Ok(Some(StoredContent {
        document,
        content_hash,
    }))
}

enum Extraction {
    Text(String),
    Skip(SkipReason),
}

#[derive(Clone, Copy)]
enum SkipReason {
    OutsideRoots,
    Unavailable,
    Extension,
    TooLarge,
    NonText,
    Io,
}

impl SkipReason {
    fn increment(self, summary: &mut ContentBuildSummary) {
        let value = match self {
            Self::OutsideRoots => &mut summary.skipped_outside_roots,
            Self::Unavailable => &mut summary.skipped_unavailable,
            Self::Extension => &mut summary.skipped_extension,
            Self::TooLarge => &mut summary.skipped_too_large,
            Self::NonText => &mut summary.skipped_non_text,
            Self::Io => &mut summary.skipped_io,
        };
        *value = value.saturating_add(1);
    }

    fn increment_sync(self, summary: &mut ContentSyncSummary) {
        let value = match self {
            Self::OutsideRoots => &mut summary.skipped_outside_roots,
            Self::Unavailable => &mut summary.skipped_unavailable,
            Self::Extension => &mut summary.skipped_extension,
            Self::TooLarge => &mut summary.skipped_too_large,
            Self::NonText => &mut summary.skipped_non_text,
            Self::Io => &mut summary.skipped_io,
        };
        *value = value.saturating_add(1);
    }
}

fn extract_text(document: &CatalogDocument, policy: &ContentIndexPolicy) -> Extraction {
    let prepared = match prepare_source(document, policy) {
        Ok(prepared) => prepared,
        Err(reason) => return Extraction::Skip(reason),
    };
    match read_text(prepared, policy) {
        Ok(text) => Extraction::Text(text),
        Err(reason) => Extraction::Skip(reason),
    }
}

struct PreparedSource {
    canonical: PathBuf,
    length: u64,
}

fn prepare_source(
    document: &CatalogDocument,
    policy: &ContentIndexPolicy,
) -> Result<PreparedSource, SkipReason> {
    if document.metadata.kind != FileKind::File
        || document.metadata.availability != Availability::Online
    {
        return Err(SkipReason::Unavailable);
    }
    let Some(extension) = document.extension.as_deref() else {
        return Err(SkipReason::Extension);
    };
    if !policy.extensions.contains(&extension.to_lowercase()) {
        return Err(SkipReason::Extension);
    }
    if document.metadata.size > policy.max_file_bytes {
        return Err(SkipReason::TooLarge);
    }
    let path = Path::new(&document.resolved_path);
    let Ok(canonical) = fs::canonicalize(path) else {
        return Err(SkipReason::Io);
    };
    if !policy.roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(SkipReason::OutsideRoots);
    }
    let Ok(metadata) = canonical.metadata() else {
        return Err(SkipReason::Io);
    };
    if !metadata.is_file() {
        return Err(SkipReason::Unavailable);
    }
    if metadata.len() > policy.max_file_bytes {
        return Err(SkipReason::TooLarge);
    }
    Ok(PreparedSource {
        canonical,
        length: metadata.len(),
    })
}

fn read_text(prepared: PreparedSource, policy: &ContentIndexPolicy) -> Result<String, SkipReason> {
    let Ok(file) = File::open(prepared.canonical) else {
        return Err(SkipReason::Io);
    };
    let mut bytes = Vec::with_capacity(usize::try_from(prepared.length).unwrap_or(0));
    if file
        .take(policy.max_file_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Err(SkipReason::Io);
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > policy.max_file_bytes {
        return Err(SkipReason::TooLarge);
    }
    if bytes.contains(&0) {
        return Err(SkipReason::NonText);
    }
    String::from_utf8(bytes).map_err(|_| SkipReason::NonText)
}

fn document_id_term(fields: Fields, document_id: DocumentId) -> Term {
    Term::from_field_text(fields.document_id, &document_id.to_string())
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let document_id = builder.add_text_field("document_id", STRING | STORED);
    let payload = builder.add_text_field("payload", STORED);
    let content = builder.add_text_field("content", TEXT);
    let content_hash = builder.add_text_field("content_hash", STRING | STORED);
    let schema = builder.build();
    (
        schema,
        Fields {
            document_id,
            payload,
            content,
            content_hash: Some(content_hash),
        },
    )
}

fn index_document(
    fields: Fields,
    document: &CatalogDocument,
    content: &str,
    content_hash: &str,
) -> ContentIndexResult<TantivyDocument> {
    let mut indexed = TantivyDocument::default();
    indexed.add_text(
        fields.document_id,
        document.identity.document_id.to_string(),
    );
    indexed.add_text(fields.payload, serde_json::to_string(document)?);
    indexed.add_text(fields.content, content);
    if let Some(field) = fields.content_hash {
        indexed.add_text(field, content_hash);
    }
    Ok(indexed)
}

fn content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn content_source_unchanged(previous: &CatalogDocument, current: &CatalogDocument) -> bool {
    previous.identity.object_key == current.identity.object_key
        && previous.metadata.kind == current.metadata.kind
        && previous.metadata.size == current.metadata.size
        && previous.metadata.modified_at_unix_ms.is_some()
        && previous.metadata.modified_at_unix_ms == current.metadata.modified_at_unix_ms
        && previous.metadata.availability == current.metadata.availability
}
