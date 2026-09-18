#ifndef FCB_ATLAS_PATHS_H
#define FCB_ATLAS_PATHS_H
#include <stdint.h>
#include "fcb_atlas_sessions.h"
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Synchronous host-WORKER operations on an existing opened atlas handle.
 * Free every non-null returned string exactly once with fcb_free_string.
 * Null indicates marshaling/panic failure. Always inspect non-null JSON status.
 * Successful results use fcb.atlas-paths/1; registry errors use fcb.atlas-session/1.
 * IDs, counts and offsets are canonical decimal STRINGS, not JSON doubles.
 * Native paths carry reversible byte payloads; labels are NOT path authority.
 */
#define FCB_PATH_FUZZY 0
#define FCB_PATH_EXACT 1
#define FCB_PATH_PREFIX 2
#define FCB_PATH_UNICODE_LOWERCASE 0
#define FCB_PATH_CASE_SENSITIVE 1

/* query: NUL-terminated UTF-8, 1..256 bytes, stable through return. Literal path
 * text, not content query syntax. max_results: 1..4096. Unknown mode/case refused.
 * Lowercase mode is Unicode lowercase, NOT full folding or normalization.
 * The prepared core path index is reused; neither this call nor paging,
 * selection or focus opens source, repeats discovery or repacks the atlas.
 * Fuzzy uses deterministic basename/path subsequences with lexical rank classes.
 * Use a fresh nonzero increasing generation for every attempt. Path generations
 * are independent of content-search and camera-plan generations. Failed/canceled
 * replacements keep old rows under their OLD generation. First page: <=64 rows.
 * search_complete describes catalog/scan/count completeness; truncated means
 * top-k omitted matching rows. next_offset only describes pagination of retained
 * rows. A partial catalog cannot produce a complete repository count.
 */
char *fcb_atlas_find_files(uint64_t handle, uint64_t generation, const char *query,
    uint64_t max_results, uint8_t mode, uint8_t case_mode);

/* start is zero-based; limit 1..128. Use returned file_id, never row position,
 * for selection/activation. The selection object can name a still-matching file
 * pinned outside a replacement's top-k. A nonmatching selection becomes null.
 */
char *fcb_atlas_file_results(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);
char *fcb_atlas_file_select(uint64_t handle, uint64_t generation, uint64_t file);
char *fcb_atlas_file_focus(uint64_t handle, uint64_t generation, uint64_t file,
    uint64_t plan_generation);

/* reader must be an EMPTY fcb_reader_create handle. This is the only path-finder
 * operation that reads source: a NEW capture after metadata-only path selection,
 * not bytes searched earlier. max_source_bytes: 1..4194304. Existing reader
 * admission and symlink/special-file policy apply; arbitrary paths are not input.
 * Use ordinary reader calls afterward. Clearing or closing the atlas leaves the
 * reader independently owned. Cancel either handle to abort current work; late
 * cancellation after installation can suppress delivery, not undo installation.
 * Inspect/close the known destination reader to reconcile such a boundary.
 * Native pathname checks do not qualify hostile ancestor-race confinement.
 */
char *fcb_atlas_file_open_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t file, uint64_t max_source_bytes);

/* Fresh path generation required. Retains prepared keys for future file queries;
 * content-search results, paused content queries and open readers are untouched.
 */
char *fcb_atlas_file_clear(uint64_t handle, uint64_t generation);

#ifdef __cplusplus
}
#endif
#endif
