# Windows Filesystem Policy

Status: **`WINDOWS-PROVIDER-CONTRACT-v1` frozen by
`ENGINEERING-GATE-001-PASS`**

The v1 portable behavior is stable object/link identity, opaque versioned
checkpoints, a continuity-checked snapshot-to-USN handoff, bounded resumable
reads, typed history-gap rejection, and reconciliation. JournalId, USN values,
file-reference layout, handles, and reason flags remain private to the Windows
adapter.

## 1. Source selection

| Source | Initial discovery | Change tracking | Reconciliation |
| --- | --- | --- | --- |
| Local supported NTFS | MFT/USN enumeration | USN journal | Native reconciliation |
| NTFS without usable journal | MFT/USN where allowed | bounded watcher or periodic check | Required |
| FAT/exFAT/removable | Directory crawl | `ReadDirectoryChangesW` where supported | Periodic crawl |
| Network path | Deferred dedicated policy | Deferred | Deferred |
| Unknown provider | Capability error | None by default | User-visible unsupported state |

Drive letter is not volume identity. Mounted folders and multiple mount points are discovered and represented explicitly.

## 2. Object policies

| Object | Default behavior |
| --- | --- |
| Normal file | Index catalog metadata |
| Directory | Index metadata and parent relationship |
| Hard link | Separate link, shared physical object |
| Symbolic link | Index the link; do not follow by default |
| Junction | Index the junction; do not traverse it |
| Generic reparse point | Do not follow unless a provider policy is registered |
| Inaccessible object | Record bounded status; never bypass ACL for content |
| Offline volume | Preserve state and mark unavailable |
| Removed removable volume | Preserve or expire according to configured volume policy |
| Cloud placeholder | Metadata first; do not force hydration |
| Sparse file | Metadata in v0.1; stream carefully in later content indexing |
| EFS file | Content only when the user process can normally read it |
| Huge file | Catalog metadata; later content handling uses large-file policy |

Traversal never follows links or reparse points implicitly. Any future provider that follows them must define cycle, boundary, network, hydration, and security behavior.

## 3. Broker output

The elevated broker may emit:

- volume identity and capabilities;
- filesystem and mount metadata;
- object ID, parent ID, name, attributes, timestamps, size, reparse tag;
- journal identifier, valid range, cursor, reason flags, and record version;
- explicit structured errors.

The broker may not emit file bytes, extracted text, file previews, hashes requiring file reads, or search results.

The metadata-only Broker Wire v1 and bounded service implementation are recorded in
[`START-009`](START-009.md). The protocol exposes canonical provider events and opaque checkpoints;
native journal fields remain inside `localsearch-windows-fs` even across the process boundary.

## 4. Record decoding

- Check record length before every field access.
- Reject zero progress, misalignment, integer overflow, and buffer overrun.
- Support only explicitly tested record versions; preserve an unsupported-version error.
- Decode UTF-16 losslessly where possible and mark replacement when malformed input requires it.
- Limit batch byte size and record count before IPC serialization.
- Treat all kernel/source buffers as untrusted inputs for parser safety.

## 5. Snapshot and journal handoff

The final protocol is evidence-driven by SPIKE-001. It must capture a journal identity/range, enumerate a bounded snapshot, replay changes after the chosen boundary, and validate that the saved cursor remains in the same journal and valid range before declaring the snapshot current.

If validity cannot be established, the operation retries or enters reconciliation. It never silently advances the durable cursor.

## 6. Event normalization

Raw reason flags are coalesced into canonical source changes:

```text
ObjectObserved
ObjectMetadataChanged
ObjectContentChanged
LinkCreated
LinkRenamedOrMoved
LinkDeleted
ObjectDeleted
ReconciliationRequired
```

Coalescing uses bounded state and flushes on close/batch boundary/timeout. Correctness does not depend on receiving a perfect rename pair.

## 7. Fallback boundary

Fallback scanning is behind the same canonical source interface. It may be less efficient but cannot introduce a second data model. v0.1 may ship native NTFS first, provided unsupported volumes are reported clearly rather than ignored.
