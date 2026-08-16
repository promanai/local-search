# START-003 substring and product-ranking spike

This crate contains the START-003 and START-003-R engineering experiments. It
is not the production query engine and does not by itself declare
`TANTIVY-SCHEMA-v1`.

It compares three isolated indexes:

- `trigram`: all overlapping filename trigrams;
- `token_prefix_limited_trigram`: exact/token/prefix retrieval followed by a
  capped trigram fallback;
- `bounded_fourgram`: a less aggressive four-gram index with lexical fallback
  below four characters.

Every accepted result path is:

```text
bounded Tantivy candidates
-> exact normalized name/path verification
-> deterministic exact > prefix > token > substring > path ranking
```

START-003-R uses positional gram phrases for candidate retrieval. Reordered
gram collisions therefore cannot consume the bounded candidate window before
verification. Exact normalized verification remains mandatory.

Product-search queries of one or two characters use only bounded lexical
retrieval. A caller forcing substring-only semantics receives the typed
`PolicyError::ExpensiveShortSubstring` before Tantivy executes.

The dataset is consumed unchanged from `localsearch-benchmark-data`. START-003
adds only the separately versioned `substring-product-ranking-v1` workload.

## Release benchmark

Run one supported scale:

```powershell
cargo run --release -p localsearch-substring-spike --bin start-003-bench -- `
  --run-id 20260815T020000Z-trial1 `
  --records 100000 `
  --seed 20260814 `
  --samples 30 `
  --candidate-limit 300 `
  --writer-heap-bytes 134217728 `
  --memory-bytes 34045902848 `
  --storage "Micron 2650 1TB NVMe SSD" `
  --power "High performance; BatteryStatus=2 (AC power); 10%" `
  --output reports/spikes/start-003/100k
```

Omit `--strategy` to run all strategies serially. A single strategy can be run
with `--strategy trigram`, `--strategy token_prefix_limited_trigram`, or
`--strategy bounded_fourgram`. The index is temporary and is removed after its
JSON/CSV/Markdown measurements are written.

Artifacts are created with no-overwrite semantics. Generated START-003 output
is excluded from subsequent provenance checks, while any source/workspace
change still sets `dirty_tree=true`. The 100K, 1M, and 5M release matrix must be
run from the same clean commit with the same seed and parameters.

The controlled recall matrix is documented in
[`docs/START-003-R.md`](../../docs/START-003-R.md) and can be run with
`benchmarks/run-start-003-r.ps1`.
