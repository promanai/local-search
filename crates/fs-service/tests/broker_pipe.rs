#![cfg(windows)]

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use localsearch_broker_api::{
    BROKER_CODEC_VERSION, BROKER_PROTOCOL_VERSION, BrokerOperation, BrokerRequest, BrokerResponse,
    decode_frame, encode_frame,
};
use localsearch_broker_client::{BrokerFilesystemProvider, NamedPipeBrokerTransport};
use localsearch_local_transport::windows_pipe::{
    NamedPipeServer, current_logon_sid, round_trip_frame_cancellable,
};
use localsearch_platform_core::FilesystemProvider;

fn unique_pipe(label: &str) -> String {
    format!(
        r"\\.\pipe\LocalSearch\WinFS\v1\{label}-{}",
        std::process::id()
    )
}

fn capability_request(id: &str) -> BrokerRequest {
    BrokerRequest {
        protocol_version: BROKER_PROTOCOL_VERSION,
        codec_version: BROKER_CODEC_VERSION,
        request_id: id.to_owned(),
        deadline_ms: 5_000,
        operation: BrokerOperation::BrokerGetCapabilities,
    }
}

#[test]
fn real_service_process_negotiates_over_authenticated_local_pipe() {
    let pipe_name = unique_pipe("process");
    let sid = current_logon_sid().expect("current logon SID");
    let mut service = Command::new(env!("CARGO_BIN_EXE_localsearch-fs-service"))
        .args([
            "--pipe",
            &pipe_name,
            "--authorized-logon-sid",
            &sid,
            "--once",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn broker service");
    let mut stderr = BufReader::new(service.stderr.take().expect("service stderr"));
    let mut ready = String::new();
    stderr.read_line(&mut ready).expect("read readiness");
    assert_eq!(ready.trim(), "LocalSearch WinFS broker ready");

    let provider = BrokerFilesystemProvider::connect(NamedPipeBrokerTransport::new(pipe_name))
        .expect("negotiate broker");
    let capabilities = provider.capabilities();
    assert!(capabilities.persistent_history);
    assert!(capabilities.stable_object_ids);
    assert!(service.wait().expect("service exit").success());
}

#[test]
fn first_instance_prevents_squatting_and_wrong_logon_sid_cannot_connect() {
    let pipe_name = unique_pipe("single-instance");
    let first = NamedPipeServer::bind(&pipe_name).expect("first instance");
    assert!(
        NamedPipeServer::bind(&pipe_name).is_err(),
        "second first-instance bind unexpectedly succeeded"
    );
    drop(first);

    let current = current_logon_sid().expect("current logon SID");
    if current == "S-1-5-18" {
        return;
    }
    let unauthorized_name = unique_pipe("unauthorized");
    let server = NamedPipeServer::bind_authorized_logon_sid(&unauthorized_name, "S-1-5-18")
        .expect("SYSTEM-only endpoint");
    let encoded = encode_frame(&capability_request("unauthorized-1")).expect("request frame");
    let result = round_trip_frame_cancellable(
        &unauthorized_name,
        &encoded,
        Duration::from_millis(250),
        &|| false,
    );
    assert!(result.is_err(), "wrong logon SID connected to broker");
    drop(server);
}

#[test]
fn malformed_typed_frame_returns_redacted_error_without_crashing_service() {
    let pipe_name = unique_pipe("malformed");
    let sid = current_logon_sid().expect("current logon SID");
    let mut service = Command::new(env!("CARGO_BIN_EXE_localsearch-fs-service"))
        .args([
            "--pipe",
            &pipe_name,
            "--authorized-logon-sid",
            &sid,
            "--once",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn broker service");
    let mut stderr = BufReader::new(service.stderr.take().expect("service stderr"));
    let mut ready = String::new();
    stderr.read_line(&mut ready).expect("read readiness");

    let encoded = encode_frame(&serde_json::json!({
        "protocol_version": 1,
        "request_id": "private-name-must-not-echo",
        "method": "read_content",
        "params": {"path": "C:\\private\\secret.txt"}
    }))
    .expect("malformed typed frame");
    let frame =
        round_trip_frame_cancellable(&pipe_name, &encoded, Duration::from_secs(5), &|| false)
            .expect("redacted broker response");
    let response: BrokerResponse = decode_frame(&frame).expect("response");
    let error = response.error.expect("error");
    assert_eq!(
        error.code,
        localsearch_broker_api::BrokerErrorCode::InvalidRequest
    );
    assert!(!error.message.contains("private"));
    assert!(!error.message.contains("secret"));
    assert!(service.wait().expect("service exit").success());
}

#[test]
fn service_stop_signal_interrupts_idle_accept_without_a_client() {
    let pipe_name = unique_pipe("stop");
    let stopping = Arc::new(AtomicBool::new(false));
    let thread_stopping = Arc::clone(&stopping);
    let (ready_tx, ready_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let server = NamedPipeServer::bind(&pipe_name).expect("bind service pipe");
        ready_tx.send(()).expect("ready");
        server.serve_frame_cancellable(
            |_frame, _cancelled| panic!("no client should be dispatched"),
            Duration::from_secs(30),
            &|| thread_stopping.load(Ordering::Acquire),
        )
    });
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("service ready");
    stopping.store(true, Ordering::Release);
    let result = server.join().expect("service thread");
    assert!(matches!(
        result,
        Err(localsearch_local_transport::windows_pipe::WindowsPipeError::Cancelled)
    ));
}
