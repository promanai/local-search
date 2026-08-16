# START-003-R — controlled substring recall

Status: **PASS — frozen as `POSITIONAL-TRIGRAM-v1` by
`ENGINEERING-GATE-001-PASS`**

## Contract

- substring queries contain at least three normalized characters;
- one/two-character queries use token/prefix behavior and are outside this
  substring contract;
- candidate recall is 100% for every controlled query whose complete
  ground-truth set fits the configured candidate budget;
- final precision is 100% after `normalized_name.contains(normalized_query)`;
- end-to-end p95 is at most 75 ms and p99 is at most 150 ms;
- the selected 5M index is preferably no larger than 2x the measured START-002
  catalog index at the same scale.

## Controlled ground truth

The workload scans every filename in the combined synthetic and controlled
dataset to compute ground truth. Labelled cases cover beginning, middle, and
end positions; spaces; `_`, `-`, `.`, and parentheses; digits; long filenames;
repeated sequences; Latin and Cyrillic; case folding; NFC/NFD equivalence;
rare substrings; a common substring with 250 true hits inside the candidate
budget; and the minimum supported three-character shape.

Candidate pressure is tested by placing the one real `qxzmarker` hit after 350
documents containing every query trigram in the wrong order. With the accepted
candidate limit of 300, retrieval must still return the real hit. Positional
gram phrases prevent reordered false candidates from consuming the bounded
verification window; exact normalized verification remains mandatory.

## Decision rule

The three existing strategies were compared at 100K. A strategy that misses a
supported three-character substring or exceeds the candidate/precision
contract is rejected regardless of latency. The recall-correct performance
front-runner then received three clean 5M confirmation trials. The accepted
evidence selected positional trigram retrieval for `TANTIVY-SCHEMA-v1`.

Run the clean evidence matrix with:

```powershell
benchmarks/run-start-003-r.ps1
```

Reports are written as JSON, CSV, and Markdown under
`reports/spikes/start-003-r/` and use the shared engineering report schema.
