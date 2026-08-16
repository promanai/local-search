# ENGINEERING-GATE-001 result

Decision: **PASS — START-005 is unlocked**

Review date: 2026-08-15

The evidence validates Tantivy catalog scale, exact/token/prefix retrieval,
controlled substring recall and candidate pressure, deterministic ranking, the
portable provider boundary, native Windows MFT/USN behavior, and release-mode
provider-level incremental consumption. No structural baseline change is
required.

## Gate decision

| Question | Decision | Evidence |
| --- | --- | --- |
| Tantivy catalog retrieval | **PASS** | Three clean trials at 100K, 1M, and 5M; zero query errors |
| Selected filename schema | **TANTIVY-SCHEMA-v1** | Exact/token/prefix plus positional filename trigrams and stored verification values |
| Selected substring strategy | **POSITIONAL-TRIGRAM-v1** | 100% supported recall and precision at 5M, including common and candidate-pressure cases |
| Windows provider lifecycle | **WINDOWS-PROVIDER-CONTRACT-v1 PASS** | Fixture, isolated native lifecycle, and release-mode Rust provider lifecycle all pass |
| Baseline architecture change | **NO structural change** | Portable contracts and the Windows-private journal/checkpoint boundary remain valid |

## Measured results

Catalog figures are medians over three trials; bracketed substring figures are
the observed range over three trials.

| Scale | Catalog index | Catalog peak RSS | Catalog build | Substring index |
| ---: | ---: | ---: | ---: | ---: |
| 1M | 75.431 MB | 336.4 MB | 10.288 s | 128.55–133.52 MiB |
| 5M | 355.921 MB | 466.9 MB | 66.276 s | 624.17–643.84 MiB |

At 5M records, warm catalog p95 was `0.025 ms` exact, `0.092 ms` token,
and `0.105 ms` prefix. Reader-cold p95 was `9.388 ms`, `10.319 ms`, and
`9.827 ms`, respectively; reader-cold recreates the reader but does not flush
the Windows filesystem cache.

Positional trigram passed 100% controlled recall and precision with
`1.847–1.851x` catalog amplification. Aggregate end-to-end p95 was
`0.946–6.790 ms`, p99 was `1.992–22.923 ms`, and the worst common-case
p95/p99 was `43.285/46.710 ms`. Every accepted substring candidate was verified
with normalized `contains` before deterministic product ranking.

The isolated `LS_TEST` VHDX native run enumerated 5,011 MFT records at
`7,231.4 records/s`. Native USN observation latency was `20.122 ms` p50,
`34.744 ms` p95, and `107.922 ms` p99. Restart/resume, journal recreation,
old-checkpoint rejection, reconciliation, offline/online recovery, and stable
volume identity all passed.

The release-mode provider run then consumed the same class of live changes
through `WindowsFilesystemProvider::read_changes`. Its 30 end-to-end samples
measured `1.0388 ms` p50, `1.4913 ms` p95, and `1.5092 ms` p99. Create,
rename, move, directory rename, hard-link create/delete, metadata modification,
final delete, restart/resume, journal recreation, old-checkpoint rejection, and
reconciliation passed with zero lost logical events and zero duplicate logical
objects.

## Frozen v1 decisions

1. `TANTIVY-SCHEMA-v1`: exact, token, prefix, and positional trigram filename
   retrieval with stored raw/normalized values for verification and display.
2. `POSITIONAL-TRIGRAM-v1`: substring length at least three, candidate limit
   `300` by default and `500` hard maximum, exact normalized verification, then
   deterministic ordering `exact > prefix > token > substring > path`.
3. `WINDOWS-PROVIDER-CONTRACT-v1`: stable object/link identity, opaque
   versioned checkpoints, snapshot-to-journal continuity, bounded resumable
   reads, typed history gaps, and reconciliation. JournalId, USN, native
   file-reference layout, reason flags, and handles remain adapter-private.

The default Windows provider constructor remains a safe `SnapshotOnly`
fallback. The explicit broker-mode constructor advertises and implements
`PersistentObjectJournal`; missing privilege or journal availability remains a
typed error rather than a silent downgrade.

## Known limitations

- Query measurements were taken warm on one Windows/NVMe host; reader-cold is
  not a physical-device-cold measurement.
- Peak RSS is sampled, and concurrent indexing/search remains an operational
  benchmark for later tuning.
- The native MFT throughput probe and provider-level canonical event test are
  independent evidence paths; production fast enumeration remains behind the
  frozen provider contract rather than leaking into portable core.

These limitations do not invalidate the selected v1 contracts. Any later
schema or provider-format change requires an explicit new version and rebuild
or reconciliation path.

## Scope unlocked

`START-005 — Filesystem Graph` is authorized. SQLite durable projection, Agent
API, and UI remain separately scoped work rather than implicit additions to
this gate.

## Evidence

- [START-002 catalog summary](../reports/benchmarks/START-002-SUMMARY.md)
- [START-003 substring and ranking summary](../reports/spikes/start-003/README.md)
- [START-003-R controlled recall summary](../reports/spikes/start-003-r/README.md)
- [START-004 Windows provider evidence](../benchmarks/start-004/README.md)
- [START-004-LIVE native lifecycle evidence](../reports/spikes/start-004-live/start-004-live-20260815T034213Z.md)
- [START-004-LIVE provider lifecycle evidence](../reports/spikes/start-004-live/start-004-provider-live-20260815T043000Z.md)
- [Machine-readable report schema](../benchmarks/report.schema.json)

Accepted provenance commits are retained in main history. The annotated tag
`ENGINEERING-GATE-001-PASS` identifies this frozen decision.
