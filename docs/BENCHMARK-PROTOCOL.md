# Benchmark Protocol

Status: v0.1 baseline

## 1. Purpose

Benchmarks select architecture and enforce regressions. A reported number is invalid without dataset version, workload version, hardware profile, build profile, commit SHA, dependency lockfile, run parameters, and raw output.

## 2. Independent axes

### CPU and RAM

- Low: approximately 8 GB and a low-end four-logical-processor class machine.
- Typical: current mainstream desktop/laptop configuration.
- High: workstation-class CPU and 32–64 GB RAM.

Exact reference machines are recorded by model/specification; labels alone are insufficient.

### Storage

- rotational HDD;
- SATA SSD;
- NVMe SSD.

Storage class is not inferred from the CPU/RAM profile.

### Dataset

- 100K records for fast iteration;
- 1M release baseline;
- 5M supported-scale gate;
- 10M target-scale report.

### Workload

- exact filename;
- prefix;
- token;
- substring;
- fuzzy;
- path and extension filters;
- mixed trace;
- incremental create/rename/move/delete;
- active indexing plus interactive search.

Content workloads begin in v0.2 and are reported separately.

## 3. Dataset requirements

The generator is seeded and versioned. It includes realistic extension frequencies, directory depth, repeated basenames, Unicode/Cyrillic names, long names, common developer trees, adversarial short substrings, and controlled exact/prefix/token/substring relevance labels.

Synthetic catalog benchmarking does not substitute for live MFT enumeration testing. Sensitive production filenames are never committed to fixtures or reports.

## 4. Query set

Queries are versioned and divided into warm-up and measured sets. They include hit-frequency bands, misses, short queries, Unicode cases, high-expansion terms, and policy-rejected queries.

Expected relevance classifications enable ranking assertions in addition to latency measurement.

## 5. Measurements

```text
enumeration records/sec
indexing documents/sec
initial catalog wall time
incremental update latency
index bytes total and per document
field/schema amplification
process peak and steady working set
CPU time and utilization
disk reads/writes where available
search p50/p95/p99/max
cold-start and cold-query latency
candidate count and verification rejection ratio
hotkey-to-visible p50/p95
```

Latency histograms or raw samples are retained; averages alone are not accepted.

## 6. Run discipline

- Use release builds and record compiler/toolchain.
- Record background state and power mode.
- Separate cold and warm runs with a documented cache procedure.
- Use repeated trials and report variance.
- Avoid mixing index construction and query measurements unless testing the explicit concurrent workload.
- Record governor decisions during concurrent tests.
- Preserve raw machine-readable output under `reports/benchmarks/` or an external artifact store when too large.

## 7. Provisional gates

```text
warm search p50 < 30 ms
warm search p95 < 75 ms
warm search p99 < 150 ms
cold search target < 250 ms
resident hotkey p50 < 50 ms
resident hotkey p95 < 100 ms
```

Gates apply to named reference hardware and workload versions. Results on other profiles characterize adaptation; they do not silently redefine the reference gate.

## 8. Regression policy

A change affecting schema, tokenizer, ranker, writer configuration, storage format, or query planner runs the relevant comparison suite. A statistically/materially significant regression requires explanation, explicit acceptance, or rollback.

Benchmark code and correctness tests share canonical normalization and query cases to avoid measuring behavior different from the product.
