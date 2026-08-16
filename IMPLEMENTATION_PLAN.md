# LocalSearch v0.1 Implementation Plan

Status: ready for implementation review  
Scope: catalog search only  
Architecture authority: [ARCHITECTURE.md](ARCHITECTURE.md)

## 1. Delivery definition

LocalSearch v0.1 is complete when a Windows user can install the product, keep its launcher resident, invoke it with a configurable global shortcut, search a catalog containing 1–5 million files by filename and path, and observe live NTFS changes without routine full rescans. The same search must be available through the versioned Agent API and the read-only MCP stdio adapter.

The release must satisfy security, recovery, latency, resource, packaging, upgrade, and uninstall gates. Passing a synthetic search benchmark alone is not a release.

## 2. Work rules

Every implementation item must provide:

- a documented input/output contract;
- unit or integration tests proportional to failure risk;
- structured errors with stable categories;
- observability sufficient to diagnose acceptance failures;
- no dependency leakage across architectural boundaries;
- benchmark evidence for performance-sensitive choices;
- a short decision record when an experiment changes this plan.

Versions are pinned only when the corresponding implementation item begins. Dependency upgrades require tests and benchmark comparison when they can affect index format, query behavior, IPC, or packaging.

## 3. Risk spikes

The three spikes are production-informed experiments, not disposable demos. Their reports live under `reports/spikes/` and record hardware, dataset, commit SHA, parameters, raw measurements, and conclusions.

### SPIKE-001: MFT and USN

Goal: prove volume discovery, MFT enumeration, path graph construction, journal continuation, and gap detection.

Required cases:

- enumerate 1M records and extrapolate/test 5M and 10M where practical;
- decode supported USN record versions without assuming one fixed layout;
- capture `JournalId`, `LowestValidUsn`, and next cursor;
- observe create, delete, rename-old, rename-new, move, and hard-link signals;
- detect journal recreation and a saved cursor below the valid range;
- reconstruct paths from parent identities;
- classify reparse points without following them.

Exit evidence:

- throughput, CPU, peak RAM, and error counts;
- replayable binary fixtures with sensitive names removed;
- explicit privilege and failure behavior;
- no lost event in the tested snapshot-to-tail handoff protocol.

### SPIKE-002: Tantivy filename retrieval

Goal: select a catalog schema and retrieval strategy empirically.

Datasets: 100K, 1M, and 5M synthetic but representative records. A 10M run is a scale validation, not a prerequisite for the first schema choice.

Compared strategies:

- exact field;
- Unicode-normalized token field;
- prefix implementation alternatives;
- limited n-gram alternatives;
- bounded fuzzy expansion;
- candidate window sizes of 200–500;
- post-verification and two-stage ranking.

Exit evidence:

- indexing time and documents/sec;
- index size and amplification by field;
- peak and steady RAM;
- warm and cold p50/p95/p99 by workload;
- false-candidate ratio for substring retrieval;
- selected schema parameters and rejected alternatives.

### SPIKE-003: durable projection recovery

Goal: prove the SQLite mutation outbox and Tantivy projection recover deterministically.

Inject process termination:

- before SQLite commit;
- after SQLite commit;
- during Tantivy batch application;
- after Tantivy commit;
- before mutation acknowledgement;
- during mutation-log compaction.

Exit evidence:

- repeated recovery produces the same logical index;
- no acknowledged mutation is absent from the index;
- replay does not create duplicate live documents;
- checkpoint invariants are machine-checked;
- corrupt or missing Tantivy state has a defined rebuild path.

## 4. Implementation sequence

### START-001: Foundation

Deliverables:

- Git repository and Rust Cargo workspace;
- formatting, linting, tests, dependency audit, x64 CI, ARM64 compile CI;
- component directories from `ARCHITECTURE.md`;
- canonical IDs, versions, errors, requests/responses, and mutation types;
- canonical filesystem observations plus `FilesystemProvider` and `ResourceProvider` boundaries;
- clock, cancellation, and filesystem interfaces suitable for testing;
- architecture dependency tests where practical.

Acceptance:

- all workspace crates compile on x64;
- `core` and `platform-core` compile and test on Windows, macOS, and Linux;
- supported libraries do not import Tantivy, SQLite, Tauri, or Windows bindings through `core`;
- platform contracts contain no MFT, USN, FSEvents, inode, Named Pipe, or Unix socket native types;
- IDs and wire representations have round-trip tests;
- malformed requests fail without panic.

### START-002: Tantivy catalog retrieval

Deliverables:

- deterministic catalog generator;
- experimental catalog schemas without selecting v1 defaults;
- exact, token, and initial prefix retrieval;
- single-writer actor and reusable reader;
- benchmark JSON/CSV plus human-readable report using the shared engineering-gate format.

Acceptance:

- deterministic 100K, 1M, and 5M datasets can be indexed repeatedly from a recorded seed;
- warm and cold exact/token/prefix workloads report p50/p95/p99 against the provisional budget;
- commit/reload visibility is deterministic in tests;
- the report identifies index size, RAM, throughput, and latency.

### START-003: Substring and product ranking

Deliverables:

- comparison of trigram, bounded n-gram, and token/prefix/trigram-fallback strategies;
- mandatory normalized substring verification;
- one/two-character query fallback;
- two-stage product ranker and stable match classification;
- query cost estimator and candidate caps.

Acceptance:

- exact results outrank prefix, token, substring, and path matches;
- candidate verification prevents n-gram false positives;
- prohibited expensive shapes return a typed policy error;
- 100K, 1M, and 5M comparison reports cover query-length and rare/common/worst-case bands.

### START-004: Windows filesystem spike and adapter

Deliverables:

- volume discovery and capability classification;
- safe handle wrappers;
- MFT/USN record decoder with bounds checking;
- snapshot-to-journal handoff protocol;
- journal identity and gap detection;
- sanitized fixture tests.
- reusable provider contract suite for current and future platform adapters.

Acceptance:

- SPIKE-001 exit criteria pass;
- malformed/truncated record buffers cannot cause memory unsafety;
- handles are closed on every error path;
- unsupported filesystems return a capability result, not a panic.
- no Windows-native type crosses the platform adapter boundary.
- create/metadata/rename/move/directory-rename/delete, hard links, restart/resume, journal gap/recreation, and offline/online scenarios have explicit evidence.

### START-005: Filesystem graph

Deliverables:

- `FileObject`, `FileLink`, and parent relationships in SQLite;
- current-path resolution with cycle and missing-parent protection;
- normal-file and directory update handling;
- rename/move state machine;
- hard-link and reparse-point policies;
- subtree path-refresh job creation.

Acceptance:

- renames preserve object identity;
- a link change does not imply content change;
- result display resolves the new path before Tantivy path refresh finishes;
- graph corruption produces a bounded error rather than unbounded recursion.

### START-006: Durable state and projection

Deliverables:

- versioned SQLite migrations;
- source cursors, mutation outbox, catalog checkpoints, and generations;
- idempotent upsert/delete projection;
- recovery coordinator and log compaction;
- fault-injection harness.

Acceptance:

- SPIKE-003 exit criteria pass;
- opaque `source_checkpoint` and `applied_seq` cannot be confused in the type/API model;
- Tantivy loss can be detected and a rebuild scheduled from durable state;
- every state transition is covered by a recovery test.

### START-007: Agent API

Deliverables:

- per-user agent process around the existing search/index components;
- `CatalogSearchPort`, `CatalogLookupPort`, `IndexStatusPort`, and `CapabilitiesPort`;
- versioned Agent Wire DTO as the client-contract source of truth;
- transport-neutral `LocalTransport` port plus a Windows current-user Named Pipe adapter with explicit DACL and remote-client rejection;
- bounded framing, request IDs, deadlines, cancellation, and capability grants;
- CLI/test client using only the public Agent API;
- no TCP listener and no cursor pagination.

Acceptance:

- CLI to `LocalTransport` to agent to Tantivy returns an `architecture` search result on the Windows Named Pipe profile;
- no public DTO contains Tantivy, SQLite, MCP, or Windows handle types;
- a search hit contains `document_id`, `object_key`, `file_link_id`, current `resolved_path`, `rank`, `match_type`, and `ranking_version`;
- backend scores are absent from the normal public response;
- malformed, oversized, unauthorized, expired, and cancelled requests fail with stable typed errors;
- v0.1 capabilities are limited to `search.catalog`, `read.metadata`, and `index.status`.

### START-008: MCP stdio adapter

Deliverables:

- separate `localsearch-mcp` executable;
- stdio JSON-RPC transport with protocol output isolated from stderr logging;
- primary MCP `2026-07-28` implementation;
- `server/discover` and stateless per-request metadata handling;
- tools `localsearch.search_files`, `localsearch.get_catalog_item`, and `localsearch.get_index_status`;
- mapping to Agent Wire DTO over Named Pipe;
- compatibility decision and fixture suite for `2025-11-25` clients.

Acceptance:

- an MCP client can launch the adapter and search the catalog end to end;
- each modern MCP request is handled without connection/session conversation state;
- MCP version/capability differences do not enter the agent or domain model;
- the adapter exposes no content-read, filesystem-write, settings, or admin tool;
- older-protocol compatibility, if enabled by the recorded ecosystem decision, passes isolated dual-era fixtures.

### START-009: Process and WinFS service architecture

Deliverables:

- installable elevated `fs-service` prototype;
- authenticated, versioned, length-bounded broker IPC;
- service operation allowlist;
- bounded event queues and backpressure;
- orderly startup, reconnect, shutdown, and upgrade behavior.

Acceptance:

- the broker has no content-read API and owns no user database/index;
- an unauthorized client cannot connect or request enumeration;
- oversized, malformed, replayed, and unknown-version messages fail closed;
- agent restart resumes from durable state.

### START-010: Resident desktop launcher

Deliverables:

- Tauri 2 resident hidden window;
- exclusive use of the public Agent API over Named Pipe;
- configurable global shortcut;
- show/focus behavior;
- search-as-you-type with debounce, request IDs, and cancellation;
- results with open, open folder, and copy path actions;
- accessibility and keyboard-navigation baseline.

Acceptance:

- resident hotkey-to-visible p50 is below 50 ms and p95 below 100 ms on reference hardware;
- stale responses never replace results for a newer query;
- the UI remains responsive during indexing;
- actions resolve current paths and handle disappeared files safely.

### START-011: Basic resource governor

Deliverables:

- RAM, CPU, power, search latency, and queue measurements;
- bounded worker/producer control;
- interactive, active, idle, battery, and battery-saver modes;
- hysteresis, dwell time, and cooldown;
- persisted safe starting profile.

Acceptance:

- sustained search pressure reduces background work;
- recovery increases work more slowly than pressure decreases it;
- battery saver preserves search and catalog updates while reducing background work;
- the controller does not oscillate under the protocol's steady-load test.

### START-012: Packaging and release candidate

Deliverables:

- signed/installable package strategy;
- service registration and removal;
- per-user agent autostart;
- upgrade and rollback behavior;
- index retention/cleanup choices;
- diagnostics export without names, paths, or content;
- x64 and ARM64 smoke matrix.

Acceptance:

- clean install, upgrade, repair, and uninstall are tested;
- uninstall asks before deleting user indexes and reports the choice;
- crash/restart does not require a full rebuild in normal cases;
- `SECURITY-001` is complete before designation as a public release.
- Agent API and MCP security acceptance suites pass;
- no Agent or broker TCP listener exists.

## 5. Release performance gates

Reference hardware and exact workloads are versioned in the benchmark protocol. Provisional catalog targets are:

```text
warm search p50 < 30 ms
warm search p95 < 75 ms
warm search p99 < 150 ms
cold search target < 250 ms
resident hotkey p50 < 50 ms
resident hotkey p95 < 100 ms
```

The primary release dataset is at least 1M records. A 5M dataset is a supported-scale gate. A 10M run is a target-scale report and may identify follow-up work without silently weakening correctness.

## 6. Test matrix

The benchmark axes are independent:

```text
CPU/RAM: Low, Typical, High
Storage: HDD, SATA SSD, NVMe
Dataset: 100K, 1M, 5M, 10M
Workload: exact, prefix, token, substring, fuzzy, path, filters, mixed
State: warm, cold, indexing-active, user-active, battery where applicable
```

Correctness suites additionally cover journal recreation/gaps, inaccessible objects, reparse points, link changes, offline/removable volumes, malformed IPC, schema migration, and injected crashes.

## 7. Deferred roadmap

- v0.2: native text/source extractors, isolated IFilter host, content index, snippets, Russian analysis.
- v0.3: calibrated governor, richer storage policies, previews, advanced planner.
- v0.4: optional local embeddings, vector backend, USearch, hybrid ranking.
- v0.5: canonical export, OpenSearch schema compiler and adapter, dual-write tests.

Platform expansion is a separate Windows-first roadmap: macOS Apple Silicon follows a crawl/FSEvents/privacy spike; Linux x86_64 follows an identity/watcher/reconciliation spike. Feature-version numbering is assigned only after Windows risk results, so it does not conflict with the content/semantic roadmap above.

No deferred crate is scaffolded merely to reserve a name. Canonical contracts and version boundaries preserve the extension path.
