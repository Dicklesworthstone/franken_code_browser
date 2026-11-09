#ifndef FCB_ATLAS_STREAM_SEARCH_H
#define FCB_ATLAS_STREAM_SEARCH_H
#include <stdint.h>
#include "fcb_atlas_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Bounded streaming literal search over ALL files in an atlas's frozen base
 * catalog, independent of its display scope. Open the atlas first; native
 * callers may admit up to 20000 metadata entries, subject to path/page limits.
 * Exclusions and incomplete discovery remain explicit. This never claims an
 * atomic workspace snapshot or unlimited repository coverage.
 *
 * Exact decoded spelling in UTF-8 or BOM-marked UTF-16 LE/BE. No complete-file
 * capture size limit. needle is 1..1024 UTF-8 bytes; max_matches is 1..256;
 * max_scan_bytes is 1..4294967296; max_read_calls is 1..1048576. Recommended
 * defaults: 256 matches, 268435456 scan bytes, 1048576 read calls. Failed work
 * consumes I/O too. Source syscalls cannot be forcibly interrupted.
 *
 * The application assigns each attempt a fresh stream query generation and
 * per-file observation revisions. These identities are separate from captured
 * atlas searches. Returned fcb.atlas-stream-search/1 JSON labels this distinction.
 * Session failures use fcb.atlas-session/1. Inspect status, search_complete,
 * discovery_complete, file outcomes and stop_reason; non-null is not success.
 * Initial search returns at most 32 hits; page returns remaining retained rows.
 *
 * All calls are synchronous host-worker operations, never input/draw callbacks.
 * An entered search holds its session permit and the HOST must retain native
 * access until that call drains, including after cancel/close. poll/context are
 * borrowed until return only; no unwinding, reentry or callback retention.
 * needle must be readable until NUL or 1025 bytes, whichever comes first.
 * Each non-null response must be freed once with the same fcb_free_string.
 * Null means no complete handoff. Late cancellation or a lost reply may follow
 * committed replacement: revoke previous result actions or reconcile by query.
 */
char *fcb_atlas_stream_search_cancelable(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    int32_t (*poll)(void *context), void *context);

/* No source I/O. generation must identify the accepted report; start is a
 * zero-based retained-row position and count is 1..32. next_offset=null means
 * the end of the terminal retained rows, not proof of full source coverage.
 */
char *fcb_atlas_stream_search_page(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t count);

/* Exact retained text/original_hex and provenance for one zero-based position,
 * with zero additional source I/O. Descriptors closed when their scans ended.
 * This is only the matching literal, not a capture or current context. Opening
 * its pathname later requires a current grant, a NEW owner/observation and
 * exact original-byte comparison before any old-match highlight is installed.
 */
char *fcb_atlas_stream_search_hit(uint64_t handle, uint64_t generation,
    uint64_t position);

/* Release the accepted report and advance its query identity. This does not
 * affect independent captured-search results, indexes or opened readers.
 */
char *fcb_atlas_stream_search_clear(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
