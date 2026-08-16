# UX-GATE-001 Physical Checklist

Status: **OPEN** until every required row is backed by a clean release report or an explicit manual
observation. The runner never changes Windows display scaling and never fabricates screen-reader or
live-filesystem evidence.

## Scaling matrix

At each Windows scale (`100`, `125`, `150`, and `200` percent), close the resident Desktop process,
keep a query-ready Agent running, and execute:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\run-start-010-ux.ps1 `
  -Samples 40 `
  -Query project `
  -RequireResultLayout `
  -OutputDirectory reports/ux/start-010
```

Use `-RequireLongContent` only when the selected query is guaranteed to return at least one name or
path wider than its result cell. A scaling row is acceptable only when:

```text
dirty_tree = false
display_scale_percent = expected row
single_instance_pass = true
focus p50 < 50 ms
focus p95 < 100 ms
layout.pass = true
result_layout_exercised = true
long_content_exercised = true for the controlled long-name row
```

The layout sample records viewport size and device-pixel ratio, document/result horizontal
overflow, selected-row visibility, scroll availability, and whether every overflowing name/path is
managed by ellipsis. Manually confirm that the visible window matches the report and that `Up`,
`Down`, `Enter`, and `Esc` retain focus and behavior.

## Live item actions

Use the implemented `START-010-U` controller and a catalog backed by the disposable `LS_TEST`
VHDX, not the synthetic benchmark catalog:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\run-start-010-real-fs-ux.ps1 `
  -Volume 'L:\' `
  -VhdxPath 'C:\Projects\local_search\.lab\localsearch-usn-test.vhdx'
```

- Search a file, rename or move it through the provider, then invoke Copy path and Open. The action
  must resolve the current path rather than the stale result path.
- Search a file, delete it, then invoke Open. A bounded unavailable message must appear and no stale
  path may be opened.
- Search a file on the disposable volume, take the volume offline, then invoke Open. The same
  bounded unavailable behavior must occur.

Record the Agent generation/checkpoint before and after every scenario and retain the action result
or error code. Synthetic paths do not qualify.

## Sustained projection load

Run the implemented 15-minute real-filesystem matrix from an elevated PowerShell:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\run-start-010-load.ps1 `
  -Volume 'L:\' `
  -DurationSeconds 900 `
  -BatchFiles 16
```

It performs create/rename/move/delete churn while issuing continuous interactive searches and
hotkey activations. Retain p50/p95/p99 for search and hotkey, cancelled/stale response counts,
maximum projection backlog, and UI stalls above 100 ms. Acceptance requires no stale result to be
rendered, no unbounded backlog, no UI freeze above 100 ms, search p95 at or below 75 ms, hotkey p95
below 100 ms, final backlog zero, and successful bounded cleanup.

## Narrator smoke

With Windows Narrator enabled, confirm that the input has the name “Search files”, result count and
errors are announced, the selected option is announced during arrow navigation, and all actions are
keyboard reachable. This remains a human observation; DOM/ARIA unit tests are supporting evidence,
not a substitute.

Only after all four sections pass may `UX-GATE-001-PASS` be created.
