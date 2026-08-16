# CONTENT-PRIVACY-001 — scope and leakage audit

Status: **PASS** for the bounded UTF-8 plaintext content feature.

## Enforced boundary

| Requirement | Enforcement and evidence |
|---|---|
| Explicit opt-in roots only | `ContentIndexPolicy` canonicalizes selected roots and rejects every source outside them. Desktop exposes `Contents` only when a content index is connected. |
| Exclusions before extraction | The scoped provider excludes configured/generated directory names before descent. Contract coverage includes an eligible-looking file under `node_modules`. |
| Link/reparse containment | Windows scoped traversal does not follow directory reparse points; extraction canonicalizes the final file and verifies it remains below an allowed root. |
| No elevated reads | Extraction uses the Agent/current-user process token. Denied, unavailable, offline, missing, binary, non-UTF-8, unsupported, and oversized inputs are skipped and counted. |
| No raw text in API | Tantivy indexes `content` without `STORED`; `ContentSearchHit` contains only a `CatalogDocument`. Agent v2 returns no raw text, snippet, content hash, or score. |
| No content/path telemetry | Scan, watcher, Agent maintenance, resource-governor, and search-stage evidence contain counters/timing/state only. Paths are present only in explicit CLI/API result metadata, where the user requested file results. |
| Per-user storage | Recommended and exercised workspaces are below `%LOCALAPPDATA%\LocalSearch`; processes do not create machine-wide index state. |
| Scope revocation | Removing a selected root tombstones its scoped graph links and projects the resulting deletes into content. Contract test verifies the term disappears. |
| Physical reset | `reset-content --workspace` removes only the validated workspace-owned content directory and resets only the `CONTENT-SCHEMA-v1` graph consumer checkpoint. Contract test verifies physical removal. |

## Sensitive data model

The term dictionary, postings, stored canonical catalog metadata, and stored SHA-256 content hashes
remain sensitive local data even though source text is not retrievable. The index must inherit the
source user's storage protections. The hash is used only to suppress unnecessary rebuilds and is
never returned through Agent API v2.

Search results are re-resolved through SQLite by `DocumentId` immediately before return. A deleted,
moved-out-of-scope, unavailable, or stale identity is omitted even if a stale posting is encountered.
Projection then removes the posting durably.

## Verified negative cases

Automated contracts cover outside-root input, excluded generated directories, binary/NUL data,
unsupported extensions, over-limit files, disappearing sources, root removal, deleted identities,
API serialization, and complete content reset. The Windows provider contracts separately cover
reparse traversal and access failures.

This gate does not claim encrypted index storage, multi-user shared-machine isolation beyond normal
per-user ACLs, or future PDF/Office/OCR extractors. Each new extractor requires its own privacy
review before enablement.
