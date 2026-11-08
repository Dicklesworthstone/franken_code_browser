#ifndef FCB_OBSERVED_READER_H
#define FCB_OBSERVED_READER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bounded observations of one explicitly opened regular file, including files
 * too large for a complete retained reader capture. These are synchronous
 * worker operations: never perform source I/O or final close on a redraw/input
 * callback. Opening is metadata-only. Each window reads at most 256 KiB plus
 * 16 bytes of scalar/CRLF context and receives a fresh observation identity.
 * Separate windows are NOT a coherent whole-file snapshot.
 *
 * Four handles, including retiring in-flight handles, are admitted per loaded
 * library. Each retains at most eight observations. Retained/copy operations
 * use only those immutable bytes; eviction is an explicit unavailable error,
 * never permission to reread a live path under an old identity.
 *
 * Keep the current root's native access grant alive through close/drain. Root
 * is absolute NUL-terminated UTF-8 (<=16384 bytes). relative_path_hex is bounded
 * lowercase hex (<=32768 chars) encoding exact native root-relative components,
 * including non-UTF8 names. Absolute paths, NUL, empty/dot/dotdot components and
 * observed symlinks are refused. Leaf no-follow/nonblocking open is enforced;
 * hostile ancestor replacement confinement is NOT claimed.
 *
 * An open descriptor continues to name its original file object after pathname
 * replacement. Explicitly open a NEW handle to follow the current pathname.
 * Source grants and filename labels do not establish old exact-anchor authority.
 *
 * Every non-null result is independently owned fcb.observed-reader/1 JSON
 * (<=8 MiB), released once by fcb_free_string from this library. A null result
 * means no response was delivered; it does not revoke previously retained
 * observations. Do not infer successful open or observation from non-null alone.
 */
typedef int32_t (*fcb_observed_reader_poll)(void *context);

/* No I/O. Zero means capacity/identity admission failed. */
uint64_t fcb_observed_reader_create(void);

/* poll/context, when supplied, remain valid through return. Poll executes only
 * on this calling worker, returns nonzero to cancel, and must not block, unwind,
 * mutate inputs or reenter the active reader. Input strings stay valid through
 * return. A loaded handle cannot be reopened. */
char *fcb_observed_reader_open(uint64_t handle, const char *root,
    const char *relative_path_hex, fcb_observed_reader_poll poll, void *context);
char *fcb_observed_reader_info(uint64_t handle);

/* bytes is 4..262144; offset is an absolute original-byte position. Explicit
 * decoding: 1=UTF-8, 2=UTF-16LE, 3=UTF-16BE. This is bounded logical text, not
 * whole-paragraph shaping, global line numbering or a new whole-file digest. */
char *fcb_observed_reader_window(uint64_t handle, uint64_t offset,
    uint64_t bytes, uint64_t encoding, fcb_observed_reader_poll poll, void *context);

/* No payload reread. Both commands require an exact retained observation ID.
 * Copy returns original bytes as hex, never silently truncated or decoded. */
char *fcb_observed_reader_retained(uint64_t handle, uint64_t observation);
char *fcb_observed_reader_copy(uint64_t handle, uint64_t observation,
    uint64_t start, uint64_t end);

/* Cancellation revokes in-flight delivery without waiting on source work.
 * Close removes authority immediately; its admission survives in-flight calls.
 * Returns 1 on success, 0 for unknown/busy/exhausted handles. */
uint8_t fcb_observed_reader_cancel(uint64_t handle);
uint8_t fcb_observed_reader_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
