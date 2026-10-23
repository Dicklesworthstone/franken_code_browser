#ifndef FCB_FILE_FINDER_H
#define FCB_FILE_FINDER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Progressive metadata-only filename/path queries, without a global file-count
 * cutoff. The product-defaults/no-rule-files-v1 policy and bounded discovery
 * omissions are reported explicitly. All calls except create/cancel are worker
 * operations; no call presents pixels or performs an implicit source read.
 *
 * A handle owns one immutable query. Query edits require a new handle and a new
 * scan from the root. There are at most four live or retiring finder handles.
 * Handles never alias reader, catalog, atlas or saved-repository identities.
 *
 * Every non-null JSON response is independently caller-owned and must be freed
 * exactly once with fcb_free_string. Success JSON is at most 4 MiB, errors below
 * 4096 bytes; allocation/panic failure can return null. Numeric wire values are
 * canonical decimal strings. Paths carry reversible native bytes; display text
 * is never a pathname capability.
 */
uint64_t fcb_file_finder_create(void);

/* root: NUL-terminated UTF-8. query: 1..256 native bytes, stable until return.
 * mode: 0 exact, 1 prefix, 2 fuzzy. match_case: 0 Unicode lowercase, 1 sensitive.
 * generation: nonzero. max_results: 1..256; top-K memory stays bounded.
 * Open admits one metadata batch, fixes query identity, and returns revision 1
 * before ranking it. Advance to search; info recovers a lost initial response.
 */
char *fcb_file_finder_open(uint64_t handle, const char *root, uint64_t generation,
    const uint8_t *query, uint64_t query_bytes, uint32_t mode, uint8_t match_case,
    uint64_t max_results);

/* Info/page/stop include the complete retained top-K and coverage. Page accepts
 * only the current query/revision and never advances. Search completeness,
 * discovery completeness, pending work and omitted result rows are separate.
 */
char *fcb_file_finder_info(uint64_t handle);
char *fcb_file_finder_page(uint64_t handle, uint64_t generation, uint64_t revision);

/* max_batches: 1..16. Each fully merged batch increments revision before later
 * cancellation or fallible delivery. The current revision authorizes new work.
 * Retrying the last accepted after_revision returns current results without
 * advancing; other stale/future tokens refuse. Info recovers current progress.
 */
char *fcb_file_finder_advance(uint64_t handle, uint64_t generation,
    uint64_t after_revision, uint64_t max_batches);
char *fcb_file_finder_stop(uint64_t handle, uint64_t generation);

/* A retained hit authorizes a NEW capture, not historical directory bytes.
 * The destination must be an already-created EMPTY reader. Source access is
 * admitted only after destination and query/revision/file checks. The resulting
 * independent reader survives finder closure and preserves its exact capture.
 * max_source_bytes: 1..4194304; larger files refuse rather than truncate.
 */
char *fcb_file_finder_open_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t revision, uint64_t file, uint64_t max_source_bytes);

/* Cancel is nonblocking and preserves accepted progress. Close is worker-only;
 * entered work retains its capacity permit until all owned work has drained.
 */
uint8_t fcb_file_finder_cancel(uint64_t handle);
uint8_t fcb_file_finder_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
