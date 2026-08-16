# MCP Adapter

Status: implemented by [`START-008`](START-008.md)
Primary MCP protocol: `2026-07-28`

## 1. Boundary

`localsearch-mcp` is a normal-user adapter, not the search service:

```text
MCP host/client
  -> launches localsearch-mcp over stdio
  -> adapter maps tools to Agent Wire DTO
  -> LocalTransport (current-user Named Pipe on Windows v0.1)
  -> LocalSearch Agent
```

The adapter owns MCP parsing, discovery, version compatibility, and tool schema. The agent owns authorization, query policy, search, ranking, and data access. No MCP type appears in domain crates or Agent API DTOs.

## 2. Protocol baseline

The primary implementation follows the [MCP `2026-07-28` specification](https://modelcontextprotocol.io/specification/2026-07-28). That revision uses stateless, self-contained requests, per-request protocol/capability metadata, and mandatory `server/discover`; it removed the older `initialize`/`notifications/initialized` handshake and protocol-level sessions. See the official [key changes](https://modelcontextprotocol.io/specification/2026-07-28/changelog).

Therefore:

- every modern request is interpreted from its own JSON-RPC body and `_meta`;
- protocol version and client capabilities are validated per request;
- client identity metadata is advisory until mapped to a locally granted client identity;
- adapter behavior never equates a stdio connection with an AI conversation/session;
- any cross-call operation state uses explicit server-minted handles, though v0.1 tools need none;
- `server/discover` advertises supported versions, identity, and capabilities.

## 3. stdio rules

The client launches the adapter as a subprocess. UTF-8 JSON-RPC messages use stdin/stdout according to the official [stdio transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio).

- stdout contains protocol messages only;
- diagnostics go to stderr and exclude sensitive query/result data by default;
- malformed JSON, duplicate in-flight IDs, unsupported versions, and oversized messages fail predictably;
- adapter termination cancels or abandons outstanding Agent requests safely;
- no listening socket is opened.

## 4. v0.1 tools

### `localsearch.search_files`

Input:

```text
query
scope?
extensions?
directory_filter?
top_k?
```

Output contains bounded catalog hits with identity, name, resolved path, availability, match type, rank, and ranking version. It does not include file content or an implied relevance probability.

Required Agent capability: `search.catalog`.

### `localsearch.get_catalog_item`

Input: one `document_id`. Output: current bounded catalog metadata for that link projection.

Required Agent capability: `read.metadata`.

### `localsearch.get_index_status`

Returns sanitized readiness, freshness, supported search features, and unavailable-volume summary without exposing hardware inventory or private filenames.

Required Agent capability: `index.status`.

## 5. Forbidden v0.1 surface

The adapter exposes no tool for:

- reading file content;
- arbitrary path lookup/open;
- filesystem writes or process execution;
- settings changes;
- reindex/reset/delete operations;
- broker access;
- unbounded batch metadata export.

Content tools are v0.2 and require separate user grant plus a new current-user ACL check on each read.

## 6. Capability mapping

MCP-declared client capabilities describe protocol behavior; they do not grant LocalSearch data scopes. LocalSearch grants are maintained locally and enforced by the Agent API.

The adapter maps tools only when both the MCP version/capabilities and LocalSearch grant permit them. Tool descriptions and schemas are deterministic and versioned.

## 7. Compatibility mode

Version 0.1 ships only `2026-07-28`. A legacy `initialize` call fails explicitly and advertises the supported modern version; there is no silent downgrade. A `2025-11-25` compatibility path may be enabled later only when recorded ecosystem testing identifies required clients. It will remain isolated inside the adapter:

```text
MCP 2025 initialize/session-era codec --+
                                         +--> Agent Wire DTO
MCP 2026 stateless/discovery codec ------+
```

Compatibility acceptance requires official-schema fixtures for both eras, explicit version detection, no ambiguous downgrade, and identical LocalSearch authorization/result semantics. The agent never learns which MCP era originated a request.

## 8. Acceptance

- `server/discover` works before any other modern request;
- search succeeds end to end through stdio and `LocalTransport` (Named Pipe on Windows v0.1);
- repeated independent requests do not require retained connection state;
- stdout remains parseable under diagnostics and failures;
- unsupported protocol versions return the correct protocol-level error;
- AI-visible results contain only granted bounded metadata;
- unsupported legacy initialization fails with an actionable modern-version response;
- compatibility mode is not shipped in v0.1.
