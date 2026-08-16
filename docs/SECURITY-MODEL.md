# Security Model

Status: broker IPC prototype implemented by [`START-009`](START-009.md); public release blocked on
`SECURITY-001`

## 1. Assets

- filenames, paths, and filesystem topology;
- search queries and result history;
- file metadata and later extracted content;
- Tantivy indexes and SQLite state;
- local IPC endpoints and service controls;
- configuration, exclusion, and volume policies.

Filenames and paths are private data even when file contents are never read.

## 2. Trust boundaries

| Component | Token | Trusted responsibilities |
| --- | --- | --- |
| Desktop | Current user | Render UI, collect input, request user actions |
| Agent | Current user | Own user state/index, search, open allowed files |
| MCP adapter | Current user | Translate approved metadata tools to Agent API |
| WinFS broker | Elevated service identity | Read volume/MFT/USN metadata only |
| Extractor host v0.2 | Current user with additional sandbox limits | Parse one assigned file |

The broker is not a general privileged filesystem proxy.

## 3. Broker allowlist

Allowed operations are limited to capability/version negotiation, volume enumeration, journal query, bounded MFT enumeration, and bounded journal reading.

Explicitly forbidden:

- arbitrary path or object content reads;
- extraction, previews, or hashing file contents;
- access-token escalation for the agent;
- writing user index/state files;
- executing caller-provided commands or loading plugins;
- returning unbounded buffers.

## 4. IPC requirements

- local-only transport with an explicit OS-enforced endpoint DACL;
- current logon SID/session isolation and `PIPE_REJECT_REMOTE_CLIENTS` for Named Pipes;
- authenticated caller identity and per-user/session authorization;
- protocol version and capability negotiation;
- length-prefixed bounded frames and bounded nested collections;
- request IDs, deadlines, cancellation where meaningful;
- no unsafe deserialization assumptions;
- rate and concurrency limits;
- structured audit events without sensitive payloads;
- fail closed for unknown operations or protocol versions.

Replay of enumeration cursors is safe and idempotent; requests that mutate service configuration require stronger authorization and are minimized.

Broker Wire v1 has no configuration-mutation operation. Exact request-ID replay is rejected within a
bounded 4096-request service window. The Windows endpoint uses an explicit authorized logon SID in
both its DACL and post-connect impersonation check, rejects remote clients, and uses first-instance
ownership. The prototype deliberately authorizes one configured logon SID; multi-user metadata
visibility remains part of `SECURITY-001` and is not claimed solved.

The default Windows Named Pipe descriptor is not used. DACL isolation protects against other users/sessions but does not claim to sandbox a malicious process already running as the same user. Detailed Agent API controls are defined in [API-SECURITY.md](API-SECURITY.md).

## 5. Metadata visibility

Engineering spikes may index elevated MFT metadata under a documented single-user workstation assumption. This is not sufficient for public distribution.

`SECURITY-001` must choose and prove one or a combination of:

- ACL-aware filtering of metadata before it reaches a user's searchable index;
- a secure non-elevated enumeration fallback for privacy-sensitive configurations;
- explicitly scoped roots whose visibility is verifiable under the current token;
- per-user/service isolation that prevents cross-user disclosure.

The decision must cover multi-user machines, service impersonation risks, ACL changes after indexing, removed users, and index file permissions.

## 6. Content rule

The elevated broker never reads content. Later extraction opens files anew from the per-user agent/extractor host for every content request, allowing Windows to enforce the user's current access. Indexed metadata is not authorization. Failed access is recorded as a bounded status; the system does not retry using elevation.

Cloud placeholders are not hydrated without an explicit content policy. EFS content is processed only when the current user can normally decrypt/read it.

## 7. Local data protection

- Per-user state and indexes inherit restrictive user-only ACLs.
- Secrets are not stored in logs or index fields.
- Diagnostic export excludes filenames, paths, queries, and content by default.
- Telemetry is optional and contains no names, paths, queries, or content.
- Uninstall and reset operations require explicit scope and report whether indexes are retained.

Encryption-at-rest beyond normal OS/user protection is a separate product decision, not silently claimed.

## 8. Search result actions

Open/copy actions resolve the current link immediately before execution and tolerate disappearance or access denial. They never invoke shell commands through unescaped strings.

`START-010` implements this rule by sending only `DocumentId` through the WebView command. The
desktop Rust layer resolves the current `CatalogItem` through Agent Wire, requires an online,
existing, supported object, and only then invokes the platform opener or clipboard. The frontend
has no opener, clipboard, shell, broker, filesystem-provider, SQLite, or Tantivy permission.

## 9. Security test gate

Before public release:

- unauthorized IPC client tests pass;
- Agent API capability and MCP adapter isolation tests pass;
- malformed/oversized message fuzzing passes;
- cross-user metadata visibility is tested;
- broker binary/API review confirms no content-read surface;
- service install/upgrade/uninstall permissions are tested;
- local state ACLs are verified after install and upgrade;
- threat model and SECURITY-001 decision are reviewed and dated.
- no v0.1 process listens on TCP.
