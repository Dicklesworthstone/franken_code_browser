# Incremental saved-index refresh

Implementation and regression tests are committed. Independent Rust compilation
and strict-RCH execution are pending; no product gate is qualified here.

## Refresh a changed saved workspace

```sh
fcb snapshot index refresh after.fcbs \
  --base before.fcbs \
  --index before.fcbi --index-digest "$BEFORE_INDEX_DIGEST" \
  --output after.fcbi --json
```

`BEFORE_INDEX_DIGEST` must be retained from the original trusted index build,
not computed from an untrusted file merely to pass validation. The prior index
is checked against that digest and its selected base snapshot before any reuse.
Refresh creates a **new** index file and returns a new `index_digest`. Search the
new snapshot using that returned value:

```sh
fcb snapshot index search after.fcbs \
  --index after.fcbi --index-digest "$AFTER_INDEX_DIGEST" \
  --text 'pub fn' --json
```

This consumes two already saved observations. It does not recapture or inspect
live source roots, run Git, infer changes from timestamps, execute source content,
or update a background service. Existing ordinary build/inspect/search commands
continue to use the same public indexing and matching engines.

## What is reused

A complete prior segment is eligible only when the target member has the same
SHA-256 source digest, original-byte length, and the index format's matching
semantics. Raw native path, ordinal, file ID and mtime are not content proofs.
An equal-length edit therefore cannot inherit its old negative certificates.

A moved or duplicated file can reuse identical-content keys without loading its
payload. This is **not rename detection**: target paths, membership and process
identities still come exclusively from the target snapshot. Reuse copies the
complete gram set into the new generation, not the old source identity.

New or changed content uses the existing digest-verifying member loader and
`EphemeralIndex` builder. A previously uncovered member can become indexed.
UTF-16 and malformed sources retain the correct raw/text coverage distinction;
text queries still fall back to their decoder where raw grams are incompatible.
The existing exact verifier, overlapping hits, short-query fallback, result-limit
lookahead and original-byte hit activation remain in charge of query results.

## Avoid rereading either archive body

Without catalog options both archives undergo ordinary full validation. Reuse
still avoids subsequent loads and segment reconstruction, but does not remove
those initial scans. Supply independent trusted catalogs to avoid them:

```sh
fcb snapshot index refresh after.fcbs \
  --base before.fcbs \
  --base-catalog before.fcbc --base-catalog-digest "$BEFORE_CATALOG_DIGEST" \
  --catalog after.fcbc --catalog-digest "$AFTER_CATALOG_DIGEST" \
  --index before.fcbi --index-digest "$BEFORE_INDEX_DIGEST" \
  --output after.fcbi --json
```

Each pinned archive open reads 56 boundary bytes plus an EOF probe, as described
in `SNAPSHOT_CATALOGS.md`. Catalog and old-index files are still read in full.
Selected new members are hashed before publication. Unread backing-file damage
remains unchecked: the result describes trusted saved observations, not a fresh
health check of all archive bytes. A bad pin fails rather than silently rebuilding
or treating an empty candidate set as complete. A fresh explicit build remains
available without trusting an old index.

The receipt reports `base_archive_validation_bytes` separately from target
`archive_validation_bytes`, and both body-validation flags. `reused_source_bytes`
is the logical size of reused members (including each duplicate), NOT a measured
disk-byte or elapsed-time saving. `member_payload_bytes_loaded`, `loaded_members`,
`new_segment_source_bytes_loaded`, and `index_build_source_bytes` report actual
construction activity separately. `reused_files`, `reused_grams`, `attempted_files`,
`rebuilt_files`, and `reuse_quota_refusals` explain the resulting coverage.

## Limits and partial results

Refresh accepts the existing build options `--max-grams`, `--max-file-bytes`, and
`--max-source-bytes`. The first two bound the new retained generation. The source
budget bounds newly loaded construction input, so `--max-source-bytes 0` can still
reuse complete old segments. The public API similarly treats scratch as a fresh
construction limit rather than requiring scratch to copy existing keys.

Target members are processed deterministically in raw-path order. A reusable
segment is copied whole or refused; it is never truncated into an unsound
negative certificate. Per-file or generation quota refusal leaves an uncovered
row eligible for exact scan fallback. Missing target captures stay unavailable;
old captures are never substituted for them. Omitted old members are not injected
into the requested new scope. Incomplete target discovery remains incomplete.

A refresh can return exit 3 for useful partial index coverage while a later query
returns a complete result by scanning uncovered members. Exit 0 does not certify
live-root freshness or native performance. Source/archive/gram size ceilings are
unchanged. The 256 MiB CLI guard charges old and new retained segments, reuse
lookup scratch, member loads, engine scratch and encoded output independently.
No shared mutable gram storage or uncharged source payload escapes are introduced.

## Publication and host API

The old index is immutable during construction and remains usable after refusal,
cancellation, or a failed target load. The new candidate owns its keys; dropping
the prior index does not invalidate it. Fresh builds and refreshes share one
construction path. Encoding remains the existing FCBI/1 format, with no external
references to old files. With sufficient quotas, an incremental and fresh index
for the same target encode identically.

`fcb::search::snapshot_index::SnapshotIndex::refresh` returns `SnapshotRefresh`,
which exposes the new `SnapshotIndex` and `RefreshStats`. Hosts supply five
distinct allocation identities: new index, member load, capture copy, indexing
engine and temporary reuse lookup. Library calls perform no pathname lookup,
filesystem publication, runtime construction or background scheduling.

CLI publication uses the existing exclusive, no-overwrite, effect-aware writer.
The base snapshot, target snapshot and prior index are never modified. Interrupted
new outputs remain explicitly reported; lost response delivery cannot undo a
completed file. Retain the new artifact digest before using it after reopen.
There is no automatic active-generation manifest switch, atomic database commit,
old-generation garbage collection, or power-loss qualification in this command.

Regression suites cover fresh-build wire equality, direct-query equivalence,
same-length edits, changed encodings, moved/duplicate content, unavailable members,
reduced quotas, canceled/failed replacement, independent old/new leases, pinned
catalogs, non-reading guards, read-only inputs, publication effects and the actual
binary. The guarded reader rejects every reused-payload read, independently of
reported counters. Committed tests are not execution receipts.
