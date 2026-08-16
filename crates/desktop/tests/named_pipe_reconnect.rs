#![cfg(windows)]

use std::{sync::Arc, thread, time::Duration};

use localsearch_agent_api::{AgentResponse, ResponsePayload, ServiceHealth};
use localsearch_core::{IndexGeneration, SearchResponse};
use localsearch_desktop::{DesktopAgentClient, DesktopErrorCode, NamedPipeAgentTransport};
use localsearch_local_transport::windows_pipe::{NamedPipeServer, current_logon_sid};

fn isolated_pipe_name() -> String {
    format!(
        r"\\.\pipe\LocalSearch\Agent\v1\{}-desktop-{}",
        current_logon_sid().expect("test logon SID must resolve"),
        std::process::id()
    )
}

#[test]
fn same_desktop_client_searches_and_reconnects_after_agent_endpoint_restart() {
    let pipe_name = isolated_pipe_name();
    let client = Arc::new(DesktopAgentClient::new(
        NamedPipeAgentTransport::with_pipe_name(pipe_name.clone()),
    ));

    let first_ready = Arc::new(std::sync::Barrier::new(2));
    let first_pipe = pipe_name.clone();
    let first_ready_server = Arc::clone(&first_ready);
    let first = thread::spawn(move || {
        let first_server =
            NamedPipeServer::bind(&first_pipe).expect("first Agent endpoint must bind");
        first_ready_server.wait();
        first_server
            .serve_one(
                |request, _disconnected| {
                    AgentResponse::success(
                        request.request_id,
                        ResponsePayload::Search(SearchResponse {
                            index_generation: IndexGeneration(7),
                            took_micros: 41,
                            hits: Vec::new(),
                        }),
                    )
                },
                Duration::from_secs(5),
            )
            .expect("first Agent exchange must complete");
    });
    first_ready.wait();
    let result = client
        .search("desktop-search".to_owned(), "architecture".to_owned())
        .expect("desktop search must use public Agent Wire");
    assert_eq!(result.response.index_generation, IndexGeneration(7));
    first.join().expect("first Agent thread must join");

    let offline = client
        .health("desktop-offline")
        .expect_err("missing Agent endpoint must be reported");
    assert!(matches!(
        offline.code,
        DesktopErrorCode::Unavailable | DesktopErrorCode::DeadlineExceeded
    ));

    let second_ready = Arc::new(std::sync::Barrier::new(2));
    let second_pipe = pipe_name;
    let second_ready_server = Arc::clone(&second_ready);
    let second = thread::spawn(move || {
        let second_server =
            NamedPipeServer::bind(&second_pipe).expect("restarted endpoint must bind");
        second_ready_server.wait();
        second_server
            .serve_one(
                |request, _disconnected| {
                    AgentResponse::success(
                        request.request_id,
                        ResponsePayload::Health(ServiceHealth {
                            service_ready: true,
                            graph_ready: true,
                            index_ready: true,
                        }),
                    )
                },
                Duration::from_secs(5),
            )
            .expect("restarted Agent exchange must complete");
    });
    second_ready.wait();
    assert!(
        client
            .health("desktop-reconnected")
            .expect("same desktop client must reconnect")
    );
    second.join().expect("second Agent thread must join");
}
