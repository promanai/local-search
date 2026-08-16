# Local Agent API

Status: implemented by [`START-007`](START-007.md)

## 1. Role

LocalSearch Agent is a per-user local search service. Desktop UI, CLI/tests, MCP, and future integration gateways are clients. No client opens Tantivy or SQLite directly.

```text
Domain model and ports
          |
          v
Agent Wire DTO (source of truth)
          |
          +-- Named Pipe codec (Windows v0.1)
          +-- Unix domain socket codec (future macOS/Linux)
          +-- MCP adapter mapping (v0.1)
          +-- HTTP gateway DTO/OpenAPI generation (future)
```

Agent API versioning is independent of index schemas, IPC framing, and MCP protocol versions.

## 2. Ports

```text
CatalogSearchPort
  search(SearchRequest) -> SearchResponse

CatalogLookupPort
  get_catalog_item(DocumentId) -> CatalogItem
  get_catalog_items(BoundedDocumentIds) -> CatalogItems

IndexStatusPort
  status() -> IndexStatus

CapabilitiesPort
  capabilities() -> Capabilities
```

Administrative ports are internal and are not included in normal AI grants. Suggestion, pagination, write operations, content, semantic search, and settings are absent from v0.1 unless a later accepted requirement changes the scope.

## 3. v0.1 methods

| Method | Required capability | Limits |
| --- | --- | --- |
| `catalog.search` | `search.catalog` | Server-clamped query bytes, filters, candidate window, `top_k`, deadline |
| `catalog.get_item` | `read.metadata` | One `DocumentId` |
| `catalog.get_items` | `read.metadata` | Bounded unique IDs and response bytes |
| `index.get_status` | `index.status` | Sanitized per-user index state |
| `agent.get_capabilities` | Connected authorized client | No sensitive machine inventory |

There is no method that accepts a path and performs an arbitrary privileged filesystem operation.

## 4. Request envelope

Conceptual Agent Wire DTO:

```text
AgentRequest
  protocol_version
  request_id
  method
  deadline_ms
  params
```

The server derives client identity and granted capabilities from the authenticated IPC endpoint/authorization state. A caller cannot grant itself a scope by adding it to a request.

Connection negotiation may cache compatible Agent protocol information, but every request remains independently identifiable, bounded, cancellable, and authorized. Agent connection behavior is not coupled to MCP session semantics.

## 5. Search request

```text
SearchRequest
  query_text
  scope: all | files | folders
  filters
  top_k
```

The server supplies or clamps execution deadline, candidate limit, fuzzy expansion, and response size according to client class and governor state. `top_k` has a small product maximum; the default desktop value is 50 and typical AI values are 10–50.

v0.1 has no cursor pagination. Adding it later requires explicit index-generation, ordering, expiration, and mutation semantics.

## 6. Search hit

```text
SearchHit
  document_id
  object_key
  file_link_id
  name
  resolved_path
  extension?
  kind
  modified_at?
  availability
  match_type
  rank
  ranking_version
```

`document_id` identifies the catalog projection/link hit. `object_key` identifies the physical object. `file_link_id` identifies the namespace link. These fields are not interchangeable because one object may have multiple paths.

The public response does not expose a backend score as a normalized relevance value. Optional internal diagnostics may include score components only through a separate sensitive diagnostic surface.

## 7. Lookup semantics

Lookup uses `DocumentId`, not a path or ambiguous `file_id`. It resolves the current graph state and can return:

- current metadata;
- moved/renamed redirect information where safely retained;
- unavailable/offline state;
- not found/deleted;
- access denied for any operation requiring a new file open.

Later content reads must reopen the current object/link under the user's token and recheck identity/version and ACL. Old indexed metadata never authorizes content access.

## 8. Errors

Stable categories include:

```text
InvalidRequest
UnsupportedProtocolVersion
UnsupportedCapability
Unauthorized
Forbidden
QueryPolicyRejected
DeadlineExceeded
Cancelled
NotFound
Unavailable
IndexNotReady
ResourceExhausted
Internal
```

Wire errors include a correlation/request ID and safe structured details. Paths, query text, tokens, and internal backend errors are redacted by default.

## 9. Named Pipe transport

The Windows v0.1 endpoint is version-namespaced conceptually as:

```text
\\.\pipe\LocalSearch\Agent\v1
```

The exact deployed name includes the correct user/session isolation strategy. Requirements:

- explicit DACL for the intended current logon SID and required service identities only;
- remote clients rejected;
- bounded length-prefixed frames and bounded nesting/collection sizes;
- one response per request ID, with out-of-order completion permitted only if specified by the codec;
- cancellation tied to request ID;
- no implicit trust in claimed client metadata;
- no TCP listener.

Future macOS/Linux agents use a permission-restricted Unix domain socket through the same Agent Wire DTO. Transport choice never changes search identities, methods, ranking semantics, or capability names.

See [API-SECURITY.md](API-SECURITY.md) and [PROTOCOL-COMPATIBILITY.md](PROTOCOL-COMPATIBILITY.md).

## 10. Contract generation and tests

The checked-in Agent Wire schema/DTO definition is authoritative. Codecs and adapters are generated or validated against it where tooling permits. Future OpenAPI is generated from the HTTP adapter representation and must map back to the same operations; it is not a competing domain model.

Contract tests cover every method, error category, limit, capability denial, identity distinction, unknown required field/version, cancellation, and path change between search and lookup.
