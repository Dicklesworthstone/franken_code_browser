# Reusable captured repository indexes

`include/fcb_atlas_index.h` connects the production ephemeral index to existing
atlas handles. It is an explicit alternative to live-capture search: prepare
once, then issue many queries over the same retained source universe. No database,
new matcher, source walker, runtime, thread or independent handle registry is added.

## Host operation

Open an atlas normally. On a host-owned worker, call `fcb_atlas_index_prepare`
with a fresh search-attempt generation and explicit capture/gram limits. It
captures the atlas's existing eligible catalog and moves the prepared engine's
segments into an owning index. Every successfully captured member is retained,
including files with no match for a previous query and gram-quota refusals.
Source buffers are shared with independent admission, not copied or rehashed
when the index acquires ownership. Initial capture/index construction is still
real source work and can examine many files in one synchronous worker call.

Call `fcb_atlas_search_indexed` with a fresh query generation and the accepted
`index_generation`. The query is a case-sensitive decoded literal, not field or
regular-expression syntax. Warm queries make no source-path calls and never
rebuild the posting arrays. They create bounded temporary document descriptors
and use the existing exact verifier. UTF-16, short queries and uncovered or
incompatible segments take the shared scan fallback. A prefilter can only rule
out compatible source; it cannot manufacture a positive hit or hide an uncovered
file. The original source revision stays attached to every returned hit.

Use the existing `fcb_atlas_search_page`, `overlay`, `focus` and `open_reader`
entrypoints afterward. Opening a hit uses its retained whole-file capture even
after a live edit, rename, index replacement or index clear. Existing destination
reader admission applies; an already-open reader cannot be overwritten. Readers
can subsequently use ordinary source windows, search, outlines and Markdown
operations without returning to the live path. Camera focus only prepares a
plan: the host must still acknowledge the frame it actually presented.

## Identity, replacement and clear

Live queries, indexed queries, index preparation and both result/index clear
operations share one strictly increasing search-attempt sequence. A newly
admitted attempt supersedes paused live search; failures consume their number.
Index preparation uses that number for its new source observation, so it cannot
reuse a direct-scan source revision. `capture_manifest` is separately allocated
above the frozen catalog revision. The legacy `source_manifest` wire field still
identifies that catalog. Filename-query and camera-plan generations remain
independent.

A successful index replacement does not replace already accepted query rows.
Those rows keep their own source pins until a successful query replacement or
result clear. Failed/canceled preparation preserves the older index and rows;
failed/canceled queries preserve their preceding accepted rows. A wrong index
number is rejected, never automatically redirected to another source universe.
`fcb_atlas_index_info` reports the accepted preparation for reconciliation.

`fcb_atlas_search_clear` clears result snapshots but leaves the reusable index.
`fcb_atlas_index_clear` releases index/source ownership but preserves accepted
query captures. Closing the atlas releases its owned state, not independently
opened readers or returned strings. Nonblocking operation locks and retiring
capacity use the existing registry. Cancellation after publication may suppress
a response without undoing publication: reconcile index info or the known query
page. Destruction and all source/index work belong on a worker.

## Coverage, resources and proof boundary

Preparation reports captured, unavailable, pending, indexed and uncovered file
counts independently. A file never examined because of a capture limit leaves
membership open, not a complete negative result. A gram quota refusal retains its
source for fallback; partial index coverage need not prevent a later complete
exact query. Results separately report search completeness, truncation, index
skips, verification/fallback attempts and captured verification bytes. Disk-read
counters on indexed queries are zero; they are not mislabeled verification costs.

The catalog remains frozen: a new file is not discovered by index refresh.
Reopen/reconcile the atlas for changed membership. Per-file observations are not
an atomic filesystem snapshot, Git history or watcher qualification. This route
is an in-memory bounded index, not persistent/out-of-core search. Preparation is
at most 4096 examined files, 1 MiB per capture and 32 MiB actual source reads;
the gram buffer is at most 2097152 entries. Queries retain at most 4096 hits and
admit at most 32 MiB verification bytes. Source, prepared segments, candidate
source overlap, query results and handoff responses are separately reserved.
Conservative managed reservations are not total process RSS measurements.

This engine traverses file metadata and probes per-file trigrams; it is not a
global inverted index. Indexed queries are currently synchronous worker calls,
not resumable across native calls. The existing one-file live begin/step route
remains available. No speedup, UI responsiveness, native Metal presentation or
physical-Mac result is inferred from implementation or counters.

Regression suites: `fcb-search/tests/owned_index.rs`,
`fcb-app/tests/retained_atlas_index.rs`, and the bridge's index registry/FFI tests.
These changes are code-first, batch verification pending. Compilation, test
execution and strict RCH have not run in the authoring environment. Native UI
wiring and hardware qualification remain outstanding; no bead or product gate
is declared complete.
