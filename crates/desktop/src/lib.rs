#![forbid(unsafe_code)]

//! Public-Agent-only state machine for the resident `LocalSearch` desktop client.

mod client;

#[cfg(windows)]
mod tauri_app;

pub use client::{
    AgentTransport, DesktopAgentClient, DesktopClientError, DesktopContentSearchResult,
    DesktopErrorCode, DesktopSearchResult, NamedPipeAgentTransport,
};

#[cfg(windows)]
pub use tauri_app::run;
