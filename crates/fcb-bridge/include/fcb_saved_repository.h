#ifndef FCB_SAVED_REPOSITORY_H
#define FCB_SAVED_REPOSITORY_H
#include <stdint.h>
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Retained FCBS archive and optional FCBD demand-paged posting index.
 * This handle kind is distinct from atlas and reader handles. Four live/retiring
 * saved handles are admitted; closing an in-flight handle does not immediately
 * release its capacity. Create reserves an EMPTY cancelable handle; zero fails.
 * All work except create/cancel belongs on a host worker. No runtime is started.
 * Non-null JSON uses fcb.saved-repository/1; inspect status and search_complete.
 * Free every response exactly once with fcb_free_string, even errors. Null is
 * marshaling/panic failure. Full-width IDs, offsets and counts are JSON strings.
 */
uint64_t fcb_saved_create(void);

/* Capture the named archive descriptor and validate the full archive once.
 * Retains its directory, not its complete source body. Default limits: 65536
 * members, 1 MiB per member, 64 MiB captured source, 80 MiB archive, 2048 bytes
 * per relative native name. UTF-8 path must be readable/NUL-terminated until
 * return. No archived pathname confers live-root or execution authority.
 * An opened handle cannot reopen another archive. Failed initial open leaves
 * it empty unless cancellation arrived AFTER acceptance; info reconciles that.
 */
char *fcb_saved_open(uint64_t handle, const char *path);
char *fcb_saved_info(uint64_t handle);

/* Metadata-only page: start/member ordinals are zero-based, limit is 1..128.
 * Captured empty members and unavailable members remain different states.
 */
char *fcb_saved_members(uint64_t handle, uint64_t start, uint64_t limit);

/* Optional FCBD index. The 64-hex trusted_digest MUST come from its original
 * trusted build receipt, NOT be inferred from the input file. It pins the
 * metadata envelope including page hashes, not a whole-file checksum. Pages
 * are verified before use; four 16 KiB pages are cached across queries.
 * Archive-binding/pin failure preserves the preceding accepted index/results.
 * Both strings must be readable/NUL-terminated UTF-8 until return.
 * Attach/detach share an increasing INDEX generation, independent of queries.
 */
char *fcb_saved_attach_index(uint64_t handle, uint64_t generation,
    const char *path, const char *trusted_digest);
char *fcb_saved_detach_index(uint64_t handle, uint64_t generation);

/* Case-sensitive decoded literal (1..1024 UTF-8 bytes), NOT query syntax.
 * max_matches 1..4096. Uses direct saved-source scanning without an index;
 * attached indexes use rarest posting candidates plus exact source verification.
 * Source residency is one member during verification, not all matching files.
 * Searches run synchronously with cancellation checkpoints; this is NOT the
 * live atlas begin/step API. A QUERY generation increases for each admitted
 * search/clear attempt, including failure. Failed replacement preserves rows.
 * First response has at most 64 hits; page using the same generation afterward.
 * Needle must be readable/NUL-terminated UTF-8 until return.
 */
char *fcb_saved_search(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t max_matches);
char *fcb_saved_results(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);
char *fcb_saved_clear_results(uint64_t handle, uint64_t generation);

/* Existing EMPTY destination from fcb_reader_create. Admission precedes member
 * I/O. hit IDs are one-based and qualified by the accepted query generation;
 * member ordinals are zero-based. Reads verify the saved member digest again;
 * in-place archive corruption is refused, never replaced by live source bytes.
 * Receipt links saved-source and reader identities, exact original selection
 * and decoded context. It does not claim native shaped/UTF-16 selection geometry.
 * Delivered readers use ordinary reader APIs and survive saved-handle closure.
 */
char *fcb_saved_open_hit_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t hit);
char *fcb_saved_open_member_reader(uint64_t handle, uint64_t reader,
    uint64_t member);

/* Cooperative cancel changes the operation epoch without waiting on source.
 * Close removes authority and cancels work, but does not close delivered readers
 * or free response strings. Close can reclaim memory and must run on a worker.
 * Both return 1 on success, 0 on unknown/busy/closed/exhausted handles.
 * Cancellation after acceptance suppresses delivery, not acceptance; reconcile
 * info/results on the still-open handle. No implicit background work exists.
 */
uint8_t fcb_saved_cancel(uint64_t handle);
uint8_t fcb_saved_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
