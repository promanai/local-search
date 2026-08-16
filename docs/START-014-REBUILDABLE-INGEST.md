# START-014-REBUILDABLE-INGEST — outbox-free initial graph build

Status: **PASS** for eliminating rebuildable projection history during a first folder build.
The controlled 1M follow-up and compact current-state schema are complete in
[`START-015-COMPACT-DESIRED-STATE`](START-015-COMPACT-DESIRED-STATE.md).

## Problem

The first folder scan previously wrote every current catalog document twice: once into authoritative
desired state and once as a JSON upsert in the durable projection outbox. The content index then
performed a full rebuild from desired state, acknowledged the outbox, and immediately compacted
those same upserts. This increased peak disk use and SQLite write amplification without adding a
recovery guarantee.

## Contract

`FilesystemGraph::apply_rebuildable_batch` provides an explicit initial-ingest transaction:

1. Validate and apply graph mutations and the provider checkpoint normally.
2. Materialize current `graph_catalog_documents` desired state atomically.
3. Suppress only the rebuildable outbox representation.
4. Verify inside the same immediate write transaction that no projection consumer exists.
5. Reject the entire batch if any consumer has already registered.

The ordinary `apply_batch` path is unchanged and always writes durable projection mutations.
Directory path-refresh jobs also remain durable.

`folder-sync` selects `graph_outbox_mode: "rebuildable-initial"` only when there is no registered
projection consumer and no already-published managed content generation. The managed content index
then completes its normal full rebuild from desired state before registering `CONTENT-SCHEMA-v1` at
the current sequence. Every later scan uses `graph_outbox_mode: "durable"`.

An absent consumer checkpoint beside an existing active generation is treated as reset/recovery,
not as initial ingest. Its changes stay durable so the published index cannot miss a delta.

## Recovery invariants

- A crash before a graph batch commits leaves neither graph state nor its provider checkpoint.
- A crash after a rebuildable batch leaves authoritative desired state for the resumable full build.
- Consumer registration cannot race with outbox suppression because both acquire SQLite's single
  writer slot.
- Once a consumer exists, rebuildable batches fail closed.
- A future/new consumer always starts with a full desired-state rebuild; sequence `0` is valid for a
  fresh graph with no historical outbox.

## Real evidence (2026-08-16)

Release `folder-sync` over `C:\Projects\local_search` in a clean disposable workspace:

```text
elapsed:                       4.721 s
filesystem observations:      3,336
graph mutations:              3,338
graph outbox mode:             rebuildable-initial
content complete:              true
content/outbox sequence:       0
compaction rows deleted:       0
physical graph bytes:          2,285,568
reusable graph bytes:          0
```

The comparable pre-change run reached 3,526,656 bytes before acknowledged-history compaction and
2,285,568 bytes afterward. The new path reaches the same compact size directly, avoiding
1,241,088 bytes (35.2%) of temporary graph allocation on this fixture. The disposable evidence
workspace was removed after verification.

## Remaining work

`START-015` subsequently normalized the full `CatalogDocument` JSON and closed `PERF-001` with a
clean 1M benchmark. Remaining whole-disk work concerns real-filesystem sustained operation and
rollout, not this initial-ingest representation.
