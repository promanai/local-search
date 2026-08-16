# START-016: Bounded volume projection refresh

Status: **engineering implementation PASS**

This milestone removes an unbounded operation from the durable filesystem graph. A volume becoming
offline or requiring reconciliation no longer enumerates every link into memory and no longer emits
the complete catalog fan-out inside the state-transition transaction.

## Production failure mode removed

Before this change, `SetVolumeState` and `RequireReconciliation` collected all links belonging to the
volume in a `BTreeSet`. On a multi-million-file volume this made a small control-plane mutation scale
with the entire volume and then attempted to materialize all projection mutations in one transaction.
The same path is used after a source-history gap, so it was a direct obstacle to safe automatic
observation.

Graph schema v4 replaces that behavior with one durable job per volume:

- the volume state and refresh job commit atomically;
- enqueueing or superseding a job is constant-size with respect to the number of files;
- `projection_scan_cursor` advances by stable `FileLinkId` order;
- every refresh transaction scans at most the caller-provided link limit;
- a newer volume transition resets the cursor, converging the projection to the newest state;
- desired-state reads expose the new availability immediately through normalized graph joins;
- durable catalog/content consumers converge through the existing outbox, ACK, and compaction path.

Schema v4 also adds `(volume_id, file_link_id)` indexes for range-seek scans. The existing directory
path-refresh scan now uses the same explicit first-page/range-page query shape instead of a nullable
cursor predicate.

## Agent scheduling

The Agent now treats pending path and volume refresh jobs as projection backlog. When the resource
governor permits background work, it drains bounded pages before running the catalog projector.
`Active`, `Pressure`, and energy-saver pause behavior remains authoritative. Completed projection
mutations are acknowledged and compacted through the normal worker path.

This also closes a pre-existing integration gap: directory path-refresh jobs were durable in the
graph but were not scheduled by the Agent.

## Contract evidence

- The graph contract transitions a two-document volume offline while asserting zero immediate
  outbox fan-out and exactly one pending refresh job.
- With a batch limit of one, each call scans and appends no more than one document; the durable job
  completes only after the terminal page.
- Desired-state documents become offline immediately, while the outbox advances only through the
  bounded refresh calls.
- The Agent contract drains the durable refresh, applies it to Tantivy, acknowledges/compacts the
  outbox, and returns the document with `Offline` availability.
- Forward migration tests cover creation of the v4 queue and both range-scan indexes.

## Remaining boundary

This milestone does **not** yet connect the Agent scheduler to the elevated WinFS USN broker. The
provider and broker can already read volume history, but production automatic observation still
needs crash-safe initial bootstrap, journal checkpoint handoff, gap-to-reconciliation wiring, and a
clear policy for user-selected directory scopes. Schema v4 makes the most expensive recovery state
transition bounded before that integration is enabled.

The next strong milestone is therefore broker-backed continuous NTFS observation with resumable USN
checkpoints and an explicit fallback to the bounded reconciliation path implemented here.

Implemented as an explicit opt-in by
[START-017-BROKER-USN-OBSERVATION](START-017-BROKER-USN-OBSERVATION.md).
