# START-007 — Agent + Local API

Status: PASS

## Outcome

`START-007` introduces the per-user application boundary over the production backend:

```text
localsearch-cli
      ↓ length-prefixed Agent Wire v1
Windows Named Pipe (same logon SID, local only)
      ↓ authenticated dispatch + capability check
LocalSearch Agent
      ↓
Query Engine → Catalog Schema v1 / SQLite desired state
```

The Agent is the only client-facing owner of SQLite/Tantivy. CLI and future adapters do not open
either backend directly. No TCP/HTTP listener or MCP dependency is present.

## Frozen v0.1 surface

- `catalog_search` → `search.catalog`
- `catalog_get_item` / `catalog_get_items` → `read.metadata`
- `index_get_status` → `index.status`
- `agent_get_capabilities`
- `agent_get_health`

The authoritative compiled DTO lives in `localsearch-agent-api`; the checked-in machine-readable
manifest is `contracts/agent-wire-v1.json`. Agent API and codec versions are independent.

Search results expose `document_id`, `object_key`, and `file_link_id`, plus deterministic
`match_type`, one-based `rank`, and `ranking_version`. Tantivy scores are not public.

## Windows transport security

- endpoint namespace: `\\.\pipe\LocalSearch\Agent\v1\<logon SID>`;
- explicit protected DACL for the current logon SID;
- specific read/write/synchronize access mask rather than Generic All;
- `FILE_FLAG_FIRST_PIPE_INSTANCE` fails on endpoint squatting;
- `PIPE_REJECT_REMOTE_CLIENTS` rejects remote pipe connections;
- the server impersonates only after reading a bounded request and compares the client logon SID;
- the client uses `SECURITY_IDENTIFICATION` SQOS;
- maximum one connected client/in-flight request in v0.1;
- nonblocking reads/writes, bounded frames, and transport/request deadlines;
- disconnect cancellation is checked between query planning, retrieval, verification, and ranking.

The DACL is same-logon isolation, not a sandbox against malicious code already running in that
logon session.

## Product query behavior

- Unicode NFKC + lowercase normalization;
- exact, prefix, token, verified substring (minimum three characters), and path-token retrieval;
- deterministic product order: exact → prefix → token → substring → path;
- scope, extension, directory-prefix, and size filters;
- global candidate window and `top_k` clamps;
- current metadata and verification come from SQLite desired state by stable `DocumentId`;
- Tantivy is candidate retrieval only and remains disposable/rebuildable.

## Acceptance evidence

The process-level test starts the real `localsearch-agent` executable over a secure Named Pipe,
runs the real `localsearch-cli search architecture` executable, and verifies the returned
`architecture-plan.md` hit. Additional contracts cover codec bounds, version rejection,
capability denial, cancellation, stable identity fields, and additive optional wire fields.
Agent restart reopens the same durable graph and active index generation. A concurrency contract
runs eight independent readers while the projection writer commits a rename, then proves zero
backlog and visibility of the new name.

```powershell
cargo test -p localsearch-agent-api --all-targets --locked
cargo test -p localsearch-agent --all-targets --locked
```

MCP, service installation/autostart, UI, content reads, administrative operations, HTTP, and
pagination remain outside `START-007`.

## SERVICE-GATE-001

`SERVICE-GATE-001 = PASS` for the implemented headless scope:

- backend and Agent restart-safe;
- independent versioned Agent API and codec;
- real same-logon/local-only Windows IPC;
- real CLI → Agent → Tantivy search;
- concurrent query readers during projection commit;
- bounded cancellation and deadline checks;
- no SQLite, Tantivy, Windows, MCP, or backend-score types in Agent Wire DTO;
- no TCP listener.
