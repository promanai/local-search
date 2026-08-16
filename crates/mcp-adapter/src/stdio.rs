use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use serde_json::Value;

use crate::{AgentInvoker, McpAdapter, McpId, McpRequest, McpResponse};

/// Maximum UTF-8 bytes in one MCP stdio JSON line.
pub const MAX_STDIO_MESSAGE_BYTES: usize = 1_048_576;
/// Maximum independently cancellable requests admitted at once.
pub const MAX_IN_FLIGHT: usize = 16;

/// Serve newline-delimited MCP JSON-RPC until stdin reaches EOF.
///
/// A dedicated writer owns stdout, so diagnostics can never corrupt protocol frames. On EOF every
/// in-flight Agent call is cancelled and joined before returning.
///
/// # Errors
///
/// Returns an IO error from stdin/stdout or when an internal worker terminates unexpectedly.
pub fn run_stdio<R, W, I>(reader: R, writer: W, adapter: &Arc<McpAdapter<I>>) -> io::Result<()>
where
    R: Read,
    W: Write + Send + 'static,
    I: AgentInvoker + 'static,
{
    let (sender, receiver) = mpsc::channel::<String>();
    let writer_thread = thread::spawn(move || -> io::Result<()> {
        let mut writer = writer;
        for message in receiver {
            writer.write_all(message.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        Ok(())
    });

    let in_flight = Arc::new(Mutex::new(HashMap::<String, Arc<AtomicBool>>::new()));
    let mut workers = Vec::new();
    let mut reader = BufReader::new(reader);

    loop {
        match read_bounded_line(&mut reader)? {
            Line::Eof => break,
            Line::Oversized => {
                let response =
                    McpResponse::error(None, -32600, "MCP stdio message exceeds size limit", None);
                send_response(&sender, &response)?;
            }
            Line::Message(bytes) => {
                handle_message(&bytes, &sender, &in_flight, adapter, &mut workers)?;
            }
        }
    }

    if let Ok(active) = in_flight.lock() {
        for token in active.values() {
            token.store(true, Ordering::Release);
        }
    }
    for worker in workers {
        worker
            .join()
            .map_err(|_| io::Error::other("MCP request worker panicked"))?;
    }
    drop(sender);
    writer_thread
        .join()
        .map_err(|_| io::Error::other("MCP stdout writer panicked"))??;
    Ok(())
}

type InFlight = Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>;

fn handle_message<I: AgentInvoker + 'static>(
    bytes: &[u8],
    sender: &mpsc::Sender<String>,
    in_flight: &InFlight,
    adapter: &Arc<McpAdapter<I>>,
    workers: &mut Vec<thread::JoinHandle<()>>,
) -> io::Result<()> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        let response = McpResponse::error(None, -32700, "invalid JSON", None);
        return send_response(sender, &response);
    };
    if is_cancel_notification(&value) {
        cancel_request(&value, in_flight);
        return Ok(());
    }
    let Ok(request) = serde_json::from_value::<McpRequest>(value) else {
        let response = McpResponse::error(None, -32600, "invalid JSON-RPC request", None);
        return send_response(sender, &response);
    };
    let Some(id) = request.id.clone() else {
        // Unknown notifications have no response by JSON-RPC contract.
        return Ok(());
    };
    let key = id.key();
    let token = Arc::new(AtomicBool::new(false));
    {
        let mut active = in_flight
            .lock()
            .map_err(|_| io::Error::other("in-flight request state poisoned"))?;
        if active.contains_key(&key) {
            let response =
                McpResponse::error(Some(id), -32600, "duplicate in-flight request id", None);
            return send_response(sender, &response);
        }
        if active.len() >= MAX_IN_FLIGHT {
            let response =
                McpResponse::error(Some(id), -32000, "too many in-flight requests", None);
            return send_response(sender, &response);
        }
        active.insert(key.clone(), Arc::clone(&token));
    }

    let worker_sender = sender.clone();
    let worker_active = Arc::clone(in_flight);
    let worker_adapter = Arc::clone(adapter);
    workers.push(thread::spawn(move || {
        let response = worker_adapter.handle(request, &|| token.load(Ordering::Acquire));
        if !token.load(Ordering::Acquire) {
            let _ = send_response(&worker_sender, &response);
        }
        if let Ok(mut active) = worker_active.lock() {
            active.remove(&key);
        }
    }));
    Ok(())
}

enum Line {
    Eof,
    Message(Vec<u8>),
    Oversized,
}

fn read_bounded_line(reader: &mut impl BufRead) -> io::Result<Line> {
    let mut message = Vec::new();
    let mut oversized = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if message.is_empty() && !oversized {
                Ok(Line::Eof)
            } else if oversized {
                Ok(Line::Oversized)
            } else {
                trim_cr(&mut message);
                Ok(Line::Message(message))
            };
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buffer.len(), |index| index + 1);
        let content_end = newline.unwrap_or(buffer.len());
        if !oversized {
            if message.len().saturating_add(content_end) > MAX_STDIO_MESSAGE_BYTES {
                oversized = true;
                message.clear();
            } else {
                message.extend_from_slice(&buffer[..content_end]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            if oversized {
                return Ok(Line::Oversized);
            }
            trim_cr(&mut message);
            return Ok(Line::Message(message));
        }
    }
}

fn trim_cr(message: &mut Vec<u8>) {
    if message.last() == Some(&b'\r') {
        message.pop();
    }
}

fn send_response(sender: &mpsc::Sender<String>, response: &McpResponse) -> io::Result<()> {
    let encoded = serde_json::to_string(response)
        .map_err(|_| io::Error::other("MCP response encoding failed"))?;
    sender
        .send(encoded)
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "MCP stdout is closed"))
}

fn is_cancel_notification(value: &Value) -> bool {
    value.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && value.get("id").is_none()
        && value.get("method").and_then(Value::as_str) == Some("notifications/cancelled")
}

fn cancel_request(value: &Value, in_flight: &Mutex<HashMap<String, Arc<AtomicBool>>>) {
    let Some(request_id) = value
        .get("params")
        .and_then(|params| params.get("requestId"))
    else {
        return;
    };
    let id = if let Some(number) = request_id.as_i64() {
        McpId::Number(number)
    } else if let Some(string) = request_id.as_str() {
        McpId::String(string.to_owned())
    } else {
        return;
    };
    if let Ok(active) = in_flight.lock()
        && let Some(token) = active.get(&id.key())
    {
        token.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_recovers_after_oversized_line() {
        let mut input = vec![b'x'; MAX_STDIO_MESSAGE_BYTES + 1];
        input.extend_from_slice(b"\n{}\n");
        let mut reader = BufReader::new(input.as_slice());
        assert!(matches!(
            read_bounded_line(&mut reader),
            Ok(Line::Oversized)
        ));
        match read_bounded_line(&mut reader) {
            Ok(Line::Message(message)) => assert_eq!(message, b"{}"),
            _ => panic!("expected bounded second message"),
        }
    }
}
