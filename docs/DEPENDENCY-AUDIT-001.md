# DEPENDENCY-AUDIT-001: source, license, and advisory policy

Status: **policy and local audit PASS; informational advisory debt remains tracked**

Review date: 2026-08-16

The release dependency gate has two independent checks. `packaging/test-dependency-policy.mjs`
loads locked Cargo metadata, rejects more than 1,000 packages, rejects non-workspace path and
non-crates.io sources, rejects missing license declarations, and admits only the exact SPDX/legacy
expressions reviewed in this repository. Any new expression therefore requires an explicit policy
review instead of silently widening the license set.

RustSec runs against the checked-in `Cargo.lock` with a fresh, non-stale advisory database and a
low severity threshold. Vulnerabilities and yanked packages fail the audit. Informational
`unmaintained` and `unsound` notices remain visible for triage rather than being placed on a hidden
ignore list. The same checks run for dependency changes and weekly in
`.github/workflows/security.yml`.

## Local evidence

```text
cargo-audit:                    0.22.2
advisory records loaded:        1216
locked dependency packages:     636
vulnerability advisories:       0
informational warnings:         16
missing license declarations:   0
unapproved dependency sources:  0
license/source policy:           PASS
```

The 16 informational warnings are not represented as a clean security result:

- eight unmaintained GTK3 bindings and the `glib 0.18.5` soundness notice are transitive Tauri
  dependencies for non-Windows targets and are not linked into the Windows release artifact;
- `proc-macro-error` and five `unic-*` packages are unmaintained transitive build/parser
  dependencies, with no vulnerability advisory in this audit;
- `lru 0.16.4` is pulled by the latest available `tantivy 0.26.1`. The advisory requires
  `LruCache::pop()` plus a key whose `Drop` panics during caught unwinding. Tantivy's only use is a
  `LruCache<usize, Block>` and it calls `get`, `put`, `len`, and test-only `peek_lru`; the key is
  `usize`. The patched `lru >= 0.18.2` is outside Tantivy's current semver requirement.

This reachability assessment is a temporary release exception, not a permanent suppression. The
weekly job will continue surfacing it, and the exception must be removed when Tantivy accepts a
patched `lru` line or an equivalent upstream fix.
