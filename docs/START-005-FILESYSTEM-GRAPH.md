# START-005 — Durable Filesystem Graph

Status: **PASS**. The behavioral contract, clean-provenance 1M benchmark, and full workspace gates
are complete.

## Boundary

```text
FilesystemProvider
      ↓
FilesystemEvent
      ↓
GraphMutationBatch
      ↓ atomic SQLite transaction
      ├─ Volume / FileObject / FileLink state
      └─ opaque ProviderCheckpoint
```

`GraphMutation` is the only write contract exposed by the graph. Provider events never execute SQL
directly. `START-006` will extend the existing transaction with the projection outbox; it will not
change the provider boundary.

## Durable identity

- `VolumeId + FileId128` identifies a physical `FileObject`.
- `FileLinkId` identifies one namespace link; several links may reference one object.
- Parent relationships reference `FileObject`, not a materialized full path.
- `ResolvedPath` is derived at read time and is never authoritative identity.
- Removing one hard link preserves the object. Removing the final link tombstones it.
- Provider checkpoints are stored as opaque bytes alongside `provider_id` and `format_version`.

The common schema contains no platform-native journal, filesystem-record, handle, or index-engine
types. The x64 build uses bundled SQLite for reproducibility. ARM64 compile-check uses the same Rust
API without building vendored C code; native ARM64 runtime linking remains a packaging task, not a
portable-contract exception.

## Rename and corruption policy

A directory rename or move changes only its link relationship. It enqueues at most one pending
`path_refresh` job for the directory object; descendants are not rewritten synchronously. The path
resolver has a caller-supplied depth limit and returns typed failures for cycles, missing parents,
ambiguous hard-linked parents, and provider traversal boundaries. A damaged branch does not prevent
other links from resolving.

## START-005 acceptance

- [x] Versioned SQLite schema (`GRAPH_SCHEMA_VERSION = 1`).
- [x] Durable `Volume` / `FileObject` / `FileLink` model.
- [x] Platform-neutral `GraphMutation` API.
- [x] Snapshot ingestion and incremental batches.
- [x] Atomic graph state + opaque provider checkpoint.
- [x] Rename/move without synchronous subtree rewrite.
- [x] Hard-link deletion and final-link tombstone semantics.
- [x] Offline/online/reconciliation state.
- [x] Deterministic derived-path resolution.
- [x] Cycle, missing-parent, orphan, duplicate-link, depth, and traversal-boundary protection.
- [x] Bounded durable subtree refresh queue.
- [x] Restart persistence and rollback contract tests.
- [x] No Tantivy dependency.
- [x] No Windows dependency or native fields in the graph schema.
- [x] Clean-provenance 1M graph benchmark.
- [x] Full workspace `fmt`, `clippy`, `test`, and ARM64 compile-check.

`START-006` owns the durable projection outbox, replay, and graph-to-Tantivy convergence. Those
tables and policies are intentionally absent from schema v1.
