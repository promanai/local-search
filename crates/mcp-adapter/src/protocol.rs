use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Only MCP protocol revision shipped by the v0.1 adapter.
pub const MCP_PROTOCOL_VERSION: &str = "2026-07-28";

/// JSON-RPC request identifier admitted by this adapter.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum McpId {
    /// Numeric JSON-RPC identifier.
    Number(i64),
    /// String JSON-RPC identifier.
    String(String),
}

impl McpId {
    pub(crate) fn key(&self) -> String {
        match self {
            Self::Number(value) => format!("n:{value}"),
            Self::String(value) => format!("s:{value}"),
        }
    }
}

/// One modern, self-contained MCP JSON-RPC request.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpRequest {
    /// Must be `2.0`.
    pub jsonrpc: String,
    /// Present for requests and absent for notifications.
    #[serde(default)]
    pub id: Option<McpId>,
    /// MCP method.
    pub method: String,
    /// Method parameters, including required modern request metadata.
    #[serde(default)]
    pub params: Value,
}

/// JSON-RPC success or error response.
#[derive(Clone, Debug, Serialize)]
pub struct McpResponse {
    jsonrpc: &'static str,
    id: Option<McpId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<McpError>,
}

impl McpResponse {
    pub(crate) fn success(id: McpId, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn error(id: Option<McpId>, code: i32, message: &str, data: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(McpError {
                code,
                message: message.to_owned(),
                data,
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct McpError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}
