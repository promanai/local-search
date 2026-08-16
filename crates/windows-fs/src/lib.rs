#![deny(unsafe_code)]

//! Windows filesystem provider spike.
//!
//! Native handles, record layouts, journal cursors, and Windows API types are confined to this
//! crate. Its public provider surface contains only `localsearch-platform-core` contracts.

mod checkpoint;
mod journal;
mod provider;
mod record;

pub use provider::{
    PreparedScopedScan, ScopedScanOptions, ScopedScanSummary, WindowsFilesystemProvider,
};
