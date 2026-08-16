use serde::{Deserialize, Serialize};

use crate::{
    CatalogIdentity, DocumentId, DocumentVersion, DomainError, DomainResult, FileKey, FileLinkId,
    IndexGeneration, MutationSeq, RankingVersion, VolumeId,
};

/// User-visible search scope available in v0.1.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchScope {
    /// Files and directories.
    #[default]
    All,
    /// Files only.
    Files,
    /// Directories only.
    Folders,
}

/// Backend-neutral catalog filters.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchFilter {
    /// Normalized extensions without a leading dot.
    pub extensions: Vec<String>,
    /// Optional normalized directory-prefix filter.
    pub directory_prefix: Option<String>,
    /// Inclusive minimum file size in bytes.
    pub minimum_size: Option<u64>,
    /// Inclusive maximum file size in bytes.
    pub maximum_size: Option<u64>,
}

/// Backend-neutral catalog search request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchRequest {
    /// User query before backend planning.
    pub query: String,
    /// Catalog scope.
    pub scope: SearchScope,
    /// Structured filters.
    pub filters: SearchFilter,
    /// Requested result count; adapters and the service clamp this value.
    pub top_k: u16,
}

/// Filesystem object category used by the catalog.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
    /// Socket, device, named pipe, or another non-regular filesystem object.
    Special,
    /// Provider-specific or currently unknown type.
    Other,
}

/// Current availability of the source object or volume.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Source can currently be resolved.
    Online,
    /// Volume/source is known but disconnected.
    Offline,
    /// Source visibility or resolution is incomplete.
    Unknown,
}

/// Canonical filesystem metadata shared by platform adapters and search projections.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileMetadata {
    /// Object category.
    pub kind: FileKind,
    /// Size in bytes.
    pub size: u64,
    /// Creation Unix timestamp in milliseconds when the provider can supply one.
    pub created_at_unix_ms: Option<i64>,
    /// Last-modified Unix timestamp in milliseconds when known.
    pub modified_at_unix_ms: Option<i64>,
    /// Hidden projection according to platform policy.
    pub hidden: bool,
    /// Current source availability.
    pub availability: Availability,
}

/// Current observation of one physical filesystem object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileObjectSnapshot {
    /// Physical object identity.
    pub object_key: FileKey,
    /// Platform-neutral metadata.
    pub metadata: FileMetadata,
}

/// Current observation of one filesystem namespace link.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileLinkSnapshot {
    /// Stable link identity assigned or derived by the platform adapter.
    pub file_link_id: FileLinkId,
    /// Linked physical object.
    pub object_key: FileKey,
    /// Parent directory object, or `None` for a provider root.
    pub parent_key: Option<FileKey>,
    /// Native display name encoded as Unicode without a full path identity.
    pub name: String,
}

/// Reason the durable filesystem graph needs reconciliation with its source.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationReason {
    /// The provider's persistent history no longer covers the saved checkpoint.
    SourceHistoryUnavailable,
    /// An ephemeral watcher queue overflowed or lost events.
    EventOverflow,
    /// Snapshot-to-change-stream handoff could not be proven continuous.
    InconsistentSnapshot,
    /// The platform adapter detected an integrity condition requiring comparison.
    ProviderRequested,
}

/// Platform-neutral, idempotent filesystem observation consumed by the state pipeline.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum FilesystemEvent {
    /// Create or replace the latest known object metadata.
    ObjectObserved {
        /// Current object state.
        object: FileObjectSnapshot,
    },
    /// Create or replace the latest known namespace-link state.
    LinkObserved {
        /// Current link state.
        link: FileLinkSnapshot,
    },
    /// Remove a namespace link if it is still current.
    LinkRemoved {
        /// Link identity.
        file_link_id: FileLinkId,
        /// Linked object identity used for consistency checks.
        object_key: FileKey,
    },
    /// Remove a physical object if no newer observation exists.
    ObjectRemoved {
        /// Physical object identity.
        object_key: FileKey,
    },
    /// Request source reconciliation without leaking a platform-native reason.
    ReconciliationRequired {
        /// Affected volume.
        volume_id: VolumeId,
        /// Canonical recovery category.
        reason: ReconciliationReason,
    },
}

/// Stable primary match classification used by product ranking.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchType {
    /// Exact normalized filename match.
    ExactName,
    /// Normalized filename prefix match.
    PrefixName,
    /// Filename token match.
    TokenName,
    /// Verified filename substring match.
    SubstringName,
    /// Path-derived match.
    Path,
}

/// Searchable, backend-neutral catalog projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogDocument {
    /// Stable object/link/document identity.
    pub identity: CatalogIdentity,
    /// Logical projection version used for idempotent updates.
    pub document_version: DocumentVersion,
    /// Current display name.
    pub name: String,
    /// Current resolved full path projection.
    pub resolved_path: String,
    /// Normalized extension without a leading dot.
    pub extension: Option<String>,
    /// Current platform-neutral object metadata.
    pub metadata: FileMetadata,
}

impl CatalogDocument {
    /// Returns the stable identity, independent of name and resolved path.
    #[must_use]
    pub const fn identity(&self) -> CatalogIdentity {
        self.identity
    }
}

/// One ranked catalog result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchHit {
    /// Search projection identity.
    pub document_id: DocumentId,
    /// Physical object identity.
    pub object_key: FileKey,
    /// Namespace-link identity.
    pub file_link_id: FileLinkId,
    /// Current display name.
    pub name: String,
    /// Current full resolved path.
    pub resolved_path: String,
    /// Normalized extension without a leading dot.
    pub extension: Option<String>,
    /// Object category.
    pub kind: FileKind,
    /// Size in bytes.
    pub size: u64,
    /// Last-modified Unix timestamp in milliseconds when known.
    pub modified_at_unix_ms: Option<i64>,
    /// Current source availability.
    pub availability: Availability,
    /// Stable primary match classification.
    pub match_type: MatchType,
    /// One-based position in this response.
    pub rank: u32,
    /// Product ranking semantics used for this hit.
    pub ranking_version: RankingVersion,
}

/// Backend-neutral catalog response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchResponse {
    /// Active materialized-index generation used by the query.
    pub index_generation: IndexGeneration,
    /// Service-side elapsed time in microseconds.
    pub took_micros: u64,
    /// Bounded ranked results.
    pub hits: Vec<SearchHit>,
}

/// Idempotent mutation against a catalog projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum IndexMutation {
    /// Delete the previous document ID and add the supplied current projection.
    Upsert {
        /// Latest canonical projection.
        document: CatalogDocument,
    },
    /// Delete the projection if present.
    Delete {
        /// Stable projection identity.
        document_id: DocumentId,
        /// Logical version of this deletion.
        document_version: DocumentVersion,
    },
}

/// One mutation at a durable outbox sequence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SequencedMutation {
    /// Durable mutation sequence.
    pub sequence: MutationSeq,
    /// Idempotent operation.
    pub mutation: IndexMutation,
}

/// Consecutive, bounded mutation unit applied by an index writer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationBatch {
    /// Ordered mutations. A valid batch is non-empty and strictly consecutive.
    pub mutations: Vec<SequencedMutation>,
}

impl MutationBatch {
    /// Validates non-empty, strictly consecutive outbox sequencing.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidRequest`] when the batch is empty, contains a sequence gap,
    /// or reaches sequence-number overflow.
    pub fn validate(&self) -> DomainResult<()> {
        let Some(first) = self.mutations.first() else {
            return Err(DomainError::InvalidRequest {
                reason: "mutation batch must not be empty".to_owned(),
            });
        };

        let mut expected = first.sequence.0;
        for mutation in &self.mutations {
            if mutation.sequence.0 != expected {
                return Err(DomainError::InvalidRequest {
                    reason: "mutation batch sequences must be strictly consecutive".to_owned(),
                });
            }
            expected = expected.checked_add(1).ok_or(DomainError::InvalidRequest {
                reason: "mutation sequence overflow".to_owned(),
            })?;
        }

        Ok(())
    }

    /// Returns the first sequence in a non-empty batch.
    #[must_use]
    pub fn first_sequence(&self) -> Option<MutationSeq> {
        self.mutations.first().map(|item| item.sequence)
    }

    /// Returns the last sequence in a non-empty batch.
    #[must_use]
    pub fn last_sequence(&self) -> Option<MutationSeq> {
        self.mutations.last().map(|item| item.sequence)
    }
}
