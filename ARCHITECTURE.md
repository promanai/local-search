# LocalSearch Architecture

Status: accepted baseline for engineering prototype  
Version: 0.1  
MVP target: Windows 10/11, x64 and ARM64

Architecture target: Windows, macOS, and Linux
Primary language: Rust  
Local search engine: Tantivy

## 1. Product boundary

LocalSearch v0.1 is a Windows-first, Everything-class local catalog search product. It indexes file-system identity, names, paths, and selected metadata; keeps the catalog current from NTFS change journals; and exposes an always-resident launcher with interactive search. Search domain, durable projection, and client contracts remain portable to macOS and Linux; v0.1 does not implement those platform adapters.

Content extraction, OCR, semantic search, OpenSearch, cloud sources, NAS federation, and multi-device search are not part of v0.1.

The v0.1 user outcome is:

```text
Ctrl+Space
    -> resident window becomes visible and focused
    -> user types a filename fragment
    -> exact/prefix/token/substring/fuzzy results appear within the latency budget
    -> create/rename/move/delete changes arrive without a full rescan
```

## 2. Architectural invariants

1. Tantivy is a rebuildable implementation detail, not the product data model.
2. SQLite is the durable control plane and source of indexing truth.
3. Interactive search always has priority over background work.
4. Catalog search works independently of content indexing.
5. Files and extracted content remain local by default.
6. The elevated component never reads file content, owns user indexes, or serves search.
7. A privileged component must never be used to bypass a user's ACLs for content access.
8. File identity is independent of path; physical objects and namespace links are distinct.
9. Search indexes are versioned projections and may be rebuilt or switched atomically.
10. OpenSearch compatibility is maintained through canonical contracts, not Tantivy index compatibility.
11. Weak and powerful machines run the same architecture with different background budgets.
12. Semantic search remains optional and can never be required for lexical search.
13. The Agent API is the sole client boundary; UI, CLI, MCP, and future gateways are adapters or clients.
14. The agent never listens on TCP in v0.1.
15. Operating-system integration is an adapter, not part of the search domain.
16. Clients use the same Search API on every platform; platform capabilities and performance may differ.

## 3. Process and privilege architecture

```text
 Desktop UI       CLI/tests       Local AI
 normal user      normal user        |
      |                |             v
      |                |       MCP stdio adapter
      +----------------+-------------+
                       | versioned Named Pipe IPC
                       v
+------------------------------+
| LocalSearch Agent            |
| normal user                  |
|                              |
| Agent API                    |
| Query engine and ranking     |
| Tantivy catalog indexes      |
| SQLite durable state         |
| Resource governor            |
| Content extraction (v0.2)    |
+---------------+--------------+
                | authenticated, least-privilege IPC
                v
+------------------------------+
| Platform Broker (optional)   |
| elevated on Windows v0.1     |
|                              |
| volume discovery             |
| FSCTL_ENUM_USN_DATA          |
| FSCTL_QUERY_USN_JOURNAL      |
| FSCTL_READ_USN_JOURNAL       |
+------------------------------+
```

### Desktop UI

The desktop process is resident and normally hidden. The global shortcut shows and focuses an already-created window. The UI does not open volume handles, mutate indexes, or access SQLite directly.

### MCP adapter

`localsearch-mcp` is a separate normal-user stdio process launched by an MCP client. It translates MCP tools to the versioned Agent API over the same Named Pipe used by other local clients. The agent and domain crates contain no MCP types or protocol state.

### LocalSearch Agent

The per-user agent is the application boundary. It owns canonical state, queues, indexing actors, search readers, ranking, and the user index directory. Later content extraction runs with the user's token so normal Windows ACL enforcement applies.

### Platform broker

The broker is capability-based and optional. Windows v0.1 uses an elevated WinFS broker for fast MFT/USN access. A future macOS adapter must not use a helper to bypass user privacy consent, and a normal Linux desktop configuration should not require a root daemon. Any broker is deliberately narrow and has no arbitrary content-read operation.

The prototype may assume a single-user workstation for metadata visibility. A public release is blocked on `SECURITY-001`, which must prove ACL-aware metadata visibility or provide a secure non-elevated fallback.

## 4. Component layout

```text
crates/
  core/              canonical IDs, documents, requests, responses, errors
  query/             query AST, parser, normalization, cost estimation
  tantivy-catalog/   schema compiler, writer, reader, candidate retrieval
  platform-core/     filesystem/resource provider contracts
  state/             SQLite schema, filesystem graph, outbox, checkpoints

platform/
  windows/           MFT, USN, Windows resource probes, Named Pipe adapter
  macos/             future crawl, FSEvents, macOS probes, Unix socket adapter
  linux/             future crawl, watcher/reconciliation, Linux probes, Unix socket adapter

apps/
  fs-service/        elevated WinFS broker
  agent/             per-user search and indexing service
  mcp-server/        stdio MCP adapter for local AI
  desktop/           resident Tauri 2 launcher

tools/
  bench/             deterministic datasets, workloads, reports
```

Only `platform/windows` is implemented for v0.1. The macOS/Linux directories are architectural destinations, not empty scaffolding requirements. Other deferred components are not created until their roadmap phase: HTTP gateway, extractors, extractor host, semantic backends, USearch, OpenSearch, and export tooling.

## 5. Platform boundary

`core` contains no MFT, USN, FSEvents, inode, Win32, Apple framework, Linux syscall, Named Pipe, or Unix socket types. `platform-core` defines canonical observation/checkpoint/capability contracts and synchronous provider ports that adapters can implement behind bounded worker actors.

```text
FilesystemProvider
  capabilities()
  discover_volumes()
  initial_scan(volume, sink)
  read_changes(checkpoint, limit, sink)
  reconcile(volume, sink)

ResourceProvider
  snapshot()
```

The common ingestion pipeline consumes canonical filesystem events. Platform-specific cursors remain opaque provider checkpoints. Windows uses MFT/USN, macOS may use crawl/FSEvents, and Linux selects crawl plus watcher/reconciliation only after platform spikes.

Agent Protocol is transport-neutral. Windows v0.1 maps it to a secured Named Pipe; future macOS/Linux builds map it to a permission-restricted Unix domain socket. MCP continues to use stdio and calls the same local transport abstraction.

## 6. Canonical contracts

No public contract contains Tantivy, SQLite, Windows handle, or OpenSearch types.

The initial ports are:

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

CatalogIndexPort
  apply_batch(MutationBatch) -> CommitToken
  checkpoint() -> IndexCheckpoint

IndexAdminPort (internal, not granted to AI clients)
  health() -> IndexHealth
  schema_version() -> SchemaVersion
  stats() -> IndexStats
```

The versioned Agent Wire DTO is the source of truth for client communication. Local-transport codecs, MCP mappings, and a future opt-in HTTP schema adapt this DTO; OpenAPI is not an independent authority. The Windows local-transport adapter uses a Named Pipe.

Deadlines, request identifiers, cancellation, hard limits, and capability enforcement belong to the agent API. Tantivy work runs behind a dedicated blocking actor; a future OpenSearch adapter may use asynchronous network I/O without changing domain types.

Search results expose stable identity, `rank`, `match_type`, and `ranking_version`. A backend score is not a public relevance probability. v0.1 returns a bounded `top_k` and has no cursor pagination.

See [LOCAL-API.md](docs/LOCAL-API.md), [MCP-ADAPTER.md](docs/MCP-ADAPTER.md), [API-SECURITY.md](docs/API-SECURITY.md), and [PROTOCOL-COMPATIBILITY.md](docs/PROTOCOL-COMPATIBILITY.md).

## 7. Identity and filesystem projection

Canonical identity is filesystem-neutral:

```text
MachineId
VolumeId
FileId128
FileKey { volume_id, file_id }
FileLinkId
```

`FileObject` represents one physical filesystem object. `FileLink` represents a directory entry that binds a parent, name, and object. Multiple links may refer to one object. `ResolvedPath` is computed data, never identity.

SQLite contains the authoritative filesystem graph. Tantivy contains a denormalized searchable path projection. A directory rename updates the graph immediately and schedules a background subtree path refresh. Results resolve their displayed path from current graph state, while path-term search is eventually consistent during the refresh.

See [DATA-MODEL.md](docs/DATA-MODEL.md) and [FS-IDENTITY.md](docs/FS-IDENTITY.md).

## 8. Durable indexing model

```text
FilesystemEvent batch + opaque ProviderCheckpoint
    -> SQLite transaction
       - update filesystem graph
       - append idempotent index mutations
       - advance provider checkpoint
    -> commit SQLite
    -> read pending mutations
    -> apply delete-by-id plus add to Tantivy
    -> commit Tantivy
    -> record index generation and applied sequence in SQLite
    -> compact acknowledged mutations later
```

This creates at-least-once projection. A crash can repeat a mutation but cannot legitimately skip one. Tantivy is a materialized view and can be rebuilt from durable state without blindly rescanning the entire computer when the required state is retained.

See [INDEX-CONSISTENCY.md](docs/INDEX-CONSISTENCY.md) and [INDEXING-PIPELINE.md](docs/INDEXING-PIPELINE.md).

## 9. Search and ranking

The initial catalog projection evaluates exact, token, prefix, limited substring, path, extension, and bounded fuzzy clauses. Substring search uses n-grams only to generate candidates. The product verifies the normalized filename before ranking the result.

One- and two-character queries do not execute substring search. Every query has a cost budget, result cap, deadline, and cancellation path.

Ranking is two-stage:

1. Tantivy retrieves the top candidate window.
2. Product ranking returns the final top results using stable match-class ordering.

The required ordering is `exact > prefix > token > substring > path`; lexical score, recency, type, and directory proximity are secondary signals. Backends must pass the same ranking contract tests.

See [TANTIVY-SCHEMA.md](docs/TANTIVY-SCHEMA.md) and [QUERY-ENGINE.md](docs/QUERY-ENGINE.md).

## 10. Resource control

The platform-neutral governor consumes `SystemResources` from a platform `ResourceProvider`. A v0.1 interface exposes indexing budget and reports search latency/system pressure; adaptive policy is calibrated only after spike measurements. Platform adapters collect CPU, memory, storage, and power observations without leaking OS-native types into governor logic.

Search arrival may pause producers, but already-running blocking work must remain small enough not to monopolize CPU, memory, or disk.

See [RESOURCE-GOVERNOR.md](docs/RESOURCE-GOVERNOR.md).

## 11. Index lifecycle

All durable formats have independent versions:

```text
canonical_schema_version
state_schema_version
catalog_schema_version
content_schema_version
vector_schema_version
tantivy_format_version
extractor_pipeline_version
embedding_model_version
agent_api_version
agent_ipc_codec_version
broker_protocol_version
mcp_protocol_version
```

Breaking catalog changes build a new version beside the active index, validate it, atomically switch an active manifest, and remove the old index only after a safe retention period.

See [INDEX-MIGRATIONS.md](docs/INDEX-MIGRATIONS.md).

## 12. Platform and release policy

- x64 is the first performance-validation platform.
- ARM64 must compile in CI from the beginning and receive native smoke testing before public release.
- `core` and `platform-core` must compile and test on Windows, macOS, and Linux in CI.
- Windows v0.1 is the only initial product/runtime commitment.
- macOS targets Apple Silicon first after a dedicated crawl/FSEvents/privacy spike.
- Linux selects inotify/fanotify and filesystem identity policy only after a dedicated spike; no root daemon is assumed.
- Engineering spikes may expose volume metadata under the explicit single-user assumption.
- Public release requires `SECURITY-001` and installer/service lifecycle validation.
- v0.1 performance claims are valid only against the versioned benchmark protocol and named reference machines.

## 13. Quality gates

Product implementation beyond the vertical slice is blocked until these risks are demonstrated:

1. MFT/USN enumeration and recovery behavior at realistic scale.
2. Tantivy filename retrieval, index amplification, and latency at 1M and 5M records.
3. Crash-safe SQLite outbox to Tantivy projection under injected failures.

The executable sequence and acceptance criteria are defined in [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md).
