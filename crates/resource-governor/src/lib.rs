#![forbid(unsafe_code)]

//! Portable, deterministic resource policy for background `LocalSearch` work.
//!
//! Native adapters report coarse measurements through `platform-core`; this crate owns only
//! policy. It never samples the operating system and never depends on `SQLite` or `Tantivy`.

use std::time::Duration;

use localsearch_platform_core::{PowerSource, SystemResources};
use serde::{Deserialize, Serialize};

const MIB: usize = 1_024 * 1_024;

/// Stable operating modes exposed to telemetry and deterministic tests.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernorMode {
    /// A user request is active or the interactive hold window has not expired.
    Active,
    /// Normal bounded background operation.
    Balanced,
    /// Backlog recovery while the user is idle and external power is available.
    IdleBoost,
    /// Resource pressure requires background work to stop.
    Pressure,
    /// Battery operation uses a deliberately reduced budget.
    Battery,
}

/// Coarse maintenance effort. Backends translate this without leaking native types here.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceIntensity {
    /// No background maintenance is admitted.
    Paused,
    /// Only latency-safe essential maintenance is admitted.
    Minimal,
    /// Normal bounded maintenance.
    Normal,
    /// Backlog recovery may use the full configured maintenance window.
    Elevated,
}

/// Portable projection controls selected as one atomic policy decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IndexingBudget {
    /// Maximum canonical outbox mutations in one commit.
    pub maximum_batch_mutations: u32,
    /// Maximum commits in one admitted projection pass.
    pub maximum_batches: u32,
    /// Maximum wall time for one admitted projection pass.
    pub maximum_run_time_millis: u64,
    /// Writer heap ceiling in bytes.
    pub writer_heap_bytes: usize,
    /// Desired-state rows read per rebuild page.
    pub rebuild_page_size: u32,
    /// Desired background projection concurrency. The v0.1 Tantivy adapter caps this at one
    /// logical writer to preserve its single-writer invariant.
    pub projection_concurrency: u8,
    /// Selected maintenance effort.
    pub maintenance_intensity: MaintenanceIntensity,
    /// Whether all background projection work is paused.
    pub background_paused: bool,
}

impl IndexingBudget {
    /// Returns the wall-time ceiling as a standard duration.
    #[must_use]
    pub const fn maximum_run_time(&self) -> Duration {
        Duration::from_millis(self.maximum_run_time_millis)
    }
}

/// Normalized portable system-pressure observation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SystemPressure {
    /// Available physical memory from 0 to 10,000 basis points when known.
    pub available_memory_basis_points: Option<u16>,
    /// System CPU load from 0 to 10,000 basis points when known.
    pub system_cpu_load_basis_points: Option<u16>,
    /// Storage busy time from 0 to 10,000 basis points when a platform monitor provides it.
    pub disk_busy_basis_points: Option<u16>,
    /// Current power source.
    pub power_source: PowerSource,
    /// Battery charge when known.
    pub battery_percent: Option<u8>,
    /// Operating-system energy saver state.
    pub energy_saver: bool,
}

impl Default for SystemPressure {
    fn default() -> Self {
        Self {
            available_memory_basis_points: None,
            system_cpu_load_basis_points: None,
            disk_busy_basis_points: None,
            power_source: PowerSource::Unknown,
            battery_percent: None,
            energy_saver: false,
        }
    }
}

impl From<&SystemResources> for SystemPressure {
    fn from(resources: &SystemResources) -> Self {
        let available_memory_basis_points = resources
            .total_memory_bytes
            .zip(resources.available_memory_bytes)
            .and_then(|(total, available)| {
                if total == 0 {
                    None
                } else {
                    let basis_points =
                        (u128::from(available.min(total)) * 10_000) / u128::from(total);
                    u16::try_from(basis_points).ok()
                }
            });
        Self {
            available_memory_basis_points,
            system_cpu_load_basis_points: resources.system_cpu_load_basis_points,
            disk_busy_basis_points: resources.storage_busy_basis_points,
            power_source: resources.power_source,
            battery_percent: resources.battery_percent,
            energy_saver: resources.energy_saver,
        }
    }
}

/// Current work that can influence admission without exposing a backend queue type.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkloadState {
    /// An owning client explicitly reports active foreground interaction.
    pub user_active: bool,
    /// A trusted platform idle monitor explicitly reports that the user is idle. Unknown or
    /// merely inactive state must remain false and cannot enable `IdleBoost`.
    pub user_idle: bool,
    /// Durable, unapplied catalog mutations.
    pub backlog_mutations: u64,
}

/// Tunable policy thresholds. Defaults are conservative v0.1 engineering values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GovernorConfig {
    /// Search SLA that causes foreground protection.
    pub search_sla_millis: u64,
    /// Available-memory threshold for sustained pressure.
    pub low_memory_basis_points: u16,
    /// Available-memory threshold for immediate critical pressure.
    pub critical_memory_basis_points: u16,
    /// Sustained CPU-pressure threshold.
    pub high_cpu_basis_points: u16,
    /// Sustained disk-pressure threshold.
    pub high_disk_basis_points: u16,
    /// Consecutive pressure observations required outside critical conditions.
    pub pressure_windows: u8,
    /// Consecutive healthy observations required before relaxing a mode.
    pub recovery_windows: u8,
    /// Minimum healthy observations after a transition before another relaxed transition.
    pub cooldown_windows: u8,
    /// Projection windows protected after an interactive search.
    pub interactive_hold_windows: u8,
    /// Minimum trusted local-input idle duration before backlog recovery may use `IdleBoost`.
    pub idle_boost_after_millis: u64,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            search_sla_millis: 75,
            low_memory_basis_points: 1_000,
            critical_memory_basis_points: 500,
            high_cpu_basis_points: 9_000,
            high_disk_basis_points: 9_000,
            pressure_windows: 2,
            recovery_windows: 5,
            cooldown_windows: 3,
            interactive_hold_windows: 3,
            idle_boost_after_millis: 5 * 60 * 1_000,
        }
    }
}

/// Reason attached to every policy decision for operational diagnosis.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    /// Initial conservative state.
    Startup,
    /// A foreground request is active.
    UserActive,
    /// Search exceeded its interactive SLA.
    SearchSla,
    /// Sustained CPU, memory, or disk pressure.
    SystemPressure,
    /// Critical memory pressure requires an immediate pause.
    CriticalMemory,
    /// The trusted system-resource observation is unavailable, so background work fails closed.
    ResourceTelemetryUnavailable,
    /// Battery power requires reduced work.
    Battery,
    /// Energy saver requires a background pause.
    EnergySaver,
    /// Healthy ordinary background operation.
    Balanced,
    /// Idle external-power backlog recovery.
    IdleBacklog,
}

/// Observable output of the deterministic state machine.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GovernorDecision {
    /// Current mode.
    pub mode: GovernorMode,
    /// Atomic controls for background work.
    pub budget: IndexingBudget,
    /// Why the current mode was selected.
    pub reason: DecisionReason,
    /// Monotonic counter incremented only when the mode changes.
    pub transition_sequence: u64,
}

/// Deterministic resource governor. Callers own sampling cadence and clock policy windows.
pub struct ResourceGovernor {
    config: GovernorConfig,
    mode: GovernorMode,
    reason: DecisionReason,
    pressure: SystemPressure,
    resource_available: bool,
    resource_recovery_required: bool,
    workload: WorkloadState,
    pressure_streak: u8,
    recovery_streak: u8,
    cooldown_remaining: u8,
    interactive_remaining: u8,
    transition_sequence: u64,
}

impl Default for ResourceGovernor {
    fn default() -> Self {
        Self::new(GovernorConfig::default())
    }
}

impl ResourceGovernor {
    /// Creates a governor in balanced mode.
    #[must_use]
    pub const fn new(config: GovernorConfig) -> Self {
        Self {
            config,
            mode: GovernorMode::Balanced,
            reason: DecisionReason::Startup,
            pressure: SystemPressure {
                available_memory_basis_points: None,
                system_cpu_load_basis_points: None,
                disk_busy_basis_points: None,
                power_source: PowerSource::Unknown,
                battery_percent: None,
                energy_saver: false,
            },
            resource_available: true,
            resource_recovery_required: false,
            workload: WorkloadState {
                user_active: false,
                user_idle: false,
                backlog_mutations: 0,
            },
            pressure_streak: 0,
            recovery_streak: 0,
            cooldown_remaining: 0,
            interactive_remaining: 0,
            transition_sequence: 0,
        }
    }

    /// Announces a latency-sensitive request before it touches shared backend state.
    pub fn begin_interactive_request(&mut self) -> GovernorDecision {
        self.interactive_remaining = self.config.interactive_hold_windows;
        if self.resource_available && !self.resource_recovery_required {
            self.transition(GovernorMode::Active, DecisionReason::UserActive);
        }
        self.decision()
    }

    /// Reports one completed interactive request and immediately protects subsequent work.
    pub fn report_search_latency(&mut self, latency: Duration) -> GovernorDecision {
        self.interactive_remaining = self.config.interactive_hold_windows;
        if !self.resource_available || self.resource_recovery_required {
            return self.decision();
        }
        let reason = if duration_millis_ceil(latency) > self.config.search_sla_millis {
            DecisionReason::SearchSla
        } else {
            DecisionReason::UserActive
        };
        self.transition(GovernorMode::Active, reason);
        self.decision()
    }

    /// Reports one portable system observation.
    pub fn report_system_pressure(&mut self, pressure: SystemPressure) -> GovernorDecision {
        self.pressure = pressure;
        self.resource_available = true;
        if self.interactive_remaining > 0 {
            self.interactive_remaining -= 1;
        }
        self.evaluate(true);
        self.decision()
    }

    /// Reports that the caller could not obtain a trusted system-resource observation. Background
    /// projection pauses immediately and can recover only through the ordinary healthy-window
    /// hysteresis after valid observations resume.
    pub fn report_resource_unavailable(&mut self) -> GovernorDecision {
        self.pressure = SystemPressure::default();
        self.resource_available = false;
        self.resource_recovery_required = true;
        self.workload.user_idle = false;
        self.pressure_streak = 0;
        self.recovery_streak = 0;
        self.interactive_remaining = 0;
        self.transition(
            GovernorMode::Pressure,
            DecisionReason::ResourceTelemetryUnavailable,
        );
        self.decision()
    }

    /// Reports foreground activity and durable backlog without counting a policy window.
    pub fn report_workload(&mut self, workload: WorkloadState) -> GovernorDecision {
        self.workload = workload;
        if workload.user_active && self.resource_available && !self.resource_recovery_required {
            self.interactive_remaining = self.config.interactive_hold_windows;
            self.transition(GovernorMode::Active, DecisionReason::UserActive);
        }
        self.decision()
    }

    /// Updates only the durable projection backlog, preserving the latest trusted activity state.
    pub fn report_backlog(&mut self, backlog_mutations: u64) -> GovernorDecision {
        self.workload.backlog_mutations = backlog_mutations;
        if backlog_mutations == 0 && self.mode == GovernorMode::IdleBoost {
            self.recovery_streak = 0;
            self.transition(GovernorMode::Balanced, DecisionReason::Balanced);
        }
        self.decision()
    }

    /// Reports elapsed time since trusted local user input. Unknown observations fail closed.
    /// Leaving the trusted idle interval cancels `IdleBoost` immediately; entering it still uses
    /// the ordinary recovery hysteresis before increasing background work.
    pub fn report_user_idle_duration(
        &mut self,
        user_idle_duration_millis: Option<u64>,
    ) -> GovernorDecision {
        self.workload.user_idle = user_idle_duration_millis
            .is_some_and(|idle| idle >= self.config.idle_boost_after_millis);
        if !self.workload.user_idle && self.mode == GovernorMode::IdleBoost {
            self.recovery_streak = 0;
            self.transition(GovernorMode::Balanced, DecisionReason::Balanced);
        } else {
            self.evaluate(false);
        }
        self.decision()
    }

    /// Advances one caller-owned policy window and applies hysteresis/cooldown.
    pub fn advance_window(&mut self) -> GovernorDecision {
        if self.interactive_remaining > 0 {
            self.interactive_remaining -= 1;
        }
        self.evaluate(true);
        self.decision()
    }

    /// Returns the currently admitted indexing budget without changing state.
    #[must_use]
    pub fn get_indexing_budget(&self) -> IndexingBudget {
        budget(self.mode, self.pressure.energy_saver)
    }

    /// Returns a reason-coded snapshot without changing state.
    #[must_use]
    pub fn decision(&self) -> GovernorDecision {
        GovernorDecision {
            mode: self.mode,
            budget: self.get_indexing_budget(),
            reason: self.reason,
            transition_sequence: self.transition_sequence,
        }
    }

    fn evaluate(&mut self, count_window: bool) {
        if !self.resource_available {
            self.transition(
                GovernorMode::Pressure,
                DecisionReason::ResourceTelemetryUnavailable,
            );
            return;
        }
        if self.resource_recovery_required {
            if count_window {
                self.recovery_streak = self.recovery_streak.saturating_add(1);
                self.cooldown_remaining = self.cooldown_remaining.saturating_sub(1);
            }
            if self.recovery_streak < self.config.recovery_windows {
                self.transition(
                    GovernorMode::Pressure,
                    DecisionReason::ResourceTelemetryUnavailable,
                );
                return;
            }
            self.resource_recovery_required = false;
            self.recovery_streak = 0;
            self.transition(GovernorMode::Balanced, DecisionReason::Balanced);
        }
        if self
            .pressure
            .available_memory_basis_points
            .is_some_and(|value| value <= self.config.critical_memory_basis_points)
        {
            self.transition(GovernorMode::Pressure, DecisionReason::CriticalMemory);
            return;
        }
        let pressured = self
            .pressure
            .available_memory_basis_points
            .is_some_and(|value| value <= self.config.low_memory_basis_points)
            || self
                .pressure
                .system_cpu_load_basis_points
                .is_some_and(|value| value >= self.config.high_cpu_basis_points)
            || self
                .pressure
                .disk_busy_basis_points
                .is_some_and(|value| value >= self.config.high_disk_basis_points);
        if pressured {
            if count_window {
                self.pressure_streak = self.pressure_streak.saturating_add(1);
            }
            self.recovery_streak = 0;
            if self.pressure_streak >= self.config.pressure_windows {
                self.transition(GovernorMode::Pressure, DecisionReason::SystemPressure);
                return;
            }
        } else {
            self.pressure_streak = 0;
        }
        if self.workload.user_active || self.interactive_remaining > 0 {
            self.recovery_streak = 0;
            self.transition(GovernorMode::Active, DecisionReason::UserActive);
            return;
        }
        if self.pressure.energy_saver {
            self.transition(GovernorMode::Battery, DecisionReason::EnergySaver);
            return;
        }
        if self.pressure.power_source == PowerSource::Battery {
            self.transition(GovernorMode::Battery, DecisionReason::Battery);
            return;
        }
        if pressured {
            return;
        }
        if count_window {
            self.recovery_streak = self.recovery_streak.saturating_add(1);
            self.cooldown_remaining = self.cooldown_remaining.saturating_sub(1);
        }
        let desired = if self.workload.user_idle
            && self.workload.backlog_mutations > 0
            && self.pressure.power_source == PowerSource::Ac
        {
            (GovernorMode::IdleBoost, DecisionReason::IdleBacklog)
        } else {
            (GovernorMode::Balanced, DecisionReason::Balanced)
        };
        if self.mode == desired.0 {
            self.reason = desired.1;
        } else if self.recovery_streak >= self.config.recovery_windows
            && self.cooldown_remaining == 0
        {
            self.transition(desired.0, desired.1);
            self.recovery_streak = 0;
        }
    }

    fn transition(&mut self, mode: GovernorMode, reason: DecisionReason) {
        if self.mode != mode {
            self.mode = mode;
            self.transition_sequence = self.transition_sequence.saturating_add(1);
            self.cooldown_remaining = self.config.cooldown_windows;
        }
        self.reason = reason;
    }
}

fn duration_millis_ceil(duration: Duration) -> u64 {
    let whole = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    whole.saturating_add(u64::from(
        !duration.subsec_nanos().is_multiple_of(1_000_000),
    ))
}

const fn budget(mode: GovernorMode, energy_saver: bool) -> IndexingBudget {
    match mode {
        GovernorMode::Active => IndexingBudget {
            maximum_batch_mutations: 0,
            maximum_batches: 0,
            maximum_run_time_millis: 0,
            writer_heap_bytes: 32 * MIB,
            rebuild_page_size: 1_000,
            projection_concurrency: 0,
            maintenance_intensity: MaintenanceIntensity::Paused,
            background_paused: true,
        },
        GovernorMode::Balanced => IndexingBudget {
            maximum_batch_mutations: 5_000,
            maximum_batches: 20,
            maximum_run_time_millis: 5_000,
            writer_heap_bytes: 64 * MIB,
            rebuild_page_size: 5_000,
            projection_concurrency: 1,
            maintenance_intensity: MaintenanceIntensity::Normal,
            background_paused: false,
        },
        GovernorMode::IdleBoost => IndexingBudget {
            maximum_batch_mutations: 10_000,
            maximum_batches: 100,
            maximum_run_time_millis: 30_000,
            writer_heap_bytes: 128 * MIB,
            rebuild_page_size: 10_000,
            projection_concurrency: 1,
            maintenance_intensity: MaintenanceIntensity::Elevated,
            background_paused: false,
        },
        GovernorMode::Pressure => IndexingBudget {
            maximum_batch_mutations: 128,
            maximum_batches: 0,
            maximum_run_time_millis: 0,
            writer_heap_bytes: 32 * MIB,
            rebuild_page_size: 500,
            projection_concurrency: 0,
            maintenance_intensity: MaintenanceIntensity::Paused,
            background_paused: true,
        },
        GovernorMode::Battery => IndexingBudget {
            maximum_batch_mutations: if energy_saver { 0 } else { 256 },
            maximum_batches: if energy_saver { 0 } else { 1 },
            maximum_run_time_millis: if energy_saver { 0 } else { 250 },
            writer_heap_bytes: 32 * MIB,
            rebuild_page_size: 1_000,
            projection_concurrency: if energy_saver { 0 } else { 1 },
            maintenance_intensity: if energy_saver {
                MaintenanceIntensity::Paused
            } else {
                MaintenanceIntensity::Minimal
            },
            background_paused: energy_saver,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use localsearch_platform_core::SystemResources;

    fn pressure(memory: u16, cpu: u16, disk: u16) -> SystemPressure {
        SystemPressure {
            available_memory_basis_points: Some(memory),
            system_cpu_load_basis_points: Some(cpu),
            disk_busy_basis_points: Some(disk),
            power_source: PowerSource::Ac,
            battery_percent: None,
            energy_saver: false,
        }
    }

    #[test]
    fn starts_balanced_with_a_bounded_serial_writer() {
        let governor = ResourceGovernor::default();
        let decision = governor.decision();
        assert_eq!(decision.mode, GovernorMode::Balanced);
        assert_eq!(decision.budget.projection_concurrency, 1);
        assert!(!decision.budget.background_paused);
    }

    #[test]
    fn interactive_search_throttles_then_recovers_without_oscillation() {
        let mut governor = ResourceGovernor::default();
        let announced = governor.begin_interactive_request();
        assert_eq!(announced.mode, GovernorMode::Active);
        assert_eq!(announced.reason, DecisionReason::UserActive);
        assert!(announced.budget.background_paused);
        let active = governor.report_search_latency(Duration::from_millis(4));
        assert_eq!(active.mode, GovernorMode::Active);
        assert_eq!(active.reason, DecisionReason::UserActive);
        assert!(active.budget.background_paused);
        assert_eq!(active.budget.maximum_batches, 0);
        assert_eq!(active.budget.projection_concurrency, 0);
        for _ in 0..6 {
            assert_eq!(governor.advance_window().mode, GovernorMode::Active);
        }
        assert_eq!(governor.advance_window().mode, GovernorMode::Balanced);
        assert_eq!(governor.decision().transition_sequence, 2);
    }

    #[test]
    fn critical_memory_and_energy_saver_pause_immediately() {
        let mut governor = ResourceGovernor::default();
        let critical = governor.report_system_pressure(pressure(400, 500, 500));
        assert_eq!(critical.mode, GovernorMode::Pressure);
        assert!(critical.budget.background_paused);

        let saver = governor.report_system_pressure(SystemPressure {
            energy_saver: true,
            power_source: PowerSource::Battery,
            ..SystemPressure::default()
        });
        assert_eq!(saver.mode, GovernorMode::Battery);
        assert!(saver.budget.background_paused);
    }

    #[test]
    fn battery_projection_is_one_short_bounded_quantum() {
        let mut governor = ResourceGovernor::default();
        let battery_pressure = SystemPressure {
            power_source: PowerSource::Battery,
            battery_percent: Some(24),
            ..SystemPressure::default()
        };
        let battery = governor.report_system_pressure(battery_pressure);
        assert_eq!(battery.mode, GovernorMode::Battery);
        assert_eq!(battery.budget.maximum_batch_mutations, 256);
        assert_eq!(battery.budget.maximum_batches, 1);
        assert_eq!(battery.budget.maximum_run_time_millis, 250);
        assert!(!battery.budget.background_paused);

        governor.begin_interactive_request();
        let active = governor.report_system_pressure(battery_pressure);
        assert_eq!(active.mode, GovernorMode::Active);
        assert!(active.budget.background_paused);

        let high_cpu = SystemPressure {
            system_cpu_load_basis_points: Some(9_500),
            ..battery_pressure
        };
        governor.report_system_pressure(high_cpu);
        let pressured = governor.report_system_pressure(high_cpu);
        assert_eq!(pressured.mode, GovernorMode::Pressure);
        assert!(pressured.budget.background_paused);
    }

    #[test]
    fn unavailable_resource_telemetry_fails_closed_and_recovers_slowly() {
        let mut governor = ResourceGovernor::default();
        governor.report_system_pressure(pressure(5_000, 500, 500));
        governor.report_workload(WorkloadState {
            user_active: false,
            user_idle: true,
            backlog_mutations: 50_000,
        });
        for _ in 0..4 {
            governor.advance_window();
        }
        assert_eq!(governor.decision().mode, GovernorMode::IdleBoost);

        let unavailable = governor.report_resource_unavailable();
        assert_eq!(unavailable.mode, GovernorMode::Pressure);
        assert_eq!(
            unavailable.reason,
            DecisionReason::ResourceTelemetryUnavailable
        );
        assert!(unavailable.budget.background_paused);
        let search_during_outage = governor.report_search_latency(Duration::from_millis(4));
        assert_eq!(search_during_outage.mode, GovernorMode::Pressure);
        assert_eq!(
            search_during_outage.reason,
            DecisionReason::ResourceTelemetryUnavailable
        );

        for _ in 0..4 {
            assert_eq!(
                governor
                    .report_system_pressure(pressure(5_000, 500, 500))
                    .mode,
                GovernorMode::Pressure
            );
        }
        assert_eq!(
            governor
                .report_system_pressure(pressure(5_000, 500, 500))
                .mode,
            GovernorMode::Balanced
        );
    }

    #[test]
    fn sustained_pressure_requires_confirmation_and_recovery_is_slower() {
        let mut governor = ResourceGovernor::default();
        assert_eq!(
            governor
                .report_system_pressure(pressure(900, 500, 500))
                .mode,
            GovernorMode::Balanced
        );
        assert_eq!(
            governor
                .report_system_pressure(pressure(900, 500, 500))
                .mode,
            GovernorMode::Pressure
        );

        governor.report_system_pressure(pressure(5_000, 500, 500));
        for _ in 0..3 {
            assert_eq!(governor.advance_window().mode, GovernorMode::Pressure);
        }
        assert_eq!(governor.advance_window().mode, GovernorMode::Balanced);
    }

    #[test]
    fn sustained_disk_pressure_throttles_but_unknown_disk_data_does_not() {
        let mut governor = ResourceGovernor::default();
        let mut disk_pressure = pressure(5_000, 500, 9_500);
        assert_eq!(
            governor.report_system_pressure(disk_pressure).mode,
            GovernorMode::Balanced
        );
        assert_eq!(
            governor.report_system_pressure(disk_pressure).mode,
            GovernorMode::Pressure
        );

        disk_pressure.disk_busy_basis_points = None;
        for _ in 0..5 {
            governor.report_system_pressure(disk_pressure);
        }
        assert_eq!(governor.decision().mode, GovernorMode::Balanced);
    }

    #[test]
    fn idle_ac_backlog_receives_boost_only_after_healthy_recovery() {
        let mut governor = ResourceGovernor::default();
        governor.report_system_pressure(pressure(5_000, 500, 500));
        governor.report_workload(WorkloadState {
            user_active: false,
            user_idle: true,
            backlog_mutations: 50_000,
        });
        for _ in 0..3 {
            assert_eq!(governor.advance_window().mode, GovernorMode::Balanced);
        }
        let boosted = governor.advance_window();
        assert_eq!(boosted.mode, GovernorMode::IdleBoost);
        assert_eq!(
            boosted.budget.maintenance_intensity,
            MaintenanceIntensity::Elevated
        );
        assert_eq!(
            governor.report_backlog(0).mode,
            GovernorMode::Balanced,
            "an empty backlog must cancel elevated recovery immediately"
        );
    }

    #[test]
    fn backlog_without_trusted_idle_evidence_never_enables_idle_boost() {
        let mut governor = ResourceGovernor::default();
        governor.report_system_pressure(pressure(5_000, 500, 500));
        governor.report_workload(WorkloadState {
            user_active: false,
            user_idle: false,
            backlog_mutations: 50_000,
        });
        for _ in 0..20 {
            assert_ne!(governor.advance_window().mode, GovernorMode::IdleBoost);
        }
        assert_eq!(governor.decision().mode, GovernorMode::Balanced);
    }

    #[test]
    fn platform_snapshot_conversion_is_portable_and_bounded() {
        let resources = SystemResources {
            logical_processors: 8,
            total_memory_bytes: Some(1_000),
            available_memory_bytes: Some(250),
            system_cpu_load_basis_points: Some(2_000),
            process_cpu_load_basis_points: Some(500),
            power_source: PowerSource::Ac,
            battery_percent: None,
            energy_saver: false,
            storage_busy_basis_points: Some(7_500),
            user_idle_duration_millis: Some(30_000),
            storage: Vec::new(),
        };
        let converted = SystemPressure::from(&resources);
        assert_eq!(converted.available_memory_basis_points, Some(2_500));
        assert_eq!(converted.system_cpu_load_basis_points, Some(2_000));
        assert_eq!(converted.disk_busy_basis_points, Some(7_500));
    }

    #[test]
    fn trusted_idle_duration_boosts_with_hysteresis_and_input_cancels_immediately() {
        let mut governor = ResourceGovernor::default();
        governor.report_system_pressure(pressure(5_000, 500, 500));
        governor.report_backlog(50_000);
        governor.report_user_idle_duration(Some(5 * 60 * 1_000));
        for _ in 0..3 {
            assert_eq!(governor.advance_window().mode, GovernorMode::Balanced);
        }
        assert_eq!(governor.advance_window().mode, GovernorMode::IdleBoost);

        let after_input = governor.report_user_idle_duration(Some(0));
        assert_eq!(after_input.mode, GovernorMode::Balanced);
        assert_eq!(after_input.reason, DecisionReason::Balanced);
    }

    #[test]
    fn unknown_activity_observation_fails_closed_and_preserves_backlog() {
        let mut governor = ResourceGovernor::default();
        governor.report_system_pressure(pressure(5_000, 500, 500));
        governor.report_workload(WorkloadState {
            user_active: false,
            user_idle: true,
            backlog_mutations: 50_000,
        });
        for _ in 0..4 {
            governor.advance_window();
        }
        assert_eq!(governor.decision().mode, GovernorMode::IdleBoost);

        assert_eq!(
            governor.report_user_idle_duration(None).mode,
            GovernorMode::Balanced
        );
        governor.report_backlog(50_001);
        for _ in 0..20 {
            assert_ne!(governor.advance_window().mode, GovernorMode::IdleBoost);
        }
    }
}
