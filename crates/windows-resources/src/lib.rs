#![deny(unsafe_op_in_unsafe_fn)]

//! Windows adapter for portable resource-governor observations.

use std::sync::Mutex;

#[cfg(windows)]
use std::time::{Duration, Instant};

use localsearch_platform_core::{
    PlatformError, PlatformErrorKind, PlatformResult, PowerSource, ResourceProvider,
    SystemResources,
};
use sysinfo::System;

/// Stateful Windows resource sampler. CPU usage is calculated between successive snapshots.
pub struct WindowsResourceProvider {
    system: Mutex<System>,
    disk_busy: Mutex<DiskBusyState>,
}

impl Default for WindowsResourceProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsResourceProvider {
    /// Creates a sampler with an initial system snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            system: Mutex::new(System::new_all()),
            disk_busy: Mutex::new(DiskBusyState::default()),
        }
    }

    /// Captures one portable snapshot without exposing native structures.
    ///
    /// # Errors
    ///
    /// Returns a categorized error if the sampler lock or Windows power query fails.
    pub fn snapshot(&self) -> PlatformResult<SystemResources> {
        <Self as ResourceProvider>::snapshot(self)
    }
}

impl ResourceProvider for WindowsResourceProvider {
    fn snapshot(&self) -> PlatformResult<SystemResources> {
        let mut system = self.system.lock().map_err(|_| {
            PlatformError::new(
                PlatformErrorKind::Unavailable,
                "sample_system_resources",
                "resource sampler lock is unavailable",
            )
        })?;
        system.refresh_memory();
        system.refresh_cpu_usage();
        let (power_source, battery_percent, energy_saver) = power_status()?;
        let storage_busy_basis_points = self
            .disk_busy
            .lock()
            .ok()
            .and_then(|mut sampler| sampler.sample());
        Ok(SystemResources {
            logical_processors: u32::try_from(system.cpus().len()).unwrap_or(u32::MAX),
            total_memory_bytes: Some(system.total_memory()),
            available_memory_bytes: Some(system.available_memory()),
            system_cpu_load_basis_points: Some(cpu_basis_points(system.global_cpu_usage())),
            process_cpu_load_basis_points: None,
            power_source,
            battery_percent,
            energy_saver,
            storage_busy_basis_points,
            user_idle_duration_millis: user_idle_duration_millis().ok(),
            storage: Vec::new(),
        })
    }
}

#[derive(Default)]
struct DiskBusyState {
    initialization_attempted: bool,
    #[cfg(windows)]
    sampler: Option<PdhDiskBusySampler>,
}

impl DiskBusyState {
    fn sample(&mut self) -> Option<u16> {
        #[cfg(windows)]
        {
            if !self.initialization_attempted {
                self.initialization_attempted = true;
                self.sampler = PdhDiskBusySampler::new().ok();
            }
            self.sampler
                .as_mut()
                .and_then(|sampler| sampler.sample().ok().flatten())
        }
        #[cfg(not(windows))]
        {
            self.initialization_attempted = true;
            None
        }
    }
}

#[cfg(windows)]
struct PdhDiskBusySampler {
    query: usize,
    counter: usize,
    last_collection: Instant,
    last_value: Option<u16>,
}

#[cfg(windows)]
impl PdhDiskBusySampler {
    const MINIMUM_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

    fn new() -> PlatformResult<Self> {
        use std::ptr::null_mut;

        use windows_sys::Win32::System::Performance::{
            PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhOpenQueryW,
        };

        let mut query = null_mut();
        // SAFETY: output points to a valid local handle slot and a null data source selects the
        // real-time local provider.
        let open_status = unsafe { PdhOpenQueryW(std::ptr::null(), 0, &raw mut query) };
        if open_status != 0 {
            return Err(pdh_error("open_disk_busy_query", open_status));
        }

        let path: Vec<u16> = r"\PhysicalDisk(_Total)\% Disk Time"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut counter = null_mut();
        // SAFETY: `query` is open, `path` is a stable NUL-terminated UTF-16 string, and the output
        // handle slot is valid for the duration of the call.
        let add_status =
            unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, &raw mut counter) };
        if add_status != 0 {
            // SAFETY: `query` was successfully opened above and is not used after this close.
            let _ = unsafe { PdhCloseQuery(query) };
            return Err(pdh_error("add_disk_busy_counter", add_status));
        }

        // Rate counters require a previous raw sample before a formatted value is valid.
        // SAFETY: `query` owns the successfully added local counter.
        let collect_status = unsafe { PdhCollectQueryData(query) };
        if collect_status != 0 {
            // SAFETY: `query` was successfully opened above and is not used after this close.
            let _ = unsafe { PdhCloseQuery(query) };
            return Err(pdh_error("prime_disk_busy_counter", collect_status));
        }

        Ok(Self {
            query: query as usize,
            counter: counter as usize,
            last_collection: Instant::now(),
            last_value: None,
        })
    }

    fn sample(&mut self) -> PlatformResult<Option<u16>> {
        use windows_sys::Win32::System::Performance::{
            PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE,
            PdhCollectQueryData, PdhGetFormattedCounterValue,
        };

        if self.last_collection.elapsed() < Self::MINIMUM_SAMPLE_INTERVAL {
            return Ok(self.last_value);
        }
        let query = self.query as *mut core::ffi::c_void;
        // SAFETY: the opaque query handle remains owned by `self` and all access is serialized by
        // the provider mutex.
        let collect_status = unsafe { PdhCollectQueryData(query) };
        self.last_collection = Instant::now();
        if collect_status != 0 {
            return Err(pdh_error("collect_disk_busy_counter", collect_status));
        }

        let counter = self.counter as *mut core::ffi::c_void;
        let mut value = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: the counter belongs to the live query and `value` is a valid output structure.
        let format_status = unsafe {
            PdhGetFormattedCounterValue(
                counter,
                PDH_FMT_DOUBLE,
                std::ptr::null_mut(),
                &raw mut value,
            )
        };
        if format_status != 0
            || !matches!(value.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA)
        {
            return Ok(None);
        }
        // SAFETY: `PDH_FMT_DOUBLE` requests and initializes the `doubleValue` union member.
        let percent = unsafe { value.Anonymous.doubleValue };
        let normalized = disk_percent_to_basis_points(percent);
        self.last_value = normalized;
        Ok(normalized)
    }
}

#[cfg(windows)]
impl Drop for PdhDiskBusySampler {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Performance::PdhCloseQuery;

        // SAFETY: the opaque query handle is owned by `self` and is closed exactly once here.
        let _ = unsafe { PdhCloseQuery(self.query as *mut core::ffi::c_void) };
    }
}

#[cfg(windows)]
fn pdh_error(operation: &'static str, status: u32) -> PlatformError {
    PlatformError::new(
        PlatformErrorKind::Unavailable,
        operation,
        format!("PDH status 0x{status:08x}"),
    )
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite disk percentage is clamped to the u16 basis-point domain before conversion"
)]
#[cfg(any(windows, test))]
fn disk_percent_to_basis_points(percent: f64) -> Option<u16> {
    percent
        .is_finite()
        .then(|| (percent.clamp(0.0, 100.0) * 100.0).round() as u16)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "CPU percentage is clamped to the u16 basis-point domain before conversion"
)]
fn cpu_basis_points(percent: f32) -> u16 {
    (f64::from(percent) * 100.0).clamp(0.0, 10_000.0).round() as u16
}

#[cfg(windows)]
fn power_status() -> PlatformResult<(PowerSource, Option<u8>, bool)> {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    let mut status = SYSTEM_POWER_STATUS {
        ACLineStatus: u8::MAX,
        BatteryFlag: u8::MAX,
        BatteryLifePercent: u8::MAX,
        SystemStatusFlag: 0,
        BatteryLifeTime: u32::MAX,
        BatteryFullLifeTime: u32::MAX,
    };
    // SAFETY: `status` is a valid writable SYSTEM_POWER_STATUS for the duration of this call.
    let succeeded = unsafe { GetSystemPowerStatus(&raw mut status) };
    if succeeded == 0 {
        return Err(PlatformError::new(
            PlatformErrorKind::Io,
            "sample_power_status",
            std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(decode_power_status(
        status.ACLineStatus,
        status.BatteryLifePercent,
        status.SystemStatusFlag,
    ))
}

#[cfg(not(windows))]
fn power_status() -> PlatformResult<(PowerSource, Option<u8>, bool)> {
    Ok((PowerSource::Unknown, None, false))
}

#[cfg(windows)]
fn user_idle_duration_millis() -> PlatformResult<u64> {
    use std::mem::size_of;

    use windows_sys::Win32::{
        System::SystemInformation::GetTickCount,
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    };

    let mut input = LASTINPUTINFO {
        cbSize: u32::try_from(size_of::<LASTINPUTINFO>()).map_err(|_| {
            PlatformError::new(
                PlatformErrorKind::Unavailable,
                "sample_user_idle",
                "native input structure size is unsupported",
            )
        })?,
        dwTime: 0,
    };
    // SAFETY: `input` is a correctly sized, writable `LASTINPUTINFO` for this call.
    let succeeded = unsafe { GetLastInputInfo(&raw mut input) };
    if succeeded == 0 {
        return Err(PlatformError::new(
            PlatformErrorKind::Unavailable,
            "sample_user_idle",
            std::io::Error::last_os_error().to_string(),
        ));
    }
    // Both values use the same wrapping 32-bit Windows uptime clock. Wrapping subtraction is
    // conservative after a full clock rollover: it can disable boost, never manufacture a value
    // outside the representable interval.
    let now = unsafe { GetTickCount() };
    Ok(elapsed_input_millis(now, input.dwTime))
}

#[cfg(not(windows))]
fn user_idle_duration_millis() -> PlatformResult<u64> {
    Err(PlatformError::new(
        PlatformErrorKind::Unsupported,
        "sample_user_idle",
        "trusted user-idle sampling is unavailable on this platform adapter",
    ))
}

fn elapsed_input_millis(now: u32, last_input: u32) -> u64 {
    u64::from(now.wrapping_sub(last_input))
}

fn decode_power_status(
    ac_line_status: u8,
    battery_life_percent: u8,
    system_status_flag: u8,
) -> (PowerSource, Option<u8>, bool) {
    let source = match ac_line_status {
        0 => PowerSource::Battery,
        1 => PowerSource::Ac,
        _ => PowerSource::Unknown,
    };
    let battery = (battery_life_percent <= 100).then_some(battery_life_percent);
    (source, battery, system_status_flag == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_power_values_are_normalized_without_leaking_flags() {
        assert_eq!(
            decode_power_status(0, 37, 1),
            (PowerSource::Battery, Some(37), true)
        );
        assert_eq!(
            decode_power_status(1, u8::MAX, 0),
            (PowerSource::Ac, None, false)
        );
        assert_eq!(
            decode_power_status(u8::MAX, 101, 0),
            (PowerSource::Unknown, None, false)
        );
    }

    #[test]
    fn input_idle_duration_uses_the_native_wrapping_uptime_clock() {
        assert_eq!(elapsed_input_millis(50_000, 49_250), 750);
        assert_eq!(elapsed_input_millis(250, u32::MAX - 249), 500);
    }

    #[test]
    fn disk_percentage_is_finite_clamped_and_normalized_to_basis_points() {
        assert_eq!(disk_percent_to_basis_points(0.0), Some(0));
        assert_eq!(disk_percent_to_basis_points(12.345), Some(1_235));
        assert_eq!(disk_percent_to_basis_points(125.0), Some(10_000));
        assert_eq!(disk_percent_to_basis_points(-5.0), Some(0));
        assert_eq!(disk_percent_to_basis_points(f64::NAN), None);
        assert_eq!(disk_percent_to_basis_points(f64::INFINITY), None);
    }

    #[test]
    fn live_snapshot_has_bounded_portable_measurements() {
        let provider = WindowsResourceProvider::new();
        let snapshot = provider.snapshot().expect("snapshot");
        assert!(snapshot.logical_processors > 0);
        assert!(
            snapshot
                .total_memory_bytes
                .zip(snapshot.available_memory_bytes)
                .is_some_and(|(total, available)| total > 0 && available <= total)
        );
        assert!(
            snapshot
                .system_cpu_load_basis_points
                .is_some_and(|cpu| cpu <= 10_000)
        );
        assert!(
            snapshot
                .user_idle_duration_millis
                .is_none_or(|idle| u32::try_from(idle).is_ok())
        );
        #[cfg(windows)]
        {
            std::thread::sleep(Duration::from_millis(1_050));
            let sampled = provider.snapshot().expect("second snapshot");
            assert!(
                sampled
                    .storage_busy_basis_points
                    .is_none_or(|busy| busy <= 10_000)
            );
        }
    }
}
