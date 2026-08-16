use std::{env, io, sync::Arc};

use localsearch_mcp::{McpAdapter, NamedPipeAgentInvoker, run_stdio};

fn main() {
    if let Err(error) = run() {
        eprintln!("LocalSearch MCP adapter stopped: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let invoker = match arguments.next().as_deref() {
        Some("--pipe") => {
            let pipe_name = arguments.next().ok_or("--pipe requires an endpoint name")?;
            if arguments.next().is_some() {
                return Err("unexpected command-line argument".into());
            }
            NamedPipeAgentInvoker::new(pipe_name)
        }
        None => NamedPipeAgentInvoker::default_endpoint()?,
        Some(_) => return Err("usage: localsearch-mcp [--pipe <local-agent-pipe>]".into()),
    };
    let adapter = Arc::new(McpAdapter::new(invoker));
    run_stdio(io::stdin(), io::stdout(), &adapter)?;
    Ok(())
}
