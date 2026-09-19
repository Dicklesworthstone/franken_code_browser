#ifndef FCB_ATLAS_INDEX_H
#define FCB_ATLAS_INDEX_H
#include <stdint.h>
#include "fcb_atlas_search.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Explicit reusable capture index on an existing OPEN atlas handle. All calls
 * are synchronous host-worker operations. No extra registry/runtime/discovery.
 * Free every non-null response once with fcb_free_string, including errors.
 * Null means marshaling/panic failure. Success JSON uses fcb.atlas-search/1;
 * errors use fcb.atlas-session/1. All full-width numbers are decimal strings.
 * Preparing/opening a repository never executes its contents.
 */

/* Capture eligible catalog members and build the existing ephemeral index.
 * Limits: files 1..4096, file bytes 1..1048576, total actual source-read work
 * 1..33554432, retained trigrams 0..2097152. Failed reads count toward the source
 * budget. Uncovered segments retain source and use exact-scan fallback; they
 * are not empty negative certificates. Inspect capture_complete, pending_files,
 * unavailable_files, indexed_files and uncovered_files independently.
 * This is a frozen-catalog/per-file observation, NOT current Git status or an
 * atomic workspace snapshot. Refresh is explicit and does not rediscover paths.
 * Preparation can examine many files in one worker call. It is not the one-file
 * progressive API. Failure/cancellation preserves the older index and results.
 * Preparation, indexed queries, live queries and both clear operations SHARE
 * one increasing search-attempt generation sequence. Admitted failures consume
 * their generation. New attempts retire obsolete progressive live work.
 */
char *fcb_atlas_index_prepare(uint64_t handle, uint64_t generation,
    uint64_t max_files, uint64_t max_file_bytes, uint64_t max_source_bytes,
    uint64_t max_index_grams);

/* Read accepted index_generation and capture_manifest for reconciliation. The
 * legacy source_manifest field identifies the atlas catalog, not captured bytes.
 */
char *fcb_atlas_index_info(uint64_t handle);

/* Literal UTF-8 needle 1..1024 bytes. NOT regex, filters, or query syntax.
 * index_generation must be the accepted preparation generation. max_matches
 * is 1..4096; max_scan_bytes is 0..33554432 captured bytes verified, NOT disk I/O.
 * UTF-16, short queries and uncovered segments use the shared exact scan route.
 * Returns at most 64 initial rows. Use existing search_page/overlay/focus and
 * search_open_reader with query generation and hit ID afterward. Source remains
 * the indexed capture even after file changes, disappearance or index refresh.
 * Search success does not imply completeness; check search_complete, truncated,
 * unavailable_files and pending_files. Disk reads are zero. Verification and
 * skipped/fallback work are separate counters. No measured speedup is claimed.
 * needle is null or valid NUL-terminated UTF-8 for the duration of this call.
 */
char *fcb_atlas_search_indexed(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *needle, uint64_t max_matches,
    uint64_t max_scan_bytes);

/* Clear index only; accepted query captures and opened readers survive. Normal
 * search_clear clears results but leaves this index available. Large retirement
 * is worker work. Cancel/close use the existing atlas functions. A cancellation
 * AFTER publication can suppress delivery without undoing publication; inspect
 * index_info or page the known query to reconcile that boundary.
 */
char *fcb_atlas_index_clear(uint64_t handle, uint64_t generation);

#ifdef __cplusplus
}
#endif
#endif
