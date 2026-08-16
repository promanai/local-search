# Resource Governor

Status: START-011 engineering implementation; live calibration pending

## 1. Goal

Protect interactive search and the user's foreground workload while maintaining catalog freshness. The governor controls admission and concurrency; it does not rely on every blocking dependency being instantly cancellable.

START-001 defined the portable `ResourceProvider::snapshot()` input boundary. START-011 now
implements `get_indexing_budget()`, `report_search_latency()`, and `report_system_pressure()` as a
deterministic policy and connects it to the Agent projection scheduler. Thresholds remain
provisional until the sustained live matrix is recorded.

The implementation is split deliberately:

- `localsearch-resource-governor` owns portable policy, hysteresis, reason codes, and budgets;
- `localsearch-windows-resources` translates RAM, CPU, AC/battery, charge, and energy-saver state;
- the Agent is the single projection owner and maps budgets to bounded worker options.

## 2. Inputs

```text
available and total RAM
system and process CPU load
storage class and current pressure
power source, battery level, battery saver
user active/idle state
queue depth and oldest age
search p50/p95/p99 and request rate
current writer budget and worker state
```

Metrics use rolling windows/EWMA where appropriate and include freshness timestamps.

`Idle` is an explicit, trusted platform signal, not the absence of an active search request. An
unknown or merely inactive user state remains non-idle and cannot enable `IDLE_BOOST`. This
fail-closed rule prevents background projection from taking an aggressive budget while the user is
working in another application.

On Windows the adapter uses the session-local last-input clock and publishes only elapsed
milliseconds through `SystemResources`; no native handle, tick value, window identity, key, or
pointer data crosses the portable boundary. The default policy requires five minutes without local
input. An unavailable sample is `None` and cancels boost immediately.

Aggregate local-storage busy time is sampled through the English PDH
`\PhysicalDisk(_Total)\% Disk Time` counter. The first observation primes the rate counter and
formatted values are refreshed at most once per second, normalized to 0-10,000 basis points. PDH
permission/data failures remain `None` and do not suppress CPU, RAM, power, or activity signals.

A failure of the complete trusted resource snapshot is different from one optional PDH field being
unknown. The Agent immediately enters `PRESSURE`, clears idle eligibility, and pauses background
projection with reason `resource_telemetry_unavailable`. Interactive requests remain serviceable,
but cannot unpause projection. Five consecutive valid snapshots are required before background
work resumes.

## 3. Outputs

- source read admission;
- mutation projection batch size and cadence;
- indexing producer concurrency;
- safe writer memory/worker configuration at commit boundaries;
- path-refresh admission;
- later extraction/OCR/embedding concurrency;
- maintenance/merge permission.

## 4. Modes

| Mode | Catalog source | Catalog projection | Maintenance |
| --- | --- | --- | --- |
| Interactive search | Keep durable ingestion | Pause projection | Pause |
| User active | Keep durable ingestion | Pause during interactive hold | Pause |
| Idle + AC | Increased | Increased within limits | Allowed |
| Battery | Keep catalog current | Reduced | Reduced |
| Battery saver | Keep essential updates | Minimum safe | Pause |
| Resource emergency | Persist resumable state | Pause if safe | Pause |

## 5. Hysteresis

The controller uses separate pressure and recovery thresholds:

```text
pressure condition for N short windows
  -> step down quickly
  -> hold minimum dwell time
  -> cooldown

recovery condition for M longer windows
and CPU/RAM/disk all healthy
  -> step up one level
```

`M` is greater than `N`. Configuration changes are rate-limited and logged as reason-coded decisions.

Current engineering defaults use two pressure observations, five recovery observations, a
three-window cooldown, and a three-window interactive hold. They are benchmark parameters, not
release constants.

`IDLE_BOOST` additionally requires explicit idle evidence from the trusted OS adapter, AC power,
and a non-empty durable backlog. Entering boost uses the normal healthy-recovery hysteresis;
observing new input, losing the activity signal, or draining the backlog exits boost immediately.

## 6. Memory budget

The provisional budget derives from both total and currently available RAM, with configurable floor and ceiling. A floor is not permission to allocate under emergency pressure: indexing remains paused until safe headroom exists.

Tantivy's per-thread minimum and total split are validated before constructing a writer. Reconfiguration drops/recreates or replaces the writer only after a durable safe boundary.

## 7. Disk policy

The agent preserves at least the greater of a configured absolute reserve and percentage reserve. As reserve shrinks, it prevents new optional projections before risking state durability. Catalog state and the ability to search the last valid index take priority.

HDD policy minimizes concurrent random work and uses larger sequential batches. SSD/NVMe may allow greater parallelism, still subordinate to interactive latency.

## 8. Acceptance

- [x] deterministic simulation tests cover threshold crossing;
- [x] steady conditions do not oscillate;
- [x] pressure reduces or pauses work through bounded budgets;
- [x] recovery is slower than pressure entry;
- [x] critical pressure cannot advance the durable projection cursor;
- [x] foreground search pauses projection and cannot advance the durable projection cursor;
- [x] foreground priority is announced before latency-sensitive SQLite/Tantivy access;
- [x] API graph reads use a query-only connection and never negotiate journal mode;
- [x] interactive search uses an already-open Tantivy snapshot, including its stored canonical
  payload, instead of reopening the index or issuing SQLite N+1 reads against an active writer;
- [x] ordinary battery mode cannot override an active foreground hold or confirmed pressure;
- [x] backlog without trusted idle evidence cannot enable `IDLE_BOOST`;
- [x] Windows session-local input age is normalized behind the portable resource contract;
- [x] Windows aggregate disk busy time is normalized behind the portable resource contract;
- [x] new input and unknown activity observations cancel `IDLE_BOOST` immediately;
- [x] draining the durable backlog cancels `IDLE_BOOST` immediately;
- [x] live Windows RAM, CPU, AC/battery, charge, and energy-saver inputs stay behind an adapter;
- [ ] foreground search SLA under the Agent-owned scheduler is recorded for 10-15 minutes;
- [ ] recovery headroom at least 2x is demonstrated;
- [x] disk-busy input is wired fail-closed and rate-limited;
- [x] complete resource-snapshot failure pauses projection immediately and recovers slowly;
- [ ] disk-pressure threshold and live telemetry-availability rate are calibrated from evidence.
