#ifndef FCB_ATLAS_INDEX_SOURCE_H
#define FCB_ATLAS_INDEX_SOURCE_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

/* Worker-only transfer from an immutable indexed source universe. reader must
 * be an EMPTY fcb_reader_create handle. Original file/revision are returned by
 * an indexed search result; source is copied without reading the live path.
 * Success uses fcb.atlas-search/1, command index-open-reader, selection_namespace
 * index-source, retained-index-capture and source_reopened=false. It carries
 * index_generation, capture_manifest, original file/revision/path, SHA-256 and
 * byte length, reader_owner and nested independent reader info. It confers NO
 * query/range selection: re-establish that in the receiving reader's namespace.
 * Query replacement/clear does not invalidate source; index clear/replacement
 * does. Preserve old atlas handles across index refresh for pinned sources.
 * Late cancellation can suppress delivery after destination installation;
 * close/reconcile the known receiver instead of reopening a live source.
 * Free every returned non-null string once with fcb_free_string. Null is failure.
 */
char *fcb_atlas_index_open_reader(uint64_t handle, uint64_t reader,
    uint64_t index_generation, uint64_t file, uint64_t source_revision);

/* Nonzero poll result requests cancellation. poll/context remain valid only
 * through this call; no callback escapes. poll must not unwind or reenter this
 * atlas. Cancellation is cooperative and cannot interrupt a blocked syscall.
 */
char *fcb_atlas_index_step_cancelable(uint64_t handle, uint64_t generation,
    int32_t (*poll)(void *), void *context);

#ifdef __cplusplus
}
#endif
#endif
