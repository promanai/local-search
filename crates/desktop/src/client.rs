use std::{
    fmt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use localsearch_agent_api::{
    AGENT_API_VERSION, AGENT_CODEC_VERSION, AgentErrorCode, AgentRequest, AgentResponse,
    Capabilities, Capability, CatalogItem, ContentSearchRequest, ContentSearchResponse,
    RequestOperation, ResponsePayload,
};
use localsearch_core::{
    Availability, DocumentId, FileKind, SearchFilter, SearchRequest, SearchResponse, SearchScope,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DESKTOP_TOP_K: u16 = 50;
const AGENT_DEADLINE_MS: u32 = 2_000;
const TRANSPORT_DEADLINE_MARGIN: Duration = Duration::from_millis(250);

/// Bounded Agent exchange used by the desktop state machine.
pub trait AgentTransport: Send + Sync + 'static {
    /// Sends one request and cooperatively observes cancellation.
    ///
    /// # Errors
    ///
    /// Returns a redacted client-facing transport category.
    fn exchange(
        &self,
        request: &AgentRequest,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<AgentResponse, DesktopClientError>;
}

/// Production same-logon Named Pipe transport. It creates no persistent connection, so an Agent
/// restart is recovered on the next request without restarting the desktop process.
#[derive(Clone, Debug, Default)]
pub struct NamedPipeAgentTransport {
    pipe_name: Option<String>,
}

impl NamedPipeAgentTransport {
    /// Uses an explicit endpoint, primarily for isolated process tests.
    #[must_use]
    pub const fn with_pipe_name(pipe_name: String) -> Self {
        Self {
            pipe_name: Some(pipe_name),
        }
    }
}

impl AgentTransport for NamedPipeAgentTransport {
    fn exchange(
        &self,
        request: &AgentRequest,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<AgentResponse, DesktopClientError> {
        #[cfg(windows)]
        {
            use localsearch_local_transport::windows_pipe::{
                default_pipe_name, round_trip_cancellable,
            };

            let pipe_name = match &self.pipe_name {
                Some(pipe_name) => pipe_name.clone(),
                None => default_pipe_name().map_err(|error| map_pipe_error(&error))?,
            };
            let transport_deadline =
                Duration::from_millis(u64::from(request.effective_deadline_ms()))
                    .saturating_add(TRANSPORT_DEADLINE_MARGIN);
            round_trip_cancellable(&pipe_name, request, transport_deadline, cancelled)
                .map_err(|error| map_pipe_error(&error))
        }
        #[cfg(not(windows))]
        {
            let _ = (
                &self.pipe_name,
                request,
                cancelled,
                TRANSPORT_DEADLINE_MARGIN,
            );
            Err(DesktopClientError::new(
                DesktopErrorCode::Unavailable,
                "Search service is unavailable on this platform",
            ))
        }
    }
}

#[cfg(windows)]
fn map_pipe_error(
    error: &localsearch_local_transport::windows_pipe::WindowsPipeError,
) -> DesktopClientError {
    use localsearch_local_transport::windows_pipe::WindowsPipeError;
    match error {
        WindowsPipeError::Cancelled => {
            DesktopClientError::new(DesktopErrorCode::Cancelled, "Search request was cancelled")
        }
        WindowsPipeError::DeadlineExceeded => DesktopClientError::new(
            DesktopErrorCode::DeadlineExceeded,
            "Search service did not respond in time",
        ),
        WindowsPipeError::Unauthorized => DesktopClientError::new(
            DesktopErrorCode::Unavailable,
            "Search service authentication failed",
        ),
        WindowsPipeError::Io { .. }
        | WindowsPipeError::Frame(_)
        | WindowsPipeError::InvalidEndpoint
        | WindowsPipeError::Protocol(_) => DesktopClientError::new(
            DesktopErrorCode::Unavailable,
            "Search service is unavailable",
        ),
    }
}

#[derive(Debug)]
struct ActiveSearch {
    request_id: String,
    cancelled: Arc<AtomicBool>,
}

/// Thread-safe desktop request coordinator.
pub struct DesktopAgentClient<T: AgentTransport> {
    transport: T,
    active_search: Mutex<Option<ActiveSearch>>,
}

impl<T: AgentTransport> fmt::Debug for DesktopAgentClient<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopAgentClient")
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl<T: AgentTransport> DesktopAgentClient<T> {
    /// Creates one coordinator around a public Agent transport.
    #[must_use]
    pub const fn new(transport: T) -> Self {
        Self {
            transport,
            active_search: Mutex::new(None),
        }
    }

    /// Cancels any older search, performs one catalog query, and rejects a late response even if
    /// the underlying transport ignored cooperative cancellation.
    ///
    /// # Errors
    ///
    /// Returns stable redacted UI errors for invalid input, transport failure, Agent failure, or
    /// a response that became stale while it was in flight.
    pub fn search(
        &self,
        request_id: String,
        query: String,
    ) -> Result<DesktopSearchResult, DesktopClientError> {
        if query.trim().is_empty() {
            return Err(DesktopClientError::new(
                DesktopErrorCode::InvalidRequest,
                "Search query must not be empty",
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut active = self.lock_active()?;
            if let Some(previous) = active.replace(ActiveSearch {
                request_id: request_id.clone(),
                cancelled: Arc::clone(&cancelled),
            }) {
                previous.cancelled.store(true, Ordering::Release);
            }
        }

        let request = AgentRequest {
            protocol_version: AGENT_API_VERSION,
            codec_version: AGENT_CODEC_VERSION,
            request_id: request_id.clone(),
            deadline_ms: AGENT_DEADLINE_MS,
            operation: RequestOperation::CatalogSearch(SearchRequest {
                query,
                scope: SearchScope::All,
                filters: SearchFilter::default(),
                top_k: DESKTOP_TOP_K,
            }),
        };
        request.validate().map_err(|_| {
            DesktopClientError::new(
                DesktopErrorCode::InvalidRequest,
                "Search request violates the public Agent contract",
            )
        })?;
        let response = self
            .transport
            .exchange(&request, &|| cancelled.load(Ordering::Acquire));

        let current = self.is_current(&request_id, &cancelled)?;
        if !current {
            return Err(DesktopClientError::new(
                DesktopErrorCode::StaleResponse,
                "A newer search request is active",
            ));
        }
        self.clear_if_current(&request_id)?;
        let response = response?;
        validate_response_identity(&response, &request_id)?;
        if let Some(error) = response.error {
            return Err(map_agent_error(error.code));
        }
        let Some(ResponsePayload::Search(response)) = response.result else {
            return Err(DesktopClientError::new(
                DesktopErrorCode::Protocol,
                "Search service returned an unexpected response",
            ));
        };
        Ok(DesktopSearchResult {
            request_id,
            response,
        })
    }

    /// Cancels any older query and searches only the explicitly enabled content projection.
    ///
    /// # Errors
    ///
    /// Returns a stable error when content search is unavailable, rejected, stale, or cancelled.
    pub fn search_content(
        &self,
        request_id: String,
        query: String,
    ) -> Result<DesktopContentSearchResult, DesktopClientError> {
        if query.trim().is_empty() {
            return Err(DesktopClientError::new(
                DesktopErrorCode::InvalidRequest,
                "Content query must not be empty",
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut active = self.lock_active()?;
            if let Some(previous) = active.replace(ActiveSearch {
                request_id: request_id.clone(),
                cancelled: Arc::clone(&cancelled),
            }) {
                previous.cancelled.store(true, Ordering::Release);
            }
        }
        let request = AgentRequest {
            protocol_version: AGENT_API_VERSION,
            codec_version: AGENT_CODEC_VERSION,
            request_id: request_id.clone(),
            deadline_ms: AGENT_DEADLINE_MS,
            operation: RequestOperation::ContentSearch(ContentSearchRequest {
                query,
                top_k: DESKTOP_TOP_K,
            }),
        };
        request.validate().map_err(|_| {
            DesktopClientError::new(
                DesktopErrorCode::InvalidRequest,
                "Content query violates the public Agent contract",
            )
        })?;
        let response = self
            .transport
            .exchange(&request, &|| cancelled.load(Ordering::Acquire));
        if !self.is_current(&request_id, &cancelled)? {
            return Err(DesktopClientError::new(
                DesktopErrorCode::StaleResponse,
                "A newer search request is active",
            ));
        }
        self.clear_if_current(&request_id)?;
        let response = response?;
        validate_response_identity(&response, &request_id)?;
        if let Some(error) = response.error {
            return Err(map_agent_error(error.code));
        }
        let Some(ResponsePayload::ContentSearch(response)) = response.result else {
            return Err(DesktopClientError::new(
                DesktopErrorCode::Protocol,
                "Search service returned an unexpected content response",
            ));
        };
        Ok(DesktopContentSearchResult {
            request_id,
            response,
        })
    }

    /// Cooperatively cancels only the matching in-flight search.
    ///
    /// # Errors
    ///
    /// Returns an internal-state error only if the coordinator lock is poisoned.
    pub fn cancel(&self, request_id: &str) -> Result<bool, DesktopClientError> {
        let active = self.lock_active()?;
        let Some(active) = active
            .as_ref()
            .filter(|active| active.request_id == request_id)
        else {
            return Ok(false);
        };
        active.cancelled.store(true, Ordering::Release);
        Ok(true)
    }

    /// Resolves the latest catalog item immediately before a user action.
    ///
    /// # Errors
    ///
    /// Returns a redacted service/identity error; a stale search path is never trusted.
    pub fn resolve_item(
        &self,
        request_id: &str,
        document_id: DocumentId,
    ) -> Result<CatalogItem, DesktopClientError> {
        let request = AgentRequest {
            protocol_version: AGENT_API_VERSION,
            codec_version: AGENT_CODEC_VERSION,
            request_id: request_id.to_owned(),
            deadline_ms: AGENT_DEADLINE_MS,
            operation: RequestOperation::CatalogGetItem { document_id },
        };
        request.validate().map_err(|_| {
            DesktopClientError::new(
                DesktopErrorCode::InvalidRequest,
                "Item request violates the public Agent contract",
            )
        })?;
        let response = self.transport.exchange(&request, &|| false)?;
        validate_response_identity(&response, request_id)?;
        if let Some(error) = response.error {
            return Err(map_agent_error(error.code));
        }
        let Some(ResponsePayload::CatalogItem(item)) = response.result else {
            return Err(DesktopClientError::new(
                DesktopErrorCode::Protocol,
                "Search service returned an unexpected response",
            ));
        };
        Ok(item)
    }

    /// Re-resolves and validates the current catalog item immediately before an OS action.
    ///
    /// Search-result paths are presentation snapshots and are deliberately not accepted by this
    /// method. The returned path came from a fresh public Agent lookup and exists at validation
    /// time.
    ///
    /// # Errors
    ///
    /// Returns a stable not-found/unavailable/protocol category when the current identity cannot
    /// be resolved into an online actionable filesystem object.
    pub fn resolve_action_target(
        &self,
        request_id: &str,
        document_id: DocumentId,
    ) -> Result<CatalogItem, DesktopClientError> {
        let item = self.resolve_item(request_id, document_id)?;
        ensure_actionable_item(&item)?;
        Ok(item)
    }

    /// Probes the public Agent health operation. Each call reconnects independently.
    ///
    /// # Errors
    ///
    /// Returns a stable unavailable/deadline/protocol category.
    pub fn health(&self, request_id: &str) -> Result<bool, DesktopClientError> {
        let request = AgentRequest {
            protocol_version: AGENT_API_VERSION,
            codec_version: AGENT_CODEC_VERSION,
            request_id: request_id.to_owned(),
            deadline_ms: 1_000,
            operation: RequestOperation::AgentGetHealth,
        };
        request.validate().map_err(|_| {
            DesktopClientError::new(
                DesktopErrorCode::InvalidRequest,
                "Health request violates the public Agent contract",
            )
        })?;
        let response = self.transport.exchange(&request, &|| false)?;
        validate_response_identity(&response, request_id)?;
        if let Some(error) = response.error {
            return Err(map_agent_error(error.code));
        }
        let Some(ResponsePayload::Health(health)) = response.result else {
            return Err(DesktopClientError::new(
                DesktopErrorCode::Protocol,
                "Search service returned an unexpected response",
            ));
        };
        Ok(health.service_ready)
    }

    /// Reads negotiated public Agent grants without inferring configuration from failures.
    ///
    /// # Errors
    ///
    /// Returns a stable transport, Agent, or protocol error.
    pub fn capabilities(&self, request_id: &str) -> Result<Capabilities, DesktopClientError> {
        let request = AgentRequest {
            protocol_version: AGENT_API_VERSION,
            codec_version: AGENT_CODEC_VERSION,
            request_id: request_id.to_owned(),
            deadline_ms: 1_000,
            operation: RequestOperation::AgentGetCapabilities,
        };
        let response = self.transport.exchange(&request, &|| false)?;
        validate_response_identity(&response, request_id)?;
        if let Some(error) = response.error {
            return Err(map_agent_error(error.code));
        }
        let Some(ResponsePayload::Capabilities(capabilities)) = response.result else {
            return Err(DesktopClientError::new(
                DesktopErrorCode::Protocol,
                "Search service returned an unexpected response",
            ));
        };
        Ok(capabilities)
    }

    /// Returns whether the authenticated local client was granted opt-in content search.
    #[must_use]
    pub fn content_search_available(&self, request_id: &str) -> bool {
        self.capabilities(request_id)
            .is_ok_and(|capabilities| capabilities.granted.contains(&Capability::SearchContent))
    }

    fn lock_active(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<ActiveSearch>>, DesktopClientError> {
        self.active_search.lock().map_err(|_| {
            DesktopClientError::new(
                DesktopErrorCode::Internal,
                "Desktop request coordinator is unavailable",
            )
        })
    }

    fn is_current(
        &self,
        request_id: &str,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<bool, DesktopClientError> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(false);
        }
        Ok(self
            .lock_active()?
            .as_ref()
            .is_some_and(|active| active.request_id == request_id))
    }

    fn clear_if_current(&self, request_id: &str) -> Result<(), DesktopClientError> {
        let mut active = self.lock_active()?;
        if active
            .as_ref()
            .is_some_and(|active| active.request_id == request_id)
        {
            *active = None;
        }
        Ok(())
    }
}

fn ensure_actionable_item(item: &CatalogItem) -> Result<(), DesktopClientError> {
    if item.availability != Availability::Online {
        return Err(item_unavailable());
    }
    if matches!(item.kind, FileKind::Special | FileKind::Other) {
        return Err(DesktopClientError::new(
            DesktopErrorCode::ItemUnavailable,
            "The selected item type cannot be opened",
        ));
    }
    match Path::new(&item.resolved_path).try_exists() {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(item_unavailable()),
    }
}

fn item_unavailable() -> DesktopClientError {
    DesktopClientError::new(
        DesktopErrorCode::ItemUnavailable,
        "The selected item is offline, missing, or no longer accessible",
    )
}

/// Search payload tagged with the originating desktop request ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DesktopSearchResult {
    /// Exact ID generated by the current UI query generation.
    pub request_id: String,
    /// Backend-neutral public Agent result.
    pub response: SearchResponse,
}

/// Content-search payload tagged with the originating desktop request ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DesktopContentSearchResult {
    /// Exact ID generated by the current UI query generation.
    pub request_id: String,
    /// Sanitized opt-in content response from Agent API v2.
    pub response: ContentSearchResponse,
}

/// Stable UI error categories. Messages never echo queries or paths.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopErrorCode {
    /// UI arguments violate local/public API bounds.
    InvalidRequest,
    /// A newer query superseded this result.
    StaleResponse,
    /// Cooperative cancellation completed.
    Cancelled,
    /// The public Agent endpoint is not currently reachable.
    Unavailable,
    /// The Agent or transport deadline elapsed.
    DeadlineExceeded,
    /// The selected identity is no longer current.
    NotFound,
    /// The source volume/item is offline or unavailable.
    ItemUnavailable,
    /// Agent response did not match the public protocol.
    Protocol,
    /// Safe internal catch-all.
    Internal,
}

/// Serializable redacted failure returned to the `WebView`.
#[derive(Clone, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
#[error("{message}")]
pub struct DesktopClientError {
    /// Stable machine-readable category.
    pub code: DesktopErrorCode,
    /// Bounded user-safe message.
    pub message: String,
}

impl DesktopClientError {
    pub(crate) fn new(code: DesktopErrorCode, message: &str) -> Self {
        Self {
            code,
            message: message.to_owned(),
        }
    }
}

fn validate_response_identity(
    response: &AgentResponse,
    request_id: &str,
) -> Result<(), DesktopClientError> {
    if response.protocol_version != AGENT_API_VERSION || response.request_id != request_id {
        return Err(DesktopClientError::new(
            DesktopErrorCode::Protocol,
            "Search service response identity is invalid",
        ));
    }
    response.validate().map_err(|_| {
        DesktopClientError::new(
            DesktopErrorCode::Protocol,
            "Search service response is invalid",
        )
    })
}

fn map_agent_error(code: AgentErrorCode) -> DesktopClientError {
    match code {
        AgentErrorCode::InvalidRequest | AgentErrorCode::QueryPolicyRejected => {
            DesktopClientError::new(
                DesktopErrorCode::InvalidRequest,
                "Search request was rejected",
            )
        }
        AgentErrorCode::DeadlineExceeded => DesktopClientError::new(
            DesktopErrorCode::DeadlineExceeded,
            "Search service did not respond in time",
        ),
        AgentErrorCode::Cancelled => {
            DesktopClientError::new(DesktopErrorCode::Cancelled, "Search request was cancelled")
        }
        AgentErrorCode::NotFound => DesktopClientError::new(
            DesktopErrorCode::NotFound,
            "The selected item no longer exists",
        ),
        AgentErrorCode::Unavailable
        | AgentErrorCode::IndexNotReady
        | AgentErrorCode::ResourceExhausted => DesktopClientError::new(
            DesktopErrorCode::Unavailable,
            "Search service is temporarily unavailable",
        ),
        AgentErrorCode::UnsupportedProtocolVersion
        | AgentErrorCode::UnsupportedCodecVersion
        | AgentErrorCode::UnsupportedCapability
        | AgentErrorCode::Unauthorized
        | AgentErrorCode::Forbidden
        | AgentErrorCode::Internal => DesktopClientError::new(
            DesktopErrorCode::Protocol,
            "Search service could not complete the request",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    };

    use localsearch_agent_api::{
        AgentRequest, AgentResponse, CatalogItem, ContentSearchHit, ContentSearchResponse,
        RequestOperation, ResponsePayload, ServiceHealth,
    };
    use localsearch_core::{
        Availability, DocumentId, DocumentVersion, FileId128, FileKey, FileKind, FileLinkId,
        IndexGeneration, SearchResponse, VolumeId,
    };

    use super::{
        AgentTransport, DesktopAgentClient, DesktopClientError, DesktopErrorCode,
        ensure_actionable_item,
    };

    fn search_response(request_id: &str) -> AgentResponse {
        AgentResponse::success(
            request_id.to_owned(),
            ResponsePayload::Search(SearchResponse {
                index_generation: IndexGeneration(1),
                took_micros: 10,
                hits: Vec::new(),
            }),
        )
    }

    struct LateTransport {
        first_started: Arc<Barrier>,
        release_first: Arc<Barrier>,
    }

    impl AgentTransport for LateTransport {
        fn exchange(
            &self,
            request: &AgentRequest,
            _cancelled: &dyn Fn() -> bool,
        ) -> Result<AgentResponse, DesktopClientError> {
            if request.request_id == "request-a" {
                self.first_started.wait();
                self.release_first.wait();
            }
            Ok(search_response(&request.request_id))
        }
    }

    #[test]
    fn late_transport_response_cannot_replace_newer_search() {
        let first_started = Arc::new(Barrier::new(2));
        let release_first = Arc::new(Barrier::new(2));
        let client = Arc::new(DesktopAgentClient::new(LateTransport {
            first_started: Arc::clone(&first_started),
            release_first: Arc::clone(&release_first),
        }));
        let first_client = Arc::clone(&client);
        let first = std::thread::spawn(move || {
            first_client.search("request-a".to_owned(), "architecture".to_owned())
        });
        first_started.wait();
        let second = client
            .search("request-b".to_owned(), "architecture v2".to_owned())
            .expect("newest request must complete");
        assert_eq!(second.request_id, "request-b");
        release_first.wait();
        let first_error = first
            .join()
            .expect("search thread must join")
            .expect_err("late response must be rejected");
        assert_eq!(first_error.code, DesktopErrorCode::StaleResponse);
    }

    struct RecoveringTransport {
        available: Arc<AtomicBool>,
    }

    impl AgentTransport for RecoveringTransport {
        fn exchange(
            &self,
            request: &AgentRequest,
            _cancelled: &dyn Fn() -> bool,
        ) -> Result<AgentResponse, DesktopClientError> {
            if !self.available.load(Ordering::Acquire) {
                return Err(DesktopClientError::new(
                    DesktopErrorCode::Unavailable,
                    "Search service is unavailable",
                ));
            }
            Ok(AgentResponse::success(
                request.request_id.clone(),
                ResponsePayload::Health(ServiceHealth {
                    service_ready: true,
                    graph_ready: true,
                    index_ready: true,
                }),
            ))
        }
    }

    #[test]
    fn on_demand_transport_recovers_without_recreating_desktop_client() {
        let available = Arc::new(AtomicBool::new(false));
        let client = DesktopAgentClient::new(RecoveringTransport {
            available: Arc::clone(&available),
        });
        let error = client
            .health("health-a")
            .expect_err("offline service must fail");
        assert_eq!(error.code, DesktopErrorCode::Unavailable);
        available.store(true, Ordering::Release);
        assert!(
            client
                .health("health-b")
                .expect("same client must reconnect")
        );
    }

    struct WrongIdentityTransport;

    impl AgentTransport for WrongIdentityTransport {
        fn exchange(
            &self,
            _request: &AgentRequest,
            _cancelled: &dyn Fn() -> bool,
        ) -> Result<AgentResponse, DesktopClientError> {
            Ok(search_response("wrong-response-id"))
        }
    }

    #[test]
    fn mismatched_response_identity_fails_closed() {
        let client = DesktopAgentClient::new(WrongIdentityTransport);
        let error = client
            .search("request-a".to_owned(), "architecture".to_owned())
            .expect_err("response identity must match");
        assert_eq!(error.code, DesktopErrorCode::Protocol);
    }

    struct ContentTransport;

    impl AgentTransport for ContentTransport {
        fn exchange(
            &self,
            request: &AgentRequest,
            _cancelled: &dyn Fn() -> bool,
        ) -> Result<AgentResponse, DesktopClientError> {
            assert!(matches!(
                &request.operation,
                RequestOperation::ContentSearch(_)
            ));
            Ok(AgentResponse::success(
                request.request_id.clone(),
                ResponsePayload::ContentSearch(ContentSearchResponse {
                    content_schema: "CONTENT-SCHEMA-v1".to_owned(),
                    took_micros: 11,
                    hits: vec![ContentSearchHit {
                        item: CatalogItem {
                            document_id: DocumentId::from_u128(1),
                            object_key: FileKey::new(
                                VolumeId::from_u128(1),
                                FileId128::from_u128(2),
                            ),
                            file_link_id: FileLinkId::from_u128(3),
                            document_version: DocumentVersion(1),
                            name: "notes.md".to_owned(),
                            resolved_path: "C:/notes.md".to_owned(),
                            extension: Some("md".to_owned()),
                            kind: FileKind::File,
                            size: 10,
                            modified_at_unix_ms: None,
                            availability: Availability::Online,
                        },
                        rank: 1,
                    }],
                }),
            ))
        }
    }

    #[test]
    fn content_mode_uses_the_explicit_agent_operation() {
        let client = DesktopAgentClient::new(ContentTransport);
        let result = client
            .search_content("content-a".to_owned(), "heliotrope".to_owned())
            .expect("content search");
        assert_eq!(result.request_id, "content-a");
        assert_eq!(result.response.content_schema, "CONTENT-SCHEMA-v1");
        assert_eq!(result.response.hits[0].item.name, "notes.md");
    }

    #[test]
    fn action_target_requires_current_online_supported_existing_path() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let mut item = CatalogItem {
            document_id: DocumentId::from_u128(1),
            object_key: FileKey::new(VolumeId::from_u128(1), FileId128::from_u128(2)),
            file_link_id: FileLinkId::from_u128(3),
            document_version: DocumentVersion(1),
            name: "Cargo.toml".to_owned(),
            resolved_path: manifest.to_string_lossy().into_owned(),
            extension: Some("toml".to_owned()),
            kind: FileKind::File,
            size: 1,
            modified_at_unix_ms: None,
            availability: Availability::Online,
        };
        assert!(ensure_actionable_item(&item).is_ok());
        item.availability = Availability::Offline;
        assert_eq!(
            ensure_actionable_item(&item)
                .expect_err("offline item must fail")
                .code,
            DesktopErrorCode::ItemUnavailable
        );
        item.availability = Availability::Online;
        item.resolved_path = manifest
            .with_extension("missing")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            ensure_actionable_item(&item)
                .expect_err("missing item must fail")
                .code,
            DesktopErrorCode::ItemUnavailable
        );
    }
}
