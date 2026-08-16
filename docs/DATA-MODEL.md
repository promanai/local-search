# Data Model

Status: baseline for v0.1

## 1. Principles

- Identity is not a path.
- Physical objects and namespace links have different lifecycles.
- Platform identifiers are adapted into filesystem-neutral canonical types.
- Stored projections carry explicit versions.
- Domain types contain no backend-native values.

## 2. Identity types

```text
MachineId      stable installation/device identifier
VolumeId       stable canonical volume identifier
FileId128      opaque 128-bit filesystem object identifier
FileKey        { volume_id, file_id }
FileLinkId     stable identifier for a namespace link record
DocumentId     typed search-document identifier
MutationSeq    monotonic local outbox sequence
ProviderCheckpoint  opaque per-volume source continuation, never reused as MutationSeq
```

`FileId128` is opaque outside a platform adapter. NTFS v0.1 may zero-extend the native file reference representation; callers must not depend on its bit layout. A checkpoint carries a provider ID, format version, volume ID, and opaque bytes; only its provider interprets the payload.

## 3. FileObject

Represents a physical filesystem object:

```text
FileObject
  key: FileKey
  kind: file | directory | symlink | special | other
  size
  created_at?
  modified_at?
  hidden
  metadata_fingerprint
  content_version?
  availability
  state_version
```

Content hashes and extraction versions are added in v0.2. An object's version changes only for mutations relevant to that projection.

## 4. FileLink

Represents a namespace entry:

```text
FileLink
  id: FileLinkId
  object: FileKey
  parent: FileKey
  name_raw
  name_normalized
  link_state: live | deleted | unresolved
  state_version
```

Multiple `FileLink` rows may refer to the same `FileObject`. Rename/move changes link state; it does not imply content extraction.

The exact derivation of `FileLinkId` must be stable, collision-tested, and documented by the filesystem adapter. If the source cannot provide a durable link identifier, the state layer assigns one and reconciles it using object, parent, name, and event sequence.

## 5. ResolvedPath

`ResolvedPath` is computed by walking live parent links in the authoritative graph. Resolution is bounded by:

- maximum depth;
- cycle detection;
- missing-parent detection;
- unavailable-volume handling;
- explicit choice when an object has multiple parent links.

Search responses contain the resolved current path plus an indication when resolution is incomplete or the volume is offline.

## 6. CatalogDocument

The canonical catalog projection is one searchable link:

```text
CatalogDocument
  document_id
  object_key
  link_id
  name_raw
  name_normalized
  resolved_path
  path_normalized
  extension_normalized?
  kind
  size
  created_at?
  modified_at?
  hidden
  availability
  projection_version
  registered_metadata
  opaque_metadata?
```

`opaque_metadata` is stored only when a policy requires it and is not automatically searchable. Registered metadata has a name, type, normalization policy, indexing policy, and compatibility definition.

## 7. Future documents

`ContentDocument` is keyed primarily by `FileKey`, not path, so hard-linked content is extracted once. Search results join content hits back to current visible links.

`ChunkDocument` is keyed by chunk identity and carries object identity, text hash, model identifier, dimensions, and metric. It does not change the v0.1 catalog model.

## 8. Versions

Every stored row or payload that may outlive a process version must be interpretable through a declared schema or payload version. Unknown future versions fail closed and remain available for migration/diagnostics rather than being silently discarded.
