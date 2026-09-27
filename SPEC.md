# apg-cache-config-key — fold the classification-config identity into the scan cache key

> High-level change-set definition. The durable spec lives in `apg/layers/` (authored
> through `apg_node`/`apg_edge`); this file is the work brief, not the graph.

## Goal
Include the graph-affecting **classification-config identity** of `apg/config.json`
(its `default`, ordered `types` rules, and `structural` scope) in the global scan
**cache key**, so a classification change forces a **full load** instead of silently
reusing cached `code_type` values.

## Why
`code_type` is computed at ingest from a record's `path` + `apg/config.json`, and only
a **full load** reclassifies. The cache key currently folds binary version + JSONL
schema + ingestor projection rules + the scan config (languages / excludes / modules)
— but **not** the classification config. A `apg/config.json` edit can therefore leave
stale classifications in the incremental / splice path. This is a correctness bug in
the released binary.

## Status / provenance
- The fix previously landed at `cbb2cd69` ("fold classification config into the scan
  cache key"), then was **reverted** by `d68c1c07` as out-of-scope for the apg-cleanup
  refactor-only change-set ("handed to a separate project"). Recover the patch from
  `cbb2cd69`.
- Adapt it to the post-decomposition layout: `src/cache.rs` → `src/cache/manifest.rs`;
  `cmd_scan` moved `src/lib.rs` → `src/scan.rs`.

## Scope
- Add `classification: String` to `ScanConfigKey`, plus `classification_digest` /
  `classification_render` (a stable digest of `default` + ordered `types` + `structural`;
  excludes the binary-managed `version`; a distinct sentinel for "no config").
- Fold the digest into `CacheKey::compute`.
- Compute it in `cmd_scan` (`src/scan.rs`) from the loaded `ApgConfig`.
- Update every `ScanConfigKey` construction / use site: `src/cache/manifest.rs`,
  `src/scan.rs`, `src/delta.rs`, `src/incremental/prepare.rs`, `src/warm.rs`.
- Update / extend tests: `src/cache/tests.rs` (the
  `cache_key_folds_the_classification_config` test from `cbb2cd69`), `src/delta/tests.rs`,
  `tests/{cache,delta,incremental}_e2e.rs`.
- Reconcile the durable spec: add / reconcile a `domain.value.cache-key` node describing
  the classification-config component (currently unmaterialized).

## Non-goals
- No scan-behaviour change beyond cache invalidation.
- No unrelated cache / manifest changes.

## Acceptance
- Equal configs → equal keys; each graph-affecting field (`default`; `types` globs,
  names, and rule **order**; `structural` include / exclude / code_type; absence vs
  presence) moves the key; languages / excludes / modules still move it independently.
- `cargo test` (unit+int) green; `cache` / `delta` / `incremental` e2e green.
- `scripts/gate.sh` green; `apg plan verify` green; merge.

## Open questions
- Sentinel semantics for "no `apg/config.json`" (must be distinct and stable) — mirror
  `cbb2cd69`.
