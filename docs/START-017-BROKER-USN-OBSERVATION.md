# START-017: Broker-backed USN observation

Status: **engineering implementation PASS; physical elevated-volume gate pending**

This milestone connects the per-user Agent scheduler to the metadata-only elevated WinFS broker as
an explicit opt-in. Selected local NTFS volumes can now bootstrap from a full scan, continue from an
opaque USN checkpoint, and fall back to bounded reconciliation when retained source history is lost.

## Durable lifecycle

Graph schema v5 adds an observation generation to every object and link plus one durable scan
session per volume. A full scan no longer needs to buffer millions of events before committing:

1. The Agent records a `Scanning` session and marks the volume `NeedsReconciliation`.
2. Each broker page is applied as an independently bounded graph transaction.
3. The broker's final provider checkpoint is staged, but is not yet authoritative.
4. Links and unlinked objects not observed in the new scan generation are removed in bounded,
   restart-safe transactions.
5. The terminal sweep atomically activates the provider checkpoint, returns the volume online,
   enqueues bounded availability refresh, and removes the scan session.

If the Agent or broker restarts during scanning, a new scan generation supersedes the partial one.
Rows emitted only by the abandoned scan become stale candidates and are removed after the new scan
completes. If restart occurs during either sweep phase, the next scheduler quantum continues from
the remaining indexed stale set.

After activation, every incremental event page and its next opaque checkpoint commit in the same
graph transaction. `SourceHistoryGap` and incompatible checkpoints become a durable
`RequireReconciliation` transition; the following scheduler pass starts a broker reconciliation
scan. A temporarily unavailable volume is marked offline and returns online with the next valid
incremental batch.

## Scheduler and scope policy

- Observation is disabled unless `--observe-usn` is present.
- `--observe-root C:\` may be repeated; mount roots are resolved case-insensitively to canonical
  `VolumeId` values during broker discovery.
- `--observe-volume volume:...` remains available for identity-exact configuration.
- If neither selector is supplied, all currently attached local NTFS volumes are selected.
- Selected volumes are serviced round-robin and rediscovered every 30 seconds.
- One scheduler iteration performs at most one broker page or one bounded stale-row transaction.
- `Active`, `Pressure`, and energy-saver resource-governor pauses also pause observation.
- Catalog projection runs after new graph work. Content projection remains restricted to its
  independently configured opt-in content roots; whole-volume metadata observation does not grant
  whole-volume content extraction.

## Manual launch

First build the release binaries and start the broker from an elevated PowerShell using the logon
SID of the normal user that will run the Agent:

```powershell
cd C:\Projects\local_search
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
.\target\release\localsearch-fs-service.exe `
  --pipe '\\.\pipe\LocalSearch\WinFS\v1\manual' `
  --authorized-logon-sid $sid
```

Then start the Agent as the normal user. This example observes two volumes and keeps content search
limited to the already configured content workspace:

```powershell
cd C:\Projects\local_search
.\target\release\localsearch-agent.exe `
  --graph C:\SearchData\filesystem-graph.sqlite `
  --index C:\SearchData\catalog-index `
  --content-index C:\SearchData\content-index-v1 `
  --observe-usn `
  --broker-pipe '\\.\pipe\LocalSearch\WinFS\v1\manual' `
  --observe-root C:\ `
  --observe-root D:\
```

Omit `--content-index` when content search is not configured. Omit both selectors only when the
intention is to observe every local NTFS volume.

## Contract evidence

- Schema migration verifies the observation-session table and both generation-sweep indexes.
- Graph recovery restarts an interrupted scan, bounds link/object cleanup to one row, keeps the
  volume non-authoritative during cleanup, and activates only the final provider checkpoint.
- Same-name replacement during reconciliation atomically removes only a generation-proven stale
  namespace identity before inserting its replacement, preserving SQLite uniqueness and outbox convergence.
- Agent controller E2E covers initial paged bootstrap, checkpoint activation, incremental metadata
  update, source-history gap, durable reconciliation state, and broker reconcile restart.
- Mount-root selection covers case and trailing-separator normalization.
- Existing graph, projection, content, Agent, broker, Desktop, and provider contracts remain in the
  workspace verification gate.

## Remaining production gate

The code path is complete but this milestone does not claim fresh physical elevated-volume evidence.
Before default enablement, run an isolated NTFS/VHDX test and a sustained multi-million-file soak
covering Agent/broker restarts, journal recreation, backlog recovery, disk growth, search latency,
and user-input pause behavior. Observation therefore remains explicit opt-in.
