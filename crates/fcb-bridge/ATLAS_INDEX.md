# Reusable captured repository indexes

`include/fcb_atlas_index.h` connects the production ephemeral index to existing
atlas handles. Prepare once, then issue queries over the same retained source
universe. No database, new matcher, source walker, runtime, thread or independent
handle registry is added. Native UI integration and qualification remain separate.

## Prepare and query

Open an atlas normally. On a host-owned worker, call `fcb_atlas_index_prepare`
with a fresh search-attempt generation and explicit capture/gram limits. It
captures the existing eligible catalog and transfers the prepared engine into
an owning index. Every successfully captured member is retained, including
previous nonmatches and gram-quota refusals. Source buffers are shared with
independent admission, not copied or rehashed during owning transfer.

Initial capture/index construction remains a synchronous multi-file worker
operation. This change makes indexed queries resumable, not index preparation.

`fcb_atlas_search_indexed` finishes a query on one worker call. The new
`fcb_atlas_search_indexed_begin` instead admits the same query cursor without
verifying source. Both take the accepted `index_generation`, a fresh query
`generation`, a case-sensitive decoded literal, maximum hits and a global
captured-verification-byte allowance. The literal is not parsed as field or
regular-expression syntax. Begin copies its foreign query text, so that string
can be released immediately after the call.

Continue with the EXISTING `fcb_atlas_search_step(handle, generation)` on a host
worker. Each step visits at most one captured file, including index-negative
files. It performs no filesystem I/O, gram reconstruction, source copying or
repository-wide descriptor rebuild. Admission inspects bounded file metadata
once; the cursor then retains its next ordinal, results and global work limits.
UTF-16, short queries and uncovered/incompatible segments keep the existing
exact-scan fallback. Both ownership forms use the same segment probes and exact
verifier. A negative prefilter cannot create a positive result or hide a source
that needs fallback.

## Early results and continuation

Use `search_in_progress`, not `search_complete`, to decide whether to continue.
A terminal query can be incomplete because membership is open, source was
unavailable, or a limit stopped verification. Query-wide byte budgets and the
unstored lookahead match apply across all steps, rather than resetting per call.
A full hit buffer alone does not establish another match exists.

The ordinary `fcb_atlas_search_page`, `overlay`, `focus` and `open_reader` calls
work on partial progress. Hits append in source-member/occurrence order with
stable query-local IDs and original source revisions. `next_offset: null` only
means the current page reached the current result end: more steps can add rows.
Diagnostics for an unavailable source are counted once, not once per delivery.

Opening an early hit uses its whole retained capture even after a live edit or
rename. Later query cancellation, index replacement or atlas closure cannot
change an independently delivered reader. That reader can use source windows,
search, outlines and Markdown through the ordinary reader APIs. Destination
admission still precedes activation; already-open readers cannot be overwritten.
Camera focus prepares a plan without acknowledging it as presented.

Only a completely prepared response publishes a step. After a lost nonterminal
response, page the known query to reconcile accepted progress; another step
advances it rather than replaying the prior step. Repeating a terminal step does
not repeat verification. Counters distinguish per-step admitted files, cumulative
verification, skipped files, fallback work and actual filesystem reads (zero).
These counters are not a measured speedup or a fixed-duration callback guarantee.

## Identity, replacement and cancellation

Live queries, indexed queries, index preparation and both clear operations
share one strictly increasing search-attempt sequence. There is ONE pending
query slot for either execution route. A newly admitted attempt supersedes it,
even when the replacement subsequently fails. The last finished result snapshot
remains available under its own generation until successful query replacement.

Cursors bind to an actual private owned-index identity, not just equal manifest
numbers. Index preparation still allocates a capture manifest distinct from the
atlas catalog and assigns fresh source observations. The legacy `source_manifest`
wire field denotes that frozen catalog; `capture_manifest` denotes indexed source.
Filename-query and camera-plan generations remain independent.

Canceling the atlas while a query is paused changes the existing operation epoch.
The next worker call retires provisional work before page, activation or resume;
an old query cannot restart under the new epoch. Canceling a reader alone leaves
other readers and their shared indexed query intact. Close/cancel/try-lock and
retiring-capacity rules use the existing registry, not another execution system.

Index replacement or clear supersedes paused queries but preserves accepted
result source pins. Result clear preserves the reusable index. Failed/canceled
preparation preserves its predecessor. Cancellation after terminal acceptance
can suppress a response without undoing acceptance: reconcile index info or the
known result page. Final destruction of captures/layouts/indexes is worker work.

## Scope and verification boundary

The catalog remains frozen: index refresh does not discover new filenames.
Reopen/reconcile the atlas for changed membership. Per-file observations are not
an atomic filesystem snapshot, Git history or watcher qualification. The index
is bounded in-memory storage, not persistent/out-of-core search or a global
inverted index. Query steps bound source members, not elapsed time: a single
admitted file can require several exact-scanner quanta.

Preparation admits at most 4096 files, 1 MiB per capture and 32 MiB actual source
reads; the gram buffer is at most 2097152 entries. Queries retain at most 4096
hits and admit at most 32 MiB verification bytes. Source pins, index storage,
query AST capacities, unavailable IDs, result overlap and returned responses
retain separate reservations. Managed limits are not process RSS measurements.

New regressions are in `fcb-search/tests/movable_indexed_query.rs`,
`fcb-app/tests/progressive_indexed_search.rs`, and the bridge's
`atlas_index_progressive_tests.rs`. Existing index C contract tests now include
the begin entrypoint. These exercise actual prepared segments, verifier results,
early activation, global budgets, cancellation, reader independence and handle
lifetimes; native pixels and physical-Mac behavior are not simulated as proof.

These changes are code-first, batch verification pending. Compilation, test
execution and strict RCH have not run in the authoring environment. Existing
borrowed-index, owned-index, retained-atlas-index and progressive-live-search
suites must also be included in independent verification. No bead or product
gate is declared complete.
