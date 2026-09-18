# Captured repository search through an existing atlas

`include/fcb_atlas_search.h` adds a retained search-to-atlas-to-reader workflow.
It uses `fcb_app::host::atlas_search::RetainedAtlasSearch`, the existing workspace
capture operation, and the existing exact decoded-text scanner. No second walker,
matcher, decoder, native renderer or runtime is introduced.

## Host sequence

On a host-owned worker, create and open an atlas using `fcb_atlas_create` and
`fcb_atlas_open`. Opening remains metadata-only. Then:

```c
char *json = fcb_atlas_search(atlas, 1, "needle", 1000, 4096,
                              1024 * 1024, 32 * 1024 * 1024);
/* Parse status and search_complete, retain generation/IDs, then free json. */
fcb_free_string(json);

json = fcb_atlas_search_page(atlas, 1, 64, 128);
/* Use next_offset from the preceding response, not this example's constant. */
fcb_free_string(json);

json = fcb_atlas_search_overlay(atlas, 1);
/* Apply matching-file counts only to the matching atlas layout revision. */
fcb_free_string(json);

json = fcb_atlas_search_focus(atlas, 1, selected_hit_id, next_plan_generation);
/* This is a candidate plan. Present it, then use fcb_atlas_present. */
fcb_free_string(json);

uint64_t reader = fcb_reader_create();
json = fcb_atlas_search_open_reader(atlas, reader, 1, selected_hit_id);
/* On success: original_range identifies the exact selection; window is context.
   The entire matching source is now retained in reader. It was NOT reopened. */
fcb_free_string(json);
```

Every non-null response must be inspected for errors before using its fields.
The abbreviated example omits error handling; a zero reader handle is refusal.
`selected_hit_id` comes from the published query's `hit_id`, not a visual row
index. Query generations and camera-plan generations are independent monotone
sequences. Even failed admitted query attempts consume their generation.

All existing `fcb_reader_*` calls work on the resulting reader, including byte
windows, line navigation, find-in-file and exact byte copying. It is independently
owned: clearing or closing the atlas does not close the reader. The caller must
close it separately with `fcb_reader_close`.

## Captures, scope, and identity

Each search reads only the frozen atlas catalog's eligible regular files, under
that catalog's existing root grant and application source policy. It does not
repeat discovery or broaden exclusions. A newly added file needs a newly opened
atlas/catalog. Files changed since metadata discovery are observed when the
query captures them; this is not a cross-file atomic snapshot.

The whole byte capture for each matching file is retained, not merely the UTF-8
needle or a tiny preview. Paging, overlay production, result focus and reader
activation never open source paths. Subsequent disk edits, renames, disappearance,
or same-size replacements therefore cannot alter an accepted hit's bytes.

`open-reader` returns the original atlas owner, file, source revision, query and
original-byte range alongside the newly owned reader's identities. These are an
explicit provenance link, not a claim that the two ownership domains are equal.
The returned logical context window uses the existing decoder, not native shaping.

The safe application object rejects another atlas with a different grant token,
even when a caller accidentally supplies identical public ID counters. Revoked
grants invalidate new result/reader delivery. Already returned bytes cannot be
retracted from an independently authorized host or reader.

## Publication and resource limits

Replacement work is private until both its retained results and complete first-page
JSON are ready. Failure or cancellation preserves the old accepted rows AND their
captures under the OLD query generation. A successful partial result may replace
them, but is explicitly incomplete. After replacement, the old generation is
rejected. `fcb_atlas_search_clear` consumes a fresh generation and releases the
accepted search captures without touching previously opened readers.

Source reads, failed-read work, examined files, per-file capture size, retained
hits, diagnostic count and output are bounded. Limits are documented in the header.
Unmatched captured files are discarded; only matching full captures remain.
Old/new overlap is charged during replacement. The atlas and search have separate
managed budgets; their limits are not claims about total process memory. Reader
copies and already returned native strings have their own lifetimes and costs.

`search_complete`, `discovery_complete`, `pending_files`, `unavailable_files`,
`matches_seen`, `retained_hits`, `truncated` and `next_offset` are distinct. A next
page is not incomplete search coverage. The sparse overlay has one row per
matching file and counts only retained occurrences, never an invented exhaustive
total after result truncation. Unknown/unread/undecodable files are not zero hits.

Calls are synchronous worker operations. Per-atlas contention returns BUSY rather
than waiting on source work, and the global handle-table lock is not held during
capture/search. Keep the last presented scene while work is pending or BUSY.
`fcb_atlas_cancel` invalidates active work without taking its operation lock. It
cannot impose a hard deadline on an operating-system filesystem call. If cancellation
arrives after acceptance, it suppresses delivery but cannot undo accepted state:
query paging with the attempted generation reconciles search acceptance; reader
info/close reconciles installation into the caller's known destination handle.

## Verification boundary

Production-path tests are in `crates/fcb-app/tests/retained_atlas_search.rs`,
`src/atlas_search_sessions_tests.rs`, and `src/atlas_search_ffi_tests.rs`. They cover
actual filesystem capture, UTF-16 overlaps, byte/source limits, unavailable files,
raw paths, stale and canceled replacement, root grants, presentation preservation,
reader destination protection, handle isolation and retiring capacity.

For the configured independent verifier:

```sh
RCH_REQUIRE_REMOTE=1 rch exec -- cargo test -p fcb-app --test retained_atlas_search
RCH_REQUIRE_REMOTE=1 rch exec -- cargo test -p fcb-bridge
```

These tests were added code-first and have not been compiled or run in the authoring
environment. The implementation is a bounded exact direct-scan host route, not a
persistent index, progressive in-query result stream, full native UI, Metal
presentation or physical-Mac qualification. The inherited pathname checking route
does not establish race-safe confinement against hostile ancestor replacement.
