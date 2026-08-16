# BACKEND-GATE-001 — PASS

Decision: LocalSearch now has a recoverable production-like backend. SQLite is authoritative;
Tantivy Catalog Schema v1 is disposable and rebuildable. `START-007+` (Agent API, MCP, service, UI)
is unblocked.

> This document preserves the original graph schema v2 gate evidence. The measured performance
> debt and recovery-headroom shortfall are closed by
> [`START-015-COMPACT-DESIRED-STATE`](START-015-COMPACT-DESIRED-STATE.md): 209.315-second ingest,
> 670,289,920-byte initial SQLite, and 2.104x recovery headroom on the same 1M dataset.

## 1M evidence

Clean benchmark implementation commit: `7cc43054cc497970cd10ba96ca4c8a95a4f19c32`.

Dataset: 1,000,000 deterministic source records plus one root document, seed `20260814`, dataset
version 1. The report records `dirty_tree=false` and validates against the shared JSON schema.

| Measurement | Result |
|---|---:|
| Atomic graph + desired state + outbox ingest | 381.399 s |
| Source ingestion throughput | 2,621.9 records/s |
| SQLite allocated before projection/prune | 1.776 GiB |
| Initial Tantivy projection | 32.425 s |
| Initial projection throughput | 30,840.9 docs/s |
| Initial Tantivy index | 254.59 MiB |
| Steady-state startup recovery | 7.024 ms |
| Incremental incoming throughput | 4,138.8 mutations/s |
| Backlog recovery throughput | 6,154.6 mutations/s |
| Recovery headroom | 1.487× |
| Average bounded Tantivy commit | 1,624.8 ms |
| Full rebuild after active-index deletion | 66.845 s |
| Pruned acknowledged outbox rows | 1,010,001 |
| Duplicate / lost / stale documents | 0 / 0 / 0 |

After pruning, the SQLite file remains 1.783 GiB because SQLite retains reusable freelist pages.
Direct inspection found 173,903 free pages and an estimated 1.119 GiB of live pages; the outbox had
zero retained rows and desired state contained exactly 1,000,001 documents. `VACUUM` is not part of
the normal projection path because retained pages are reusable by later mutations.

Machine-readable evidence:

- `reports/benchmarks/start-006/start-006-1000000-records.json`
- `reports/benchmarks/start-006/start-006-1000000-records.csv`
- `reports/benchmarks/start-006/start-006-1000000-records.md`

## Gate matrix

| Gate | Result | Evidence |
|---|---|---|
| Clean initial projection | PASS | 1,000,001 desired and searchable documents |
| Incremental projection | PASS | 10,000 mutations consumed in bounded commit |
| Rename / move / delete / hard link | PASS | graph + backend recovery contract tests |
| Directory rename descendants | PASS | durable bounded scan updates only changed paths |
| Crash before Tantivy commit | PASS | no ACK; restart consumes durable backlog |
| Crash after Tantivy commit before ACK | PASS | replay leaves one document per ID |
| Provider checkpoint atomicity/resume | PASS | START-004-LIVE + graph/outbox transaction tests |
| Reconciliation | PASS | portable state transition does not create phantom projection work |
| Lost/corrupt Tantivy index | PASS | generation 1 deleted, generation 2 rebuilt |
| Interrupted rebuild | PASS | incomplete generation is skipped and never activated |
| Atomic generation switch | PASS | SQLite checkpoint changes only after Tantivy commit |
| Poison mutation | PASS | rejected before commit and ACK |
| Duplicate searchable documents | PASS | 0 by exact unique-ID accounting |
| Lost documents | PASS | 0 by full count/fingerprint comparison |
| Stale documents | PASS | 0 by dual canonical-payload fingerprint |

## Performance decision

Correctness and recovery pass. Projection recovery is 1.487× faster than the measured normal
incoming mutation stream, so the worker can drain backlog under this workload.

The new durable guarantees are not free: initial graph+outbox ingest is 7.624× slower than the
frozen START-005 graph-only benchmark, and live SQLite state is roughly 1.12 GiB per 1M source
records. This is now a measured optimization target, not an architecture blocker. The first
optimization pass should reduce per-document SQL/JSON work and benchmark deeper directory
topologies; it must not weaken the atomic outbox or disposable-index invariants.

## Validation

- Shared report JSON schema and clean provenance: PASS.
- Full convergence fingerprint: PASS.
- Workspace tests, including migration, projection, fault, and search-field contracts: PASS.
- Strict workspace Clippy and formatting: PASS.
- Windows ARM64 workspace/all-target compile-check with warnings denied: PASS.

## Scope intentionally deferred

- Agent process lifecycle and background scheduling.
- API/MCP transport and authentication.
- Product query planner, final verification/ranker integration, and UI.
- Native Windows ARM64 SQLite runtime packaging.
- Remote repository, hosted CI execution, and branch protection.
