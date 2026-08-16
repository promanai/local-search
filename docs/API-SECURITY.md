# Agent API Security

Status: Windows Agent transport implemented by [`START-007`](START-007.md), MCP stdio boundary
implemented by [`START-008`](START-008.md); remaining release security work stays tracked for later
stages

## 1. Threat model

Protected data includes filenames, paths, topology, queries, result history, and index status. Relevant threats include:

- a different Windows user or session connecting to an Agent pipe;
- a remote SMB/named-pipe client;
- an unapproved local application enumerating private metadata;
- a same-user malicious process;
- malformed or oversized frames exhausting memory/CPU;
- a client granting itself capabilities;
- sensitive information leaking through logs/errors;
- a future localhost HTTP endpoint reached through a browser or DNS rebinding.

An explicit pipe DACL isolates other identities but is not a sandbox against malicious code already running with the same user token. Product documentation and tests must not claim otherwise.

## 2. Agent local transport: Windows v0.1 profile

Agent authorization and framing rules belong to the transport-neutral protocol boundary. Windows v0.1 maps that boundary to a Named Pipe. Windows' default named-pipe security descriptor is not acceptable because it can grant read access beyond the creator. LocalSearch supplies an explicit descriptor as described by [Microsoft's named-pipe security documentation](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights).

Requirements:

- DACL scoped to the intended logon SID/session and only required service identities;
- `PIPE_REJECT_REMOTE_CLIENTS`;
- least-specific access rights rather than broad generic rights where practical;
- server verification of the connected client identity/security context;
- per-user endpoint uniqueness and protection against pipe-name squatting;
- bounded concurrent connections and in-flight requests;
- bounded frame bytes, string bytes, collection lengths, nesting, query cost, and response bytes;
- idle/read/write/request deadlines;
- fail closed on authentication, impersonation, descriptor, or version errors.

A future Unix-domain-socket adapter must provide equivalent same-user endpoint isolation, permissions, bounds, deadlines, and anti-squatting guarantees. It does not inherit Windows DACL or impersonation mechanics.

## 3. Capability grants

v0.1 data capabilities are:

```text
search.catalog
read.metadata
index.status
```

The agent derives grants from locally approved client identity/configuration. Request fields and MCP client metadata cannot elevate privileges. Capabilities are checked per method and again at sensitive downstream actions.

No AI client receives `admin.*`, `read.content`, filesystem-write, settings, or broker capabilities in v0.1.

## 4. MCP process

The stdio adapter runs as the current user and therefore is not an OS sandbox. Its narrower tool surface limits what LocalSearch discloses, but it cannot protect the machine from an already-malicious MCP host running under the same user.

The host is responsible for user consent before launching a local MCP server or invoking tools. LocalSearch additionally requires explicit local client approval/grants for metadata access in the public release model.

The v0.1 adapter exposes only three capability-derived metadata tools, caps messages and concurrent
requests, and reaches search exclusively through Agent Wire over the secured current-user pipe. It
does not link the SQLite/Tantivy backend and opens no network listener.

## 5. TOCTOU and content

Search authorization and old metadata never authorize a future content read. In v0.2 each content request:

1. resolves the current link/object;
2. opens the source as the current user;
3. lets Windows re-evaluate current ACL/EFS state;
4. verifies expected identity/version where available;
5. enforces byte/range limits;
6. returns `AccessDenied`, `NotFound`, or `Changed` when appropriate.

The elevated broker is never a fallback for failed content access.

## 6. Logging and diagnostics

- Never log authorization material, pipe secrets, full queries, result names/paths, or content by default.
- Stable error categories and correlation IDs are safe for clients; detailed internal causes stay local and redacted.
- Administrative audit records contain operation, approved client identity, outcome, and time, not arbitrary payloads.
- Sensitive diagnostics require explicit short-lived enablement and clear user warning.

## 7. Future HTTP gateway gate

The agent never listens on TCP. A future HTTP API is a separate normal-user gateway over `LocalTransport` and is disabled by default.

Before implementation/shipping, `API-SECURITY-001` must define and test:

- exact loopback IPv4/IPv6 binding and Host validation;
- Origin validation and browser/CORS policy against DNS rebinding;
- per-client authorization, consent, token lifetime/rotation, and secure storage;
- CSRF/browser request defenses;
- request/rate/response limits;
- gateway discovery and port lifecycle;
- same-user attacker limitations;
- disable/uninstall behavior and audit.

No HTTP/OpenAPI work is required for v0.1.

## 8. Acceptance tests

- another user/session cannot connect;
- remote pipe clients are rejected;
- unauthorized and self-asserted scopes fail;
- malformed/oversized/deep messages do not cause unbounded allocation or panic;
- cancellation/deadline/rate limits release resources;
- error/log snapshots contain no test secrets, queries, names, or paths;
- MCP adapter cannot reach internal admin operations;
- no agent, MCP adapter, or elevated broker opens a listening TCP socket.
