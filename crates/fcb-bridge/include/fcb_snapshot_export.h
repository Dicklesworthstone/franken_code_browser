#ifndef FCB_SNAPSHOT_EXPORT_H
#define FCB_SNAPSHOT_EXPORT_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

/* Explicit source-containing FCBS export on a host-owned worker. The root
 * requires current read authority; destination requires a separate user-chosen
 * write grant. Existing destinations are never overwritten or removed.
 * Strings are readable NUL-terminated UTF-8 and stay valid through return.
 * Limits: files 1..65536, per-file bytes 0..1048576, total reads 0..67108864.
 * respect_ignores: 1 bounded .gitignore/.fcbignore rules, 0 default exclusions.
 * Poll runs only on this thread; zero continues, nonzero cancels. It must not
 * block, unwind or reenter. Retain context and BOTH grants until return.
 *
 * A non-null fcb.cli/1 JSON receipt may report partial/error/canceled outcome.
 * Always inspect effect: none, destination-created-incomplete,
 * complete-file-sync-unconfirmed, or complete-file-sync-requested. Late cancel
 * does not suppress a write receipt. Interrupted destinations remain on disk.
 * Null means invalid input or missing delivery; it does not establish no write.
 * Never retry automatically. Free each result once with fcb_free_string.
 * No atomic workspace snapshot, encryption or power-loss qualification.
 */
char *fcb_snapshot_save_cancelable(const char *root, const char *destination,
    uint64_t max_files, uint64_t max_file_bytes, uint64_t max_source_bytes,
    uint64_t respect_ignores, int32_t (*poll)(void *context), void *context);

#ifdef __cplusplus
}
#endif
#endif
