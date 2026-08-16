# Protocol Compatibility

Status: v0.1 baseline plus opt-in Agent API v2 content extension

## 1. Version domains

These versions are independent:

```text
canonical_schema_version
agent_api_version
agent_ipc_codec_version
broker_protocol_version
mcp_protocol_version
http_api_version (future)
state_schema_version
catalog_schema_version
ranking_version
```

An MCP protocol change does not force an Agent API or catalog migration. A Tantivy schema rebuild does not change client operations unless product semantics change.

## 2. Source of truth

```text
Canonical domain types
        |
        v
Versioned Agent Wire DTO  <-- client contract authority
        |
        +-- LocalTransport codec (Named Pipe on Windows; Unix socket later)
        +-- MCP mapping
        +-- future HTTP adapter DTO/OpenAPI
```

MCP schemas and future OpenAPI describe adapters. Neither becomes an alternate core model.

## 3. Agent protocol evolution

- Additive optional fields require defined safe defaults.
- Unknown optional fields are ignored/preserved according to the codec contract.
- Unknown required behavior causes an explicit unsupported-version/capability error.
- Renamed/removed semantics require a new protocol version.
- Limits may become stricter without a protocol bump when responses use stable policy errors.
- Identity fields are never repurposed; new identity forms add typed fields/versions.
- Ranking changes increment `ranking_version`, not necessarily Agent API version.

Desktop, CLI, and MCP adapter negotiate only Agent protocol versions they implement. Installer upgrades allow a documented bounded skew and retain a compatible path or perform coordinated restart.

## 4. MCP `2026-07-28`

The primary adapter follows the official [MCP `2026-07-28` version](https://modelcontextprotocol.io/specification/2026-07-28): stateless self-contained requests, per-request version/capabilities, `server/discover`, and no protocol-level session identity.

MCP client information is mapped to diagnostics/approval identity only after local validation. It is not an Agent authorization token.

## 5. MCP `2025-11-25` compatibility

This compatibility front end is **not shipped in v0.1**. A legacy `initialize` request is rejected explicitly with the supported `2026-07-28` version, and no silent downgrade occurs. If ecosystem evidence later justifies enabling it, the adapter will contain a separate initialization/session-era front end. Its negotiated connection state will exist only to satisfy that older MCP contract and map every tool call to a fresh bounded Agent request.

Version selection rules:

- modern `server/discover`/per-request metadata is preferred;
- older initialization is accepted only in explicit compatibility mode;
- a client cannot mix eras on one logical adapter instance ambiguously;
- no silent downgrade after a modern version mismatch;
- LocalSearch capabilities and results remain identical across eras.

Compatibility is justified by recorded client ecosystem tests and can be deprecated independently of Agent API support.

## 6. Broker protocol

The agent-to-broker protocol is separate, narrower, and not exposed to UI/MCP. Broker Wire v1 is
implemented by [`START-009`](START-009.md) with independent semantic/codec versions, exact
metadata-operation allowlist, bounded frames/pages, and explicit rejection of unknown versions.
Version skew is constrained by installer/service upgrade rules. Unknown operations fail closed, and
no compatibility layer may introduce content-read functionality.

## 7. Desktop adapter

The resident desktop client is an Agent Wire v2 consumer. Agent Wire v2 adds only the explicitly
authorized `search.content` operation; `agent-wire-v1.json` remains checked in as the immutable
catalog-only historical contract. Its request generations are tagged with
bounded request IDs; both the Rust coordinator and WebView state reject late responses. The client
creates a fresh same-logon Named Pipe exchange per operation, so Agent restart does not require a
desktop restart. Desktop code may translate presentation and user actions, but it cannot depend on
Broker Wire, provider contracts, SQLite, Tantivy, or backend-native scores.

## 8. Future HTTP

If an HTTP gateway is approved, `/v1` identifies HTTP adapter semantics, not the domain or index schema. OpenAPI is generated/validated from the adapter DTO and maps to a supported Agent API version. The gateway publishes its own compatibility and deprecation window.

## 9. Compatibility tests

- golden wire fixtures for every supported Agent version;
- new/old desktop-agent skew matrix;
- MCP official-schema fixtures for every shipped MCP era;
- unknown optional/required field behavior;
- unsupported version and capability errors;
- adapter equality tests showing equivalent Agent requests/results;
- coordinated upgrade/restart and rollback tests;
- no backend-native type or score leakage in any supported version.
