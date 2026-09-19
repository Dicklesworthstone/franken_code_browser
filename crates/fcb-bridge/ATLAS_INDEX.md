# Reusable captured repository indexes

`include/fcb_atlas_index.h` connects the production ephemeral index to existing
atlas handles. Both preparation and queries can yield between files. No database,
new matcher, source walker, runtime, thread or independent registry is added.
Native UI integration, scheduling and hardware qualification remain separate.

## Incremental preparation

Open an atlas normally. On a host-owned worker, call `fcb_atlas_index_begin`
with a fresh search-attempt generation and explicit capture/gram limits. This
admits a private replacement without source I/O or segment construction. Continue
with `fcb_atlas_index_step(handle, generation)`, not `fcb_atlas_search_step`.
Each successful step examines at most one catalog member and builds its segment
using the existing capture reader and production EphemeralIndex builder.
Unavailable members are recorded separately. Gram-quota refusals retain source
for exact-scan fallback rather than dropping the member from search membership.

The source backing is Arc-shared without copying or rehashing during retention.
A completed per-file gram segment is copied once into an admitted aggregate;
earlier segments are neither rebuilt nor reallocated by subsequent steps.
Finalization moves already-prepared storage. It does not perform a hidden
whole-repository scan, descriptor-array rebuild, or gram reconstruction.
`fcb_atlas_index_prepare` remains available as a synchronous worker call that
runs this same preparation pipeline to completion without yielding.

Use `index_build_in_progress`, not `capture_complete`, to continue preparation.
A terminal build may have pending or unavailable members, or uncovered segments.
Per-step files/read bytes/read calls and cumulative counts are explicit.
`fcb_atlas_index_progress(handle, generation)` reconciles accepted provisional
progress without advancing it. A lost nonterminal reply is not replayed by
another step: that advances the next member. Completed-step retries are read-only.
Wrong generations never advance or remove a newer candidate.

The old accepted index remains queryable between replacement steps. Begin only
supersedes an unfinished query at admission; later queries can run on the old
index while preparation continues. Only after preparing the complete terminal
response is the new index installed. Failure or cancellation keeps the old index
and completed results. A successful replacement retires an unfinished indexed
query tied to the old engine, not a pending live query. Completed hit captures
and independently opened readers continue to name their old exact source.

## Captured queries and early results

`fcb_atlas_search_indexed` finishes a query in one worker call. The progressive
`fcb_atlas_search_indexed_begin` instead admits the same cursor without verifying
source. Both take the accepted index generation, a fresh query generation, a
case-sensitive decoded literal, maximum hits and a query-wide verification-byte
allowance. The literal is not field or regular-expression syntax. Begin copies
its foreign query text, so the caller can release that string after return.

Continue with the EXISTING `fcb_atlas_search_step(handle, generation)`. Each step
visits at most one captured file without source-path I/O, rebuilding grams, or
reconstructing the repository descriptor array. UTF-16, short queries and
uncovered/incompatible segments keep the existing exact-verification fallback.
An index-negative certificate cannot create a positive result or hide a member
that requires fallback.

Use `search_in_progress`, not `search_complete`, to continue queries. Verification
bytes and unstored lookahead remain query-wide, not reset per step. A full hit
buffer is not proof another match exists. Unavailable diagnostics are counted
once. A terminal query can still be incomplete because of membership or limits.

Normal `search_page`, `overlay`, `focus` and `open_reader` operations work on early
results. Hits append with stable query-local IDs and original source revisions.
`next_offset: null` means end of the current rows, not a finished running query.
Opening a hit uses the whole retained capture after live changes or renames.
Subsequent query/index replacement, clear or atlas closure cannot change a reader
already delivered. Reader destination admission is checked before activation;
an already-open reader cannot be overwritten. Camera focus prepares a plan
without falsely acknowledging it as presented.

## Ownership, cancellation and independent state

Preparation, queries and clear operations share a non-reusing increasing attempt
sequence. A new query supersedes the pending query, not a replacement builder.
A new index preparation or index-clear supersedes a builder and pending queries.
Thus at most one builder and one query can coexist, each separately admitted.
Result-clear preserves the reusable index and builder. Index-clear preserves
completed result/source pins and readers. The legacy `source_manifest` field
names the frozen catalog; `capture_manifest` names the indexed source universe.
Filename-query and camera-plan generations remain independent.

The existing `fcb_atlas_cancel` invalidates BOTH paused query and construction
work. The next worker operation retires them before progress/page/activation or
resume; an old builder cannot restart in a fresh cancellation epoch. Reader-only
cancellation leaves shared atlas work intact. The safe host also exposes explicit
`cancel_index_build` for retiring only its construction candidate on a worker.

Close removes authority immediately; source/index destruction and retiring-slot
admission keep the existing registry lifetime rules. Cancellation after terminal
acceptance may suppress its response but is not rollback: reconcile accepted
index info, known build progress or query page. Already-returned JSON strings
remain independently owned and must be freed exactly once with fcb_free_string.

## Scope, limits and verification

The catalog stays frozen: rebuilding an index does not discover new filenames.
Reopen/reconcile the atlas for changed membership. Per-file observations are not
an atomic filesystem snapshot, Git history or watcher qualification. This is a
bounded in-memory per-file trigram index, not persistent/out-of-core storage or
a global inverted index. Counters are not a measured speedup or RSS measurement.

Preparation admits at most 4096 files, 1 MiB per capture and 32 MiB actual source
reads; failed reads consume that allowance. The aggregate gram limit is 2097152.
Queries retain at most 4096 hits and admit at most 32 MiB verification bytes.
Source pins, old/new overlap, aggregate segments, per-file scratch, query output
and response buffers are separately reserved. A construction step includes
bounded synchronous per-file sorting and OS calls, not an elapsed-time deadline.
There is no hidden worker scheduling or claim that these APIs render a native UI.

New tests: `fcb-search/tests/incremental_index_build.rs`,
`fcb-app/tests/progressive_index_build.rs`, and the bridge's
`atlas_index_build_sessions_tests.rs` / `atlas_index_build_ffi_tests.rs`.
They exercise real engine segments, capture/query/reader workflows, one-file
preparation, old-index availability, cancellation, failure, limits and ownership.
Existing owned/borrowed index, progressive query, retained-reader and registry
suites also require independent verification after this shared-path change.

Code-first, batch verification pending. Compilation, test execution, strict RCH
and native hardware qualification have not run in the authoring environment.
No bead or product gate is declared complete and no tests are claimed passing.
