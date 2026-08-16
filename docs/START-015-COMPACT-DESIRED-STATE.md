# START-015-COMPACT-DESIRED-STATE — normalized graph projection payload

Status: **PASS**. `PERF-001` and the sustained recovery-headroom target are closed by a clean
1,000,000-record release benchmark.

## Problem

Graph schema v2 stored every desired catalog document as full JSON even though stable identity,
link name, and file metadata were already authoritative in normalized graph tables. The duplicate
payload dominated current-state bytes and repeated JSON serialization on initial ingest.

## Graph schema v3

The desired row now materializes only data that cannot be read directly from normalized state:

```text
document_version
resolved path
32-byte semantic fingerprint
```

`CatalogDocument` is reconstructed with indexed joins to volume, object, and link rows. The
fingerprint covers identity, name, path, extension, and metadata, but excludes document version;
it preserves semantic no-op suppression after a graph mutation has updated normalized metadata.
The old `document_json` column remains as an empty compatibility field for new rows.

Cursor reads use two explicit SQL plans. The first page scans the primary-key order; subsequent
pages use `document_id > cursor`. An earlier nullable-OR form was rejected during benchmark review
because it caused repeated prefix scans and a 285-second rebuild regression.

## Existing database migration

Opening a writable v2 graph adds nullable `projection_path` and `projection_fingerprint` columns
without rewriting its multi-million-row table. Until a row is compacted, v3 readers decode its
legacy JSON exactly as before.

Legacy payload maintenance:

- rewrites at most 100,000 rows per immediate transaction;
- copies the existing resolved path and computes the semantic fingerprint;
- clears redundant JSON only after successful decoding;
- rolls the whole bounded transaction back on malformed input;
- reports rows rewritten and remaining backlog;
- runs under the Agent resource governor and the existing workspace `compact` command;
- leaves freed pages to incremental-vacuum/reusable-page maintenance.

The retained `C:\Projects\PRO` graph was deliberately not migrated during implementation because
previously started Agent/Desktop binaries only understand schema v2. It should be migrated after
the new binaries are rebuilt and stopped old processes are confirmed.

## Clean 1M evidence (2026-08-16)

Implementation commit: `81ede4dd316edaa1115e7a4b11718cd9cb574437`; release profile;
`dirty_tree=false`; deterministic seed `20260814`; 1,000,000 source records plus one root.

| Measurement | v2 baseline | schema v3 | Change |
|---|---:|---:|---:|
| Initial graph ingest | 381.399 s | 209.315 s | -45.1% |
| Ingest throughput | 2,621.9 records/s | 4,777.5 records/s | +82.2% |
| SQLite after initial ingest | 1,906,946,048 B | 670,289,920 B | -64.9% |
| Initial projection | 32.425 s | 24.425 s | -24.7% |
| Full lost-index rebuild | 66.845 s | 25.221 s | -62.3% |
| Recovery headroom | 1.487x | 2.104x | target `>=2x` PASS |
| Steady-state startup | 7.024 ms | 5.988 ms | PASS |

Additional v3 results:

```text
initial outbox high-water:       0
initial searchable documents:    1,000,001
incremental mutations:           10,000
incremental incoming rate:       8,893.9 mutations/s
backlog recovery rate:           18,712.6 mutations/s
Tantivy index:                    264,531,135 bytes
duplicate / lost / stale:        0 / 0 / 0
desired/index fingerprints:       MATCH
```

Machine-readable evidence:

- `reports/benchmarks/start-015/start-015-1000000-records.json`
- `reports/benchmarks/start-015/start-015-1000000-records.csv`
- `reports/benchmarks/start-015/start-015-1000000-records.md`

## Decision

The initial graph path no longer pays for rebuildable outbox JSON or duplicated desired-document
JSON. It preserves atomic checkpoints, deterministic rebuilds, semantic delta detection, and
bounded migration. Whole-disk release work can now focus on sustained real-filesystem observation
and operational rollout rather than current-state SQLite density.
