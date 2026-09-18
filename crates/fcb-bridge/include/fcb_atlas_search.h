#ifndef FCB_ATLAS_SEARCH_H
#define FCB_ATLAS_SEARCH_H
#include <stdint.h>
#include "fcb_atlas_sessions.h"
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* All calls are synchronous host-WORKER operations, not input/render callbacks.
 * Reuse an opened fcb_atlas_create/fcb_atlas_open handle. No extra discovery or
 * execution of repository content occurs. Only fcb_atlas_search reads source.
 * Free every non-null returned string exactly once with fcb_free_string.
 * Null indicates marshaling/panic failure; non-null may contain an error object.
 * Successful search JSON is fcb.atlas-search/1. Errors use fcb.atlas-session/1.
 * Full-width IDs, counts, offsets and ranges are canonical decimal JSON strings.
 * Native paths retain reversible payloads; display strings are not path grants.
 */

/* New generation per attempt. Literal UTF-8 query <=1024 bytes, interpreted via
 * the existing UTF-8/UTF-16 decoder, never as regex/query syntax. Limits:
 * max_matches 1..4096, max_files 1..4096, max_file_bytes 1..1048576,
 * max_source_bytes 1..33554432. The source-read budget includes failed reads.
 * Frozen-catalog membership is not a current/atomic filesystem snapshot.
 * Old results/captures survive canceled or failed replacement. A successful
 * partial query may replace them; always check search_complete, unavailable_files,
 * pending_files and truncated separately. First page has at most 64 hits.
 * needle is null or valid NUL-terminated UTF-8 stable throughout the call.
 */
char *fcb_atlas_search(uint64_t handle, uint64_t generation, const char *needle,
    uint64_t max_matches, uint64_t max_files, uint64_t max_file_bytes,
    uint64_t max_source_bytes);

/* Pages contain at most limit (1..128) rows. next_offset is a zero-based page
 * continuation, distinct from search completeness. Activation uses hit_id from
 * the response, NOT a row position. Wrong/replaced generations are rejected.
 */
char *fcb_atlas_search_page(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);

/* One overlay entry per matching file, not per text occurrence. Counts cover
 * retained hits only; layout_revision and generation must agree with the host.
 */
char *fcb_atlas_search_overlay(uint64_t handle, uint64_t generation);
char *fcb_atlas_search_clear(uint64_t handle, uint64_t generation);

/* Produces an UNPRESENTED plan. Use the existing fcb_atlas_present protocol.
 * Does not reread source, repack the atlas or replace the acknowledged pick frame.
 */
char *fcb_atlas_search_focus(uint64_t handle, uint64_t generation,
    uint64_t hit, uint64_t plan_generation);

/* reader must be an existing EMPTY fcb_reader_create handle. The entire retained
 * source is copied to that reader before delivery; use all fcb_reader_* methods
 * afterward. Reader ownership is distinct and linked explicitly in the JSON.
 * No live path is reopened. Overwriting an already-open reader is refused.
 * Cancel either handle to abort preparation. A late cancellation can suppress a
 * reply AFTER reader installation: inspect/close that known reader to reconcile.
 * Closing/clearing the atlas does not close independently opened readers.
 */
char *fcb_atlas_search_open_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t hit);

#ifdef __cplusplus
}
#endif
#endif
