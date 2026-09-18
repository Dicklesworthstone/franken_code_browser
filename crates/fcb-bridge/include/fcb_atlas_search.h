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
 * execution of repository content occurs. Search and search_step read source;
 * begin, page, overlay and result activation do not open source paths.
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
 * Old finished results/captures survive failed/canceled replacement; an admitted
 * new attempt supersedes pending work. Check every returned status and generation.
 * Successful partial completion may replace finished results; unavailable_files,
 * pending_files, truncated and search_complete have separate meanings.
 * needle is null or valid NUL-terminated UTF-8 stable throughout the call.
 * This compatibility call scans to a terminal state. First page has <=64 hits.
 */
char *fcb_atlas_search(uint64_t handle, uint64_t generation, const char *needle,
    uint64_t max_matches, uint64_t max_files, uint64_t max_file_bytes,
    uint64_t max_source_bytes);

/* Same query/options as search, but begin only admits the work and copies the
 * bounded needle. No source bytes are read. Release needle after begin returns.
 * step examines <=1 file per call under max_file_bytes (hard maximum 1 MiB).
 * I/O/decoding are still worker operations; filesystem calls have no hard deadline.
 * Allow camera, result paging, activation and cancellation between step calls.
 * JSON search_in_progress=true means more steps remain. search_complete=false
 * does NOT mean keep stepping: a terminal quota/unavailable result is incomplete.
 * stop_reason is null while running, otherwise all-files-examined, file-limit,
 * source-byte-limit or match-limit. step_count/last_step_* report performed work.
 * Partial rows append with stable IDs. The last finished generation stays usable
 * until replacement finishes; failure/cancel invalidates only provisional rows.
 * fcb_atlas_cancel invalidates paused work without taking its operation lock;
 * the next worker access or close retires that work before further delivery.
 */
char *fcb_atlas_search_begin(uint64_t handle, uint64_t generation, const char *needle,
    uint64_t max_matches, uint64_t max_files, uint64_t max_file_bytes,
    uint64_t max_source_bytes);
char *fcb_atlas_search_step(uint64_t handle, uint64_t generation);

/* Pages contain <=limit (1..128) rows. next_offset is a zero-based continuation,
 * not coverage. null means end of CURRENT rows: a running query can append more.
 * Activation uses (generation, hit_id), never a visual row or an ID alone.
 * Finished and running generations can be paged. Replaced/canceled ones reject.
 */
char *fcb_atlas_search_page(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);

/* One overlay entry per matching file, not per occurrence. Counts cover retained
 * hits only, including running progress; validate layout_revision and generation.
 * clear consumes a fresh generation, retiring both finished and pending results.
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
 * Works for running-query hits too. Cancel either handle to abort preparation.
 * Reader-only cancellation leaves the shared query alive. A late cancellation
 * may suppress a reply AFTER installation: inspect/close that known reader.
 * Closing/clearing/canceling the atlas does not retract independently delivered
 * readers. Cancel after final query acceptance is not a rollback of that result.
 */
char *fcb_atlas_search_open_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t hit);

#ifdef __cplusplus
}
#endif
#endif
