#![cfg(windows)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    time::Duration,
};

use localsearch_agent::{AgentService, ClientAuthorization};
use localsearch_agent_api::{AgentErrorCode, AgentResponse};
use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, VolumeId,
};
use localsearch_filesystem_graph::FilesystemGraph;
use localsearch_platform_core::{ProviderCheckpoint, VolumeDescriptor};
use serde_json::{Value, json};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().expect("temp");
    let graph_path = temp.path().join("graph.sqlite3");
    let index_path = temp.path().join("catalog");
    let volume = VolumeId::from_u128(8);
    let root = FileKey::new(volume, FileId128::from_u128(1));
    let file = FileKey::new(volume, FileId128::from_u128(2));
    let mut graph = FilesystemGraph::open(&graph_path).expect("graph");
    graph
        .ingest_snapshot(
            VolumeDescriptor {
                volume_id: volume,
                display_name: Some("mcp-test".to_owned()),
                mount_points: vec!["root".to_owned()],
                filesystem: Some("testfs".to_owned()),
                removable: false,
                local: true,
            },
            ProviderCheckpoint {
                provider_id: "mcp-test".to_owned(),
                format_version: 1,
                volume_id: volume,
                opaque: vec![1],
            },
            [
                FilesystemEvent::ObjectObserved {
                    object: FileObjectSnapshot {
                        object_key: root,
                        metadata: metadata(FileKind::Directory, 0),
                    },
                },
                FilesystemEvent::LinkObserved {
                    link: FileLinkSnapshot {
                        file_link_id: FileLinkId::from_u128(201),
                        object_key: root,
                        parent_key: None,
                        name: "root".to_owned(),
                    },
                },
                FilesystemEvent::ObjectObserved {
                    object: FileObjectSnapshot {
                        object_key: file,
                        metadata: metadata(FileKind::File, 42),
                    },
                },
                FilesystemEvent::LinkObserved {
                    link: FileLinkSnapshot {
                        file_link_id: FileLinkId::from_u128(202),
                        object_key: file,
                        parent_key: Some(root),
                        name: "architecture-plan.md".to_owned(),
                    },
                },
            ],
        )
        .expect("snapshot");
    drop(graph);
    (temp, graph_path, index_path)
}

fn metadata(kind: FileKind, size: u64) -> FileMetadata {
    FileMetadata {
        kind,
        size,
        created_at_unix_ms: None,
        modified_at_unix_ms: Some(123),
        hidden: false,
        availability: Availability::Online,
    }
}

fn request(id: u64, method: &str, params: &Value) -> Value {
    let mut params = params.as_object().cloned().expect("params object");
    params.insert(
        "_meta".to_owned(),
        json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": "start-008-e2e", "version": "1"}
        }),
    );
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn exchange(stdin: &mut impl Write, stdout: &mut impl BufRead, request: &Value) -> Value {
    serde_json::to_writer(&mut *stdin, request).expect("write request");
    stdin.write_all(b"\n").expect("newline");
    stdin.flush().expect("flush");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read response");
    assert!(!line.is_empty(), "MCP adapter closed stdout");
    serde_json::from_str(&line).expect("MCP response JSON")
}

#[test]
fn real_stdio_adapter_searches_through_secure_agent_pipe() {
    let (_temp, graph, index) = fixture();
    let service = Arc::new(
        AgentService::open(graph, index, ClientAuthorization::v0_1_metadata()).expect("agent"),
    );
    let pipe_name = format!(
        r"\\.\pipe\LocalSearch\Agent\v1\mcp-e2e-{}",
        std::process::id()
    );
    let (ready_tx, ready_rx) = mpsc::channel();
    let server_name = pipe_name.clone();
    let server_service = Arc::clone(&service);
    let server = std::thread::spawn(move || {
        let server = localsearch_agent::windows_pipe::NamedPipeServer::bind(&server_name)
            .expect("secure bind");
        ready_tx.send(()).expect("ready");
        for _ in 0..2 {
            let service = Arc::clone(&server_service);
            server
                .serve_one(
                    |request, cancelled| service.dispatch_cancellable(request, cancelled),
                    Duration::from_secs(10),
                )
                .expect("serve Agent request");
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("Agent pipe ready");

    let mut process = Command::new(env!("CARGO_BIN_EXE_localsearch-mcp"))
        .args(["--pipe", &pipe_name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn MCP adapter");
    let mut stdin = process.stdin.take().expect("MCP stdin");
    let mut stdout = BufReader::new(process.stdout.take().expect("MCP stdout"));

    let discovery = exchange(
        &mut stdin,
        &mut stdout,
        &request(1, "server/discover", &json!({})),
    );
    assert_eq!(
        discovery["result"]["supportedVersions"],
        json!(["2026-07-28"])
    );

    let tools = exchange(
        &mut stdin,
        &mut stdout,
        &request(2, "tools/list", &json!({})),
    );
    assert_eq!(tools["result"]["tools"].as_array().expect("tools").len(), 3);

    let search = exchange(
        &mut stdin,
        &mut stdout,
        &request(
            3,
            "tools/call",
            &json!({
                "name": "localsearch.search_files",
                "arguments": {"query": "architecture", "top_k": 10}
            }),
        ),
    );
    assert_eq!(search["result"]["resultType"], "complete");
    assert_eq!(search["result"]["isError"], false);
    let hits = search["result"]["structuredContent"]["value"]["hits"]
        .as_array()
        .expect("search hits");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["name"], "architecture-plan.md");
    assert!(
        hits[0]["document_id"]
            .as_str()
            .expect("document id")
            .starts_with("document:")
    );
    assert!(
        hits[0]["file_link_id"]
            .as_str()
            .expect("link id")
            .starts_with("link:")
    );

    drop(stdin);
    assert!(process.wait().expect("MCP exit").success());
    server.join().expect("Agent server");
}

#[test]
fn cancelled_notification_disconnects_agent_request_and_emits_no_response() {
    let pipe_name = format!(
        r"\\.\pipe\LocalSearch\Agent\v1\mcp-cancel-{}",
        std::process::id()
    );
    let (ready_tx, ready_rx) = mpsc::channel();
    let (dispatch_tx, dispatch_rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let server_name = pipe_name.clone();
    let server = std::thread::spawn(move || {
        let server = localsearch_agent::windows_pipe::NamedPipeServer::bind(&server_name)
            .expect("secure bind");
        ready_tx.send(()).expect("ready");
        let result = server.serve_one(
            |request, cancelled| {
                dispatch_tx.send(()).expect("dispatched");
                let started = std::time::Instant::now();
                while !cancelled() && started.elapsed() < Duration::from_secs(3) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                cancel_tx.send(cancelled()).expect("cancellation result");
                AgentResponse::failure(
                    request.request_id,
                    AgentErrorCode::Cancelled,
                    "request cancelled",
                )
            },
            Duration::from_secs(5),
        );
        assert!(
            result.is_err(),
            "disconnected client must reject response write"
        );
    });
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("Agent pipe ready");

    let mut process = Command::new(env!("CARGO_BIN_EXE_localsearch-mcp"))
        .args(["--pipe", &pipe_name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn MCP adapter");
    let mut stdin = process.stdin.take().expect("MCP stdin");
    serde_json::to_writer(
        &mut stdin,
        &request(
            41,
            "tools/call",
            &json!({
                "name": "localsearch.get_index_status",
                "arguments": {}
            }),
        ),
    )
    .expect("write call");
    stdin.write_all(b"\n").expect("newline");
    stdin.flush().expect("flush call");
    dispatch_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("Agent dispatch");

    serde_json::to_writer(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 41, "reason": "stale query"}
        }),
    )
    .expect("write cancellation");
    stdin.write_all(b"\n").expect("newline");
    stdin.flush().expect("flush cancellation");
    assert!(
        cancel_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("cancellation observed"),
        "Agent did not observe MCP cancellation as a disconnected pipe"
    );

    drop(stdin);
    assert!(process.wait().expect("MCP exit").success());
    let mut stdout = String::new();
    process
        .stdout
        .take()
        .expect("MCP stdout")
        .read_to_string(&mut stdout)
        .expect("read MCP stdout");
    assert!(stdout.is_empty(), "cancelled request emitted: {stdout}");
    server.join().expect("Agent server");
}
