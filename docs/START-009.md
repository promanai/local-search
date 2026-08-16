# START-009 — Process and WinFS Service Architecture

Status: **PASS (engineering prototype)**

Public-release status: blocked on installer validation and `SECURITY-001` multi-user metadata policy.

## Outcome

`START-009` introduces a real metadata-only privilege boundary:

```text
Agent / indexing coordinator (current user)
  -> BrokerFilesystemProvider
  -> Broker Wire v1
  -> authenticated local Named Pipe
  -> localsearch-fs-service (elevated Windows service)
  -> WindowsFilesystemProvider (MFT/USN internals)
```

The Agent-facing client implements the existing portable `FilesystemProvider` contract. Neither
the Agent, graph, core domain, nor broker wire DTO learns about `USN`, `MFT`, Windows handles,
journal layouts, or service-control types.

## Broker Wire v1

The checked-in machine contract is [`contracts/broker-wire-v1.json`](../contracts/broker-wire-v1.json).
Semantic and codec versions are independent. Frames use an exact four-byte little-endian length
prefix plus bounded JSON, with a one MiB maximum.

The complete allowlist is:

1. `broker_get_capabilities`;
2. `discover_volumes`;
3. `start_scan`;
4. `read_scan_page`;
5. `cancel_scan`;
6. `read_changes`.

There is no arbitrary path, file-content, extraction, hashing, write, process execution, search,
user-database, plugin, configuration, or service-admin operation.

Every request has protocol/codec versions, a bounded request ID, and a deadline. Exact request-ID
replay is rejected within a bounded 4096-request window. Unknown versions/operations, malformed
JSON, hostile lengths, missing scans, and oversized pages fail with stable redacted categories.

## Streaming and backpressure

Full enumeration and reconciliation run in bounded background producers:

```text
native provider
  -> 256-event sync queue
  -> read_scan_page(maximum 256)
  -> portable FilesystemEvent sink
```

The producer waits when the queue is full instead of growing memory. Cancellation or service drop
releases a producer blocked by backpressure. A completed stream returns exactly one opaque provider
checkpoint; incremental journal reads are also capped at 256 canonical events per response.

## Authentication and endpoint ownership

The broker Named Pipe:

- accepts only `LocalSearch/WinFS/v1` endpoint names;
- is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`;
- uses an explicit DACL for one configured logon SID;
- verifies the connected client by pipe impersonation against the same logon SID;
- sets `PIPE_REJECT_REMOTE_CLIENTS`;
- never accepts an identity or grant asserted inside Broker Wire.

The prototype is intentionally single-authorized-logon. Cross-user metadata filtering and the final
multi-user service topology remain a release blocker under `SECURITY-001`.

## Windows service lifecycle

`localsearch-fs-service --windows-service` uses the Windows Service Control Dispatcher, registers
`Stop`, `Shutdown`, and `Interrogate`, reports `Running`/`StopPending`/`Stopped`, and propagates stop
into accept/read/write waits and active scan workers. The same executable has an elevated-console
mode and `--once` test mode. Registration/removal, account hardening, recovery policy, signing, and
upgrade orchestration remain correctly owned by `START-012`.

## Evidence

Automated tests prove:

- broker DTO round-trip and hostile-length rejection;
- exact six-operation allowlist with no privileged proxy surface;
- 600 events traverse a queue smaller than the stream without loss;
- service drop cancels a producer blocked by backpressure;
- replay, unknown version, malformed request, and missing scan fail closed;
- a real service subprocess negotiates over an authenticated Named Pipe;
- the first pipe instance prevents endpoint squatting;
- the current process cannot connect to a pipe authorized for another logon SID;
- malformed content-like input returns a redacted error and does not crash the service;
- broker snapshot -> SQLite/outbox -> Tantivy -> Agent search succeeds;
- Agent restart reopens the durable state and returns the same result.

The already accepted elevated VHDX evidence proves the underlying Windows provider's live
MFT/USN lifecycle. `START-009` proves the new process, protocol, authentication, flow-control, and
restart boundaries around that provider.

## Deliberate exclusions

- service installation/uninstallation and signed packaging (`START-012`);
- final multi-user metadata visibility policy (`SECURITY-001`);
- Agent autostart (`START-012`);
- UI (`START-010`);
- resource governor (`START-011`);
- file content in every form.
