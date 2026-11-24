#ifndef FCB_CATALOG_STRUCTURE_H
#define FCB_CATALOG_STRUCTURE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Progressive project symbols and Markdown headings, independent of retained
 * atlas file-count limits. Discovery pages, complete source witnesses and result
 * rows have fixed bounds; visited files grow only counters. A finished traversal
 * is an observation, not an atomic workspace snapshot or compiler-complete
 * declaration inventory. Symbols remain heuristic outline candidates; headings
 * use qualified FrankenMarkdown title/slug and original-source ranges.
 *
 * Each handle owns one immutable query. Four live or retiring handles are
 * admitted, in a shared non-reusing identity domain with other bridge handles.
 * All calls except create/cancel are worker operations. No callbacks occur.
 * Owned JSON uses fcb.catalog-structure/1 and canonical decimal-string integers.
 * Native path bytes are reversible. Release each non-null JSON buffer exactly
 * once with fcb_free_string. Allocation/panic failure may return null.
 */
uint64_t fcb_catalog_structure_create(void);

/* root: NUL-terminated UTF-8. query: 1..1024 readable UTF-8 bytes, no terminating
 * NUL needed. Embedded NUL is refused. generation is nonzero.
 * mode: 0 symbols, 1 headings. name_mode: 0 exact, 1 prefix, 2 contains.
 * Names/titles match case-sensitively without Unicode normalization.
 *
 * Null extensions AND filenames means all file types; a non-null empty list
 * means an empty predicate. Lists use at most 1024 bytes of shared ASCII CSV.
 * path_kind: 0 workspace, 1 directory, 2 exact file, 3 frozen visible membership.
 * Directory/file paths are relative UTF-8, at most 4096 bytes. Workspace requires
 * null/empty path. Visible is sorted unique lowercase-hex raw-path CSV, at most
 * 1048576 bytes and 20000 paths. Scope filters run before source I/O budgets.
 *
 * max_matches: 1..256, max_scan_bytes: 1..4294967296,
 * max_read_calls: 1..1048576. Counts continue beyond retained rows. start_after
 * skips that many matching ordinals; another window requires a new session and
 * new file observations. Source capture limits are 64 KiB per symbol file and
 * 256 KiB per Markdown file, intersected with cumulative I/O budgets. Retained
 * complete source is capped at 32 MiB independently of row capacity. Unsupported,
 * oversized, unavailable or analysis-limited sources remain explicit incomplete
 * coverage. Row/source-retention truncation is separate from count completeness.
 *
 * Open fixes the query and first metadata batch at revision 1, with no source
 * payload reads. info recovers accepted state after a lost response.
 */
char *fcb_catalog_structure_open(uint64_t handle, const char *root,
    uint64_t generation, const uint8_t *query, uint64_t query_bytes,
    uint64_t mode, uint64_t name_mode, uint64_t start_after,
    const char *extensions, const char *filenames, uint64_t path_kind,
    const char *path, uint64_t max_matches, uint64_t max_scan_bytes,
    uint64_t max_read_calls);

/* Immutable discovery_policy: 0 repository-rules-v1; 1 includes ignored regular
 * files without reading rule configuration. Both retain grant, own-artifact,
 * symlink/special-file and query limits. Invalid scalars/tags refuse before any
 * foreign string is dereferenced or filesystem access is attempted.
 */
char *fcb_catalog_structure_open_with_policy(uint64_t handle, const char *root,
    uint64_t generation, const uint8_t *query, uint64_t query_bytes,
    uint64_t mode, uint64_t name_mode, uint64_t start_after,
    const char *extensions, const char *filenames, uint64_t path_kind,
    const char *path, uint64_t max_matches, uint64_t max_scan_bytes,
    uint64_t max_read_calls, uint64_t discovery_policy);

/* Explicit immutable case policy in addition to discovery_policy above.
 * case_sensitive: 1 preserves exact case; 0 uses Unicode scalar lowercase for
 * exact, prefix and contains matching. This is not full Unicode case folding
 * or canonical normalization. The two legacy open functions remain sensitive.
 * Returned name_case is "sensitive" or "unicode-lowercase", and case_sensitive
 * echoes the accepted flag. Original spelling, UTF-8/UTF-16 source byte ranges,
 * result-window ordinals and count-completeness guarantees are unchanged.
 * Invalid case flags refuse before dereferencing any foreign input pointer.
 */
char *fcb_catalog_structure_open_matching_with_policy(uint64_t handle, const char *root,
    uint64_t generation, const uint8_t *query, uint64_t query_bytes,
    uint64_t mode, uint64_t name_mode, uint64_t start_after,
    const char *extensions, const char *filenames, uint64_t path_kind,
    const char *path, uint64_t max_matches, uint64_t max_scan_bytes,
    uint64_t max_read_calls, uint64_t case_sensitive, uint64_t discovery_policy);

char *fcb_catalog_structure_info(uint64_t handle);

/* max_steps: 1..16 bounded metadata/file steps, at most 4 MiB source input per
 * call, additionally constrained by cumulative byte/read limits. A step may
 * synchronously parse one bounded complete capture; it is not a UI callback or
 * fixed wall-clock promise. The current revision authorizes work; retrying the
 * latest accepted predecessor replays progress without repeated I/O/counts.
 * Earlier/future revisions refuse. Per-file rows/counts publish together after
 * source qualification and cancellation checks.
 */
char *fcb_catalog_structure_advance(uint64_t handle, uint64_t generation,
    uint64_t after_revision, uint64_t max_steps);

/* These calls require the current generation/revision and do no source I/O.
 * Pages contain at most 32 retained rows. Rows append in discovery/extraction
 * order and retain stable occurrence IDs. Hit includes selected original bytes.
 * Captures survive discovery-page eviction and later file replacement/deletion.
 */
char *fcb_catalog_structure_page(uint64_t handle, uint64_t generation,
    uint64_t revision, uint64_t start, uint64_t count);
char *fcb_catalog_structure_hit(uint64_t handle, uint64_t generation,
    uint64_t revision, uint64_t position);

/* reader must be an empty fcb_reader_create handle. After destination admission
 * and grant/generation validation, copy the exact retained complete source into
 * that reader without resolving/reopening a live path. Receipt includes source
 * identity/digest, selected range, ordinary reader info and a source window.
 * A copied reader remains independently owned after the search closes. Failed
 * or canceled preparation leaves an unaccepted destination empty; acceptance
 * followed by late cancellation is recoverable through ordinary reader info.
 */
char *fcb_catalog_structure_open_hit_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t revision, uint64_t position);

/* Stop after bounded in-flight work drains; retains finalized source evidence
 * and reports user-stopped partial coverage. Cancel is nonblocking epoch
 * revocation. Cancellation during file work ends that query with incomplete
 * coverage, preserving previously accepted hits; info may recover that state.
 * Cancellation before work leaves the accepted query recoverable. Close retires
 * ownership; capacity returns only after entered work and destruction drain.
 */
char *fcb_catalog_structure_stop(uint64_t handle, uint64_t generation);
uint8_t fcb_catalog_structure_cancel(uint64_t handle);
uint8_t fcb_catalog_structure_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
