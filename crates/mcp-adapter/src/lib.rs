#![forbid(unsafe_code)]

//! MCP `2026-07-28` stdio adapter over the versioned `LocalSearch` Agent contract.

mod adapter;
mod protocol;
mod stdio;

pub use adapter::{AgentInvoker, InvokeError, McpAdapter, NamedPipeAgentInvoker};
pub use protocol::{MCP_PROTOCOL_VERSION, McpId, McpRequest, McpResponse};
pub use stdio::{MAX_IN_FLIGHT, MAX_STDIO_MESSAGE_BYTES, run_stdio};
