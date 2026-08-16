#![forbid(unsafe_code)]

//! Platform-neutral contracts implemented by operating-system adapters.
//!
//! This crate owns no native handles, OS structs, transport choices, or indexing types. Windows,
//! macOS, and Linux adapters translate their native observations into these contracts.

mod errors;
mod filesystem;
mod resources;

#[cfg(any(test, feature = "provider-contract-testing"))]
pub mod testing;

pub use errors::{PlatformError, PlatformErrorKind, PlatformResult};
pub use filesystem::{
    ChangeBatch, ChangeTrackingMode, FilesystemEventSink, FilesystemProvider, InitialScanMode,
    PlatformCapabilities, PlatformFamily, PrivilegeModel, ProviderCheckpoint, ScanSummary,
    VolumeDescriptor,
};
pub use resources::{
    PowerSource, ResourceProvider, StorageClass, StorageResources, SystemResources,
};
