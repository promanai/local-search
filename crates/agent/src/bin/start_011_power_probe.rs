#[cfg(windows)]
mod windows {
    use std::{
        error::Error,
        thread,
        time::{Duration, Instant},
    };

    use localsearch_platform_core::PowerSource;
    use localsearch_resource_governor::{
        DecisionReason, GovernorDecision, GovernorMode, ResourceGovernor, SystemPressure,
    };
    use localsearch_windows_resources::WindowsResourceProvider;
    use serde::Serialize;

    const DEFAULT_DURATION_SECONDS: u64 = 30;
    const DEFAULT_INTERVAL_MILLIS: u64 = 1_000;
    const DEFAULT_BACKLOG_MUTATIONS: u64 = 50_000;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ProbeOptions {
        duration_seconds: u64,
        interval_millis: u64,
        backlog_mutations: u64,
    }

    impl ProbeOptions {
        fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
            let mut options = Self {
                duration_seconds: DEFAULT_DURATION_SECONDS,
                interval_millis: DEFAULT_INTERVAL_MILLIS,
                backlog_mutations: DEFAULT_BACKLOG_MUTATIONS,
            };
            let mut arguments = arguments.into_iter();
            while let Some(argument) = arguments.next() {
                let value = arguments
                    .next()
                    .ok_or_else(|| format!("missing value for {argument}"))?;
                match argument.as_str() {
                    "--duration-seconds" => {
                        options.duration_seconds = bounded_number(&value, 10, 900, "duration")?;
                    }
                    "--interval-milliseconds" => {
                        options.interval_millis = bounded_number(&value, 250, 5_000, "interval")?;
                    }
                    "--backlog-mutations" => {
                        options.backlog_mutations =
                            bounded_number(&value, 1, 10_000_000, "backlog")?;
                    }
                    _ => return Err(format!("unknown option: {argument}")),
                }
            }
            Ok(options)
        }
    }

    fn bounded_number(value: &str, minimum: u64, maximum: u64, name: &str) -> Result<u64, String> {
        let parsed = value
            .parse::<u64>()
            .map_err(|_| format!("{name} must be an unsigned integer"))?;
        if !(minimum..=maximum).contains(&parsed) {
            return Err(format!("{name} must be in {minimum}..={maximum}"));
        }
        Ok(parsed)
    }

    #[derive(Debug, Serialize)]
    struct PowerSample {
        elapsed_millis: u64,
        sample_available: bool,
        pressure: Option<SystemPressure>,
        user_idle_duration_millis: Option<u64>,
        decision: GovernorDecision,
        policy_invariant_valid: bool,
    }

    #[derive(Debug, Serialize)]
    struct PowerProbeReport {
        schema_version: u32,
        gate: &'static str,
        requested_duration_seconds: u64,
        actual_duration_millis: u64,
        interval_millis: u64,
        backlog_mutations: u64,
        samples: Vec<PowerSample>,
        unavailable_samples: usize,
        ac_samples: usize,
        battery_samples: usize,
        unknown_power_samples: usize,
        energy_saver_samples: usize,
        policy_invariant_violations: usize,
    }

    pub fn main() -> Result<(), Box<dyn Error>> {
        let options = ProbeOptions::parse(std::env::args().skip(1))?;
        let provider = WindowsResourceProvider::new();
        let mut governor = ResourceGovernor::default();
        let started = Instant::now();
        let deadline = Duration::from_secs(options.duration_seconds);
        let interval = Duration::from_millis(options.interval_millis);
        let mut samples = Vec::new();

        while started.elapsed() < deadline {
            let sample_started = Instant::now();
            let sample = if let Ok(resources) = provider.snapshot() {
                let pressure = SystemPressure::from(&resources);
                governor.report_backlog(options.backlog_mutations);
                governor.report_system_pressure(pressure);
                let decision =
                    governor.report_user_idle_duration(resources.user_idle_duration_millis);
                PowerSample {
                    elapsed_millis: elapsed_millis(started.elapsed()),
                    sample_available: true,
                    pressure: Some(pressure),
                    user_idle_duration_millis: resources.user_idle_duration_millis,
                    policy_invariant_valid: policy_invariant_valid(true, Some(pressure), &decision),
                    decision,
                }
            } else {
                let decision = governor.report_resource_unavailable();
                PowerSample {
                    elapsed_millis: elapsed_millis(started.elapsed()),
                    sample_available: false,
                    pressure: None,
                    user_idle_duration_millis: None,
                    policy_invariant_valid: policy_invariant_valid(false, None, &decision),
                    decision,
                }
            };
            samples.push(sample);
            if let Some(remaining) = interval.checked_sub(sample_started.elapsed()) {
                thread::sleep(remaining.min(deadline.saturating_sub(started.elapsed())));
            }
        }

        let report = summarize(options, started.elapsed(), samples);
        println!("{}", serde_json::to_string_pretty(&report)?);
        if report.policy_invariant_violations > 0 {
            return Err("resource policy invariant violation detected".into());
        }
        Ok(())
    }

    fn summarize(
        options: ProbeOptions,
        elapsed: Duration,
        samples: Vec<PowerSample>,
    ) -> PowerProbeReport {
        let unavailable_samples = samples
            .iter()
            .filter(|sample| !sample.sample_available)
            .count();
        let ac_samples = power_samples(&samples, PowerSource::Ac);
        let battery_samples = power_samples(&samples, PowerSource::Battery);
        let unknown_power_samples = power_samples(&samples, PowerSource::Unknown);
        let energy_saver_samples = samples
            .iter()
            .filter(|sample| {
                sample
                    .pressure
                    .is_some_and(|pressure| pressure.energy_saver)
            })
            .count();
        let policy_invariant_violations = samples
            .iter()
            .filter(|sample| !sample.policy_invariant_valid)
            .count();
        PowerProbeReport {
            schema_version: 1,
            gate: "START-011-P",
            requested_duration_seconds: options.duration_seconds,
            actual_duration_millis: elapsed_millis(elapsed),
            interval_millis: options.interval_millis,
            backlog_mutations: options.backlog_mutations,
            samples,
            unavailable_samples,
            ac_samples,
            battery_samples,
            unknown_power_samples,
            energy_saver_samples,
            policy_invariant_violations,
        }
    }

    fn power_samples(samples: &[PowerSample], source: PowerSource) -> usize {
        samples
            .iter()
            .filter(|sample| {
                sample
                    .pressure
                    .is_some_and(|pressure| pressure.power_source == source)
            })
            .count()
    }

    fn policy_invariant_valid(
        sample_available: bool,
        pressure: Option<SystemPressure>,
        decision: &GovernorDecision,
    ) -> bool {
        if !sample_available {
            return decision.mode == GovernorMode::Pressure
                && decision.reason == DecisionReason::ResourceTelemetryUnavailable
                && decision.budget.background_paused;
        }
        let Some(pressure) = pressure else {
            return false;
        };
        let recovering_from_unavailable_telemetry = decision.mode == GovernorMode::Pressure
            && decision.reason == DecisionReason::ResourceTelemetryUnavailable
            && decision.budget.background_paused;
        if pressure.energy_saver {
            return matches!(
                decision.mode,
                GovernorMode::Battery | GovernorMode::Pressure
            ) && decision.budget.background_paused
                && (decision.reason == DecisionReason::EnergySaver
                    || recovering_from_unavailable_telemetry);
        }
        if pressure.power_source == PowerSource::Battery {
            return recovering_from_unavailable_telemetry
                || (decision.mode == GovernorMode::Battery
                    && decision.reason == DecisionReason::Battery
                    && !decision.budget.background_paused
                    && decision.budget.maximum_batches <= 2);
        }
        true
    }

    fn elapsed_millis(duration: Duration) -> u64 {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn options_are_bounded_and_reject_unknown_or_incomplete_input() {
            assert_eq!(
                ProbeOptions::parse([
                    "--duration-seconds".to_owned(),
                    "10".to_owned(),
                    "--interval-milliseconds".to_owned(),
                    "250".to_owned(),
                    "--backlog-mutations".to_owned(),
                    "1".to_owned(),
                ]),
                Ok(ProbeOptions {
                    duration_seconds: 10,
                    interval_millis: 250,
                    backlog_mutations: 1,
                })
            );
            assert!(
                ProbeOptions::parse(["--duration-seconds".to_owned(), "9".to_owned()]).is_err()
            );
            assert!(ProbeOptions::parse(["--unknown".to_owned(), "1".to_owned()]).is_err());
            assert!(ProbeOptions::parse(["--duration-seconds".to_owned()]).is_err());
        }

        #[test]
        fn unavailable_battery_and_energy_saver_decisions_obey_policy() {
            let mut governor = ResourceGovernor::default();
            let unavailable = governor.report_resource_unavailable();
            assert!(policy_invariant_valid(false, None, &unavailable));

            let battery = SystemPressure {
                power_source: PowerSource::Battery,
                ..SystemPressure::default()
            };
            let battery_decision = governor.report_system_pressure(battery);
            assert!(policy_invariant_valid(
                true,
                Some(battery),
                &battery_decision
            ));

            let saver = SystemPressure {
                power_source: PowerSource::Battery,
                energy_saver: true,
                ..SystemPressure::default()
            };
            for _ in 0..5 {
                governor.report_system_pressure(saver);
            }
            let saver_decision = governor.decision();
            assert!(policy_invariant_valid(true, Some(saver), &saver_decision));
        }
    }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    windows::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("START-011 power probe requires Windows");
    std::process::exit(2);
}
