# START-012-CONTENT — opt-in document content search

Status: **production hardening PASS** for bounded UTF-8 plaintext content search. See
[`CONTENT-PRODUCTION-GATE-001`](CONTENT-PRODUCTION-GATE-001.md) and
[`CONTENT-PRIVACY-001`](CONTENT-PRIVACY-001.md). This does not change `Catalog Schema v1` or the
default filename/path search contract.

## Product boundary

Content indexing is explicit and separate:

```text
SQLite filesystem graph
        ├─ Catalog Schema v1  → default filename/path search
        └─ Content Schema v1  → opt-in plaintext content search
```

`Content Schema v1` reads only files under canonical roots explicitly supplied by the
user. It currently accepts UTF-8 plaintext with these extensions:

```text
bat c cc cfg cmd conf cpp cs css csv cxx go h hpp htm html ini java js json jsx
kt kts log markdown md php ps1 py rb rs sh sql swift toml ts tsx txt vue xml yaml yml
```

The default bound is 1 MiB per file and the hard configurable maximum is 16 MiB.
Directories, offline files, unsupported extensions, binary/NUL-containing content,
non-UTF-8 content, files outside the selected roots, and files that disappear during a
build are skipped and counted. Canonical path checks prevent a link from escaping an
allowed root.

Raw document text is indexed but is not stored as a retrievable Tantivy field and is
never returned in a search hit. A hit contains the canonical `CatalogDocument` metadata
needed to identify and re-resolve the file. This reduces accidental disclosure, but the
term index itself remains sensitive local data and must be protected like the source
documents.

Interactive content queries match complete terms and a bounded single-token prefix after four
characters, so Desktop search remains useful while typing without enabling arbitrary substring
expansion. Retrieval stops after the requested bounded result count and does not expose or promise
Tantivy score ordering. Results are explicitly marked `Content`; source text, scores, and snippets
remain absent.

## CLI and Agent API v2

Create or refresh a complete real-folder workspace with stable Windows file identities. The state
directory must be outside every selected root. Roots must not overlap, but may reside on different
physical disks. Each run performs a current-user scoped scan, reconciles
create/update/rename/delete into the durable graph, and builds or synchronizes content:

```powershell
cargo run --release -p localsearch-content-index -- folder-sync `
  --workspace $env:LOCALAPPDATA\LocalSearch\folder-index `
  --root C:\Users\me\Documents
```

Index multiple disks by repeating the root:

```powershell
localsearch-content-index.exe folder-sync `
  --workspace $env:LOCALAPPDATA\LocalSearch\all-disks `
  --root C:\ `
  --root D:\ `
  --max-graph-gib 10 `
  --exclude-dir Archive `
  --exclude-dir Worktrees
```

The scan never enumerates sibling directories outside a selected root and never follows directory
reparse points. Observations commit to SQLite in bounded 10,000-mutation batches, so multi-million
trees do not accumulate every event in RAM; `--scan-batch-size` can tune the bounded batch up to
50,000. An interrupted filesystem pass leaves a durable incomplete checkpoint and is safe to
repeat. The subsequent graph-to-content build has an exact durable `DocumentId` checkpoint and
resumes without rebuilding committed batches.

Graph storage has a default 10 GiB budget, configurable with `--max-graph-gib` or exact
`--max-graph-bytes`. The budget includes the SQLite database, WAL, and shared-memory sidecars.
The crawler reserves bounded headroom for its final transaction and stops cooperatively before the
configured ceiling. A budget-limited snapshot is explicitly reported as `scan_complete: false`;
it never runs absence-based delete reconciliation, but it does build/search content for the durable
partial graph. The final JSON reports `graph_bytes`, `live_files`, `live_file_bytes`, object/link
counts, and whether the limit was reached, making storage-to-source-data measurements reproducible.
Progress counters, without source paths or content, are written to stderr every 250,000
observations while stdout remains one machine-readable JSON result.

By default the crawler observes but does not descend into common system/generated directories such
as `Windows`, `Program Files`, `.git`, `node_modules`, caches, virtual environments, `target`,
`build`, and `dist`. Add repeatable `--exclude-dir NAME` entries for local archive/worktree policy,
or use `--include-generated` for an intentionally exhaustive traversal. `folder-roots.json` keeps
the legacy explicit root list; `content-workspace.json` persists roots, exclusions, file/graph byte
limits, content capacity/free-space policy, and scan batch policy. The returned JSON includes
ready-to-use graph, catalog, and managed content-index paths for Agent startup.

Both graph and content storage default to 10 GiB ceilings. Content additionally defaults to
5,000,000 indexed documents and preserves free disk equal to the greater of 2 GiB or 1% of the
volume. Configure these independently:

```powershell
localsearch-content-index.exe folder-sync `
  --workspace $env:LOCALAPPDATA\LocalSearch\all-disks `
  --root C:\Projects `
  --max-graph-gib 10 `
  --max-content-index-gib 10 `
  --max-content-documents 5000000 `
  --min-free-disk-gib 2 `
  --min-free-disk-percent 1 `
  --content-batch-documents 4096
```

Before each content batch, the manager checks projected index growth plus writer headroom. Reaching
any ceiling leaves the candidate in `BUILDING` with `capacity_limit` set to
`CONTENT_INDEX_BYTES`, `DOCUMENTS`, or `FREE_DISK`; no partial candidate becomes searchable.

Build a new immutable generation from the durable graph:

```powershell
cargo run --release -p localsearch-content-index -- build `
  --graph C:\path\to\filesystem-graph.sqlite `
  --index C:\path\to\content-index-v1 `
  --root C:\Users\me\Documents
```

Synchronize that generation after the graph advances. The command re-resolves every current
`DocumentId`, avoids reading unchanged eligible files, applies additions/replacements/deletions in
one Tantivy commit, and emits machine-readable accounting:

```powershell
cargo run --release -p localsearch-content-index -- sync `
  --graph C:\path\to\filesystem-graph.sqlite `
  --index C:\path\to\content-index-v1 `
  --root C:\Users\me\Documents
```

An already running Agent reloads the committed generation before content reads, so a successful
sync becomes visible without restarting the Agent. A failed sync never publishes a partial batch.

Initial content construction uses immutable managed generations:

```text
BUILDING -> READY -> ACTIVE -> RETIRED
     |                    previous ACTIVE remains available
     +-> FAILED
```

Every bounded commit persists generation id, scan id, selected roots, last `DocumentId`, document
and byte counters, commit count, target graph sequence, state, and capacity reason. A crash after a
Tantivy commit but before its state checkpoint safely replays that batch. Validation occurs before
an atomic active-pointer switch; an already-open Agent reader notices the switch. One retired
generation is retained by default for rollback.

Resume or deliberately rebuild a managed generation and collect owned stale generations:

```powershell
localsearch-content-index.exe generation-rebuild `
  --workspace $env:LOCALAPPDATA\LocalSearch\all-disks

localsearch-content-index.exe gc `
  --workspace $env:LOCALAPPDATA\LocalSearch\all-disks `
  --retain-retired-generations 1
```

After the initial/full reconciliation, consume only durable graph changes with bounded,
crash-replay-safe content commits:

```powershell
localsearch-content-index.exe project `
  --workspace $env:LOCALAPPDATA\LocalSearch\all-disks `
  --batch-size 1024 `
  --maximum-batches 64
```

`project` coalesces each ordered SQLite outbox batch by `DocumentId`, reads only changed eligible
files, commits Tantivy, then advances the independent `CONTENT-SCHEMA-v1` consumer checkpoint. A
commit-before-ACK crash is safe because replayed upserts/deletes are idempotent. Metadata-only
changes avoid source reads; changed metadata whose extracted SHA-256 is unchanged avoids a content
document rebuild.

Run the same bounded projection continuously with debounce interval and bounded retry:

```powershell
localsearch-content-index.exe watch `
  --workspace $env:LOCALAPPDATA\LocalSearch\all-disks `
  --batch-size 1024 `
  --maximum-batches 64 `
  --watch-interval-ms 1000
```

The Agent invokes this content projector through the existing resource governor. Filesystem
changes still enter through the graph/outbox; the watcher never performs a periodic full content
rescan.

Search content independently from catalog search:

```powershell
cargo run --release -p localsearch-content-index -- search `
  --index C:\path\to\content-index-v1 `
  --query architecture `
  --top-k 20
```

Start a content-enabled Agent and use the public Named Pipe API:

```powershell
localsearch-agent.exe `
  --graph C:\path\to\filesystem-graph.sqlite `
  --index C:\path\to\catalog-root `
  --content-index C:\path\to\content-index-v1

localsearch-cli.exe content architecture
```

Agent API v2 exposes the separately authorized `search.content` capability. Every
content hit is resolved again against the durable graph by `DocumentId`; stale or deleted
identities are omitted. The desktop exposes the same operation behind the explicit
`Contents` mode button and continues to use catalog search by default.

The legacy direct `build`/`sync` commands remain compatible. New workspace flows use the generation
manager. Remove all owned content generations and reset only the content projection checkpoint with:

```powershell
localsearch-content-index.exe reset-content `
  --workspace $env:LOCALAPPDATA\LocalSearch\all-disks
```

This does not remove the filesystem graph or filename catalog.

Projection maintenance also compacts acknowledged graph outbox history. Run it explicitly when
needed:

```powershell
localsearch-content-index.exe compact `
  --workspace $env:LOCALAPPDATA\LocalSearch\all-disks
```

Compaction is bounded by rows and pages per pass. The 10 GiB graph limit uses live storage pressure
(allocated pages minus reusable pages, plus WAL/SHM), not the historical high-water file length.
For an intentionally unprojected graph, `--allow-without-consumers` may discard outbox history
because future consumers rebuild from authoritative desired catalog state. See
[`START-013-GRAPH-STORAGE`](START-013-GRAPH-STORAGE.md).

## Local field evidence (2026-08-16)

An excluded-directory scan of `C:\Projects\local_search` completed in 5.8 seconds: 3,326
observations produced a 3.4 MiB graph and 525 indexed UTF-8 documents in a 1.47 MiB content index.
The query `Stable object link semantics` returned `docs\README.md`. A temporary indexed Markdown
document was then deleted; the next full reconciliation reported `removed_documents: 1`, and the
same content query returned no result.

A bounded partial scan of `C:\Projects\PRO` produced 3,360,000 filesystem objects/catalog records and a 6.381 GiB
durable graph. Its first content generation reached 5.107 GiB after about 156 minutes before the
operator stopped the deliberately large trial. Because an initial generation is published by one
atomic Tantivy commit, that interrupted generation was not searchable and was removed while the
durable graph was preserved. This is useful production evidence: the graph ceiling bounds graph
growth. The hardening work prompted by this trial now supplies content byte/document/free-space
budgets, restartable intermediate commits, atomic activation, and owned-generation GC.

## Deliberately frozen feature scope

- no PDF, Office, archive, OCR, or encoding detection;
- no content snippets or highlighting;
- no source text or score in the API.

Those features remain downstream of the completed bounded plaintext production gate.
