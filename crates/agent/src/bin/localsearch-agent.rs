#[cfg(windows)]
type WindowsObservation = localsearch_agent::BrokerObservationController<
    localsearch_broker_client::BrokerFilesystemProvider<
        localsearch_broker_client::NamedPipeBrokerTransport,
    >,
>;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        collections::BTreeSet,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use localsearch_agent::{
        AgentService,
        windows_pipe::{NamedPipeServer, WindowsPipeError, default_pipe_name},
    };
    use localsearch_core::VolumeId;
    let mut graph: Option<PathBuf> = None;
    let mut index: Option<PathBuf> = None;
    let mut pipe: Option<String> = None;
    let mut content_index: Option<PathBuf> = None;
    let mut broker_pipe: Option<String> = None;
    let mut observe_usn = false;
    let mut observed_volumes = BTreeSet::new();
    let mut observed_roots = BTreeSet::new();
    let mut once = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--graph" => graph = arguments.next().map(PathBuf::from),
            "--index" => index = arguments.next().map(PathBuf::from),
            "--pipe" => pipe = arguments.next(),
            "--content-index" => content_index = arguments.next().map(PathBuf::from),
            "--broker-pipe" => broker_pipe = arguments.next(),
            "--observe-usn" => observe_usn = true,
            "--observe-volume" => {
                let volume = arguments
                    .next()
                    .ok_or("--observe-volume <volume:...> is incomplete")?
                    .parse::<VolumeId>()?;
                observed_volumes.insert(volume);
            }
            "--observe-root" => {
                observed_roots.insert(
                    arguments
                        .next()
                        .ok_or("--observe-root <volume-root> is incomplete")?,
                );
            }
            "--once" => once = true,
            _ => return Err(format!("unknown or incomplete argument: {argument}").into()),
        }
    }
    let graph = graph.ok_or("--graph <sqlite-path> is required")?;
    let index = index.ok_or("--index <catalog-root> is required")?;
    let pipe = pipe.map_or_else(default_pipe_name, Ok)?;
    let observation = connect_observation(
        observe_usn,
        broker_pipe,
        observed_volumes,
        observed_roots,
        &graph,
        once,
    )?;
    let authorization = client_authorization(content_index.is_some());
    let service = Arc::new(AgentService::open_with_content(
        graph,
        index,
        content_index.as_ref(),
        authorization,
    )?);
    let server = NamedPipeServer::bind(&pipe)?;
    let stop = Arc::new(AtomicBool::new(false));
    let scheduler = if once {
        None
    } else {
        Some(spawn_scheduler(
            Arc::clone(&service),
            Arc::clone(&stop),
            observation,
        ))
    };
    eprintln!("LocalSearch Agent ready: {pipe}");
    let result = loop {
        match server.serve_one(
            |request, cancelled| service.dispatch_cancellable(request, cancelled),
            Duration::from_secs(30),
        ) {
            Ok(()) => {
                if once {
                    break Ok(());
                }
            }
            Err(WindowsPipeError::DeadlineExceeded) if !once => {}
            Err(error) => break Err(error.into()),
        }
    };
    stop.store(true, Ordering::Release);
    join_scheduler(scheduler)?;
    result
}

#[cfg(windows)]
fn client_authorization(content_enabled: bool) -> localsearch_agent::ClientAuthorization {
    if content_enabled {
        localsearch_agent::ClientAuthorization::v0_2_with_content()
    } else {
        localsearch_agent::ClientAuthorization::v0_1_metadata()
    }
}

#[cfg(windows)]
fn join_scheduler(
    scheduler: Option<std::thread::JoinHandle<()>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(scheduler) = scheduler {
        scheduler
            .join()
            .map_err(|_| "resource scheduler thread panicked")?;
    }
    Ok(())
}

#[cfg(windows)]
fn connect_observation(
    enabled: bool,
    broker_pipe: Option<String>,
    volumes: std::collections::BTreeSet<localsearch_core::VolumeId>,
    roots: std::collections::BTreeSet<String>,
    graph: &std::path::Path,
    once: bool,
) -> Result<Option<WindowsObservation>, Box<dyn std::error::Error>> {
    use localsearch_agent::{BrokerObservationController, ObservationSelection};
    use localsearch_broker_client::{BrokerFilesystemProvider, NamedPipeBrokerTransport};

    if once && enabled {
        return Err("--observe-usn cannot be combined with --once".into());
    }
    if !enabled && (broker_pipe.is_some() || !volumes.is_empty() || !roots.is_empty()) {
        return Err("--broker-pipe/--observe-volume/--observe-root require --observe-usn".into());
    }
    if !enabled {
        return Ok(None);
    }
    let pipe = broker_pipe.ok_or("--observe-usn requires --broker-pipe <name>")?;
    let provider = BrokerFilesystemProvider::connect(NamedPipeBrokerTransport::new(pipe))?;
    let selection = if volumes.is_empty() && roots.is_empty() {
        ObservationSelection::AllLocalNtfs
    } else if roots.is_empty() {
        ObservationSelection::Volumes(volumes)
    } else {
        ObservationSelection::VolumesAndMountPoints {
            volume_ids: volumes,
            mount_points: roots,
        }
    };
    Ok(Some(BrokerObservationController::new(
        provider, graph, selection,
    )?))
}

#[cfg(windows)]
fn spawn_scheduler(
    service: std::sync::Arc<localsearch_agent::AgentService>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut observation: Option<WindowsObservation>,
) -> std::thread::JoinHandle<()> {
    use std::{sync::atomic::Ordering, thread, time::Duration};

    use localsearch_resource_governor::SystemPressure;
    use localsearch_windows_resources::WindowsResourceProvider;

    thread::spawn(move || {
        let resources = WindowsResourceProvider::new();
        let mut last_decision = None;
        let mut last_resource_evidence = None;
        let resource_evidence_enabled = std::env::var_os("LOCALSEARCH_RESOURCE_EVIDENCE").is_some();
        let mut maintenance_failed = false;
        let mut observation_failed = false;
        while !stop.load(Ordering::Acquire) {
            let backlog = service.projection_backlog();
            let resource_evidence_due = resource_evidence_enabled
                && last_resource_evidence.is_none_or(|sampled: std::time::Instant| {
                    sampled.elapsed() >= Duration::from_secs(1)
                });
            let decision = match resources.snapshot() {
                Err(_) => {
                    if resource_evidence_due {
                        eprintln!(
                            "LOCALSEARCH_RESOURCE_JSON={}",
                            serde_json::json!({ "sample_available": false })
                        );
                        last_resource_evidence = Some(std::time::Instant::now());
                    }
                    service.report_resource_unavailable()
                }
                Ok(snapshot) => {
                    let pressure = SystemPressure::from(&snapshot);
                    if resource_evidence_due {
                        let evidence = serde_json::json!({
                            "sample_available": true,
                            "pressure": pressure,
                            "user_idle_duration_millis": snapshot.user_idle_duration_millis,
                        });
                        eprintln!("LOCALSEARCH_RESOURCE_JSON={evidence}");
                        last_resource_evidence = Some(std::time::Instant::now());
                    }
                    backlog.as_ref().map_or_else(
                        |_| service.governor_decision(),
                        |backlog| {
                            service.report_resource_observation(
                                pressure,
                                snapshot.user_idle_duration_millis,
                                *backlog,
                            )
                        },
                    )
                }
            };
            let delay = decision
                .as_ref()
                .map_or(Duration::from_secs(1), |decision| {
                    maintenance_interval(decision.mode)
                });
            if let Ok(decision) = decision {
                let decision_key = (decision.transition_sequence, decision.reason);
                if last_decision != Some(decision_key) {
                    if let Ok(encoded) = serde_json::to_string(&decision) {
                        eprintln!("LOCALSEARCH_GOVERNOR={encoded}");
                    }
                    last_decision = Some(decision_key);
                }
                if !decision.budget.background_paused
                    && let Some(observer) = observation.as_mut()
                {
                    let maximum_events = u16::try_from(
                        decision
                            .budget
                            .maximum_batch_mutations
                            .clamp(1, u32::from(localsearch_broker_api::MAX_BROKER_PAGE_EVENTS)),
                    )
                    .unwrap_or(localsearch_broker_api::MAX_BROKER_PAGE_EVENTS);
                    match observer.step(maximum_events) {
                        Ok(_) => observation_failed = false,
                        Err(_) if !observation_failed => {
                            eprintln!("LocalSearch broker observation is temporarily unavailable");
                            observation_failed = true;
                        }
                        Err(_) => {}
                    }
                }
                let maintenance_backlog = service.projection_backlog();
                match maintenance_backlog {
                    Ok(0) => maintenance_failed = false,
                    Ok(_) if service.maintain_all_projections_scheduled().is_ok() => {
                        maintenance_failed = false;
                    }
                    Ok(_) | Err(_) if !maintenance_failed => {
                        eprintln!("LocalSearch projection maintenance is temporarily unavailable");
                        maintenance_failed = true;
                    }
                    Ok(_) | Err(_) => {}
                }
            }
            thread::sleep(delay);
        }
    })
}

#[cfg(windows)]
fn maintenance_interval(mode: localsearch_resource_governor::GovernorMode) -> std::time::Duration {
    use localsearch_resource_governor::GovernorMode;
    use std::time::Duration;

    match mode {
        GovernorMode::Active => Duration::from_millis(250),
        GovernorMode::Balanced => Duration::from_millis(100),
        GovernorMode::IdleBoost => Duration::from_millis(25),
        GovernorMode::Pressure => Duration::from_millis(500),
        GovernorMode::Battery => Duration::from_secs(1),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::time::Duration;

    use localsearch_resource_governor::GovernorMode;

    use super::maintenance_interval;

    #[test]
    fn scheduler_cadence_prioritizes_idle_recovery_without_busy_spinning() {
        assert_eq!(
            maintenance_interval(GovernorMode::IdleBoost),
            Duration::from_millis(25)
        );
        assert_eq!(
            maintenance_interval(GovernorMode::Balanced),
            Duration::from_millis(100)
        );
        assert_eq!(
            maintenance_interval(GovernorMode::Pressure),
            Duration::from_millis(500)
        );
        assert_eq!(
            maintenance_interval(GovernorMode::Battery),
            Duration::from_secs(1)
        );
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("LocalSearch Agent v0.1 Named Pipe transport requires Windows");
    std::process::exit(2);
}
