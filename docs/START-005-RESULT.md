# START-005 Result — PASS

Decision: the platform-neutral durable filesystem graph is accepted as `Filesystem Graph Schema
v1`. `START-006 — Durable Projection` is unblocked.

## Evidence

Clean benchmark implementation commit: `5f350639a1719d515f0de93b89e5b756e54a594b`.

Dataset: 1,000,000 deterministic records, seed `20260814`, dataset version 1. The benchmark ran on
the isolated `L:` NTFS test volume and persisted graph state in SQLite WAL mode.

| Measurement | Result |
|---|---:|
| Initial ingest | 50.026 s |
| Initial ingest throughput | 19,989 objects/s |
| SQLite database size | 392.81 MiB |
| Storage per source record | 411.89 bytes |
| Single-event apply p50 / p95 / p99 | 0.029 / 0.036 / 0.082 ms |
| Rename/move apply p95 | 0.081 ms |
| Directory rename + bounded enqueue p95 | 0.182 ms |
| Warm path resolve p95 / p99 | 0.012 / 0.015 ms |
| Reader-cold path resolve p95 / p99 | 12.526 / 13.110 ms |

Machine-readable evidence:

- `reports/benchmarks/start-005/start-005-1000000-records.json`
- `reports/benchmarks/start-005/start-005-1000000-records.csv`
- `reports/benchmarks/start-005/start-005-1000000-records.md`

The JSON report validates against `benchmarks/report.schema.json`, records `dirty_tree=false`, and
names the exact implementation commit above.

## Correctness gates

- Graph mutations and the opaque provider checkpoint commit atomically; a failed invariant rolls
  both back.
- Multiple links preserve one physical object; the final-link removal tombstones it.
- Directory rename changes parent/link state and enqueues one bounded refresh job without rewriting
  descendants.
- Restart persistence reproduces the same checkpoint, graph counts, and derived path.
- The resolver contains cycles, missing parents, ambiguous hard-linked parents, depth overflow, and
  traversal boundaries as typed errors.
- Integrity audit reports orphan objects and missing parents. Exact duplicate namespace links are
  rejected by the durable schema.
- Offline and reconciliation state remain portable; provider-native checkpoint bytes remain opaque.
- The crate has no Tantivy or Windows dependency. CI enforces both boundaries.

## Validation

- `cargo fmt --all --check`: PASS.
- `cargo test --workspace --all-targets --locked`: PASS, including 10 graph contract tests.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: PASS.
- ARM64 workspace/all-target compile-check with warnings denied: PASS.

## Known limits carried forward

- The 1M benchmark uses a deliberately simple two-level namespace to isolate graph storage and
  ingestion cost. Deep-tree and mass-refresh throughput should be measured with the projection
  pipeline in `START-006`.
- Reader-cold measurements reopen SQLite but do not flush the Windows filesystem cache.
- Native Windows ARM64 SQLite runtime packaging is not validated; the ARM64 gate currently proves
  the Rust/FFI compile boundary.
- Projection outbox, replay, Tantivy rebuild, and three-way convergence are intentionally deferred
  to `START-006` / `BACKEND-GATE-001`.
