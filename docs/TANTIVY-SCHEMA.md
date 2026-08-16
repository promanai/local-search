# Tantivy Catalog Schema v1

Status: **FROZEN by `ENGINEERING-GATE-001-PASS`**

Schema identifier: `TANTIVY-SCHEMA-v1`

## 1. Role

Tantivy is the candidate-retrieval projection for catalog search. Its schema is not the canonical product schema and may change independently through versioned rebuilds.

## 2. Candidate fields

| Field | Purpose | Initial treatment |
| --- | --- | --- |
| `document_id` | Idempotent delete/upsert key | Indexed exact; stored |
| `object_key` | Join to durable state | Indexed exact; stored |
| `link_id` | Link identity | Indexed exact; stored |
| `name_raw` | Display and verification | Stored |
| `name_exact` | Normalized exact lookup | Indexed exact |
| `name_tokens` | Filename token lookup | Indexed text |
| `name_ngram` | Substring candidates | Indexed positional trigrams |
| `path_raw` | Debug/fallback display projection | Stored subject to size results |
| `path_exact` | Exact normalized path | Indexed exact where justified |
| `path_tokens` | Path component lookup | Indexed text |
| `extension` | Filter | Indexed exact; optional fast field after benchmark |
| `kind` | File/folder filter | Indexed exact/fast field |
| `size` | Range/filter | Fast field |
| `created_at` | Range | Fast field when present |
| `modified_at` | Range/ranking | Fast field when present |
| `hidden` | Filter | Fast field |
| `availability` | Filter/status | Indexed exact/fast field |
| `projection_version` | Stale projection detection | Stored/indexed exact |

`path_raw` is retained for display and deterministic ranking. Path substring is
not part of v1. Exact-path indexing and new fast fields require an explicit
schema version and rebuild when later product measurements justify them.

## 3. Normalization

Normalization is a canonical function shared by indexing, query planning, verification, and rank classification. v1 defines:

- Unicode NFKC followed by Unicode-aware lowercase normalization;
- stable separator normalization for search;
- extension normalization without a leading dot;
- no locale-dependent transformation that differs across machines;
- raw name/path retained for display.

The handling of Unicode normalization form and Windows case behavior requires golden tests with Latin, Cyrillic, combining marks, supplementary characters, and malformed source names.

## 4. Substring strategy

`name_ngram` produces overlapping positional trigrams. Retrieval uses a phrase
query so grams must occur in order and adjacently. The field produces
candidates only.

For query length:

- 0: no search;
- 1–2 normalized characters: prefix/token strategy only, strict candidate cap;
- 3 or more: n-gram candidate query may run within cost limits;
- every candidate: verify `normalized_name.contains(normalized_query)` before assigning substring match class.

Path substring is not automatically enabled merely because filename substring succeeds; path amplification is measured separately.

The v1 default candidate limit is `300`; the hard maximum is `500`. Every
substring candidate must pass `normalized_name.contains(normalized_query)`
before ranking. The stable match order is
`exact > prefix > token > substring > path`.

## 5. Writer and visibility

One actor owns the catalog writer. It applies consecutive mutation ranges and performs bounded commits. Readers are long-lived and acquire a current searcher per request. Tests explicitly control reader reload when deterministic visibility is required.

Writer memory budget and worker count come from the governor's safe configuration. Reconfiguration occurs only at a safe commit boundary.

## 6. Delete and upsert

Every upsert is logically:

```text
delete(document_id)
add(latest document version)
```

Repeated application of the same mutation range must leave one live logical document. Search-time duplicate detection is diagnostic protection, not the primary correctness mechanism.

## 7. Ranking boundary

Tantivy score retrieves a candidate window; it does not define final product order. Each candidate carries enough information to classify exact, prefix, token, verified substring, or path match deterministically.

## 8. Schema acceptance

A schema version is accepted only with a report containing:

- field-by-field index amplification;
- indexing throughput;
- peak/steady memory;
- candidate counts and false-candidate ratios;
- warm/cold latency distributions;
- representative Unicode and Windows filename correctness;
- migration/rebuild estimate.
