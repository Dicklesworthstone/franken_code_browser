# Captured repository search through an existing atlas

`include/fcb_atlas_search.h` exposes retained search-to-atlas-to-reader workflows.
Both synchronous and resumable queries use `RetainedAtlasSearch`, the existing
workspace capture operation and the existing exact decoded-text scanner. There is
no second walker, matcher, decoder, native renderer, scheduler or runtime.

## Host sequence

On a host-owned worker, create/open an atlas using `fcb_atlas_create` and
`fcb_atlas_open`. Opening remains metadata-only. A synchronous search uses:

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
/* Candidate plan only. Display it, then use fcb_atlas_present. */
fcb_free_string(json);

uint64_t reader = fcb_reader_create();
json = fcb_atlas_search_open_reader(atlas, reader, 1, selected_hit_id);
/* original_range is the exact selection; window is decoded context.
   The entire source is retained in reader. It was NOT reopened. */
fcb_free_string(json);
```

Every non-null response must be checked for errors. The abbreviated example omits
error handling; a zero reader handle is refusal. `selected_hit_id` comes from the
published query's `hit_id`, not its visual row. Query generations and camera-plan
generations are independent monotone sequences. Failed admitted query attempts
consume their generation; pre-admission C argument rejection is not acceptance.

All `fcb_reader_*` calls work on the resulting reader. It is independently owned:
clearing/closing the atlas does not close it. Close it with `fcb_reader_close`.

## Resumable search and early results

Use `fcb_atlas_search_begin` with the same arguments instead of the synchronous
search call. Begin admits capacities and owns the bounded query text but performs
no source read. Then schedule `fcb_atlas_search_step(atlas, generation)` on the
host worker. Each call examines at most one catalog entry, with a per-file capture
cap of 1 MiB (or the requested smaller limit). The same existing source reader and
chunked exact-text scanner perform that file's work; no provider is bypassed.

Return control to the host after each step. Camera operations, result paging,
overlays and reader activation can run between steps instead of waiting for an
entire repository scan. This is bounded file-at-a-time scheduling, not a claimed
wall-clock deadline for an OS call, byte-granular preemption or automatic execution
on a newly created worker.

The response includes `search_in_progress`, `stop_reason`, `step_count`,
`last_step_files`, `last_step_source_bytes` and `last_step_read_calls`.
Continue only while `search_in_progress` is true. **Do not use search_complete
as a continuation flag:** a terminal query with unavailable or quota-limited
coverage remains incomplete. Stop reasons distinguish exhausted catalog membership,
file quota, source-read quota and match quota; discovery may itself be partial.

Running-query hits are already exact retained-source occurrences. They append in
catalog order without changing IDs. Page/overlay/focus/open-reader work for that
generation immediately. `next_offset=null` means the end of currently retained
rows; another step can append more. Keep `(atlas, generation, hit_id)` together.
Opening a reader from partial progress retains the complete matching file, not a
fragment of its match or a promise to reread it later.

At most one running replacement and one finished query coexist. The old finished
query remains readable under its own generation until the replacement terminates
successfully (possibly with explicitly partial coverage). A new admitted attempt
supersedes old pending work even if its own admission fails. A failed/canceled
step retires its provisional progress and leaves the finished query unchanged.
Hosts displaying provisional rows must discard them on that error and can revert
to the prior finished generation. Independently delivered readers survive.

`fcb_atlas_cancel` invalidates paused queries too. It changes the handle epoch
without taking the operation lock or freeing source on the caller. Active work
observes cancellation; between calls, the next worker access or close retires the
obsolete job before any page, activation or resumed step can use it. A stale step
cannot erase a newer query. Canceling a reader alone does not cancel shared search.

If delivery is lost after a successful step, paging its known generation reveals
the current progress. Repeating a terminal step returns retained results without
source I/O. Cancellation after terminal acceptance cannot undo that accepted
result; likewise, reader info/close reconciles a known destination after a late
cancellation suppresses its installation receipt.

## Captures, scope, and identity

Each query reads only the frozen atlas catalog's eligible regular files under its
root grant and application source policy. It does not repeat discovery or broaden
exclusions. Added files require a new atlas/catalog. Files changed since metadata
discovery are observed when that query reaches them: no cross-file atomicity is
claimed. Already examined matching files stay immutable while later steps proceed.

Paging, overlays, result focus and reader activation do not open source paths.
Disk edits, renames, disappearance or same-size replacement cannot change a
retained hit's bytes. The response links the original owner/file/source revision,
query and original-byte range to the reader's distinct ownership domain. Decoded
context is logical text, not native shaping.

An independently issued root grant is rejected even when public ID counters match.
Revocation stops new source/result/reader delivery. It cannot retract bytes already
returned to an independently authorized host or reader. Inherited pathname checks
do not establish race-safe confinement against hostile ancestor replacement.

## Resource and integration boundaries

Source reads (including failed work), examined files, per-file size, retained hits,
diagnostic count, output, and old/new capture overlap are bounded. Unmatched
captures are discarded; matching whole captures remain. Atlas and search managed
budgets are separate and are not claims about total process memory. Reader copies
and returned native strings have independently owned lifetimes and costs.

`search_complete`, `discovery_complete`, `pending_files`, `unavailable_files`,
`matches_seen`, `retained_hits`, `truncated` and `next_offset` are distinct. Unknown
or unread files are not zero-hit files. Sparse overlays have one row per matching
file and count retained occurrences, never invented exhaustive totals.

The global handle-table lock is not held across source work. Same-atlas operation
contention returns BUSY, not a wait. Between resumable steps the per-atlas operation
lock is released; unrelated atlas handles remain independent. Keep the last
acknowledged scene until a new candidate plan is explicitly presented.

## Verification boundary

Production-path tests include `crates/fcb-app/tests/retained_atlas_search.rs`,
`crates/fcb-app/tests/progressive_atlas_search.rs`, `src/atlas_search_sessions_tests.rs`,
`src/atlas_progressive_tests.rs`, and `src/atlas_search_ffi_tests.rs`.

For the configured independent verifier:

```sh
RCH_REQUIRE_REMOTE=1 rch exec -- cargo test -p fcb-app --test retained_atlas_search --test progressive_atlas_search
RCH_REQUIRE_REMOTE=1 rch exec -- cargo test -p fcb-bridge
```

The added tests are code-first and have not been compiled or executed in the
authoring environment. This is an exact direct-scan host route with file-bounded
progressive publication, not persistent/out-of-core indexing, a full native UI,
Metal presentation or physical-Mac qualification.
