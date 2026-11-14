#ifndef FCB_CATALOG_STREAM_H
#define FCB_CATALOG_STREAM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Progressive whole-file workspace content search without an atlas file-count
 * cutoff. This owns an independent repository-rules-v1 discovery cursor and
 * bounded streaming matcher. Discovery, source I/O and result-row coverage are
 * reported separately; discovered-so-far counts are not a frozen workspace size.
 *
 * Each handle owns one immutable query. Query edits and ordinal result windows
 * use a new handle and a new per-file observation. Four live or retiring handles
 * are admitted. Handles never alias reader, atlas, catalog or finder identities.
 * All calls except create/cancel are worker operations. There are no callbacks.
 *
 * Non-null JSON is independently owned and must be released exactly once with
 * fcb_free_string. Errors are below 4096 bytes; allocation/panic failure may
 * return null. Wire schema is fcb.catalog-stream/1. Wide integer fields are
 * canonical decimal strings, and paths preserve reversible native bytes.
 * No report claims an atomic workspace snapshot or a full source-file capture.
 */
uint64_t fcb_catalog_stream_create(void);

/* root: NUL-terminated UTF-8. query: 1..1024 readable UTF-8 bytes, no terminating
 * NUL required. generation: nonzero. mode: 0 text, 1 identifier occurrences,
 * 2 bounded expression. case_sensitive and count_all: exactly 0 or 1.
 * Occurrences use case-sensitive identifiers; expressions use shared syntax
 * and count all matching occurrences within the requested I/O bounds.
 *
 * Null extensions AND filenames means all types; a non-null empty list is an
 * explicit empty predicate. Lists use shared comma-separated ASCII tokens, at
 * most 1024 bytes each. path_kind: 0 workspace, 1 directory, 2 exact file,
 * 3 explicit raw visible membership. Directory/file paths are relative UTF-8,
 * at most 4096 bytes. Workspace requires null/empty path. Visible membership is
 * a canonical sorted unique lowercase-hex CSV (at most 1048576 bytes, 20000
 * paths), not atlas frame proof. Membership is applied before source I/O.
 *
 * max_matches: 1..256, max_scan_bytes: 1..4294967296, max_read_calls: 1..1048576.
 * count_all continues counting beyond retained rows within the same I/O bounds.
 * start_after skips matching ordinals; each such window starts a fresh scan.
 * Open fixes the query and admits its first metadata batch at revision 1.
 * It performs no source-content reads. Advance begins content search; info
 * recovers the accepted initial state after a lost open response.
 */
char *fcb_catalog_stream_open(uint64_t handle, const char *root, uint64_t generation,
    const uint8_t *query, uint64_t query_bytes, uint64_t mode, uint64_t case_sensitive,
    uint64_t count_all, uint64_t start_after, const char *extensions, const char *filenames,
    uint64_t path_kind, const char *path, uint64_t max_matches, uint64_t max_scan_bytes,
    uint64_t max_read_calls);

/* Info, page and hit do not advance discovery or reopen source. Page/hit require
 * the current query generation and revision. Pages contain at most 32 rows.
 * Retained witnesses survive discovery-page eviction and later file mutation.
 */
char *fcb_catalog_stream_info(uint64_t handle);
char *fcb_catalog_stream_page(uint64_t handle, uint64_t generation, uint64_t revision,
    uint64_t start, uint64_t count);
char *fcb_catalog_stream_hit(uint64_t handle, uint64_t generation, uint64_t revision,
    uint64_t position);

/* max_steps: 1..256 bounded discovery/matcher steps (at most 4 MiB source work per
 * call, also constrained by cumulative byte/read limits). The current revision
 * authorizes new work. Retrying the most recently accepted predecessor replays
 * its accepted progress instead of advancing. Other stale/future tokens refuse.
 * Each accepted advance updates revision before fallible publication.
 */
char *fcb_catalog_stream_advance(uint64_t handle, uint64_t generation,
    uint64_t after_revision, uint64_t max_steps);

/* Stop scheduling advances, await the bounded in-flight call, then stop. This
 * discards an unfinished file, retains finalized witnesses and explicitly
 * reports user-stopped coverage. A stopped query needs a new handle to restart.
 * Cancel is nonblocking epoch revocation for an in-flight call. If it interrupts
 * a shared matcher, the query is revoked and must be restarted on a fresh handle;
 * already delivered JSON remains independently owned. Cancellation before work
 * may leave the accepted query recoverable. Close retires ownership; capacity is
 * returned only after all entered work has drained.
 */
char *fcb_catalog_stream_stop(uint64_t handle, uint64_t generation);
uint8_t fcb_catalog_stream_cancel(uint64_t handle);
uint8_t fcb_catalog_stream_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
