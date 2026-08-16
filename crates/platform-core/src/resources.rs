use localsearch_core::VolumeId;
use serde::{Deserialize, Serialize};

use crate::PlatformResult;

/// Current system power source.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    /// Connected to external power.
    Ac,
    /// Running on a battery.
    Battery,
    /// Provider cannot determine the source.
    Unknown,
}

/// Coarse storage classification suitable for portable policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageClass {
    /// Rotational locally attached storage.
    Hdd,
    /// Solid-state storage without a known `NVMe` transport.
    Ssd,
    /// `NVMe` solid-state storage.
    Nvme,
    /// Network-backed storage.
    Network,
    /// Removable storage with unknown media characteristics.
    Removable,
    /// Provider cannot classify the storage.
    Unknown,
}

/// Resource information for one canonical volume.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageResources {
    /// Canonical volume identity.
    pub volume_id: VolumeId,
    /// Portable storage class.
    pub class: StorageClass,
    /// Total volume capacity in bytes when known.
    pub capacity_bytes: Option<u64>,
    /// Currently available capacity in bytes when known.
    pub available_bytes: Option<u64>,
}

/// Point-in-time system measurements used as input to resource policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SystemResources {
    /// Logical processors visible to the process.
    pub logical_processors: u32,
    /// Total physical memory in bytes when known.
    pub total_memory_bytes: Option<u64>,
    /// Available physical memory in bytes when known.
    pub available_memory_bytes: Option<u64>,
    /// System CPU load from 0 to 10,000 basis points when known.
    pub system_cpu_load_basis_points: Option<u16>,
    /// Current process CPU load from 0 to 10,000 basis points when known.
    pub process_cpu_load_basis_points: Option<u16>,
    /// Current power source.
    pub power_source: PowerSource,
    /// Battery charge from 0 to 100 percent when present and known.
    pub battery_percent: Option<u8>,
    /// Whether the operating system reports an energy-saving mode.
    pub energy_saver: bool,
    /// Aggregate local-storage busy time from 0 to 10,000 basis points when available.
    /// `None` means the platform cannot currently provide a trusted sample.
    pub storage_busy_basis_points: Option<u16>,
    /// Milliseconds since the last trusted local user input when the platform can observe it.
    /// `None` means unknown and must never be interpreted as idle.
    pub user_idle_duration_millis: Option<u64>,
    /// Per-volume storage observations.
    pub storage: Vec<StorageResources>,
}

/// Adapter boundary for portable resource-governor inputs.
pub trait ResourceProvider: Send + Sync {
    /// Captures current system and storage measurements.
    ///
    /// # Errors
    ///
    /// Returns a categorized platform error when required measurements cannot be obtained.
    fn snapshot(&self) -> PlatformResult<SystemResources>;
}
