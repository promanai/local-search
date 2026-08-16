# LocalSearch Design Specifications

| Document | Responsibility |
| --- | --- |
| [Data model](DATA-MODEL.md) | Canonical identities, objects, links, and search documents |
| [Filesystem identity](FS-IDENTITY.md) | Stable object/link semantics and journal continuity |
| [Windows filesystem policy](FS-WINDOWS-POLICY.md) | NTFS, USN, reparse points, volumes, and fallback boundary |
| [Index consistency](INDEX-CONSISTENCY.md) | SQLite authority, mutation outbox, commits, and recovery |
| [Indexing pipeline](INDEXING-PIPELINE.md) | Stages, queues, backpressure, retry, and observability |
| [Tantivy schema](TANTIVY-SCHEMA.md) | Catalog projection and experimental retrieval fields |
| [Query engine](QUERY-ENGINE.md) | AST, planning, cost control, cancellation, and ranking |
| [Local Agent API](LOCAL-API.md) | Client boundary, wire DTO, methods, identities, and Named Pipe transport |
| [MCP adapter](MCP-ADAPTER.md) | Stateless MCP 2026 stdio mapping and compatibility isolation |
| [Agent API security](API-SECURITY.md) | Pipe DACL, capability grants, limits, and future HTTP gate |
| [SECURITY-001](SECURITY-001.md) | v0.1 current-user metadata policy and elevated-broker exclusion |
| [Protocol compatibility](PROTOCOL-COMPATIBILITY.md) | Independent version domains and adapter compatibility |
| [Security model](SECURITY-MODEL.md) | Trust boundaries, IPC, metadata privacy, and release gate |
| [Extraction sandbox](EXTRACTION-SANDBOX.md) | Required v0.2 isolation of native parsers and IFilter |
| [Resource governor](RESOURCE-GOVERNOR.md) | Metrics, modes, hysteresis, and budgets |
| [Index migrations](INDEX-MIGRATIONS.md) | Independent versions, parallel rebuild, switch, rollback |
| [Benchmark protocol](BENCHMARK-PROTOCOL.md) | Datasets, hardware/storage axes, metrics, and regression gates |
| [ENGINEERING-GATE-001](ENGINEERING-GATE-001.md) | Shared evidence contract and START-002/003/004 review decision |
| [ENGINEERING-GATE-001 result](ENGINEERING-GATE-001-RESULT.md) | Measured START-002/003/004 outcome, unresolved gates, and bounded follow-ups |
| [START-002](START-002.md) | Reproducible Tantivy catalog-retrieval spike and its experimental boundary |
| [START-003-R](START-003-R.md) | Controlled substring recall, candidate-pressure, and latency acceptance |
| [START-008](START-008.md) | MCP 2026 stdio adapter, bounded metadata tools, cancellation, and Agent E2E evidence |
| [START-009](START-009.md) | Elevated WinFS broker wire contract, SCM lifecycle, bounded streaming, and Agent restart evidence |
| [START-010](START-010.md) | Resident Tauri launcher, public-Agent-only client, cancellation, reconnect, and file actions |
| [START-010-U](START-010-U.md) | Disposable real-filesystem long-name, stale-action, offline-volume, and cleanup evidence fixture |
| [START-010-L](START-010-L.md) | Sustained real-USN projection load with concurrent Agent/Desktop latency, backlog, and UI-stall evidence |
| [START-011](START-011.md) | Deterministic resource governor, live Windows pressure adapter, and Agent-owned projection scheduler |
| [START-011-P](START-011-P.md) | Clean-provenance Windows AC, battery, and energy-saver policy evidence |
| [START-012-CONTENT](START-012-CONTENT.md) | Opt-in bounded plaintext content search, managed generations, and operations |
| [CONTENT-PRODUCTION-GATE-001](CONTENT-PRODUCTION-GATE-001.md) | Capacity, resumability, scheduler, GC, and 500k/1M/3M performance evidence |
| [CONTENT-PRIVACY-001](CONTENT-PRIVACY-001.md) | Explicit-root, extraction, API leakage, scope-revocation, and reset audit |
| [START-013-GRAPH-STORAGE](START-013-GRAPH-STORAGE.md) | Bounded outbox compaction, reusable-page budgets, and incremental vacuum evidence |
| [START-014-REBUILDABLE-INGEST](START-014-REBUILDABLE-INGEST.md) | Outbox-free initial graph build with fail-closed consumer transition |
| [START-015-COMPACT-DESIRED-STATE](START-015-COMPACT-DESIRED-STATE.md) | Graph schema v3 compact payloads, bounded migration, and clean 1M evidence |
| [START-016-BOUNDED-VOLUME-REFRESH](START-016-BOUNDED-VOLUME-REFRESH.md) | Graph schema v4 restart-safe bounded fan-out for volume state and reconciliation |
| [START-017-BROKER-USN-OBSERVATION](START-017-BROKER-USN-OBSERVATION.md) | Opt-in Agent/broker full-volume bootstrap, resumable USN polling, and bounded gap recovery |
| [START-018-WINDOWS-PACKAGING](START-018-WINDOWS-PACKAGING.md) | Reproducible signed/unsigned bundle policy, guarded install lifecycle, retention, and redacted diagnostics |
| [DEPENDENCY-AUDIT-001](DEPENDENCY-AUDIT-001.md) | Locked source/license allowlist, recurring RustSec audit, and informational-advisory triage |
| [Project readiness checklist](PROJECT-READINESS-CHECKLIST.md) | Complete v0.1 implementation, evidence, debt, and release checklist |
| [UX-GATE-001 result](UX-GATE-001-RESULT.md) | Measured hotkey/focus evidence and remaining physical UX validation |
| [UX-GATE-001 checklist](UX-GATE-001-CHECKLIST.md) | Repeatable scaling, live-action, load, and Narrator evidence procedure |

Cross-cutting authority remains in [ARCHITECTURE.md](../ARCHITECTURE.md). Work sequencing and definition of done remain in [IMPLEMENTATION_PLAN.md](../IMPLEMENTATION_PLAN.md).
