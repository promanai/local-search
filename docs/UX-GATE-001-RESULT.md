# UX-GATE-001 Result

Status: **CONDITIONAL**

`START-010` is implemented, but the product UX gate is kept separate from code completeness.

## Passed evidence

- one resident process and one existing WebView;
- real global-hotkey delivery and focused-input acknowledgement;
- reference-hardware hotkey latency below `p50 50 ms / p95 100 ms`;
- second launch exits through the single-instance boundary;
- dual stale-response rejection in Rust and WebView state;
- on-demand reconnect after a real Agent Named Pipe endpoint restart;
- bounded 50-hit state and keyboard/accessibility contracts;
- current-item re-resolution and missing/offline/type guards before actions;
- public-Agent-only dependency boundary;
- x64 release build, strict quality gates, and ARM64 compile-check.
- evidence-only WebView telemetry rejects unfocused input, viewport clipping, horizontal document or
  result overflow, an invisible selected result, unmanaged long-content overflow, and more than 50
  rendered results;
- the release runner can require a live query/result layout and records viewport, DPR, scrolling,
  ellipsis coverage, and every layout invariant in the same machine-readable report as hotkey
  latency.
- `START-010-U` implements a disposable real-filesystem evidence path for long names,
  rename/move/delete, current-item action resolution, VHDX offline/online state, and cleanup. Its
  physical elevated report is still pending and is not listed as passed evidence yet.
- `START-010-L` implements the 10–15 minute real-USN churn, concurrent public-Agent search,
  global-hotkey, projection backlog, cancellation/stale-outcome, and WebView-stall evidence path.
  Its bounded process supervisor now prevents indefinite fixture/CLI waits, enforces a churn
  deadline, performs bounded cleanup, and preserves a sanitized failure capsule. Its full-duration
  elevated report is still pending.

The accepted machine report is stored under `reports/ux/start-010/`. It contains commit/dirty-tree,
display scale, sample count, p50/p95/p99/max, and single-instance evidence.

## Remaining physical validation

- repeat visual smoke at Windows scaling 100%, 125%, and 200% (150% is measured on the reference
  machine);
- run the clean elevated `START-010-U` VHDX matrix and retain its live Open/Open folder/Copy path,
  rename/move/delete, offline/online, and cleanup report;
- run the clean elevated 15-minute `START-010-L` matrix and retain its search/hotkey latency,
  backlog, cancellation/stale, WebView-stall, and cleanup report;
- basic screen-reader announcement smoke.

These are UX/release validation tasks, not permission to move search or filesystem logic into the
desktop process. `UX-GATE-001-PASS` must not be tagged until the physical matrix is recorded.

The exact operator procedure and evidence fields are in
[`UX-GATE-001-CHECKLIST.md`](UX-GATE-001-CHECKLIST.md). A layout report may pass without claiming
long-name coverage; the matrix requires `long_content_exercised = true` before that row is accepted.
