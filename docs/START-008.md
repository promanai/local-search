# START-008 — MCP stdio Adapter

Status: **PASS**  
Protocol: MCP `2026-07-28`  
Agent boundary: Agent Wire v1 over current-user Windows Named Pipe

## Outcome

`START-008` turns the restart-safe Agent into a bounded local AI integration without granting the
adapter direct access to the durable graph or Tantivy:

```text
MCP host
  -> newline-delimited JSON-RPC over stdio
  -> localsearch-mcp
  -> Agent Wire v1
  -> same-logon local-only Named Pipe
  -> LocalSearch Agent
  -> SQLite/outbox/Tantivy backend
```

The adapter has no normal dependency on `localsearch-agent`, `filesystem-graph`, `rusqlite`,
`catalog-index`, or `tantivy`. A CI dependency-tree gate enforces this boundary.

## Protocol contract

The implementation is stateless MCP `2026-07-28`:

- every request carries and validates protocol version and client capabilities in `_meta`;
- `server/discover` advertises the one shipped version and the tools capability;
- requests are independently identifiable and require no initialization session;
- legacy `initialize` and unsupported revisions fail explicitly without downgrade;
- messages are one bounded UTF-8 JSON value per line;
- stdout contains protocol responses only, while process failures use stderr;
- at most 16 requests may be in flight and every line is capped at 1 MiB;
- duplicate in-flight IDs and malformed/oversized frames fail predictably;
- `notifications/cancelled` stops the adapter response and disconnects the Agent pipe request.

## AI-visible surface

The checked-in, deterministic tool manifest is [`contracts/mcp-tools-v1.json`](../contracts/mcp-tools-v1.json).
Only three read-only metadata tools exist:

1. `localsearch.search_files` — bounded catalog search, maximum AI-visible `top_k = 50`;
2. `localsearch.get_catalog_item` — one canonical `DocumentId` lookup;
3. `localsearch.get_index_status` — sanitized readiness and backlog state.

`tools/list` is filtered from the Agent's trusted capability grant. MCP client metadata cannot grant
LocalSearch access. There are no content-read, arbitrary-path, write, process, settings, reindex,
reset, broker, or administrative tools.

## Cancellation path

```text
notifications/cancelled(requestId)
  -> per-request atomic cancellation token
  -> cancellable Named Pipe client wait
  -> client pipe handle closes
  -> Agent observes disconnect
  -> AgentService dispatch_cancellable stops between query phases
  -> no MCP response is emitted for the cancelled request
```

EOF applies the same cancellation to every outstanding request before process exit.

## Evidence

The process-level integration test starts a real `localsearch-mcp` subprocess and a secured Agent
Named Pipe backed by the durable SQLite/outbox/Tantivy chain. It proves discovery, capability-derived
tool listing, and `search "architecture"` returning `architecture-plan.md` with canonical document,
link, and object identities. A second process test proves cancellation reaches the Agent as a pipe
disconnect and produces no stale stdout response.

The unit/contract suite additionally proves:

- deterministic grant-filtered tool exposure;
- MCP search arguments map to versioned Agent DTOs;
- independent repeated discovery requests retain no session state;
- unsupported/legacy version handling is explicit;
- an oversized line is rejected without desynchronizing the next message.

## Deliberate exclusions

MCP `2025-11-25` compatibility, content reads, pagination, resources/prompts, TCP/HTTP transport,
service installation/autostart, UI, and broker operations remain outside `START-008`.
