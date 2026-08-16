# START-006 — Durable Projection

Status: **PASS**. Contract/fault-injection tests, clean-provenance 1M benchmark, lost-index rebuild,
and full workspace gates are complete.

## Recovery boundary

```text
FilesystemProvider
        ↓
GraphMutationBatch
        ↓
┌────────────────────────────────────┐
│ one SQLite transaction             │
│                                    │
│ graph state                        │
│ opaque provider checkpoint         │
│ desired catalog state              │
│ canonical projection outbox        │
└──────────────────┬─────────────────┘
                   ↓
          ProjectionWorker
                   ↓
              Tantivy commit
                   ↓
       SQLite projector checkpoint
```

The outbox stores backend-neutral `IndexMutation::{Upsert, Delete}` payloads. It contains no
Tantivy terms, commands, fields, or scores. A whole bounded batch is committed to Tantivy before
its final `MutationSeq` is acknowledged. Replaying a committed-but-unacknowledged batch is
idempotent because every upsert deletes the stable `DocumentId` before adding current state.

## Schema and generation model

SQLite graph schema v2 adds:

- current desired `CatalogDocument` state;
- monotonic canonical projection outbox;
- per-consumer `(last_sequence, index_generation)` checkpoints;
- restart-safe cursor for bounded directory-path refresh.

Catalog Schema v1 (`TANTIVY-SCHEMA-v1`) materializes stable document identity, canonical stored
payload, exact name, token, prefix, positional trigram, and path-token fields. Tantivy generations
are disposable directories. The active generation is selected only by the SQLite consumer
checkpoint; therefore rebuild activation is one atomic SQLite update after the new Tantivy commit.

If the active directory is missing or corrupt, the worker creates a new generation, pages through
current desired SQLite state, commits it, and atomically activates it. Interrupted rebuild
directories remain inactive and are never mistaken for truth.

## Directory rename

Directory mutation time stays bounded. The graph enqueues one durable refresh job. Each worker step
scans at most the configured number of links from a durable cursor, derives current paths, and emits
only semantically changed documents. A restart resumes the scan; unchanged branches do not produce
outbox traffic.

## Fault matrix

- [x] Failure before SQLite transaction leaves no state.
- [x] Graph invariant failure rolls back graph, provider checkpoint, desired state, and outbox.
- [x] State-without-outbox is impossible because both share one transaction.
- [x] Crash immediately after SQLite commit leaves durable backlog.
- [x] Crash before Tantivy commit leaves the batch unacknowledged for retry.
- [x] Crash after Tantivy commit but before ACK replays without duplicates.
- [x] Bounded worker reports remaining backlog and later catches up.
- [x] Missing active index triggers rebuild from desired SQLite state.
- [x] Interrupted rebuild generation is skipped and never activated.
- [x] Generation activation occurs only after the new Tantivy commit.
- [x] Poisoned canonical identity is rejected before commit/ACK.
- [x] Rename, delete, hard-link, reconciliation, and restart contracts are covered.
- [x] START-005 schema migrates forward to graph schema v2 without graph rebuild.

## START-006 acceptance

- [x] Backend-neutral canonical outbox.
- [x] Atomic graph + provider checkpoint + desired state + outbox.
- [x] Monotonic sequences and bounded reads.
- [x] Whole-generation ACK after Tantivy commit.
- [x] Idempotent replay.
- [x] Bounded batch count and wall time.
- [x] Poison mutation failure without ACK.
- [x] Disposable generation rebuild.
- [x] Atomic SQLite generation activation.
- [x] Safe acknowledged-outbox pruning after every consumer advances.
- [x] Exact/token/prefix/positional-trigram Catalog Schema v1 fields.
- [x] Full convergence fingerprint and duplicate-ID accounting.
- [x] Clean-provenance 1M filesystem → SQLite → Tantivy benchmark.
- [x] Lost-index 1M rebuild and convergence evidence.
- [x] Full workspace `fmt`, `clippy`, `test`, and ARM64 compile-check.

The worker is deliberately synchronous and pull-based. Service scheduling, cancellation wiring, and
telemetry export belong to the future agent/service layer; bounded calls and returned metrics are
already suitable for that integration.
