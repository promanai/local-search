# START-011 — Resource Governor and Agent Projection Scheduler

Status: **ENGINEERING IMPLEMENTATION PASS — LIVE CALIBRATION PENDING**

START-011 converts the earlier resource-policy interface into an operational feedback loop:

```text
Windows resource adapter + Agent search latency + durable backlog
                              |
                              v
                 deterministic ResourceGovernor
                              |
                              v
       batch / cadence / heap / maintenance / pause-resume budget
                              |
                              v
                  single Agent projection writer
```

## Implemented

- portable modes `ACTIVE`, `BALANCED`, `IDLE_BOOST`, `PRESSURE`, and `BATTERY`;
- immediate response to critical memory and energy saver;
- confirmed-window response to ordinary RAM/CPU pressure;
- slower recovery, cooldown, and interactive hold to prevent oscillation;
- fail-closed idle semantics: only a trusted OS-idle signal may enable `IDLE_BOOST`;
- reason-coded, serializable decisions with a monotonic transition sequence;
- bounded batch count, mutation count, wall time, writer heap, rebuild page, maintenance intensity,
  concurrency cap, and background pause/resume;
- live Windows RAM, CPU, AC/battery, charge, and energy-saver sampling behind portable contracts;
- trusted session-local Windows input-age sampling with a fail-closed portable representation;
- fail-closed aggregate Windows disk-busy sampling through a one-second PDH rate counter;
- immediate background pause on complete resource-telemetry failure, with five-sample recovery;
- complete background projection pause during `ACTIVE` interactive-search windows;
- interactive priority announced before graph/index access, with SQLite API reads using a
  migration-free read-only connection that never renegotiates WAL mode;
- one long-lived immutable Tantivy snapshot reader for interactive requests, atomically replaced
  by the scheduler only after projection commit; retrieval, verification, and ranking consume the
  stored canonical payload from that same snapshot and perform no SQLite N+1 reads;
- policy precedence where confirmed pressure and foreground `ACTIVE` pause background work before
  ordinary battery-mode throughput is considered;
- automatic Agent projection maintenance after startup;
- search latency fed back into the governor without changing Agent wire DTOs;
- durable-cursor proof: critical pressure leaves pending outbox work unapplied;
- single-owner UX load path: fixture writes filesystem/SQLite state, Agent alone writes Tantivy;
- bounded retry for transient Windows Tantivy segment-open failures in the disposable fixture.
- immediate SQLite writer admission for concurrent ingestion/projection ACK without snapshot
  upgrade failure;
- bounded USN cleanup behavior when a journal record outlives its deleted parent directory.

The catalog adapter remains one logical writer in v0.1. `projection_concurrency` is therefore
capped at one even in `IDLE_BOOST`; parallelism may be added later only if the durable ordering and
Tantivy writer invariants remain intact.

## Provisional budgets

| Mode | Batch | Batches/pass | Pass time | Heap | Maintenance | Pause |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| ACTIVE | 0 admitted | 0 | 0 | 32 MiB ceiling | paused | yes |
| BALANCED | 5,000 | 20 | 5 s | 64 MiB | normal | no |
| IDLE_BOOST | 10,000 | 100 | 30 s | 128 MiB | elevated | no |
| PRESSURE | 0 admitted | 0 | 0 | 32 MiB ceiling | paused | yes |
| BATTERY | 256 | 1 | 250 ms | 32 MiB | minimal | saver only |

These values are evidence targets, not frozen release defaults.

The Agent now receives the elapsed duration since trusted local Windows input. The portable policy
uses a conservative five-minute default threshold: backlog alone cannot enable `IDLE_BOOST`,
entering boost still requires healthy recovery windows, and a new-input or unknown observation
cancels boost immediately. An empty backlog also returns the policy to `BALANCED`. Projection
maintenance updates backlog independently and therefore cannot erase or manufacture the activity
state.

## Evidence and gate boundary

Automated acceptance covers deterministic transitions, no fast recovery oscillation, live portable
Windows sampling, Agent integration, concurrent readers, and no cursor advance while paused.
It also proves that a long-lived backlog without trusted idle evidence remains `BALANCED`.
Integration coverage proves that scheduled projection preserves trusted idle state and that a new
input observation immediately returns an already-boosted Agent to `BALANCED`.

The bounded [START-011-P](START-011-P.md) probe now supplies reproducible physical power evidence.
It builds the exact clean commit, samples the real Windows adapter, validates portable Governor
invariants, and emits a machine-readable report without administrator privileges or power-setting
mutation. AC, battery, and battery-plus-energy-saver are explicit physical matrix rows rather than
inferences from deterministic unit tests.

`START-011-PASS` is intentionally not tagged yet. A full pass still requires the elevated
`START-010-L`/governor run on the disposable NTFS volume with:

```text
interactive search p95 <= 75 ms
hotkey p95 < 100 ms
no sustained SLA violation
final projection backlog = 0
recovery headroom target >= 2x
battery and energy-saver mode smoke
```

Disk busy time is now supplied by the Windows adapter when the per-user PDH provider permits it.
The elevated sustained-load runner records one sanitized pressure sample per second, including
disk-busy p50/p95/p99/max, unavailable-sample count, and fail-closed transition count. Its
provisional 90 percent pressure threshold still requires live calibration rather than being frozen
from a guessed default.
