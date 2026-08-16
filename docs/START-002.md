# START-002 — Tantivy Catalog Retrieval

Status: experimental implementation; engineering decision pending measured evidence

## Scope

This spike measures exact filename, filename-token, and filename-prefix retrieval at 100K, 1M, and 5M records. It intentionally contains no substring, fuzzy matching, candidate verification, or product ranking.

The reusable `localsearch-benchmark-data` crate generates records independently from `(dataset version, seed, ordinal)`. Generation is streaming and order-independent, includes realistic extension frequencies and directory depth, repeated basenames, and Unicode/Cyrillic names. Changing generation semantics requires incrementing `DATASET_VERSION`.

## Experimental schema

`start002-name-exact-token-prefix-e1` contains only:

- a stored ordinal used to verify retrieval;
- a normalized raw-name term for exact retrieval;
- lowercased simple filename tokens;
- lowercased 1–32 character edge n-grams for filename-prefix retrieval.

This is benchmark instrumentation, not a declaration of `TANTIVY-SCHEMA-v1`. In particular, the edge n-gram cost is part of the evidence to review.

One `CatalogWriter` exclusively owns mutation. `CatalogReader` retains Tantivy's reusable reader/search resources and exposes explicit reload, with a contract test proving that a committed update becomes visible only after reload.

## Reproducible release run

From a clean implementation commit:

```powershell
cargo run -p localsearch-catalog-spike --release -- `
  --records 100000,1000000,5000000 `
  --seed 20260814 `
  --queries-per-kind 30 `
  --warm-repetitions 3 `
  --top-k 20 `
  --writer-heap-mb 256 `
  --index-root C:\Temp\localsearch-start002 `
  --output-root reports\benchmarks `
  --storage "measured device description" `
  --power "AC, measured mode"
```

Each scale emits JSON conforming to `benchmarks/report.schema.json`, lossless raw latency samples in JSON and CSV, and a concise Markdown view. Index directories use unique run IDs and are removed after index-size and query measurements, so the multi-million-record indexes are not retained.

Warm numbers use one previously warmed reusable reader. `reader_cold` numbers include opening a fresh Tantivy `Index` and `IndexReader` for every query. The runner does not flush the operating-system filesystem cache, so these are reader-cold rather than physical-device-cold results; reports record that limitation explicitly.

Recorded release evidence and its three-trial interpretation are in [`reports/benchmarks/START-002-SUMMARY.md`](../reports/benchmarks/START-002-SUMMARY.md).
