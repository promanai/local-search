use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::{
        Mutex, RwLock,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    time::{Duration, Instant},
};

use localsearch_agent_api::{
    AGENT_API_VERSION, AGENT_CODEC_VERSION, AgentErrorCode, AgentRequest, AgentResponse,
    AgentResult, AgentWireError, Capabilities, CapabilitiesPort, Capability, CatalogItem,
    CatalogLookupPort, CatalogSearchPort, ContentSearchHit, ContentSearchResponse, IndexStatus,
    IndexStatusPort, MAX_FRAME_BYTES, MAX_TOP_K, RequestOperation, ResponsePayload, ServiceHealth,
};
use localsearch_catalog_index::{
    CATALOG_SCHEMA_ID, CatalogQueryMode, CatalogReader, ProjectionWorker, ProjectionWorkerError,
    ProjectionWorkerOptions,
};
use localsearch_content_index::{
    CONTENT_SCHEMA_ID, ContentProjectionOptions, ContentProjectionWorker, ContentReader,
    DurableContentProjectionSummary,
};
use localsearch_core::{
    CatalogDocument, DocumentId, IndexGeneration, MatchType, RankingVersion, SearchHit,
    SearchRequest, SearchResponse, SearchScope,
};
use localsearch_filesystem_graph::FilesystemGraph;
use localsearch_resource_governor::{GovernorDecision, ResourceGovernor, SystemPressure};
use unicode_normalization::UnicodeNormalization;

const RANKING_VERSION: RankingVersion = RankingVersion::new(1);
const MAX_CANDIDATES: usize = 5_000;
static SEARCH_EVIDENCE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Capability set derived from trusted endpoint/client configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientAuthorization {
    granted: BTreeSet<Capability>,
}

impl ClientAuthorization {
    /// Creates an authorization grant supplied by the authenticated transport.
    #[must_use]
    pub fn new(granted: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            granted: granted.into_iter().collect(),
        }
    }

    /// Standard v0.1 local desktop/CLI metadata grant.
    #[must_use]
    pub fn v0_1_metadata() -> Self {
        Self::new([
            Capability::SearchCatalog,
            Capability::ReadMetadata,
            Capability::IndexStatus,
        ])
    }

    /// Local desktop/CLI grant including explicitly configured content search.
    #[must_use]
    pub fn v0_2_with_content() -> Self {
        Self::new([
            Capability::SearchCatalog,
            Capability::SearchContent,
            Capability::ReadMetadata,
            Capability::IndexStatus,
        ])
    }

    fn permits(&self, capability: Capability) -> bool {
        self.granted.contains(&capability)
    }
}

struct AgentState {
    graph: FilesystemGraph,
    reader: CatalogReader,
}

struct CachedCatalogReader {
    reader: CatalogReader,
    generation: IndexGeneration,
}

struct CatalogSnapshot {
    reader: CatalogReader,
    generation: IndexGeneration,
}

/// `LocalSearch` application service owning durable graph and materialized catalog readers.
pub struct AgentService {
    graph_path: PathBuf,
    index_root: PathBuf,
    projection_writer: Mutex<()>,
    governor: Mutex<ResourceGovernor>,
    catalog_reader: RwLock<CachedCatalogReader>,
    content_reader: Option<ContentReader>,
    content_projector: Option<ContentProjectionWorker>,
    content_projection_writer: Mutex<()>,
    authorization: ClientAuthorization,
}

impl AgentService {
    /// Opens durable state, recovers/replays the catalog projection, and becomes query-ready.
    ///
    /// # Errors
    ///
    /// Returns a redacted internal error if state or projection cannot become consistent.
    pub fn open(
        graph_path: impl AsRef<Path>,
        index_root: impl AsRef<Path>,
        authorization: ClientAuthorization,
    ) -> AgentResult<Self> {
        Self::open_with_content(graph_path, index_root, None::<&Path>, authorization)
    }

    /// Opens the Agent with an optional immutable opt-in content generation.
    ///
    /// # Errors
    ///
    /// Returns a redacted internal error if durable catalog or configured content state is invalid.
    pub fn open_with_content(
        graph_path: impl AsRef<Path>,
        index_root: impl AsRef<Path>,
        content_index: Option<impl AsRef<Path>>,
        authorization: ClientAuthorization,
    ) -> AgentResult<Self> {
        let graph_path = graph_path.as_ref().to_owned();
        let index_root = index_root.as_ref().to_owned();
        let graph = FilesystemGraph::open(&graph_path).map_err(internal)?;
        let worker = ProjectionWorker::new(&index_root, ProjectionWorkerOptions::default());
        let summary = run_projection_with_retry(&worker, &graph)?;
        if summary.backlog_remaining {
            return Err(wire_error(
                AgentErrorCode::IndexNotReady,
                "catalog recovery did not drain the durable backlog",
            ));
        }
        let index = worker.active_index(&graph).map_err(internal)?;
        let reader = index.reader().map_err(internal)?;
        let content_path = content_index.map(|path| path.as_ref().to_owned());
        let content_reader = content_path
            .as_ref()
            .map(|path| localsearch_content_index::ContentIndex::open(path))
            .transpose()
            .map_err(internal)?;
        let content_projector = content_path.as_ref().and_then(|path| {
            path.parent()
                .filter(|workspace| workspace.join("content-workspace.json").is_file())
                .and_then(|workspace| ContentProjectionWorker::from_workspace(workspace).ok())
        });
        Ok(Self {
            graph_path,
            index_root,
            projection_writer: Mutex::new(()),
            governor: Mutex::new(ResourceGovernor::default()),
            catalog_reader: RwLock::new(CachedCatalogReader {
                reader,
                generation: IndexGeneration(summary.index_generation),
            }),
            content_reader,
            content_projector,
            content_projection_writer: Mutex::new(()),
            authorization,
        })
    }

    /// Executes a bounded recovery/projection maintenance pass and refreshes the reader.
    ///
    /// # Errors
    ///
    /// Returns a stable unavailable/internal error without exposing backend details.
    pub fn maintain_projection(&self) -> AgentResult<IndexStatus> {
        self.maintain_projection_inner(true)
    }

    /// Executes a scheduler-owned pass after that scheduler has already reported the current
    /// resource-policy window.
    ///
    /// # Errors
    ///
    /// Returns a stable unavailable/internal error without exposing backend details.
    pub fn maintain_projection_scheduled(&self) -> AgentResult<IndexStatus> {
        self.maintain_projection_inner(false)
    }

    /// Executes catalog and optional content projection under one scheduler policy window.
    ///
    /// # Errors
    ///
    /// Returns a redacted unavailable/internal error when either admitted projection cannot make
    /// durable progress after its bounded retry budget.
    pub fn maintain_all_projections_scheduled(&self) -> AgentResult<IndexStatus> {
        let status = self.maintain_projection_scheduled()?;
        let _ = self.maintain_content_projection_scheduled()?;
        self.maintain_graph_storage_scheduled()?;
        Ok(status)
    }

    fn maintain_graph_storage_scheduled(&self) -> AgentResult<()> {
        let decision = self
            .governor
            .lock()
            .map_err(|_| {
                wire_error(
                    AgentErrorCode::Unavailable,
                    "resource governor is unavailable",
                )
            })?
            .decision();
        if decision.budget.background_paused {
            return Ok(());
        }
        let mut graph = FilesystemGraph::open(&self.graph_path).map_err(internal)?;
        let maximum_rows = decision.budget.maximum_batch_mutations.clamp(1, 100_000);
        graph
            .compact_legacy_desired_payloads_bounded(maximum_rows)
            .map_err(internal)?;
        graph
            .prune_rebuildable_outbox_bounded(maximum_rows, false)
            .map_err(internal)?;
        graph.reclaim_reusable_pages(4_096).map_err(internal)?;
        Ok(())
    }

    fn maintain_content_projection_scheduled(
        &self,
    ) -> AgentResult<Option<DurableContentProjectionSummary>> {
        let Some(worker) = &self.content_projector else {
            return Ok(None);
        };
        let decision = self
            .governor
            .lock()
            .map_err(|_| {
                wire_error(
                    AgentErrorCode::Unavailable,
                    "resource governor is unavailable",
                )
            })?
            .decision();
        if decision.budget.background_paused {
            return Ok(None);
        }
        let _writer = self.content_projection_writer.lock().map_err(|_| {
            wire_error(
                AgentErrorCode::Unavailable,
                "content projection writer is unavailable",
            )
        })?;
        let options = ContentProjectionOptions {
            batch_size: decision.budget.maximum_batch_mutations.min(10_000),
            maximum_batches: decision.budget.maximum_batches.min(1_024),
        };
        let mut last_error = None;
        for _ in 0..3 {
            match worker.project(options) {
                Ok(summary) => return Ok(Some(summary)),
                Err(error) => last_error = Some(error),
            }
        }
        match last_error {
            Some(error) => Err(internal(error)),
            None => Err(internal(std::io::Error::other(
                "content projection retry budget exhausted",
            ))),
        }
    }

    fn maintain_projection_inner(&self, advance_policy_window: bool) -> AgentResult<IndexStatus> {
        let _writer = self.projection_writer.lock().map_err(|_| {
            wire_error(
                AgentErrorCode::Unavailable,
                "projection writer is unavailable",
            )
        })?;
        let mut graph = FilesystemGraph::open(&self.graph_path).map_err(internal)?;
        let durable_sequence = graph.latest_outbox_sequence().map_err(internal)?.0;
        let applied_sequence = graph
            .projector_checkpoint(CATALOG_SCHEMA_ID)
            .map_err(internal)?
            .map_or(0, |checkpoint| checkpoint.last_sequence);
        let decision = {
            let mut governor = self.governor.lock().map_err(|_| {
                wire_error(
                    AgentErrorCode::Unavailable,
                    "resource governor is unavailable",
                )
            })?;
            let refresh_pending = graph
                .projection_refresh_maintenance_pending()
                .map_err(internal)?;
            governor.report_backlog(
                durable_sequence
                    .saturating_sub(applied_sequence)
                    .max(u64::from(refresh_pending)),
            );
            if advance_policy_window {
                governor.advance_window()
            } else {
                governor.decision()
            }
        };
        if decision.budget.background_paused {
            return index_status_from_state(&self.query_state()?);
        }
        for _ in 0..decision.budget.maximum_batches.min(1_024) {
            let maximum_links = decision.budget.maximum_batch_mutations.max(1);
            let volume = graph
                .refresh_volume_projection(maximum_links)
                .map_err(internal)?;
            if volume.links_scanned > 0 || volume.job_completed {
                continue;
            }
            let paths = graph
                .refresh_projection_paths(maximum_links)
                .map_err(internal)?;
            if paths.links_scanned == 0 && !paths.job_completed {
                break;
            }
        }
        let worker = ProjectionWorker::new(&self.index_root, projection_options(&decision));
        let summary = run_projection_with_retry(&worker, &graph)?;
        self.refresh_catalog_reader(&worker, &graph, summary.index_generation)?;
        index_status_from_state(&self.query_state()?)
    }

    fn refresh_catalog_reader(
        &self,
        worker: &ProjectionWorker,
        graph: &FilesystemGraph,
        generation: u64,
    ) -> AgentResult<()> {
        let index = worker.active_index(graph).map_err(internal)?;
        let reader = index.reader().map_err(internal)?;
        let mut cached = self.catalog_reader.write().map_err(|_| {
            wire_error(AgentErrorCode::Unavailable, "catalog reader is unavailable")
        })?;
        *cached = CachedCatalogReader {
            reader,
            generation: IndexGeneration(generation),
        };
        Ok(())
    }

    /// Reports portable system pressure to the policy engine.
    ///
    /// # Errors
    ///
    /// Returns unavailable only when the in-process policy lock was poisoned.
    pub fn report_system_pressure(
        &self,
        pressure: SystemPressure,
    ) -> AgentResult<GovernorDecision> {
        let mut governor = self.governor.lock().map_err(|_| {
            wire_error(
                AgentErrorCode::Unavailable,
                "resource governor is unavailable",
            )
        })?;
        Ok(governor.report_system_pressure(pressure))
    }

    /// Atomically reports one scheduler resource observation, including trusted local-input idle
    /// duration. An unavailable activity observation fails closed inside the portable governor.
    ///
    /// # Errors
    ///
    /// Returns unavailable only when the in-process policy lock was poisoned.
    pub fn report_resource_observation(
        &self,
        pressure: SystemPressure,
        user_idle_duration_millis: Option<u64>,
        backlog_mutations: u64,
    ) -> AgentResult<GovernorDecision> {
        let mut governor = self.governor.lock().map_err(|_| {
            wire_error(
                AgentErrorCode::Unavailable,
                "resource governor is unavailable",
            )
        })?;
        governor.report_backlog(backlog_mutations);
        governor.report_system_pressure(pressure);
        Ok(governor.report_user_idle_duration(user_idle_duration_millis))
    }

    /// Fails background projection closed after a trusted system-resource sample cannot be read.
    ///
    /// # Errors
    ///
    /// Returns unavailable only when the in-process policy lock was poisoned.
    pub fn report_resource_unavailable(&self) -> AgentResult<GovernorDecision> {
        let mut governor = self.governor.lock().map_err(|_| {
            wire_error(
                AgentErrorCode::Unavailable,
                "resource governor is unavailable",
            )
        })?;
        Ok(governor.report_resource_unavailable())
    }

    /// Returns the current reason-coded background policy decision.
    ///
    /// # Errors
    ///
    /// Returns unavailable only when the in-process policy lock was poisoned.
    pub fn governor_decision(&self) -> AgentResult<GovernorDecision> {
        let governor = self.governor.lock().map_err(|_| {
            wire_error(
                AgentErrorCode::Unavailable,
                "resource governor is unavailable",
            )
        })?;
        Ok(governor.decision())
    }

    /// Returns the durable projection backlog without opening the materialized index.
    ///
    /// # Errors
    ///
    /// Returns a redacted internal error when the authoritative graph cannot be read.
    pub fn projection_backlog(&self) -> AgentResult<u64> {
        let graph = FilesystemGraph::open_read_only(&self.graph_path).map_err(internal)?;
        let durable = graph.latest_outbox_sequence().map_err(internal)?.0;
        let applied = graph
            .projector_checkpoint(CATALOG_SCHEMA_ID)
            .map_err(internal)?
            .map_or(0, |checkpoint| checkpoint.last_sequence);
        let catalog_backlog = durable.saturating_sub(applied);
        let content_backlog = self
            .content_projector
            .as_ref()
            .map(ContentProjectionWorker::backlog)
            .transpose()
            .map_err(internal)?
            .unwrap_or(0);
        let maintenance_backlog = u64::from(
            graph
                .consumed_outbox_maintenance_pending()
                .map_err(internal)?
                || graph
                    .legacy_desired_payload_maintenance_pending()
                    .map_err(internal)?
                || graph
                    .projection_refresh_maintenance_pending()
                    .map_err(internal)?,
        );
        Ok(catalog_backlog
            .max(content_backlog)
            .max(maintenance_backlog))
    }

    /// Validates, authorizes, and dispatches one authoritative Agent Wire request.
    #[must_use]
    pub fn dispatch(&self, request: AgentRequest) -> AgentResponse {
        self.dispatch_cancellable(request, || false)
    }

    /// Dispatches one request while checking a transport-owned cancellation signal between
    /// planning, retrieval, verification, and ranking units.
    #[must_use]
    pub fn dispatch_cancellable(
        &self,
        request: AgentRequest,
        cancelled: impl Fn() -> bool,
    ) -> AgentResponse {
        let request_id = if request.request_id.len() <= 64 {
            request.request_id.clone()
        } else {
            String::new()
        };
        if let Err(error) = request.validate() {
            let code = match error {
                localsearch_agent_api::WireContractError::UnsupportedProtocolVersion => {
                    AgentErrorCode::UnsupportedProtocolVersion
                }
                localsearch_agent_api::WireContractError::UnsupportedCodecVersion => {
                    AgentErrorCode::UnsupportedCodecVersion
                }
                _ => AgentErrorCode::InvalidRequest,
            };
            return AgentResponse::failure(request_id, code, "request contract rejected");
        }
        let deadline_ms = request.effective_deadline_ms();
        let result = match request.operation {
            RequestOperation::CatalogSearch(search) => self
                .require(Capability::SearchCatalog)
                .and_then(|()| self.search_impl(&search, deadline_ms, &cancelled))
                .map(ResponsePayload::Search),
            RequestOperation::ContentSearch(search) => self
                .require(Capability::SearchContent)
                .and_then(|()| {
                    self.content_search_impl(&search.query, search.top_k, deadline_ms, &cancelled)
                })
                .map(ResponsePayload::ContentSearch),
            RequestOperation::CatalogGetItem { document_id } => self
                .require(Capability::ReadMetadata)
                .and_then(|()| self.get_catalog_item(document_id))
                .map(ResponsePayload::CatalogItem),
            RequestOperation::CatalogGetItems { document_ids } => self
                .require(Capability::ReadMetadata)
                .and_then(|()| self.get_catalog_items(&document_ids))
                .map(ResponsePayload::CatalogItems),
            RequestOperation::IndexGetStatus => self
                .require(Capability::IndexStatus)
                .and_then(|()| self.index_status())
                .map(ResponsePayload::IndexStatus),
            RequestOperation::AgentGetCapabilities => {
                Ok(ResponsePayload::Capabilities(self.capabilities()))
            }
            RequestOperation::AgentGetHealth => Ok(ResponsePayload::Health(self.health())),
        };
        match result {
            Ok(payload) => AgentResponse::success(request_id, payload),
            Err(error) => AgentResponse {
                protocol_version: AGENT_API_VERSION,
                request_id,
                result: None,
                error: Some(error),
            },
        }
    }

    fn health(&self) -> ServiceHealth {
        let state = self.query_state();
        let index_ready = state
            .as_ref()
            .is_ok_and(|state| state.reader.document_count().is_ok());
        ServiceHealth {
            service_ready: index_ready,
            graph_ready: FilesystemGraph::open_read_only(&self.graph_path).is_ok(),
            index_ready,
        }
    }

    fn require(&self, capability: Capability) -> AgentResult<()> {
        if self.authorization.permits(capability) {
            Ok(())
        } else {
            Err(wire_error(
                AgentErrorCode::Forbidden,
                "client capability grant does not permit this method",
            ))
        }
    }

    fn query_state(&self) -> AgentResult<AgentState> {
        let graph = FilesystemGraph::open_read_only(&self.graph_path).map_err(internal)?;
        let checkpoint = graph
            .projector_checkpoint(CATALOG_SCHEMA_ID)
            .map_err(internal)?
            .ok_or_else(|| {
                wire_error(
                    AgentErrorCode::IndexNotReady,
                    "no active catalog generation",
                )
            })?;
        let cached = self.catalog_reader.read().map_err(|_| {
            wire_error(AgentErrorCode::Unavailable, "catalog reader is unavailable")
        })?;
        let generation = IndexGeneration(checkpoint.index_generation);
        if cached.generation != generation {
            return Err(wire_error(
                AgentErrorCode::IndexNotReady,
                "catalog reader generation is not active",
            ));
        }
        let reader = cached.reader.clone();
        Ok(AgentState { graph, reader })
    }

    fn catalog_snapshot(&self) -> AgentResult<CatalogSnapshot> {
        let cached = self.catalog_reader.read().map_err(|_| {
            wire_error(AgentErrorCode::Unavailable, "catalog reader is unavailable")
        })?;
        Ok(CatalogSnapshot {
            reader: cached.reader.clone(),
            generation: cached.generation,
        })
    }

    fn content_search_impl(
        &self,
        query: &str,
        top_k: u16,
        deadline_ms: u32,
        cancelled: &dyn Fn() -> bool,
    ) -> AgentResult<ContentSearchResponse> {
        let started = Instant::now();
        let deadline = Duration::from_millis(u64::from(deadline_ms));
        check_request(started, deadline, cancelled)?;
        let reader = self.content_reader.as_ref().ok_or_else(|| {
            wire_error(
                AgentErrorCode::IndexNotReady,
                "content search is not enabled for this Agent",
            )
        })?;
        let candidates = reader
            .search(query, usize::from(top_k.min(MAX_TOP_K)))
            .map_err(internal)?;
        check_request(started, deadline, cancelled)?;
        let graph = FilesystemGraph::open_read_only(&self.graph_path).map_err(internal)?;
        let mut hits = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            check_request(started, deadline, cancelled)?;
            if let Some(current) = graph
                .desired_catalog_document(candidate.document.identity.document_id)
                .map_err(internal)?
            {
                hits.push(ContentSearchHit {
                    item: item_from_document(current),
                    rank: u32::try_from(hits.len().saturating_add(1)).unwrap_or(u32::MAX),
                });
            }
        }
        Ok(ContentSearchResponse {
            content_schema: CONTENT_SCHEMA_ID.to_owned(),
            took_micros: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            hits,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the linear search pipeline keeps cancellation, deadline, and redacted stage evidence at each boundary"
    )]
    fn search_impl(
        &self,
        request: &SearchRequest,
        deadline_ms: u32,
        cancelled: &dyn Fn() -> bool,
    ) -> AgentResult<SearchResponse> {
        let started = Instant::now();
        let evidence_sequence = SEARCH_EVIDENCE_SEQUENCE
            .fetch_add(1, AtomicOrdering::Relaxed)
            .saturating_add(1);
        emit_search_stage(evidence_sequence, "accepted", started, None, 0);
        let deadline = Duration::from_millis(u64::from(deadline_ms));
        {
            let mut governor = self.governor.lock().map_err(|_| {
                wire_error(
                    AgentErrorCode::Unavailable,
                    "resource governor is unavailable",
                )
            })?;
            governor.begin_interactive_request();
        }
        let state = self.catalog_snapshot()?;
        emit_search_stage(evidence_sequence, "snapshot", started, None, 0);
        let normalized_query = normalize(&request.query);
        if normalized_query.is_empty() {
            return Err(wire_error(
                AgentErrorCode::InvalidRequest,
                "query is empty after normalization",
            ));
        }
        let top_k = usize::from(request.top_k.min(MAX_TOP_K));
        let candidate_limit = (top_k.saturating_mul(40)).clamp(200, MAX_CANDIDATES);
        let mut candidate_documents = Vec::new();
        let mut seen = HashSet::new();
        let mut modes = vec![
            CatalogQueryMode::Exact,
            CatalogQueryMode::Prefix,
            CatalogQueryMode::Token,
            CatalogQueryMode::Path,
        ];
        if normalized_query.chars().count() >= 3 {
            modes.push(CatalogQueryMode::Substring);
        }
        for mode in modes {
            check_request(started, deadline, cancelled)?;
            let remaining = candidate_limit.saturating_sub(candidate_documents.len());
            if remaining == 0 {
                break;
            }
            let mode_name = query_mode_name(mode);
            let candidates = state
                .reader
                .search_candidate_documents(&normalized_query, mode, remaining)
                .map_err(internal)?;
            emit_search_stage(
                evidence_sequence,
                "retrieval",
                started,
                Some(mode_name),
                candidates.len(),
            );
            for document in candidates {
                if seen.insert(document.identity.document_id) {
                    candidate_documents.push(document);
                }
            }
        }

        let mut ranked = Vec::new();
        for document in candidate_documents {
            check_request(started, deadline, cancelled)?;
            if !matches_filters(&document, request) {
                continue;
            }
            if let Some(match_type) = classify(&document, &normalized_query) {
                ranked.push((match_type, document));
            }
        }
        emit_search_stage(
            evidence_sequence,
            "verification",
            started,
            None,
            ranked.len(),
        );
        check_request(started, deadline, cancelled)?;
        ranked.sort_by(compare_ranked);
        ranked.truncate(top_k);
        let hits: Vec<SearchHit> = ranked
            .into_iter()
            .enumerate()
            .map(|(index, (match_type, document))| SearchHit {
                document_id: document.identity.document_id,
                object_key: document.identity.object_key,
                file_link_id: document.identity.file_link_id,
                name: document.name,
                resolved_path: document.resolved_path,
                extension: document.extension,
                kind: document.metadata.kind,
                size: document.metadata.size,
                modified_at_unix_ms: document.metadata.modified_at_unix_ms,
                availability: document.metadata.availability,
                match_type,
                rank: u32::try_from(index + 1).unwrap_or(u32::MAX),
                ranking_version: RANKING_VERSION,
            })
            .collect();
        let took_micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        if let Ok(mut governor) = self.governor.lock() {
            governor.report_search_latency(Duration::from_micros(took_micros));
        }
        emit_search_stage(evidence_sequence, "completed", started, None, hits.len());
        Ok(SearchResponse {
            index_generation: state.generation,
            took_micros,
            hits,
        })
    }
}

fn query_mode_name(mode: CatalogQueryMode) -> &'static str {
    match mode {
        CatalogQueryMode::Exact => "exact",
        CatalogQueryMode::Token => "token",
        CatalogQueryMode::Prefix => "prefix",
        CatalogQueryMode::Substring => "substring",
        CatalogQueryMode::Path => "path",
    }
}

fn emit_search_stage(
    sequence: u64,
    stage: &'static str,
    started: Instant,
    mode: Option<&'static str>,
    candidates: usize,
) {
    if std::env::var_os("LOCALSEARCH_SEARCH_EVIDENCE").is_none() {
        return;
    }
    let elapsed_micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let evidence = serde_json::json!({
        "sequence": sequence,
        "stage": stage,
        "mode": mode,
        "elapsed_micros": elapsed_micros,
        "candidates": candidates,
    });
    eprintln!("LOCALSEARCH_SEARCH_JSON={evidence}");
}

impl CatalogSearchPort for AgentService {
    fn search(&self, request: &SearchRequest, deadline_ms: u32) -> AgentResult<SearchResponse> {
        self.search_impl(request, deadline_ms, &|| false)
    }
}

impl CatalogLookupPort for AgentService {
    fn get_catalog_item(&self, document_id: DocumentId) -> AgentResult<CatalogItem> {
        let graph = FilesystemGraph::open_read_only(&self.graph_path).map_err(internal)?;
        let document = graph
            .desired_catalog_document(document_id)
            .map_err(internal)?
            .ok_or_else(|| wire_error(AgentErrorCode::NotFound, "catalog item is not current"))?;
        Ok(item_from_document(document))
    }

    fn get_catalog_items(&self, document_ids: &[DocumentId]) -> AgentResult<Vec<CatalogItem>> {
        let graph = FilesystemGraph::open_read_only(&self.graph_path).map_err(internal)?;
        let mut items = Vec::new();
        for document_id in document_ids {
            if let Some(document) = graph
                .desired_catalog_document(*document_id)
                .map_err(internal)?
            {
                items.push(item_from_document(document));
            }
        }
        Ok(items)
    }
}

impl IndexStatusPort for AgentService {
    fn index_status(&self) -> AgentResult<IndexStatus> {
        let state = self.query_state()?;
        index_status_from_state(&state)
    }
}

impl CapabilitiesPort for AgentService {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            agent_api_versions: vec![AGENT_API_VERSION],
            codec_versions: vec![AGENT_CODEC_VERSION],
            granted: self.authorization.granted.clone(),
            maximum_top_k: MAX_TOP_K,
            maximum_frame_bytes: u32::try_from(MAX_FRAME_BYTES).unwrap_or(u32::MAX),
            ranking_version: RANKING_VERSION,
        }
    }
}

fn index_status_from_state(state: &AgentState) -> AgentResult<IndexStatus> {
    let durable = state.graph.latest_outbox_sequence().map_err(internal)?.0;
    let checkpoint = state
        .graph
        .projector_checkpoint(CATALOG_SCHEMA_ID)
        .map_err(internal)?;
    let applied = checkpoint.as_ref().map_or(0, |value| value.last_sequence);
    let count = state.reader.document_count().map_err(internal)?;
    Ok(IndexStatus {
        ready: checkpoint.is_some(),
        index_generation: checkpoint.map(|value| IndexGeneration(value.index_generation)),
        document_count: u64::try_from(count).unwrap_or(u64::MAX),
        durable_sequence: durable,
        applied_sequence: applied,
        backlog_mutations: durable.saturating_sub(applied),
    })
}

fn item_from_document(document: CatalogDocument) -> CatalogItem {
    CatalogItem {
        document_id: document.identity.document_id,
        object_key: document.identity.object_key,
        file_link_id: document.identity.file_link_id,
        document_version: document.document_version,
        name: document.name,
        resolved_path: document.resolved_path,
        extension: document.extension,
        kind: document.metadata.kind,
        size: document.metadata.size,
        modified_at_unix_ms: document.metadata.modified_at_unix_ms,
        availability: document.metadata.availability,
    }
}

fn matches_filters(document: &CatalogDocument, request: &SearchRequest) -> bool {
    if matches!(request.scope, SearchScope::Files)
        && document.metadata.kind == localsearch_core::FileKind::Directory
    {
        return false;
    }
    if matches!(request.scope, SearchScope::Folders)
        && document.metadata.kind != localsearch_core::FileKind::Directory
    {
        return false;
    }
    if !request.filters.extensions.is_empty()
        && !document.extension.as_ref().is_some_and(|extension| {
            request
                .filters
                .extensions
                .iter()
                .any(|expected| normalize(expected) == normalize(extension))
        })
    {
        return false;
    }
    if request
        .filters
        .directory_prefix
        .as_ref()
        .is_some_and(|prefix| !normalize(&document.resolved_path).starts_with(&normalize(prefix)))
    {
        return false;
    }
    if request
        .filters
        .minimum_size
        .is_some_and(|minimum| document.metadata.size < minimum)
        || request
            .filters
            .maximum_size
            .is_some_and(|maximum| document.metadata.size > maximum)
    {
        return false;
    }
    true
}

fn classify(document: &CatalogDocument, query: &str) -> Option<MatchType> {
    let name = normalize(&document.name);
    if name == query {
        return Some(MatchType::ExactName);
    }
    if name.starts_with(query) {
        return Some(MatchType::PrefixName);
    }
    if tokens(&name).any(|token| token == query) {
        return Some(MatchType::TokenName);
    }
    if query.chars().count() >= 3 && name.contains(query) {
        return Some(MatchType::SubstringName);
    }
    normalize(&document.resolved_path)
        .contains(query)
        .then_some(MatchType::Path)
}

fn compare_ranked(
    (left_match, left): &(MatchType, CatalogDocument),
    (right_match, right): &(MatchType, CatalogDocument),
) -> Ordering {
    rank_class(*left_match)
        .cmp(&rank_class(*right_match))
        .then_with(|| normalize(&left.name).cmp(&normalize(&right.name)))
        .then_with(|| normalize(&left.resolved_path).cmp(&normalize(&right.resolved_path)))
        .then_with(|| left.identity.document_id.cmp(&right.identity.document_id))
}

const fn rank_class(value: MatchType) -> u8 {
    match value {
        MatchType::ExactName => 0,
        MatchType::PrefixName => 1,
        MatchType::TokenName => 2,
        MatchType::SubstringName => 3,
        MatchType::Path => 4,
    }
}

fn normalize(value: &str) -> String {
    value.nfkc().flat_map(char::to_lowercase).collect()
}

fn tokens(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
}

fn check_request(
    started: Instant,
    deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> AgentResult<()> {
    if cancelled() {
        Err(wire_error(
            AgentErrorCode::Cancelled,
            "request was cancelled",
        ))
    } else if started.elapsed() >= deadline {
        Err(wire_error(
            AgentErrorCode::DeadlineExceeded,
            "request deadline exceeded",
        ))
    } else {
        Ok(())
    }
}

fn internal(error: impl std::fmt::Display) -> AgentWireError {
    let _ = error;
    wire_error(AgentErrorCode::Internal, "agent backend operation failed")
}

fn run_projection_with_retry(
    worker: &ProjectionWorker,
    graph: &FilesystemGraph,
) -> AgentResult<localsearch_catalog_index::ProjectionRunSummary> {
    const MAX_ATTEMPTS: usize = 50;
    for attempt in 0..MAX_ATTEMPTS {
        match worker.run(graph) {
            Ok(summary) => return Ok(summary),
            Err(ProjectionWorkerError::Catalog(_)) if attempt + 1 < MAX_ATTEMPTS => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(internal(error)),
        }
    }
    Err(wire_error(
        AgentErrorCode::Unavailable,
        "projection retry budget exhausted",
    ))
}

fn projection_options(decision: &GovernorDecision) -> ProjectionWorkerOptions {
    ProjectionWorkerOptions {
        maximum_batch_mutations: decision.budget.maximum_batch_mutations,
        maximum_batches: decision.budget.maximum_batches,
        maximum_run_time: decision.budget.maximum_run_time(),
        writer_heap_bytes: decision.budget.writer_heap_bytes,
        rebuild_page_size: decision.budget.rebuild_page_size,
    }
}

fn wire_error(code: AgentErrorCode, message: &str) -> AgentWireError {
    AgentWireError {
        code,
        message: message.to_owned(),
    }
}
