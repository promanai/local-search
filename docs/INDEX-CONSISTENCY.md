# Index Consistency and Recovery

Status: baseline; crash behavior validated by SPIKE-003

## 1. Authority model

```text
Filesystem source
    -> SQLite filesystem graph and mutation outbox
    -> Tantivy materialized catalog index
```

SQLite is the durable control plane. Tantivy is replaceable. A search index commit cannot advance source ingestion, and source ingestion cannot imply that a mutation is already searchable.

## 2. Independent cursors

`source_checkpoint` means all accepted source events through that opaque provider continuation are durably represented in filesystem state and the mutation outbox. The state layer persists but never interprets its provider payload.

`index_applied_mutation_seq` means all mutations through that local sequence are included in the recorded Tantivy generation.

The types, table columns, and APIs keep these positions distinct.

## 3. Outbox transaction

One SQLite transaction:

1. validates provider/checkpoint compatibility and relies on the adapter's continuity result;
2. applies source events to the filesystem graph;
3. appends versioned mutations;
4. advances `source_checkpoint`;
5. commits.

No Tantivy operation occurs inside this transaction.

## 4. Projection transaction

The single catalog writer reads a consecutive bounded mutation range, applies idempotent delete-by-document-ID plus optional add, and commits Tantivy. Only after successful commit does SQLite record:

```text
catalog_schema_version
index_generation
tantivy_opstamp
index_applied_mutation_seq
committed_at
```

If acknowledgement fails, the same range is replayed. Idempotency ensures one live logical document per `DocumentId`.

## 5. Crash matrix

| Failure point | Required recovery |
| --- | --- |
| Before SQLite commit | Source batch may be read again; no partial durable state |
| After SQLite commit | Pending outbox range is projected |
| During Tantivy batch | Reopen last successful Tantivy commit and replay |
| After Tantivy commit, before ACK | Replay same idempotent range |
| During ACK | Transaction rollback or complete checkpoint, never a torn cursor |
| During compaction | Checkpoint remains sufficient to distinguish pending/applied rows |

## 6. Rebuild

Missing, corrupt, or incompatible Tantivy state creates a new versioned index. The builder walks durable live catalog state, produces a checkpointed projection, validates counts and sampled queries, then atomically activates it.

A filesystem rescan is required only when authoritative state is itself missing, inconsistent beyond repair, or journal continuity cannot be reconciled from retained information.

## 7. Mutation-log retention

Applied rows may be compacted only below a durable checkpoint and after the active index generation is validated. Retention policy preserves enough diagnostic metadata to explain the last projection failure without retaining unnecessary filenames or payload copies.

## 8. Invariants checked in tests

- `applied_seq <= maximum committed mutation seq`;
- applied sequences are contiguous per catalog generation;
- a live document has the latest applied version for its ID;
- a delete followed by replay remains deleted;
- rebuilding from the same durable state produces an equivalent logical index;
- checkpoint and active-index manifest refer to compatible schema versions.
