# START-013-GRAPH-STORAGE — bounded graph compaction

Status: **PASS** for projection-history growth and reusable-page accounting. Initial rebuildable
outbox writes are now eliminated by
[`START-014-REBUILDABLE-INGEST`](START-014-REBUILDABLE-INGEST.md). Controlled 1M speed and
current-state row density are closed by
[`START-015-COMPACT-DESIRED-STATE`](START-015-COMPACT-DESIRED-STATE.md).

## Problem

Every graph mutation materializes current desired catalog state and appends a durable JSON outbox
mutation. Before this milestone, acknowledged outbox rows were retained indefinitely. On a
multi-million tree, storage therefore represented both current state and complete projection
history. The configured 10 GiB ceiling used physical file length, so reusable SQLite pages could
also stop scanning unnecessarily.

## Contract

Outbox maintenance now follows these rules:

1. Compute the minimum checkpoint across registered projection consumers.
2. Delete only sequences at or below that checkpoint.
3. Delete at most 100,000 rows in one immediate transaction.
4. Preserve the `AUTOINCREMENT` high-water mark after all rows are gone.
5. Keep desired catalog state untouched, so a missing/new consumer performs its normal full rebuild.
6. With no registered consumer, delete nothing unless the operator explicitly supplies
   `--allow-without-consumers`.
7. Reclaim at most 4,096 SQLite pages per maintenance pass.

The Agent executes this after catalog and optional content ACK under the existing resource-governor
window. `folder-sync`, workspace `project`, and `watch` use the same primitive. Foreground activity
or confirmed pressure pauses maintenance together with projection.

New graph databases select `auto_vacuum=INCREMENTAL` before their first table is created. Existing
databases created with `auto_vacuum=NONE` reuse freed pages internally but require an explicit full
SQLite vacuum to reduce historical file length. No automatic full vacuum is attempted because it
requires a second large copy and a long exclusive operation.

## Capacity accounting

The scan budget now uses:

```text
graph pressure =
    allocated main-database bytes
  - reusable freelist bytes
  + WAL bytes
  + SHM bytes
```

Physical file length and reusable bytes remain separately reported. Thus an old 6 GiB file with
2 GiB on its freelist has about 4 GiB of capacity pressure and can absorb future mutations without
growing.

## CLI

```powershell
localsearch-content-index.exe compact `
  --workspace $env:LOCALAPPDATA\LocalSearch\folder-index `
  --compact-batch-rows 50000 `
  --compact-maximum-batches 64 `
  --reclaim-pages 4096
```

For a graph that deliberately has no materialized consumer:

```powershell
localsearch-content-index.exe compact `
  --workspace $env:LOCALAPPDATA\LocalSearch\graph-only `
  --allow-without-consumers
```

The command validates that `graph.sqlite3` is an exact child of the canonical workspace. Its JSON
reports the safe sequence, deleted rows, remaining backlog, reclaimed pages, physical bytes, page
allocation/freelist, and auto-vacuum mode before and after.

## Real evidence (2026-08-16)

Fresh `C:\Projects\local_search` workspace:

```text
filesystem observations:          3,334
outbox rows compacted:             1,667
physical graph before reclaim:    3,526,656 bytes
physical graph after reclaim:     2,285,568 bytes
physical reduction:               35.2%
incremental-vacuum pages returned: 303
```

Retained `C:\Projects\PRO` graph:

```text
authoritative catalog records:     3,360,000 -> 3,360,000
outbox rows:                        3,360,000 -> 0
sequence high-water:                3,360,000 -> 3,360,000
registered consumers:               0
physical main database:             6,851,637,248 bytes
new reusable bytes:                 2,582,581,248 bytes
effective graph pressure:           ~3.98 GiB
pressure reduction:                 37.7%
```

The old `PRO` database remains physically 6.38 GiB because it predates incremental vacuum, but
future graph writes reuse its 2.41 GiB freelist before extending the file. The current desired
catalog and stable identities were verified after compaction.

## Remaining performance work

`START-014` avoids initial outbox construction when a future consumer must perform a full
desired-state rebuild. `START-015` additionally removes full `CatalogDocument` JSON from new desired
rows and provides bounded migration for legacy rows. The clean 1M rerun reduced ingest to
209.315 seconds and initial graph allocation to 670,289,920 bytes while preserving convergence.
