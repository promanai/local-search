# Index and State Migrations

Status: v0.1 baseline

## 1. Independent versions

```text
canonical_schema_version
state_schema_version
catalog_schema_version
content_schema_version
vector_schema_version
tantivy_format_version
extractor_pipeline_version
embedding_model_version
agent_api_version
agent_ipc_codec_version
broker_protocol_version
mcp_protocol_version
```

Changing one version does not automatically change the others. Compatibility rules are explicit and tested.

## 2. Directory layout

Conceptual layout:

```text
indexes/
  catalog-v3-generation-17/
  catalog-v4-generation-1.building/
  active.json

state/
  state.sqlite3
```

Names are illustrative; production paths are per-user and platform-safe. The active manifest is atomically replaced and includes schema, format, generation, checkpoint, creation version, and validation status.

## 3. Catalog rebuild

For a breaking catalog schema or unsupported Tantivy format:

1. continue searching the current valid index;
2. create a new `.building` directory;
3. project live durable catalog state at a recorded mutation boundary;
4. catch up later mutations;
5. commit and validate counts, IDs, sampled queries, and checkpoint;
6. atomically switch `active.json`;
7. retain the prior generation for rollback;
8. remove it later under disk policy.

The active directory is never mutated into a new breaking schema in place.

## 4. SQLite migration

SQLite migrations are ordered, transactional where SQLite permits, and tested from every supported upgrade origin. Destructive compaction occurs only after a backup/rollback strategy and successful post-migration validation.

An application that is too old for the state schema refuses to write it.

## 5. IPC compatibility

Desktop, MCP adapter, agent, and broker negotiate only the protocol versions and capabilities relevant to their boundary. Upgrade sequencing allows temporary skew defined by a compatibility matrix. Unknown required fields or operations fail closed; optional fields have safe defaults. See [PROTOCOL-COMPATIBILITY.md](PROTOCOL-COMPATIBILITY.md).

## 6. Rollback

Rollback may reactivate the previous compatible catalog generation. It must not run an older binary against a newer incompatible SQLite schema. Installer rollback and data rollback are separate operations.

## 7. Disk pressure during migration

Before building a parallel index, the planner estimates required space and preserves the configured reserve. If insufficient, it postpones migration or requests an explicit storage decision; it never deletes the only valid index first.

## 8. Validation

- active manifest and directory agree;
- schema/format versions are readable by the running binary;
- checkpoint does not exceed durable outbox state;
- sampled live IDs are searchable and deleted IDs are absent;
- logical document count is within explained tolerance;
- rollback generation remains intact until the retention gate passes.
