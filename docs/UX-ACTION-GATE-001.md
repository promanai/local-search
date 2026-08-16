# UX-ACTION-GATE-001: live filesystem actions

Status: **engineering controller and fail-closed verdict PASS; elevated VHDX run pending**

`UX-ACTION-GATE-001` is the release-hardening wrapper for `START-010-U`. It proves that a clean
release build searches and acts on real NTFS identities through the production
WinFS/USN -> SQLite/outbox -> Tantivy -> Agent -> Desktop boundary.

## Release evidence boundary

The gate requires:

- an elevated PowerShell;
- an explicit non-system drive root backed by local NTFS and labelled `LS_TEST`;
- an explicit `.vhdx` below the repository `.lab` directory;
- `-ConfirmDisposableVolume`;
- a clean repository and a clean, non-elevated release build manifest;
- exact SHA-256 and length verification for Agent, CLI, content index, Desktop, UX fixture, and
  `ux_action_probe`.

The controller refuses a catalog-only run. Offline, fail-closed action behavior and reattachment of
the same VHDX are required for a release-eligible PASS.

## Acceptance

The fail-closed contract requires:

- long-name result layout with managed ellipsis and no horizontal result overflow;
- rename and move preserve `DocumentId`, reject the stale path, and resolve the current path;
- delete rejects the stale action as `not_found` and disappears from search within five seconds;
- VHDX detach projects `offline`, rejects the action as unavailable, and does not invent deletion;
- VHDX reattach restores the same logical object;
- all seven controlled files and their indexed results are removed;
- source and verified binaries match the same clean commit.

The private source report may contain controlled diagnostic identities. The derived verdict
contains only commit, source-report SHA-256, aggregate booleans, counts, and delete visibility
latency. It excludes volume, VHDX path, filenames, queries, `DocumentId`, and content.

## Running the gate

Prepare the exact release bundle from a normal non-elevated PowerShell:

```powershell
.\benchmarks\prepare-start-010-load.ps1
```

Inspect the execution plan:

```powershell
.\benchmarks\invoke-ux-action-gate.ps1 -PlanOnly
```

Then use an elevated PowerShell on the disposable volume:

```powershell
.\benchmarks\invoke-ux-action-gate.ps1 `
  -Volume 'L:\' `
  -VhdxPath 'C:\Projects\local_search\.lab\localsearch-usn-test.vhdx' `
  -ConfirmDisposableVolume
```

Reports remain below ignored `reports/ux/start-010-u/`. No `UX-ACTION-GATE-001-PASS` claim is
allowed until the physical verdict has `status = PASS`.
