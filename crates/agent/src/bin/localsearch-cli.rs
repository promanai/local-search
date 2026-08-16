#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use localsearch_agent::windows_pipe::{default_pipe_name, round_trip};
    use localsearch_agent_api::{
        AGENT_API_VERSION, AGENT_CODEC_VERSION, AgentRequest, ContentSearchRequest,
        RequestOperation,
    };
    use localsearch_core::{SearchFilter, SearchRequest, SearchScope};

    let mut pipe: Option<String> = None;
    let mut positional = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--pipe" {
            pipe = arguments.next();
        } else {
            positional.push(argument);
        }
    }
    let operation = match positional.first().map(String::as_str) {
        Some("search") => {
            let query = positional.get(1..).unwrap_or_default().join(" ");
            if query.is_empty() {
                return Err("search requires a query".into());
            }
            RequestOperation::CatalogSearch(SearchRequest {
                query,
                scope: SearchScope::All,
                filters: SearchFilter::default(),
                top_k: 50,
            })
        }
        Some("content") => {
            let query = positional.get(1..).unwrap_or_default().join(" ");
            if query.is_empty() {
                return Err("content requires a query".into());
            }
            RequestOperation::ContentSearch(ContentSearchRequest { query, top_k: 50 })
        }
        Some("status") => RequestOperation::IndexGetStatus,
        Some("capabilities") => RequestOperation::AgentGetCapabilities,
        Some("health") => RequestOperation::AgentGetHealth,
        _ => {
            return Err(
                "usage: localsearch-cli [--pipe NAME] <search QUERY|content QUERY|status|capabilities|health>"
                    .into(),
            );
        }
    };
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let request = AgentRequest {
        protocol_version: AGENT_API_VERSION,
        codec_version: AGENT_CODEC_VERSION,
        request_id: format!("cli-{nonce:x}"),
        deadline_ms: 2_000,
        operation,
    };
    let pipe = pipe.map_or_else(default_pipe_name, Ok)?;
    let response = round_trip(&pipe, &request, Duration::from_secs(5))?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    if response.error.is_some() {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("LocalSearch CLI v0.1 Named Pipe transport requires Windows");
    std::process::exit(2);
}
