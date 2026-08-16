# ENGINEERING-GATE-001

Status: `CONDITIONAL`; see the [engineering review result](ENGINEERING-GATE-001-RESULT.md)

## Purpose

This gate converts the three initial risk spikes into explicit architecture decisions. START-005 and later product work does not begin until one engineering review accepts or rejects this gate.

## Shared evidence contract

Every measured run records:

- spike ID, implementation commit, dirty-tree state, UTC timestamp, and report-format version;
- deterministic dataset/workload version, record count, and seed;
- Rust version, release/debug profile, target triple, logical CPU count, memory, storage, and power state;
- raw latency samples or a lossless histogram plus p50/p95/p99/max;
- wall time, throughput, peak working set, output/index bytes, errors, and run parameters;
- JSON conforming to [`benchmarks/report.schema.json`](../benchmarks/report.schema.json); tabular samples may additionally be emitted as CSV;
- a concise Markdown interpretation that links to the machine-readable artifacts.

Reports without the required provenance are diagnostic only and cannot select an architecture.

## START-002 gate: catalog retrieval

- Run deterministic 100K, 1M, and 5M datasets.
- Measure build time, documents/second, index bytes and bytes/document, peak RAM, and exact/token/prefix warm and cold p50/p95/p99.
- Keep fuzzy, substring, and product ranking out of this spike.
- Do not declare catalog schema defaults before the comparison is reviewed.

## START-003 gate: substring and product ranking

Compare at least:

1. trigram candidates;
2. token + prefix + limited trigram fallback;
3. a less aggressive bounded n-gram candidate strategy.

For each strategy, measure 1-character, 2-character, 3–5-character, and 6+-character queries across rare, common, and adversarial substrings. Report index amplification, build throughput, peak RAM, candidate retrieval latency, verification latency/rejection ratio, ranker latency, and end-to-end p50/p95/p99.

Every accepted path is two-stage: bounded Tantivy candidate retrieval, exact normalized substring verification, then deterministic product ranking with `exact > prefix > token > substring > path`.

## START-004 gate: Windows filesystem provider

Demonstrate `initial scan -> checkpoint -> changes -> restart -> resume` for create, metadata modification, rename, move, directory rename, delete, hard-link create/delete, agent/service restart, journal gap/recreation, and offline/online volume behavior.

MFT references, USN values, reparse data, handles, and Windows API types remain private to the Windows adapter. The adapter emits only `platform-core` contracts.

Every provider implementation must run a reusable behavioral contract suite covering stable initial observations, opaque checkpoint round-trip, canonical events, rename/move object identity, canonical deletion, and reconciliation convergence.

Current evidence: [START-004 Windows fixture and live-discovery report](../benchmarks/start-004/README.md). The reusable fixture contract passes, but elevated live MFT/USN, snapshot-handoff, throughput, latency, and controlled volume-lifecycle gates remain explicitly unvalidated. START-004 is therefore `CONDITIONAL`, not `PASS`.

## Decision record

The review publishes one result:

```text
ENGINEERING-GATE-001

Tantivy: PASS | FAIL | CONDITIONAL
Selected filename schema: ...
Selected substring strategy: ...
1M index: ...
5M index: ...
Search p95: ...
Windows enumeration throughput: ...
USN incremental latency: ...
Known filesystem limitations: ...
Architecture changes required: YES | NO
Decision commit: ...
Evidence: ...
```

Passing the gate permits a separate `TANTIVY-SCHEMA-v1` decision and START-005. A failed or conditional gate identifies the next bounded experiment; it does not silently relax the performance or correctness target.
