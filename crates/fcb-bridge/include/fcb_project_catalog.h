#ifndef FCB_PROJECT_CATALOG_H
#define FCB_PROJECT_CATALOG_H
#include <stdint.h>
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
#ifdef __cplusplus
}
#endif
#endif
