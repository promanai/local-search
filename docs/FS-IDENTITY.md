# Filesystem Identity

Status: baseline; details validated by SPIKE-001

## 1. Canonical identity

The public model uses `VolumeId + FileId128`. NTFS-specific record index and sequence-number layout remains private to the WinFS adapter. ReFS and later providers may populate the same opaque representation differently.

`MachineId` scopes volumes across devices but is not required in every in-memory key when the agent is already machine-local.

## 2. Object and link semantics

- Rename or move preserves `FileObject` and changes a `FileLink`.
- Creating a hard link creates another `FileLink` for the same object.
- Deleting one hard link deletes that link; the object remains while another live link exists.
- Content modification changes the object projection and later content projection.
- `ResolvedPath` is presentation/projection state, never a primary key.

## 3. Initial enumeration

The WinFS adapter emits bounded records containing at least object ID, parent ID, name, attributes, and source metadata required by the snapshot protocol. Records are decoded with length and version validation before conversion to canonical types.

The graph builder tolerates children arriving before parents. Unresolved records remain explicit and are retried after the enumeration pass; they are not dropped.

## 4. Journal continuity

For each volume, durable state records a platform-neutral checkpoint:

```text
volume_id
provider_id
checkpoint_format_version
opaque_checkpoint
```

For Windows, the adapter's opaque payload contains the journal identity, ingested USN, lowest valid USN, and snapshot generation it needs to prove continuity. These fields never cross the adapter boundary. If the journal identifier changes, or a saved cursor is below the current valid range, the adapter returns a canonical history-gap error and the volume enters reconciliation. Tantivy need not be destroyed; reconciliation produces ordinary idempotent mutations against authoritative state.

The snapshot-to-tail algorithm is finalized by SPIKE-001 and must demonstrate that changes occurring during enumeration are replayed or force a detected retry.

## 5. Rename and move

Rename-old/new journal records are correlated within a bounded window. Missing pairs are legal recovery inputs: the reconciler consults current graph/source state and produces the final idempotent link mutation.

A directory rename updates its link immediately. Descendant paths are resolved from the new graph for display, while a bounded subtree path-refresh job updates the Tantivy projection asynchronously.

## 6. Hard-link limitation gate

USN/MFT behavior for enumerating every hard-link name must be proven by fixture and live-volume tests. If the selected native enumeration does not expose all links, v0.1 must document the limitation and add an explicit reconciliation mechanism; it must not pretend one observed name is the complete object identity.

## 7. Volume lifecycle

Offline and removed volumes preserve identity and state. Search policy may hide or mark their results. Reattachment is matched using stable volume identity and journal continuity, not drive letter alone.

Drive letters and mount paths are mutable presentation data.
