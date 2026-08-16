# Indexing Pipeline

Status: v0.1 baseline

## 1. Stages

```text
FilesystemProvider
  -> bounded source batch
  -> canonical FilesystemEvent + opaque ProviderCheckpoint
  -> SQLite filesystem graph plus mutation outbox
  -> pending mutation reader
  -> catalog document projection
  -> Tantivy writer actor
  -> Tantivy commit
  -> SQLite catalog checkpoint
  -> mutation compaction
```

v0.2 adds content eligibility, extraction supervisor, normalization, content outbox, and content writer as a separate lower-priority pipeline.

## 2. Queue policy

Every queue has:

- item and byte limits;
- producer and consumer ownership;
- workload priority;
- backpressure behavior;
- shutdown/drain semantics;
- retry policy;
- observable depth and oldest-item age.

Filesystem source events are made durable before optional downstream work. When pressure rises, source reading can pause at a resumable cursor. In-memory queues are never the only copy of accepted mutations.

## 3. Batching

Batch sizes are bounded by record count, serialized bytes, elapsed collection time, and interactive mode. Large bursts commit incrementally. A directory rename creates a compact refresh job rather than expanding an entire subtree inside the source transaction.

## 4. Idempotency and versions

Every mutation contains:

```text
mutation_seq
document_id
operation
document_version
payload_version
source_reference
```

The latest object/link state generates the latest document version. Replaying an old batch cannot resurrect a newer-deleted link. The projection validates ordering and records stale-mutation diagnostics.

## 5. Retry classes

| Error | Default action |
| --- | --- |
| Cancellation/shutdown | Resume from durable cursor |
| Transient index lock/I/O | Bounded retry with backoff |
| Disk reserve violation | Pause projection; keep durable source position only if state storage remains safe |
| Invalid source record | Quarantine record/batch metadata and trigger reconciliation as required |
| Schema mismatch | Stop writer and initiate versioned rebuild/migration |
| Corrupt active index | Isolate index, continue durable ingestion, build replacement |
| Permanent policy rejection | Record terminal status with reason |

Retries have caps and never spin without delay.

## 6. Priorities

```text
Interactive search
Catalog source ingestion
Catalog projection
Subtree path refresh
Index maintenance
```

Later content, OCR, and semantic stages are inserted below catalog freshness. Search readers never wait for background queues to drain.

## 7. Shutdown

On normal shutdown, the agent stops accepting new low-priority work, persists resumable source positions, finishes or abandons only idempotent bounded batches, and records whether the active writer needs recovery. Shutdown has a deadline and may rely on normal crash recovery after it expires.

## 8. Observability

Minimum metrics:

- source and mutation cursors;
- queue count/bytes/oldest age;
- events and mutations by type;
- batch and commit latency;
- documents/sec and bytes/sec;
- replay counts;
- reconciliation and path-refresh backlog;
- writer errors and active index generation;
- search p50/p95/p99 while indexing.
