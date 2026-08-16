use std::{
    collections::BTreeSet,
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use localsearch_agent_api::{
    AGENT_API_VERSION, AGENT_CODEC_VERSION, AgentRequest, AgentResponse, Capability,
    RequestOperation, ResponsePayload,
};
use localsearch_core::{DocumentId, SearchFilter, SearchRequest, SearchScope};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::protocol::{MCP_PROTOCOL_VERSION, McpRequest, McpResponse};

const AGENT_DEADLINE_MS: u32 = 2_000;
const PIPE_DEADLINE: Duration = Duration::from_secs(3);
const MCP_SEARCH_MAX_TOP_K: u16 = 50;

/// Failure to reach or decode the local Agent. Display text never contains request payloads.
#[derive(Clone, Copy, Debug, Error)]
pub enum InvokeError {
    /// Local Agent transport is not available.
    #[error("LocalSearch Agent is unavailable")]
    Unavailable,
    /// Request was cancelled by the MCP client.
    #[error("LocalSearch request was cancelled")]
    Cancelled,
    /// Agent returned a structurally incompatible response.
    #[error("LocalSearch Agent returned an incompatible response")]
    Incompatible,
}

/// Narrow port used by the MCP mapper; implementations must enforce the Agent boundary.
pub trait AgentInvoker: Send + Sync {
    /// Invoke exactly one versioned Agent request.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport, cancellation, or compatibility category.
    fn invoke(
        &self,
        request: &AgentRequest,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<AgentResponse, InvokeError>;
}

/// Current-user Named Pipe implementation of [`AgentInvoker`].
#[derive(Clone, Debug)]
pub struct NamedPipeAgentInvoker {
    pipe_name: String,
}

impl NamedPipeAgentInvoker {
    /// Bind the adapter client to an explicit local Agent endpoint.
    #[must_use]
    pub fn new(pipe_name: String) -> Self {
        Self { pipe_name }
    }

    /// Resolve the same-logon default Agent endpoint.
    ///
    /// # Errors
    ///
    /// Returns unavailable when the current platform cannot provide the Windows v0.1 transport.
    pub fn default_endpoint() -> Result<Self, InvokeError> {
        #[cfg(windows)]
        {
            let pipe_name = localsearch_local_transport::windows_pipe::default_pipe_name()
                .map_err(|_| InvokeError::Unavailable)?;
            Ok(Self::new(pipe_name))
        }
        #[cfg(not(windows))]
        {
            Err(InvokeError::Unavailable)
        }
    }
}

impl AgentInvoker for NamedPipeAgentInvoker {
    fn invoke(
        &self,
        request: &AgentRequest,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<AgentResponse, InvokeError> {
        #[cfg(windows)]
        {
            use localsearch_local_transport::windows_pipe::{
                WindowsPipeError, round_trip_cancellable,
            };

            round_trip_cancellable(&self.pipe_name, request, PIPE_DEADLINE, cancelled).map_err(
                |error| match error {
                    WindowsPipeError::Cancelled => InvokeError::Cancelled,
                    WindowsPipeError::Frame(_) | WindowsPipeError::Protocol(_) => {
                        InvokeError::Incompatible
                    }
                    _ => InvokeError::Unavailable,
                },
            )
        }
        #[cfg(not(windows))]
        {
            let _ = (request, cancelled);
            Err(InvokeError::Unavailable)
        }
    }
}

/// Stateless MCP method mapper over one bounded Agent invoker.
pub struct McpAdapter<I> {
    invoker: I,
    next_request_id: AtomicU64,
}

impl<I: AgentInvoker> McpAdapter<I> {
    /// Create a mapper with no retained protocol session state.
    #[must_use]
    pub const fn new(invoker: I) -> Self {
        Self {
            invoker,
            next_request_id: AtomicU64::new(1),
        }
    }

    /// Handle one self-contained modern MCP request.
    #[must_use]
    pub fn handle(&self, request: McpRequest, cancelled: &dyn Fn() -> bool) -> McpResponse {
        let Some(id) = request.id else {
            return McpResponse::error(None, -32600, "request id is required", None);
        };
        if request.jsonrpc != "2.0" {
            return McpResponse::error(Some(id), -32600, "jsonrpc must be 2.0", None);
        }
        if request.method == "initialize" {
            return McpResponse::error(
                Some(id),
                -32601,
                "legacy initialize is not supported; use MCP 2026-07-28 server/discover",
                Some(json!({"supportedVersions": [MCP_PROTOCOL_VERSION]})),
            );
        }
        if let Err(error) = validate_modern_meta(&request.params) {
            return meta_error(id, error);
        }

        match request.method.as_str() {
            "server/discover" => McpResponse::success(id, discovery()),
            "tools/list" => self.list_tools(id, cancelled),
            "tools/call" => self.call_tool(id, &request.params, cancelled),
            _ => McpResponse::error(Some(id), -32601, "method not found", None),
        }
    }

    fn list_tools(&self, id: crate::McpId, cancelled: &dyn Fn() -> bool) -> McpResponse {
        let response = match self.invoke(RequestOperation::AgentGetCapabilities, cancelled) {
            Ok(response) => response,
            Err(error) => return invocation_error(id, error),
        };
        let Some(ResponsePayload::Capabilities(capabilities)) = response.result else {
            return McpResponse::error(
                Some(id),
                -32603,
                "Agent capabilities response was incompatible",
                None,
            );
        };
        let mut tools = tool_manifest();
        tools.retain(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .and_then(required_capability)
                .is_some_and(|capability| capabilities.granted.contains(&capability))
        });
        McpResponse::success(
            id,
            json!({
                "resultType": "complete",
                "tools": tools,
                "ttlMs": 300_000,
                "cacheScope": "private"
            }),
        )
    }

    fn call_tool(
        &self,
        id: crate::McpId,
        params: &Value,
        cancelled: &dyn Fn() -> bool,
    ) -> McpResponse {
        let Ok(call) = serde_json::from_value::<ToolCallParams>(params.clone()) else {
            return McpResponse::error(Some(id), -32602, "invalid tools/call params", None);
        };
        let operation = match map_tool(&call.name, call.arguments) {
            Ok(operation) => operation,
            Err(message) => return McpResponse::error(Some(id), -32602, message, None),
        };
        let response = match self.invoke(operation, cancelled) {
            Ok(response) => response,
            Err(InvokeError::Cancelled) => {
                return McpResponse::error(Some(id), -32800, "request cancelled", None);
            }
            Err(error) => return invocation_error(id, error),
        };
        if let Some(error) = response.error {
            let structured = json!({
                "code": serde_json::to_value(error.code).unwrap_or(Value::String("internal".into())),
                "message": error.message
            });
            return McpResponse::success(id, tool_result(&structured, true));
        }
        let Some(payload) = response.result else {
            return McpResponse::error(Some(id), -32603, "Agent response was incompatible", None);
        };
        let Ok(structured) = serde_json::to_value(payload) else {
            return McpResponse::error(Some(id), -32603, "response encoding failed", None);
        };
        McpResponse::success(id, tool_result(&structured, false))
    }

    fn invoke(
        &self,
        operation: RequestOperation,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<AgentResponse, InvokeError> {
        let sequence = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        self.invoker.invoke(
            &AgentRequest {
                protocol_version: AGENT_API_VERSION,
                codec_version: AGENT_CODEC_VERSION,
                request_id: format!("mcp-{sequence}"),
                deadline_ms: AGENT_DEADLINE_MS,
                operation,
            },
            cancelled,
        )
    }
}

enum MetaError {
    Missing,
    Unsupported(Option<String>),
    InvalidCapabilities,
}

fn validate_modern_meta(params: &Value) -> Result<(), MetaError> {
    let Some(meta) = params.get("_meta").and_then(Value::as_object) else {
        return Err(MetaError::Missing);
    };
    let requested = meta
        .get("io.modelcontextprotocol/protocolVersion")
        .and_then(Value::as_str);
    if requested != Some(MCP_PROTOCOL_VERSION) {
        return Err(MetaError::Unsupported(requested.map(str::to_owned)));
    }
    if !meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .is_some_and(Value::is_object)
    {
        return Err(MetaError::InvalidCapabilities);
    }
    Ok(())
}

fn meta_error(id: crate::McpId, error: MetaError) -> McpResponse {
    match error {
        MetaError::Missing => McpResponse::error(
            Some(id),
            -32602,
            "modern MCP request metadata is required",
            None,
        ),
        MetaError::Unsupported(requested) => McpResponse::error(
            Some(id),
            -32022,
            "unsupported MCP protocol version",
            Some(json!({
                "supportedVersions": [MCP_PROTOCOL_VERSION],
                "requestedVersion": requested
            })),
        ),
        MetaError::InvalidCapabilities => McpResponse::error(
            Some(id),
            -32602,
            "clientCapabilities metadata must be an object",
            None,
        ),
    }
}

fn discovery() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": [MCP_PROTOCOL_VERSION],
        "capabilities": {"tools": {"listChanged": false}},
        "_meta": {
            "io.modelcontextprotocol/serverInfo": {
                "name": "LocalSearch",
                "version": env!("CARGO_PKG_VERSION")
            }
        },
        "ttlMs": 300_000,
        "cacheScope": "private"
    })
}

fn tool_manifest() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../contracts/mcp-tools-v1.json"))
        .unwrap_or_else(|_| Vec::new())
}

fn required_capability(name: &str) -> Option<Capability> {
    match name {
        "localsearch.search_files" => Some(Capability::SearchCatalog),
        "localsearch.get_catalog_item" => Some(Capability::ReadMetadata),
        "localsearch.get_index_status" => Some(Capability::IndexStatus),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCallParams {
    #[serde(rename = "_meta")]
    _meta: Value,
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    query: String,
    #[serde(default)]
    scope: SearchScope,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    directory_filter: Option<String>,
    #[serde(default = "default_top_k")]
    top_k: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemArguments {
    document_id: String,
}

fn default_top_k() -> u16 {
    10
}

fn map_tool(name: &str, arguments: Value) -> Result<RequestOperation, &'static str> {
    match name {
        "localsearch.search_files" => {
            let args: SearchArguments = serde_json::from_value(arguments)
                .map_err(|_| "invalid localsearch.search_files arguments")?;
            if args.query.is_empty()
                || args.query.len() > localsearch_agent_api::MAX_QUERY_BYTES
                || args.top_k == 0
                || args.top_k > MCP_SEARCH_MAX_TOP_K
                || args.extensions.len() > 32
            {
                return Err("search arguments exceed bounded policy");
            }
            let extensions = normalize_extensions(args.extensions)?;
            Ok(RequestOperation::CatalogSearch(SearchRequest {
                query: args.query,
                scope: args.scope,
                filters: SearchFilter {
                    extensions,
                    directory_prefix: args.directory_filter,
                    minimum_size: None,
                    maximum_size: None,
                },
                top_k: args.top_k,
            }))
        }
        "localsearch.get_catalog_item" => {
            let args: ItemArguments = serde_json::from_value(arguments)
                .map_err(|_| "invalid localsearch.get_catalog_item arguments")?;
            let document_id = DocumentId::from_str(&args.document_id)
                .map_err(|_| "invalid canonical document_id")?;
            Ok(RequestOperation::CatalogGetItem { document_id })
        }
        "localsearch.get_index_status" => {
            let object = arguments
                .as_object()
                .filter(|object| object.is_empty())
                .ok_or("localsearch.get_index_status takes no arguments")?;
            let _ = object;
            Ok(RequestOperation::IndexGetStatus)
        }
        _ => Err("unknown or unavailable LocalSearch tool"),
    }
}

fn normalize_extensions(values: Vec<String>) -> Result<Vec<String>, &'static str> {
    let mut unique = BTreeSet::new();
    for value in values {
        let normalized = value.trim_start_matches('.').to_lowercase();
        if normalized.is_empty()
            || normalized.len() > 32
            || !normalized.chars().all(char::is_alphanumeric)
        {
            return Err("extension filters must be bounded alphanumeric values");
        }
        unique.insert(normalized);
    }
    Ok(unique.into_iter().collect())
}

fn tool_result(structured: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string(&structured).unwrap_or_else(|_| "{}".to_owned());
    json!({
        "resultType": "complete",
        "content": [{"type": "text", "text": text}],
        "structuredContent": structured,
        "isError": is_error
    })
}

fn invocation_error(id: crate::McpId, error: InvokeError) -> McpResponse {
    let (code, message) = match error {
        InvokeError::Cancelled => (-32800, "request cancelled"),
        InvokeError::Unavailable => (-32603, "LocalSearch Agent is unavailable"),
        InvokeError::Incompatible => (-32603, "LocalSearch Agent response was incompatible"),
    };
    McpResponse::error(Some(id), code, message, None)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use localsearch_agent_api::{Capabilities, ResponsePayload};
    use localsearch_core::{IndexGeneration, RankingVersion, SearchResponse};

    use super::*;

    struct RecordingInvoker {
        requests: Mutex<Vec<AgentRequest>>,
        grants: BTreeSet<Capability>,
    }

    impl RecordingInvoker {
        fn new(grants: impl IntoIterator<Item = Capability>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                grants: grants.into_iter().collect(),
            }
        }
    }

    impl AgentInvoker for RecordingInvoker {
        fn invoke(
            &self,
            request: &AgentRequest,
            _cancelled: &dyn Fn() -> bool,
        ) -> Result<AgentResponse, InvokeError> {
            self.requests
                .lock()
                .expect("requests")
                .push(request.clone());
            let result = match &request.operation {
                RequestOperation::AgentGetCapabilities => {
                    ResponsePayload::Capabilities(Capabilities {
                        agent_api_versions: vec![AGENT_API_VERSION],
                        codec_versions: vec![AGENT_CODEC_VERSION],
                        granted: self.grants.clone(),
                        maximum_top_k: 100,
                        maximum_frame_bytes: 1_048_576,
                        ranking_version: RankingVersion::new(1),
                    })
                }
                RequestOperation::CatalogSearch(_) => ResponsePayload::Search(SearchResponse {
                    index_generation: IndexGeneration(1),
                    took_micros: 4,
                    hits: Vec::new(),
                }),
                RequestOperation::IndexGetStatus => {
                    ResponsePayload::IndexStatus(localsearch_agent_api::IndexStatus {
                        ready: true,
                        index_generation: Some(IndexGeneration(1)),
                        document_count: 1,
                        durable_sequence: 1,
                        applied_sequence: 1,
                        backlog_mutations: 0,
                    })
                }
                RequestOperation::CatalogGetItem { .. } => {
                    return Ok(AgentResponse::failure(
                        request.request_id.clone(),
                        localsearch_agent_api::AgentErrorCode::NotFound,
                        "catalog item not found",
                    ));
                }
                _ => return Err(InvokeError::Incompatible),
            };
            Ok(AgentResponse::success(request.request_id.clone(), result))
        }
    }

    fn request(id: i64, method: &str, extra: &Value) -> McpRequest {
        let mut params = serde_json::Map::new();
        params.insert(
            "_meta".to_owned(),
            json!({
                "io.modelcontextprotocol/protocolVersion": MCP_PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientCapabilities": {}
            }),
        );
        if let Some(extra) = extra.as_object() {
            params.extend(extra.clone());
        }
        McpRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(crate::McpId::Number(id)),
            method: method.to_owned(),
            params: Value::Object(params),
        }
    }

    fn as_value(response: McpResponse) -> Value {
        serde_json::to_value(response).expect("response JSON")
    }

    #[test]
    fn discovery_is_stateless_and_advertises_only_modern_protocol() {
        let adapter = McpAdapter::new(RecordingInvoker::new([]));
        let first = as_value(adapter.handle(request(1, "server/discover", &json!({})), &|| false));
        let second = as_value(adapter.handle(request(2, "server/discover", &json!({})), &|| false));
        assert_eq!(
            first["result"]["supportedVersions"],
            json!([MCP_PROTOCOL_VERSION])
        );
        assert_eq!(second["result"]["resultType"], "complete");
        assert_eq!(adapter.invoker.requests.lock().expect("requests").len(), 0);
    }

    #[test]
    fn tool_list_is_derived_from_agent_grants_and_is_bounded() {
        let adapter = McpAdapter::new(RecordingInvoker::new([
            Capability::SearchCatalog,
            Capability::IndexStatus,
        ]));
        let response = as_value(adapter.handle(request(1, "tools/list", &json!({})), &|| false));
        let tools = response["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "localsearch.search_files");
        assert_eq!(tools[1]["name"], "localsearch.get_index_status");
        assert!(
            tools
                .iter()
                .all(|tool| !tool["name"].as_str().expect("name").contains("content"))
        );
    }

    #[test]
    fn search_tool_maps_to_versioned_agent_request_and_complete_result() {
        let adapter = McpAdapter::new(RecordingInvoker::new([Capability::SearchCatalog]));
        let response = as_value(adapter.handle(
            request(
                1,
                "tools/call",
                &json!({
                    "name": "localsearch.search_files",
                    "arguments": {
                        "query": "architecture",
                        "scope": "files",
                        "extensions": [".MD", "md"],
                        "top_k": 7
                    }
                }),
            ),
            &|| false,
        ));
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(response["result"]["isError"], false);
        let requests = adapter.invoker.requests.lock().expect("requests");
        let RequestOperation::CatalogSearch(search) = &requests[0].operation else {
            panic!("expected catalog search")
        };
        assert_eq!(search.query, "architecture");
        assert_eq!(search.scope, SearchScope::Files);
        assert_eq!(search.filters.extensions, ["md"]);
        assert_eq!(search.top_k, 7);
        assert_eq!(requests[0].protocol_version, AGENT_API_VERSION);
        assert_eq!(requests[0].codec_version, AGENT_CODEC_VERSION);
    }

    #[test]
    fn unsupported_version_and_legacy_initialize_fail_explicitly() {
        let adapter = McpAdapter::new(RecordingInvoker::new([]));
        let mut unsupported = request(1, "server/discover", &json!({}));
        unsupported.params["_meta"]["io.modelcontextprotocol/protocolVersion"] =
            Value::String("2025-11-25".to_owned());
        let response = as_value(adapter.handle(unsupported, &|| false));
        assert_eq!(response["error"]["code"], -32022);
        assert_eq!(
            response["error"]["data"]["supportedVersions"],
            json!([MCP_PROTOCOL_VERSION])
        );

        let legacy = McpRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(crate::McpId::Number(2)),
            method: "initialize".to_owned(),
            params: json!({}),
        };
        let response = as_value(adapter.handle(legacy, &|| false));
        assert_eq!(response["error"]["code"], -32601);
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains("2026-07-28")
        );
    }

    #[test]
    fn metadata_and_status_tools_map_without_backend_types() {
        let adapter = McpAdapter::new(RecordingInvoker::new([
            Capability::ReadMetadata,
            Capability::IndexStatus,
        ]));
        let document_id = DocumentId::from_u128(77);
        let item = as_value(adapter.handle(
            request(
                1,
                "tools/call",
                &json!({
                    "name": "localsearch.get_catalog_item",
                    "arguments": {"document_id": document_id.to_string()}
                }),
            ),
            &|| false,
        ));
        assert_eq!(item["result"]["isError"], true);
        assert_eq!(item["result"]["structuredContent"]["code"], "not_found");

        let status = as_value(adapter.handle(
            request(
                2,
                "tools/call",
                &json!({
                    "name": "localsearch.get_index_status",
                    "arguments": {}
                }),
            ),
            &|| false,
        ));
        assert_eq!(
            status["result"]["structuredContent"]["type"],
            "index_status"
        );
        let requests = adapter.invoker.requests.lock().expect("requests");
        assert!(matches!(
            requests[0].operation,
            RequestOperation::CatalogGetItem { document_id: id } if id == document_id
        ));
        assert!(matches!(
            requests[1].operation,
            RequestOperation::IndexGetStatus
        ));
    }

    #[test]
    #[cfg(not(target_arch = "aarch64"))]
    fn checked_in_tool_schemas_compile_and_search_output_conforms() {
        let manifest = tool_manifest();
        assert_eq!(manifest.len(), 3);
        for tool in &manifest {
            jsonschema::validator_for(&tool["inputSchema"]).expect("valid input schema");
            jsonschema::validator_for(&tool["outputSchema"]).expect("valid output schema");
        }

        let adapter = McpAdapter::new(RecordingInvoker::new([Capability::SearchCatalog]));
        let response = as_value(adapter.handle(
            request(
                1,
                "tools/call",
                &json!({
                    "name": "localsearch.search_files",
                    "arguments": {"query": "architecture"}
                }),
            ),
            &|| false,
        ));
        let validator =
            jsonschema::validator_for(&manifest[0]["outputSchema"]).expect("search output schema");
        let structured = &response["result"]["structuredContent"];
        let validation = validator.validate(structured);
        assert!(validation.is_ok(), "schema error: {validation:?}");
    }
}
