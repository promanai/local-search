# START-010-U — Real Filesystem UX Fixture

Status: **IMPLEMENTED — LIVE ACCEPTANCE PENDING**

`START-010-U` is an isolated evidence stage for the existing Desktop client. It creates actual
files on the explicitly selected `LS_TEST` NTFS volume and exercises the production path:

```text
Windows filesystem -> WinFS/USN -> SQLite/outbox -> Tantivy -> Agent -> Desktop client
```

It does not add search or filesystem authority to the Desktop process.

## Safety boundary

The fixture accepts only a non-system drive root whose discovered volume is local NTFS and whose
label is exactly `LS_TEST`. Its only recursive removal target must be a direct child of that volume
named `localsearch-ux-fixture-{run_id}`. The optional detach/reattach step accepts only an explicit
`.vhdx` below the repository `.lab` directory.

All durable fixture databases, logs, and intermediate reports live below the unique run directory.
The runner rejects a dirty repository, a non-elevated shell, missing release executables, and an
already-running release Desktop process.

## Proved invariants

The implementation makes these assertions executable:

- the volume root is a persistent graph object/link, so projected filesystem paths are absolute;
- a rename/move pair preserves `FileObject`, `FileLinkId`, and therefore `DocumentId`;
- `SearchHit.resolved_path` is presentation-only;
- every Open, Open folder, and Copy path target comes from a fresh Agent `CatalogItem` lookup;
- a deleted identity returns a controlled `not_found` action result and disappears from search;
- an offline volume keeps the catalog identity, projects `availability = offline`, and rejects the
  action as `item_unavailable` rather than treating the item as deleted;
- reattaching the same VHDX must recover the same `VolumeId` and logical document;
- cleanup must converge through the same USN/outbox/index path before the report can pass.

Provider checkpoints remain opaque portable state. Journal identifiers, USNs, native file
references, Windows handles, and VHDX paths do not enter the graph or Agent wire contracts.

## Controlled data

The controller creates seven documents under a unique real directory: long English, Cyrillic, and
mixed names; rename, move, and delete targets; and a deep-path target. The report captures live
layout geometry, stable identity, stale-path rejection, current action path, controlled deletion,
eventual search removal, offline/online transitions, and cleanup.

## Operator run

Build the exact release tools from a clean candidate commit:

```powershell
cargo build --release --locked `
  -p localsearch-agent `
  -p localsearch-desktop `
  -p localsearch-ux-fixture `
  --bins --examples
```

Close the resident LocalSearch Desktop, open PowerShell as Administrator, and run the full VHDX
matrix:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\run-start-010-real-fs-ux.ps1 `
  -Volume 'L:\' `
  -VhdxPath 'C:\Projects\local_search\.lab\localsearch-usn-test.vhdx'
```

Omitting `-VhdxPath` runs the long-name, rename, move, delete, and cleanup matrix without claiming
physical offline-volume evidence. A `START-010-U-PASS` tag requires the full VHDX report with
`offline_volume.pass = true`, `acceptance.pass = true`, and `dirty_tree = false`.

## Gate boundary

Even a full `START-010-U` pass does not close `UX-GATE-001`. The 100/125/200 percent physical DPI
rows, sustained projection churn, and Narrator smoke remain independent release evidence.
