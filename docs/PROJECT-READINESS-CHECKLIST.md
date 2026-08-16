# LocalSearch v0.1 project readiness checklist

Review date: 2026-08-16

Legend: `[x]` accepted evidence, `[~]` engineering implementation complete but physical/release
evidence incomplete, `[ ]` not complete.

## Architecture and risk retirement

- [x] `ARCHITECTURE-BASELINE-0.1` frozen.
- [x] Portable canonical IDs, wire DTOs, errors, and independent schema/protocol versions.
- [x] Windows, Tantivy, SQLite, MCP, and Desktop dependency boundaries enforced by CI.
- [x] Windows x64 workspace tests and strict Clippy.
- [x] Windows ARM64 workspace/all-target compile-check.
- [x] Portable contract jobs defined for Windows, Linux, and macOS.
- [x] `ENGINEERING-GATE-001-PASS`.
- [x] Tantivy 1M/5M capacity and exact/token/prefix latency evidence.
- [x] Positional trigram recall/precision and candidate-pressure evidence.
- [x] Native Windows MFT/USN lifecycle, restart, gap, recreation, and reconciliation evidence.

## Durable backend

- [x] `START-005-PASS`: versioned portable SQLite filesystem graph.
- [x] Volume, object, hard-link, parent-link, tombstone, and offline/online semantics.
- [x] Derived path resolution with cycle, depth, missing-parent, and boundary containment.
- [x] Bounded durable subtree refresh after directory rename.
- [x] `START-006-PASS`: authoritative graph plus desired catalog and transactional outbox.
- [x] Crash-before/after-commit replay and idempotent projection.
- [x] Lost/corrupt Tantivy generation rebuild without filesystem rescan.
- [x] `BACKEND-GATE-001-PASS`: SQLite/Tantivy convergence with zero duplicate/lost/stale docs.
- [x] `PERF-001`: 1M initial ingest reduced from `381.399 s` to `209.315 s`.
- [x] First full build materializes desired state without temporary rebuildable outbox JSON.
- [x] Rebuildable ingest fails atomically once any projection consumer is registered.
- [x] Bounded projection-outbox compaction preserves the slowest consumer and sequence high-water.
- [x] New graphs use incremental vacuum; Agent/content watcher reclaim pages under governor bounds.
- [x] Graph schema v3 removes duplicated desired-document JSON; 1M initial SQLite is 64.9% smaller.
- [x] Sustained recovery headroom raised from `1.487x` to `2.104x` (`>= 2x` target PASS).
- [x] Volume offline/reconciliation projection fan-out uses a restart-safe bounded cursor.
- [x] Agent drains durable path/volume refresh queues under the resource governor before projection ACK.
- [x] Opt-in Agent scheduler consumes bounded metadata pages from the authenticated WinFS broker.
- [x] Full-volume bootstrap/reconciliation is crash-safe and activates its checkpoint after bounded stale-row sweeps.
- [x] Incremental USN event pages and opaque checkpoints commit atomically; history gaps trigger durable reconciliation.
- [ ] Fresh elevated multi-million-file Agent/broker soak with restart and journal-recreation evidence.

## Local service and public clients

- [x] `START-007-PASS`: versioned Agent Wire v1 and per-user Agent.
- [x] Same-logon, local-only Named Pipe with explicit DACL and bounded frames.
- [x] Deadlines, request IDs, cancellation, capability grants, and concurrent readers.
- [x] Public-API-only CLI end-to-end search.
- [x] `SERVICE-GATE-001-PASS`: restart-safe Agent and secured real IPC.
- [x] `START-008-PASS`: stateless read-only MCP 2026 stdio adapter.
- [x] MCP exposes catalog search, bounded metadata, and index status only.
- [x] `START-009-PASS`: authenticated metadata-only elevated WinFS broker prototype.
- [x] Broker backpressure, reconnect, restart, malformed/replay/version rejection.
- [~] Real Windows SCM/install lifecycle remains release evidence, not prototype evidence.
- [x] `SECURITY-001` fixes v0.1 to current-user-only discovery and excludes elevated broker
  observation from public-release plans.
- [ ] Second-user install/repair/upgrade/uninstall isolation evidence.

## Desktop and UX

- [x] `START-010-PASS`: resident Tauri client using only the public Agent API.
- [x] Configurable global shortcut, single instance, debounce, cancellation, stale-response guards.
- [x] Keyboard navigation, dark/reduced-motion baseline, bounded 50-result rendering.
- [x] Current `DocumentId` re-resolution before Open/Open folder/Copy path.
- [x] Agent restart reconnect.
- [x] Measured 150% hotkey p95 `7.658 ms` against `< 100 ms`.
- [~] `START-010-U` real-filesystem action fixture implemented.
- [~] `START-010-L` sustained-load fixture and fail-fast process supervisor implemented.
- [x] Process-supervisor self-test: success, quoting, invalid JSON, deadline, and kill path.
- [x] Non-elevated release bundle with commit/SHA-256/length provenance for elevated UX evidence.
- [x] Provenance self-test rejects executable tampering and commit mismatch.
- [ ] Scaling report at 100%.
- [ ] Scaling report at 125%.
- [x] Scaling report at 150%.
- [ ] Scaling report at 200%.
- [ ] Elevated live rename/move/delete/offline action report.
- [ ] Clean 15-minute sustained projection/search/hotkey report with no UI stall.
- [ ] Narrator screen-reader smoke.
- [ ] `UX-GATE-001-PASS` and tag.

## Resource governor

- [x] Deterministic `ACTIVE`, `BALANCED`, `IDLE_BOOST`, `PRESSURE`, and `BATTERY` modes.
- [x] CPU, memory, disk, power, trusted input-idle, search latency, and backlog inputs.
- [x] Batch, cadence, heap, maintenance, and pause/resume budgets.
- [x] Pressure confirmation, slower recovery, hysteresis, cooldown, and no-oscillation tests.
- [x] Fail-closed resource-telemetry loss and five-valid-sample recovery.
- [x] Interactive `ACTIVE` windows fully pause projection and preserve the durable cursor.
- [x] Interactive priority precedes backend access; API graph reads cannot enter SQLite's writer
  negotiation path.
- [x] Interactive search uses one cached immutable Tantivy generation for retrieval, stored
  payload verification, and ranking; the request path performs no SQLite N+1 reads.
- [x] `ACTIVE` and confirmed `PRESSURE` take precedence over ordinary battery throughput.
- [x] Load driver gives the debounced Desktop request a bounded foreground dispatch window before
  its competing CLI probe.
- [x] Real battery / energy-saver-off physical evidence: 30/30 accepted samples.
- [ ] Real AC / energy-saver-off physical evidence.
- [ ] Real battery / energy-saver-on physical evidence.
- [ ] Clean sustained-load proof that interactive SLA is preserved.
- [ ] Recovery headroom `>= 2x` without violating interactive SLA.
- [ ] `START-011-PASS` and tag.

## Packaging, security, and operations

- [x] Local Git history and frozen architecture/engineering/backend/service tags.
- [x] GitHub Actions workflow is defined, including x64, ARM64, portable boundaries, frontend, and
  sustained-load supervisor tests.
- [x] Public sanitized source snapshot backed up at `promanai/local-search`; private `reports/`
  and evidence tags intentionally remain local.
- [x] Hosted GitHub Actions accepted on Windows x64, Windows ARM64 compile-check, Ubuntu, and macOS.
- [x] `main` protection requires all four CI jobs, PRs, linear history, resolved conversations,
  and forbids force-push/delete, including for administrators.
- [~] Authenticode/timestamped package strategy implemented fail-closed; accepted signed artifact
  remains pending.
- [~] Windows service registration/removal implemented; elevated disposable-VM evidence pending.
- [~] Limited per-user Agent/Desktop autostart tasks implemented; physical logon evidence pending.
- [x] Transactional repair/upgrade rollback restores payload, signature, marker, service, tasks,
  and running state at controlled failure points.
- [x] `OPS-GATE-001` controller covers fresh install, repair, two forced rollbacks, upgrade,
  both retention modes, orphan checks, ACLs, and redacted evidence.
- [~] Guarded install, repair, upgrade, binary rollback, and uninstall implemented; physical matrix
  pending.
- [x] Explicit `KeepIndexes` / marker-guarded `RemoveIndexes` uninstall contract.
- [x] Redacted diagnostics export with path/filename/query/content non-disclosure fixture.
- [ ] x64 and physical ARM64 release smoke.
- [~] Locked dependency source/license and recurring RustSec audit implemented with zero
  vulnerabilities; 16 documented informational warnings remain tracked.
- [~] `SECURITY-001` engineering policy and fail-closed package enforcement complete;
  second-user VM evidence remains.
- [x] `CONTENT-PRODUCTION-GATE-001` passes for bounded opt-in UTF-8 plaintext.
- [ ] Signed release-candidate tag.

## Current evidence-backed assessment

| Dimension | Score | Meaning |
| --- | ---: | --- |
| Architecture and retired technical risk | `98/100` | frozen contracts and measured search/WinFS decisions |
| Backend correctness and recovery | `94/100` | production-like invariants; measured performance debt remains |
| API/MCP/service engineering | `92/100` | headless product works; installer/SCM security evidence remains |
| Desktop engineering | `86/100` | implementation is strong; physical UX matrix remains conditional |
| Resource adaptation | `79/100` | policy and live telemetry implemented; two power rows/load gate remain |
| Packaging/security/release | `74/100` | transactional lifecycle automation passes; signed artifact and physical evidence remain |
| Overall engineering completeness | `93/100` | core, content, security policy, and operational automation are complete |
| Public release readiness | `78/100` | dominated by physical UX/load, second-user security, and signed release evidence |

Scores are planning estimates, not substitutes for the binary gates above. No unchecked release
row may be treated as complete because an adjacent engineering prototype passed.
