# CONTENT-PRODUCTION-GATE-001

Status: **PASS** for opt-in bounded UTF-8 plaintext content search.

Feature scope remains frozen: PDF, Office, archives, OCR, snippets, and highlighting are not part of
this gate.

## Gate result

| Work item | Result | Production evidence |
|---|---:|---|
| CONTENT-001 capacity/free-space limits | PASS | Independent graph bytes, content bytes, document count, absolute reserve, and percentage reserve. Any limit leaves an explicit non-active `BUILDING / CAPACITY_LIMIT` candidate. |
| CONTENT-002 resumable initial build | PASS | Durable graph-to-content checkpoint after every bounded commit. Commit-before-checkpoint crash injection at 10%, 50%, and 90% resumes the same generation and validates all 100 test documents. |
| CONTENT-003 atomic generations + GC | PASS | `BUILDING`, `READY`, `ACTIVE`, `RETIRED`, `FAILED`; checksum-protected alternating state slots; atomic active pointer; live reader refresh; old active retained; failed/old retired generation collection; exact content reset. |
| CONTENT-004 continuous scheduler/watcher | PASS | Bounded outbox batches, per-`DocumentId` coalescing, commit-before-ACK replay, three bounded retries, interval debounce, delete propagation, metadata-only skip, content-hash skip, and Agent resource-governor integration. |
| CONTENT-005 million-scale benchmark | PASS | Controlled real Tantivy indexes at 500k, 1M, and 3M documents; cold/warm workloads, p50/p95/p99, memory/CPU/disk counters, and concurrent projection measurements. Filename mode is not invoked. |
| CONTENT-PRIVACY-001 | PASS | See [`CONTENT-PRIVACY-001`](CONTENT-PRIVACY-001.md). |

## State and crash contract

```text
durable graph -> BUILDING generation -> bounded commit -> durable checkpoint
                         |                         |
                         | crash                   | crash before checkpoint
                         v                         v
                    resume same generation <- idempotent replay
                         |
                    validate count
                         v
                       READY -> atomic pointer -> ACTIVE -> RETIRED -> GC
```

Only the active pointer selects searchable data. A candidate that reaches a capacity limit or
crashes never replaces the old active generation. Recovery treats the pointer as authoritative and
repairs lifecycle labels after an interrupted activation.

Generation state records contain `scan_id`, `generation_id`, selected root ids, target graph
sequence, last `DocumentId` checkpoint, documents seen/projected, bytes processed, commits, index
generation/bytes, capacity reason, state, and timestamps. Alternating fsynced JSON slots with a
sequence and checksum tolerate a torn state write on Windows.

## Capacity policy

Defaults:

```text
max graph storage       10 GiB
max content index       10 GiB
max content documents   5,000,000
minimum free disk       max(2 GiB, 1% of volume)
content commit batch    4,096 documents
```

Before a content batch, the manager forecasts index growth and includes writer memory headroom in
the free-space decision. A partial graph snapshot does not run absence-based delete reconciliation.
Content projection may still safely cover the durable partial graph.

## Controlled scale evidence (2026-08-16)

The benchmark uses the production `CONTENT-SCHEMA-v1` fields and controlled content containing rare,
common, two-term, phrase, Cyrillic, Latin, code-identifier, very-common, and prefix workloads.
Warm SLA is p95 <= 150 ms and p99 <= 300 ms. “Cold” means the first query through a newly opened
reader; the operating-system file cache is deliberately not flushed. Each warm percentile has 100
samples. Build times below are from the first build; process counters below are from the final
search/concurrent-projection pass that reused the validated index.

| Scale | Initial build | Index bytes | Worst warm p95 | Worst warm p99 | Concurrent projection/search p95 | Peak RAM | Result |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 500,000 | 4.14 s | 69,568,913 | 0.88 ms | 1.03 ms | 1.22 ms | 20.8 MB | PASS |
| 1,000,000 | 7.17 s | 140,102,479 | 1.26 ms | 1.53 ms | 1.14 ms | 18.5 MB | PASS |
| 3,000,000 | 25.52 s | 415,765,221 | 3.62 ms | 3.71 ms | 4.47 ms | 19.5 MB | PASS |

At 3M, the final pass recorded 2,765 ms accumulated CPU time, 2,587,104 read bytes, and 158,445
written bytes. Exact OS-cache behavior makes these process I/O counters comparative evidence, not a
hardware-independent storage guarantee. The benchmark report explicitly records
`filename_mode_touched: false`; catalog/filename search retains its independent p95 <= 75 ms gate.

Content retrieval is deliberately bounded and unscored: it preserves Tantivy query/phrase matching
but stops after `top_k`. Agent API v2 never returns a score. This avoids corpus-wide score collection
for extremely common terms and makes latency scale with the requested result count.

Run a reproducible benchmark in an owned directory:

```powershell
cargo run --release -p localsearch-content-index --bin content_production_bench -- `
  --documents 1000000 `
  --samples 100 `
  --index $env:LOCALAPPDATA\LocalSearch\content-bench-1m\index `
  --output $env:LOCALAPPDATA\LocalSearch\content-bench-1m\report.json
```

`--rebuild` refuses recursive deletion unless the destination contains the benchmark-specific
ownership marker.

## Operational commands

```powershell
# Build/resume the selected workspace and its content generation.
localsearch-content-index.exe folder-sync --workspace <workspace> --root <root>

# Keep projecting only durable graph/outbox changes.
localsearch-content-index.exe watch --workspace <workspace> --watch-interval-ms 1000

# Build/resume a fresh candidate while the old active generation serves search.
localsearch-content-index.exe generation-rebuild --workspace <workspace>

# Keep one retired rollback generation; active/building data are never removed.
localsearch-content-index.exe gc --workspace <workspace> --retain-retired-generations 1

# Physically erase owned content state without deleting graph/catalog state.
localsearch-content-index.exe reset-content --workspace <workspace>
```

## Gate boundary

The gate proves safe bounded operation across millions of plaintext content documents, crashes,
restarts, disk pressure, stale/deleted identities, incremental mutation, and scope revocation. It
does not claim that every filesystem object becomes a content document: extension, UTF-8, size,
availability, ACL, root, and exclusion policy intentionally reduce that set.
