# LOAD-GATE-001: sustained recovery and interactive SLA

Status: **engineering runner and fail-closed verdict PASS; elevated 15-minute physical run
pending**

## Purpose

`LOAD-GATE-001` turns the existing real-NTFS `START-010-L` workload into one release-hardening
gate. During continuous create, rename, move, and delete traffic it supervises:

```text
real USN ingestion
    -> SQLite graph/outbox
    -> catalog + opt-in content projection
    -> CLI and Desktop search
    -> global hotkey
    -> forced Agent outage/restart
    -> bounded recovery to backlog = 0
```

The v0.1 public policy is current-user-only, so the public load gate restarts the Agent and does not
enable or install the elevated broker.

## Evidence boundary

Release binaries are prepared from a clean, non-elevated checkout. The manifest allowlists and
hashes the Agent, CLI, Desktop, content index, and disposable fixture executables. The elevated
runner rejects a dirty checkout, a system drive, or execution without
`-ConfirmDisposableVolume`.

The live source report stays below the ignored private `reports/load/` tree. A separate verdict contains only
the source commit/hash, aggregate counts, latency percentiles, storage sizes, boolean checks, and
the binary status. It excludes volume/root paths, filenames, queries, document IDs, source text,
and snippets.

## Workload and recovery

The gate runs for at least 900 seconds and schedules at least two Agent restarts. At each restart it:

1. terminates only the Agent process created by the runner;
2. leaves filesystem churn and durable USN ingestion running for a bounded outage;
3. snapshots catalog and content outbox checkpoints;
4. starts the same provenance-verified Agent binary;
5. waits for the public status API to report zero backlog;
6. verifies the durable graph checkpoints also reached zero.

The fixture's `verify` operation computes an order-independent fingerprint over authoritative
desired documents and the active Tantivy catalog. A pass requires equal document counts and
payload fingerprints with zero duplicate searchable IDs.

## Acceptance

```text
duration                              >= 15 minutes
forced Agent recoveries               >= 2 with observed backlog
catalog search p95                    <= 75 ms
content search p95 / p99              <= 150 / 300 ms
hotkey p95                            < 100 ms
non-cancellation search errors        0
stale results rendered                0
UI stalls > 100 ms                    0
maximum backlog                       <= 10,000 mutations
final backlog                         0 within 120 seconds
graph/catalog fingerprint             exact match
duplicate searchable IDs              0
graph storage                         <= 10 GiB
content-index storage                 <= 10 GiB
resource telemetry gaps               0
unsafe IDLE_BOOST transitions         0
fixture cleanup                       complete
```

Recovery headroom `>= 2x` is recorded as a target KPI, not an independent release blocker. The hard
invariant is that backlog decreases to zero within the declared bound while interactive SLA remains
green.

## Running

Prepare release binaries from a normal PowerShell:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\prepare-start-010-load.ps1
```

Inspect the non-mutating plan:

```powershell
.\benchmarks\invoke-load-gate.ps1 -PlanOnly
```

Then use an elevated PowerShell on a disposable NTFS test volume:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\invoke-load-gate.ps1 `
  -Volume 'L:\' `
  -ConfirmDisposableVolume
```

No `LOAD-GATE-001-PASS` tag or public-release claim is allowed until the full physical run produces
`status = PASS`.
