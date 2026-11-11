#ifndef FCB_ATLAS_INDEX_H
#define FCB_ATLAS_INDEX_H
#include <stdint.h>
#include "fcb_atlas_search.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Reusable capture index on an existing OPEN atlas handle. All operations are
 * synchronous host-worker calls; the library does not start a background task.
 * Free every non-null response exactly once with fcb_free_string, including
 * errors. Null means marshaling/panic failure. Success: fcb.atlas-search/1;
 * errors: fcb.atlas-session/1. Full-width numbers are decimal JSON strings.
 * No repository content is executed by opening, indexing, querying or reading.
 */

/* Capture and index to completion using the same one-file pipeline as begin/
 * step below. Limits: files 1..4096, file bytes 1..1048576, actual source reads
 * 1..33554432, retained trigrams 0..2097152. Failed reads consume source budget.
 * Quota-uncovered segments retain source for exact-scan fallback. Inspect
 * capture_complete, pending_files, unavailable_files and uncovered_files.
 * Membership stays the frozen atlas catalog: no new filenames are discovered.
 * Per-file observations are NOT an atomic workspace snapshot or Git history.
 * Preparation/query/result-clear/index-clear attempts share a strictly
 * increasing generation sequence. Failed admitted attempts consume their ID.
 */
char *fcb_atlas_index_prepare(uint64_t handle, uint64_t generation,
    uint64_t max_files, uint64_t max_file_bytes, uint64_t max_source_bytes,
    uint64_t max_index_grams);

/* Begin index preparation without source I/O or segment construction. Continue
 * using index_step, NOT search_step. One replacement builder may coexist with
 * queries on the previous accepted index. New build/index-clear supersedes an
 * older builder; ordinary queries and result-clear do not cancel the builder.
 * Starting a build still supersedes an unfinished query. Failure/cancellation
 * preserves the old accepted index and completed result/source pins.
 */
char *fcb_atlas_index_begin(uint64_t handle, uint64_t generation,
    uint64_t max_files, uint64_t max_file_bytes, uint64_t max_source_bytes,
    uint64_t max_index_grams);

/* Prepare the frozen file-type/path intersection before capture quotas. Type
 * lists and path_kind use fcb_atlas_search_path_scoped: 0 workspace, 1 directory,
 * 2 exact file, 3 canonical raw-byte hex CSV of visible files. Null/null type
 * lists mean all; present empty lists mean none. An empty visible manifest
 * selects zero files. Strings are copied during admission and need not survive
 * the call. The existing begin/prepare APIs remain all/workspace operations.
 * Build counters (examined/pending/captured/unavailable) describe admitted
 * capture membership; catalogued_files still describes the full atlas catalog.
 * capture_eligible_files/capture_excluded_files and capture_* scope echoes pin
 * that preparation independently of later queries. A query outside those
 * captures remains incomplete; only an explicit new preparation reads disk.
 */
char *fcb_atlas_index_prepare_scoped(uint64_t handle, uint64_t generation,
    uint64_t max_files, uint64_t max_file_bytes, uint64_t max_source_bytes,
    uint64_t max_index_grams, const char *extensions, const char *filenames,
    uint64_t path_kind, const char *path);
char *fcb_atlas_index_begin_scoped(uint64_t handle, uint64_t generation,
    uint64_t max_files, uint64_t max_file_bytes, uint64_t max_source_bytes,
    uint64_t max_index_grams, const char *extensions, const char *filenames,
    uint64_t path_kind, const char *path);

/* Capture AND index at most one catalog member, then return. Finalization moves
 * completed storage without a full-source scan/rebuild. Use
 * index_build_in_progress, NOT capture_complete, to decide whether to continue.
 * Actual read quotas and gram quotas apply across all steps. A completed build
 * can remain incomplete/uncovered. A step bounds members, not elapsed time;
 * OS I/O and the bounded per-file sort do not gain a hard cancellation deadline.
 * A replacement is published only after preparing its full response. Publication
 * retires unfinished queries tied to the replaced index; completed result pins,
 * opened readers and independent live queries remain usable.
 * Lost nonterminal replies reconcile through progress. Another step advances
 * work; completed-step retries are read-only. Wrong generations never advance
 * or discard a newer builder. Use existing atlas_cancel/close for cancellation.
 */
char *fcb_atlas_index_step(uint64_t handle, uint64_t generation);
char *fcb_atlas_index_progress(uint64_t handle, uint64_t generation);

/* Accepted index information, distinct from provisional build progress. The
 * legacy source_manifest identifies the catalog; capture_manifest names source.
 */
char *fcb_atlas_index_info(uint64_t handle);

/* Literal UTF-8 needle 1..1024 bytes. NOT regex, filters, or query syntax.
 * index_generation must name the accepted preparation. max_matches: 1..4096;
 * max_scan_bytes: 0..33554432 captured verification bytes, NOT disk I/O.
 * UTF-16, short queries and uncovered segments use shared exact verification.
 * Uses the same cursor as the progressive query form, to completion. Results
 * support existing search_page/overlay/focus/open_reader. Original captures
 * survive live changes and index replacement through independent result pins.
 * needle is null or valid NUL-terminated UTF-8 stable until the call returns.
 */
char *fcb_atlas_search_indexed(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, uint64_t max_matches,
    uint64_t max_scan_bytes);

/* Admit without verifying source. Query text is copied. Continue using the
 * EXISTING fcb_atlas_search_step(handle, generation), not index_step. Each step
 * visits one captured member without rebuilding descriptors/grams. Query-wide
 * verification/lookahead limits remain global. Use search_in_progress to
 * continue; next_offset=null means end of current rows, not a complete query.
 * Early pages, overlays and captured reader activation are usable immediately.
 * IDs append stably, completed-step retries do no more work, and paused epoch
 * cancellation prevents stale resumption. Reader-only cancel does not affect
 * the atlas. No measured speedup or native presentation is inferred here.
 */
char *fcb_atlas_search_indexed_begin(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, uint64_t max_matches,
    uint64_t max_scan_bytes);

/* The same immutable query scope and list contract as search_scoped. Exclusion
 * happens before gram candidates, exact verification and occurrence quotas.
 * Reuses the accepted index; never silently rereads live files. Resume the begin
 * form with search_step. eligible_captured_files and
 * scoped_capture_unavailable_files describe selected retained membership.
 * unavailable_files additionally includes unsupported selected verification.
 * Selected catalog entries missing because of index build limits remain pending.
 * A closed catalog with every selected file verified can complete even when the
 * index omitted only excluded files. No scope change broadens the source grant.
 */
char *fcb_atlas_search_indexed_scoped(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, const char *extensions,
    const char *filenames, uint64_t max_matches, uint64_t max_scan_bytes);
char *fcb_atlas_search_indexed_begin_scoped(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, const char *extensions,
    const char *filenames, uint64_t max_matches, uint64_t max_scan_bytes);

/* Same path-kind/input/echo contract as fcb_atlas_search_path_scoped. Intersect
 * the selected directory/exact file with types BEFORE gram candidates, captured
 * verification bytes and hit quotas. The index and original capture identities
 * remain unchanged. A selected catalog member omitted by index build limits is
 * pending, not an empty successful result. Excluded omissions do not prevent a
 * complete query. No live recapture or index rebuild occurs when scope changes.
 */
char *fcb_atlas_search_indexed_path_scoped(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, const char *extensions,
    const char *filenames, uint64_t path_kind, const char *path,
    uint64_t max_matches, uint64_t max_scan_bytes);
char *fcb_atlas_search_indexed_begin_path_scoped(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, const char *extensions,
    const char *filenames, uint64_t path_kind, const char *path,
    uint64_t max_matches, uint64_t max_scan_bytes);

/* Frozen case choice: 1 exact spelling, 0 Unicode lowercase comparison. Invalid
 * values preserve accepted/pending work. All scope and matched_text contracts
 * match fcb_atlas_search_begin_matching. Captures are reused across case changes;
 * incompatible exact-case gram exclusions cannot remove candidates from an
 * insensitive query. Verification retains the same source-byte budget. */
char *fcb_atlas_search_indexed_begin_matching(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, const char *extensions,
    const char *filenames, uint64_t path_kind, const char *path,
    uint64_t case_sensitive, uint64_t max_matches, uint64_t max_scan_bytes);

/* Count every verified occurrence over the selected retained captures while
 * retaining the first max_matches rows (0..4096; zero supports count-only).
 * max_scan_bytes stays an independent verification bound; cancellation and
 * omitted captures still make counts partial. matches_seen is the occurrence
 * count, retained_hits bounds navigation, and match_count_complete reports
 * count coverage independently of truncated (retained_hits < matches_seen).
 * match_count_complete equals search_complete: a closed, fully examined
 * selected universe with no unavailable/pending files is required.
 * matching_files counts all observed matching files; retained_matching_files
 * counts files that back retained rows. Both Unicode matching modes are supported.
 * Existing indexed entrypoints preserve their early-stop/nonzero-limit contract.
 * Resume the begin form with fcb_atlas_search_step. Same pointer/owned-string
 * contract as indexed_begin_matching; no source pathname is reopened.
 */
char *fcb_atlas_search_indexed_begin_counted_matching(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, const char *extensions,
    const char *filenames, uint64_t path_kind, const char *path,
    uint64_t case_sensitive, uint64_t max_matches, uint64_t max_scan_bytes);
char *fcb_atlas_search_indexed_counted_matching(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, const char *extensions,
    const char *filenames, uint64_t path_kind, const char *path,
    uint64_t case_sensitive, uint64_t max_matches, uint64_t max_scan_bytes);

/* Clear index and builder, not completed result captures or opened readers.
 * Result-clear leaves index/build state intact. Atlas cancellation invalidates
 * provisional query AND build state; accepted state is not rolled back. A late
 * cancellation can suppress a committed reply: reconcile info/progress/page.
 * Destruction and close belong on a worker, not an input/redraw callback.
 */
char *fcb_atlas_index_clear(uint64_t handle, uint64_t generation);

#ifdef __cplusplus
}
#endif
#endif
