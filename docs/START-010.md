# START-010 — Resident Desktop Launcher

Status: **PASS (engineering implementation)**

UX gate: [`UX-GATE-001`](UX-GATE-001-RESULT.md) remains **CONDITIONAL** until the remaining
physical display/action/load matrix is recorded.

## Outcome

`START-010` adds one resident Tauri 2 process whose initially hidden `main` WebView is created once
and then shown/focused by the configured global shortcut:

```text
Ctrl+Space (configurable through LOCALSEARCH_HOTKEY)
  -> existing resident process
  -> existing hidden WebView
  -> Rust desktop request coordinator
  -> Agent Wire v1 / same-logon Named Pipe
  -> SearchResponse
  -> keyboard-first result list
```

The single-instance callback and global-shortcut callback call the same `show + unminimize + focus`
path. Closing the window is converted to hide, so neither a hotkey nor a second launch creates a
new WebView.

## Public boundary

Normal desktop dependencies are limited to `localsearch-agent-api`, `localsearch-core`, and the
Agent-enabled local transport plus Tauri presentation plugins. CI rejects Tantivy, SQLite, graph,
Windows provider, broker, service, Agent implementation, or MCP dependencies.

The frontend has no direct opener, clipboard, global-shortcut, filesystem, shell, or backend plugin
permission. Its typed commands can only:

- search/cancel through Agent Wire;
- probe Agent health;
- hide the existing window;
- acknowledge focus telemetry;
- request an action by stable `DocumentId`.

For `open`, `open_folder`, and `copy_path`, Rust re-resolves current metadata through
`catalog_get_item`, requires an online existing supported object, and passes only that current path
to the Rust-side Tauri opener/clipboard API. A stale path from `SearchHit` is never trusted.

## Search state and failure behavior

- 90 ms search-as-you-type debounce;
- hard maximum of 50 rendered hits;
- every query generation receives a bounded request ID;
- starting a generation cooperatively cancels the previous Named Pipe request;
- Rust rejects a late response after its generation was superseded;
- WebView state independently rejects a mismatched response ID;
- unavailable/deadline failures render `Search service unavailable`;
- a two-second health probe reconnects without recreating the desktop process;
- an Agent endpoint destroy/rebind test proves the same client object recovers;
- missing, offline, unsupported, moved, or deleted action targets produce a bounded error.

## Interaction and accessibility baseline

- `Up` / `Down` bounded result selection;
- `Enter` opens the current selection;
- `Esc` hides the launcher;
- explicit Open, Open folder, and Copy path buttons;
- listbox/option semantics, live status, active-descendant tracking, visible focus, dark mode, and
  reduced-motion handling;
- responsive logical-pixel layout with no animation dependency.

## Automated evidence

- Rust unit contracts cover stale A/B ordering, response-ID mismatch, reconnect, action guards,
  focus metric exact-once behavior, and percentile calculation;
- a real Named Pipe test destroys and recreates the endpoint while retaining one desktop client;
- JavaScript tests cover stale result rejection, the 50-hit bound, keyboard selection, query clear,
  and backend-score-free presentation;
- the release UX runner starts the real Tauri/WebView2 executable, sends OS global-hotkey events,
  waits for input-focus acknowledgement, and verifies the second process exits through the
  single-instance boundary;
- evidence mode collects bounded real-WebView layout invariants after focus and result rendering;
  the Rust boundary validates that WebView data cannot claim `pass` when focus, clipping, overflow,
  selected-row visibility, managed ellipsis, scrolling, or the 50-hit bound fails;
- the runner can require a real Agent query, a non-empty result layout, and exercised long-content
  ellipsis before accepting a scaling row;
- x64 release build and ARM64 workspace compile-check are release gates;
- earlier `START-007` evidence remains authoritative for concurrent Agent readers during projection.

## Deliberate exclusions

- preview/content/snippets;
- settings center and tray dashboard;
- animations and semantic/AI search;
- advanced filters;
- installer/autostart/signing and release icon assets (`START-012`);
- direct filesystem, index, graph, broker, or service access from Desktop.

The remaining physical procedure is versioned in
[`UX-GATE-001-CHECKLIST.md`](UX-GATE-001-CHECKLIST.md); automation deliberately does not impersonate
Narrator or claim that an unexercised long filename was visually validated.
