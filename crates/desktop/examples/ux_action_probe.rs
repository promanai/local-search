#![forbid(unsafe_code)]

use std::{error::Error, str::FromStr, time::SystemTime};

use localsearch_core::DocumentId;
use localsearch_desktop::{DesktopAgentClient, NamedPipeAgentTransport};
use serde::Serialize;

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ProbeResult {
    Resolved {
        item: localsearch_agent_api::CatalogItem,
    },
    Rejected {
        error: localsearch_desktop::DesktopClientError,
    },
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut pipe = None;
    let mut document_id = None;
    let mut raw = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--pipe" => pipe = arguments.next(),
            "--document-id" => document_id = arguments.next(),
            "--raw" => raw = true,
            _ => return Err(format!("unknown or incomplete argument: {argument}").into()),
        }
    }
    let pipe = pipe.ok_or("--pipe NAME is required")?;
    let document_id = DocumentId::from_str(
        document_id
            .as_deref()
            .ok_or("--document-id DOCUMENT_ID is required")?,
    )?;
    let client = DesktopAgentClient::new(NamedPipeAgentTransport::with_pipe_name(pipe));
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let resolved = if raw {
        client.resolve_item(&format!("ux-raw-{nonce:x}"), document_id)
    } else {
        client.resolve_action_target(&format!("ux-action-{nonce:x}"), document_id)
    };
    let result = match resolved {
        Ok(item) => ProbeResult::Resolved { item },
        Err(error) => ProbeResult::Rejected { error },
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
