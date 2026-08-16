# Query Engine

Status: v0.1 baseline

## 1. Boundary

The query engine accepts product requests and produces a backend-neutral plan. Tantivy's query parser is not exposed to UI, IPC, saved settings, or canonical tests.

## 2. Request model

```text
SearchRequest
  request_id
  query_text
  scope: all | files | folders
  filters
  top_k
  deadline
  cursor?
  query_language_version
```

`top_k` and every expansion limit are server-clamped. The caller cannot request an unbounded result set.

## 3. AST

The initial typed AST supports only operations required by the implemented UI:

```text
MatchName
MatchPath
Exact
Prefix
Substring
Fuzzy
TermFilter
RangeFilter
Bool { must, should, must_not, filter }
```

Wildcard, regex, phrase, and content clauses remain reserved until implemented with explicit cost rules. Unknown syntax returns a typed parse/capability error.

## 4. Planning

Planning performs:

1. parsing and normalization;
2. field/scope selection;
3. query-length policy;
4. cost estimation;
5. expansion and candidate caps;
6. backend capability validation;
7. creation of retrieval and post-verification steps.

A plan is inspectable in diagnostics without exposing filenames or query text unless the user explicitly enables sensitive diagnostics.

## 5. Cancellation and deadlines

Cancellation is end-to-end at request boundaries. Blocking backend operations may not be instantly interruptible, so candidate caps and small units of work bound their cost. Results are tagged with `request_id`; the UI discards a response that is not current.

The agent distinguishes cancellation, deadline exceeded, policy rejection, backend failure, and unavailable index.

## 6. Cost policy

Cost includes normalized query length, number of clauses, fuzzy edit distance, expected term expansion, candidate window, requested filters, and cold/warm state where observable.

Initial constraints:

- no substring for fewer than three normalized characters;
- fuzzy distance and expansions are capped by length;
- no leading wildcard or arbitrary regex in v0.1;
- bounded boolean clause count;
- bounded candidate window and final `top_k`;
- deadline checked between plan/retrieval/ranking phases.

## 7. Product ranking

The ranker assigns a stable primary class:

```text
ExactName
PrefixName
TokenName
SubstringName
Path
```

Primary class dominates secondary scoring. Secondary factors may include lexical score, recency, file type, path depth, directory proximity, and availability. Every factor is versioned and benchmarked; personalized behavior is deferred.

Tie-breaking is deterministic to prevent result flicker. At minimum it includes normalized name/path and stable document identity after score factors.

## 8. Contract tests

Backends and schema versions must pass shared cases for:

- match classification and ordering;
- Unicode/case normalization;
- filters and scopes;
- expensive-query rejection;
- deterministic ties;
- stale/cancelled request handling;
- missing/unavailable current path;
- capability negotiation.
