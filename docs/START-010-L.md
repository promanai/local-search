# START-010-L — Sustained Projection Load UX Evidence

Status: **IMPLEMENTED — LIVE ACCEPTANCE PENDING**

`START-010-L` is the sustained-load evidence stage for `UX-GATE-001`. It keeps real NTFS/USN
create, rename, move, and delete traffic flowing while the release Agent and Desktop serve
interactive searches and global-hotkey activations.

```text
real LS_TEST files
    -> WinFS/USN fixture ingestion
    -> SQLite graph + outbox
    -> Agent-owned Tantivy projection
    -> concurrent Agent searches
    -> Desktop/WebView interaction
```

The stage changes no product ranking, schema, or UI behavior. Evidence hooks are enabled only when
`LOCALSEARCH_UX_EVIDENCE` is present and never record query text, names, paths, or document IDs.

## Workload

The disposable fixture creates bounded batches under its unique root and executes four real
filesystem phases:

```text
create -> ingest -> rename -> ingest -> move -> ingest -> delete -> ingest
```

Each complete cycle has a bounded minimum interval (250 ms by default, configurable from 50 to
5,000 ms). The evidence workload therefore applies sustained pressure without becoming an
unbounded filesystem denial-of-service loop.

The fixture records filesystem operations, canonical provider events, and durable backlog while
the Agent remains the only Tantivy writer. The runner waits for the Agent scheduler to drain the
final outbox before stopping it, then performs bounded fixture cleanup with no competing writer.
This ownership rule prevents Windows mmap/write races and matches the production process model.
Every lookup is scoped to the current unique fixture root, so an older interrupted run cannot
create duplicate-name ambiguity. If initialization fails before its state file is durable, the
fixture validates and removes only the root it created during that invocation.

Live churn also exercises two Windows-specific races. Graph ingestion acquires an immediate SQLite
WAL write transaction before reading its generation, preventing `SQLITE_BUSY_SNAPSHOT` when the
Agent ACKs projection concurrently. A USN parent that disappears during subtree deletion is treated
as a transient namespace lookup; the journal stream continues to its canonical delete records
instead of failing cleanup with Win32 error 87.

At the same time the runner:

- issues public Agent CLI searches once per second and samples end-to-end p50/p95/p99 without
  starving the Desktop on the security-hardened single-instance pipe;
- records redacted, correlation-sequenced Agent search stages and timings without query text,
  filenames, or paths, so an interrupted physical run still identifies its last completed stage;
- changes the visible Desktop query through an evidence-only, two-value allowlisted
  single-instance event that dispatches the production WebView `input` path at a separately bounded
  cadence (2.5 seconds by default), then reserves a bounded 200 ms dispatch window for the
  debounced foreground request before starting the competing CLI probe; it samples the hotkey at
  2.5-second intervals so the harness does not monopolize window focus;
- activates the real global hotkey and samples focus acknowledgement p50/p95/p99;
- polls sanitized Agent backlog state;
- records Desktop accepted/cancelled/stale/error outcomes;
- records any WebView main-thread timer stall above 100 ms;
- records sanitized resource-pressure samples and disk-busy p50/p95/p99/max once per second;
- records unavailable resource samples and fail-closed Governor transitions separately;
- aborts immediately if the Desktop process stops responding or reports a UI stall;
- aborts immediately on the first failed or three-second-stalled Agent search;
- executes every foreground fixture/CLI command under an external process deadline;
- requires a clean non-elevated release-build manifest whose commit, executable allowlist, length,
  and SHA-256 hashes all match immediately before the elevated run;
- enforces a hard churn duration plus bounded grace period;
- kills a deadline-exceeding child and rejects invalid or non-zero JSON subprocess output;
- writes a sanitized machine-readable failure capsule after bounded cleanup on every supervised
  abort;
- limits best-effort emergency cleanup after failure to five seconds, so evidence emission cannot
  look like another application hang;
- rejects `IDLE_BOOST` while continuous interactive search and hotkey activity are running;
- verifies final backlog drain and bounded fixture cleanup.

## Acceptance

```text
duration                         10-15 minutes (default 15)
CLI interactive search p95      <= 75 ms
global hotkey p95               < 100 ms
non-cancellation search errors  0
Desktop search samples          >= 75% of configured UI-input cadence (minimum 20)
stale results rendered          0
UI stalls > 100 ms              0
final projection backlog        0
maximum backlog                 <= 10,000 mutations
fixture cleanup                 complete
repository before evidence      clean
Desktop remains responsive      entire run
unsafe IDLE_BOOST transitions   0
resource telemetry gaps         0
supervisor deadline overruns    0
```

Cancelled or stale transport responses may be observed during rapid query replacement, but they
must be rejected before rendering. They are reported separately and are not counted as product
errors.

## Operator run

First close the resident Desktop and prepare the exact release bundle from a normal, non-elevated
PowerShell. This prevents compiling project code inside the administrator session:

Preparation fails before compiling when a release Agent, Desktop, CLI, or fixture process is still
running. Processes whose executable path is hidden by elevation are conservatively rejected by
their LocalSearch executable name and reported with PID.

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\prepare-start-010-load.ps1
```

Then open PowerShell as Administrator and run:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\run-start-010-load.ps1 `
  -Volume 'L:\' `
  -DurationSeconds 900 `
  -BatchFiles 16 `
  -ChurnCycleMilliseconds 250
```

After a code change, run a 60-second smoke first. Only proceed to the 15-minute acceptance run if
the smoke completes, drains the backlog, and leaves the Desktop responsive.

The machine-readable report is written below `reports/ux/start-010-l/` and records the churn, CLI,
visible UI-input, and hotkey driver intervals. The runner uses an isolated
Agent pipe, rejects a dirty tree or system drive, kills only processes it created, and invokes the
same bounded fixture cleanup on both success and failure.

The process supervisor has a non-elevated deterministic self-test in
`benchmarks/test-start-010-load-supervisor.ps1`. CI proves successful JSON transport, Windows
argument quoting, invalid-output rejection, and bounded timeout/kill behavior. A live failure emits
`start-010-l-failure-<run>.json` with a bounded reason code, phase, and exception category but no
query, path, document ID, or raw native error.

The separate provenance self-test creates a synthetic four-executable bundle, proves the valid
path, then proves that both byte tampering and commit mismatch fail closed. The accepted live report
embeds the verified relative-path hashes and Rust toolchain identity.

## Gate boundary

The first provenance-verified 60-second smoke at commit `178f46b` failed during interactive
supervision after the Desktop observed a `2.263 s` search deadline while system CPU had reached
`95.28%`. Cleanup completed and no LocalSearch process survived. The result is retained under
`reports/ux/start-010-l/` as failure evidence and does not satisfy this gate.

A later operator-observed run exposed a separate startup race: latency-sensitive API reads opened
the graph through the migration-capable path, which repeated `PRAGMA journal_mode = WAL` and could
wait behind ingestion for the five-second SQLite busy timeout. API reads now use a query-only,
current-schema connection, foreground priority is announced before backend access, battery
projection uses one 256-mutation/250-ms quantum, and the harness aborts its first stalled search
within three seconds. This remediation still requires a clean physical rerun.

The next operator run proved that `ACTIVE` was entered before the query but still produced no
completed search: the request path recreated `CatalogIndex`/`IndexReader` while the scheduler owned
the projection writer. The Agent now keeps one immutable Tantivy snapshot reader alive for all
interactive requests and replaces it only after a successful projection commit. A contract test
removes the on-disk schema marker after Agent startup and proves that search continues from the
already-open snapshot, so request latency no longer depends on reopening the writer's directory.

A clean report with `acceptance.pass = true` closes the sustained projection-load row of
`UX-GATE-001`. It does not replace the physical 100/125/200 percent scaling matrix or Narrator
smoke. `START-010-L-PASS` must not be tagged until the full-duration elevated report exists.
