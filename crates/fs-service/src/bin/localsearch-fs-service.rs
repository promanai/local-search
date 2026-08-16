#[cfg(windows)]
mod windows {
    use std::{
        sync::{
            Arc, OnceLock,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use localsearch_broker_api::{
        BrokerErrorCode, BrokerRequest, BrokerResponse, decode_frame, encode_frame,
    };
    use localsearch_fs_service::BrokerService;
    use localsearch_local_transport::windows_pipe::{NamedPipeServer, WindowsPipeError};
    use localsearch_windows_fs::WindowsFilesystemProvider;
    use windows_service::{
        define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
    };

    const SERVICE_NAME: &str = "LocalSearchWinFS";
    static SERVICE_CONFIG: OnceLock<Config> = OnceLock::new();

    #[derive(Clone)]
    struct Config {
        pipe: String,
        authorized_logon_sid: String,
        once: bool,
        windows_service: bool,
    }

    define_windows_service!(ffi_service_main, service_main);

    pub(crate) fn main() -> Result<(), Box<dyn std::error::Error>> {
        let config = parse_arguments()?;
        if config.windows_service {
            SERVICE_CONFIG
                .set(config)
                .map_err(|_| "Windows service configuration already initialized")?;
            service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
            Ok(())
        } else {
            run_broker(&config, &AtomicBool::new(false), true)
        }
    }

    fn parse_arguments() -> Result<Config, Box<dyn std::error::Error>> {
        let mut pipe = None;
        let mut authorized_logon_sid = None;
        let mut once = false;
        let mut windows_service = false;
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--pipe" => pipe = arguments.next(),
                "--authorized-logon-sid" => authorized_logon_sid = arguments.next(),
                "--once" => once = true,
                "--windows-service" => windows_service = true,
                _ => return Err(format!("unknown or incomplete argument: {argument}").into()),
            }
        }
        Ok(Config {
            pipe: pipe.ok_or("--pipe <WinFS broker pipe> is required")?,
            authorized_logon_sid: authorized_logon_sid
                .ok_or("--authorized-logon-sid <SID> is required")?,
            once,
            windows_service,
        })
    }

    fn service_main(_arguments: Vec<std::ffi::OsString>) {
        if let Err(error) = run_windows_service() {
            eprintln!("LocalSearch WinFS service stopped with a lifecycle error: {error}");
        }
    }

    fn run_windows_service() -> Result<(), Box<dyn std::error::Error>> {
        let config = SERVICE_CONFIG
            .get()
            .ok_or("Windows service configuration is unavailable")?
            .clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let handler_stopping = Arc::clone(&stopping);
        let status =
            service_control_handler::register(SERVICE_NAME, move |control| match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    handler_stopping.store(true, Ordering::Release);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            })?;
        status.set_service_status(service_status(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ))?;
        let result = run_broker(&config, &stopping, false);
        status.set_service_status(service_status(
            ServiceState::StopPending,
            ServiceControlAccept::empty(),
        ))?;
        status.set_service_status(service_status(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
        ))?;
        result
    }

    fn service_status(
        state: ServiceState,
        controls_accepted: ServiceControlAccept,
    ) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::ZERO,
            process_id: None,
        }
    }

    fn run_broker(
        config: &Config,
        stopping: &AtomicBool,
        announce: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let provider = Arc::new(WindowsFilesystemProvider::new_with_usn_journal());
        let service = BrokerService::new(provider);
        let server =
            NamedPipeServer::bind_authorized_logon_sid(&config.pipe, &config.authorized_logon_sid)?;
        if announce {
            eprintln!("LocalSearch WinFS broker ready");
        }
        while !stopping.load(Ordering::Acquire) {
            let result = server.serve_frame_cancellable(
                |frame, cancelled| {
                    let response = match decode_frame::<BrokerRequest>(&frame) {
                        Ok(request) => service.dispatch(request, cancelled),
                        Err(_) => BrokerResponse::failure(
                            String::new(),
                            BrokerErrorCode::InvalidRequest,
                            "broker frame rejected",
                        ),
                    };
                    encode_frame(&response)
                        .map_err(|_| WindowsPipeError::Protocol("broker response encoding failed"))
                },
                Duration::from_secs(35),
                &|| stopping.load(Ordering::Acquire),
            );
            match result {
                Ok(()) if config.once => break,
                Ok(()) | Err(WindowsPipeError::DeadlineExceeded) => {}
                Err(WindowsPipeError::Cancelled) if stopping.load(Ordering::Acquire) => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    windows::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("LocalSearch WinFS broker requires Windows");
    std::process::exit(2);
}
