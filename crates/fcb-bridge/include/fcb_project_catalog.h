#ifndef FCB_PROJECT_CATALOG_H
#define FCB_PROJECT_CATALOG_H
#include <stdint.h>
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Worker-only metadata discovery/projection through the shared atlas engine.
 * root is a readable NUL-terminated UTF-8 path, max_files is 1..20000.
 * poll/context stay alive until return; no callbacks escape. A nonzero poll
 * result requests cooperative cancellation, not interruption of a syscall.
 * Null means failure/cancellation. Non-null UTF-8 JSON uses fcb.project-catalog/1
 * and must be freed exactly once with this library's fcb_free_string.
 * discovery_complete=false means known partial membership, not missing files.
 * IDs and byte lengths are canonical decimal strings. Paths are reversible
 * native payloads; display labels are not openable paths. IDs are response-local,
 * not retained handles. No source payload, profile, highlighting or cache work
 * is performed; this response does not establish a source capture or frame.
 */
char *fcb_project_catalog_cancelable(const char *root, uint64_t max_files,
    int32_t (*poll)(void *), void *context);

/* Progressive project file discovery with retained page/row authority.
 * Four live/retiring handles; shared identity allocator with reader/atlas/saved
 * sessions. Create reserves an EMPTY cancelable handle without filesystem I/O.
 * All other work except cancel belongs on a host worker. No runtime is started.
 * Every non-null JSON result, including errors, must be released exactly once
 * with fcb_free_string. Null is marshaling/panic failure, not an empty catalog.
 * Schema: fcb.project-catalog-session/1. Full-width integers are JSON strings.
 * Successful replies are bounded to 1 MiB. Inspect status/state and omissions.
 */
uint64_t fcb_catalog_create(void);

/* UTF-8 root must remain readable/NUL-terminated until return. Opens the root
 * and consumes ONE bounded discovery batch, not the complete directory tree.
 * No source payload or repository rule file is read. The explicit policy is
 * product-defaults/no-rule-files-v1; symlinks are not followed.
 * Default traversal bounds: 256 entries and 64 KiB of batch paths (at most one
 * additional 2048-byte path), depth 32, queue 4096, one directory descriptor.
 * At most FOUR pages / 1024 file rows remain retained; there is no global file
 * admission cutoff. Dropped pages lose row activation authority.
 * Returns page 1 and current info fields, possibly empty with more=true.
 */
char *fcb_catalog_open(uint64_t handle, const char *root);
char *fcb_catalog_info(uint64_t handle);

/* Page IDs begin at 1; old pages can be replayed while retained. Metadata-only.
 * All current navigation/completeness counters describe the CURRENT session,
 * even when replaying an older page. Only page_id/after_page/files are the
 * immutable page record. Ordering is provisional discovery order, not sorted.
 */
char *fcb_catalog_page(uint64_t handle, uint64_t page);

/* If after_page is latest, consume one batch and retain its successor. If that
 * successor already exists and is retained, replay it without another walk.
 * Future/evicted cursors fail. The consumed page is retained BEFORE delivery;
 * a lost or canceled response is recovered with info/page or the same advance.
 * Each bounded batch completes before cooperative cancellation is observed;
 * cancellation cannot roll back ReadDir or interrupt a blocking system call.
 * Empty pages with more=true are useful progress, not complete empty results.
 */
char *fcb_catalog_advance(uint64_t handle, uint64_t after_page);

/* Pause with pages and bounded cursor retained. Advance after latest resumes.
 * Already opened readers remain independent of pause, eviction and close.
 */
char *fcb_catalog_stop(uint64_t handle);

/* Activate exactly (handle,page,file) as a NEW source capture, not a frozen
 * version of earlier metadata. An existing EMPTY reader destination is required
 * and admitted before path resolution or source I/O. The selected raw path is
 * resolved by the engine; no caller path string is accepted. Stale/evicted rows
 * fail before source access. max_source_bytes: 1..4 MiB; oversize files refuse.
 * Receipt includes new-capture-after-metadata-selection, reader metadata and
 * its first byte window. Continue with fcb_reader_* operations afterward.
 * Uses existing path checks and leaf no-follow policy, not a race-safe sandbox.
 */
char *fcb_catalog_open_reader(uint64_t handle, uint64_t reader,
    uint64_t page, uint64_t file, uint64_t max_source_bytes);

uint8_t fcb_catalog_cancel(uint64_t handle);
uint8_t fcb_catalog_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
