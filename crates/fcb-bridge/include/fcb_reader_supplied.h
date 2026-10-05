#ifndef FCB_READER_SUPPLIED_H
#define FCB_READER_SUPPLIED_H
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* HOST-WORKER operation on an empty fcb_reader_create handle. The engine owns
 * a bounded copy before returning. It never opens or canonicalizes label and
 * performs no filesystem or network I/O. A label is metadata, not a root grant.
 * Subsequent reader/search/outline/document calls share the supplied capture.
 * Native hosts can therefore preview source already displayed after disk edits.
 *
 * label: nonempty NUL-terminated UTF-8, at most 16384 bytes (null is refused).
 * bytes: readable initialized storage of length bytes, stable until return;
 *        need not be UTF-8 or NUL-terminated. Null is permitted only for length 0.
 * length: 0..FCB_READER_MAX_SOURCE_BYTES. Over-limit values are refused before
 *         either pointer is dereferenced. Invalid/dangling admitted pointers
 *         remain caller errors, not a safe-input validation feature.
 *
 * Failed/canceled initialization leaves an empty handle retryable. An already
 * open handle cannot replace its source. Close/cancel rules and fcb_free_string
 * ownership are exactly those in fcb_reader_sessions.h. Inspect non-null JSON:
 * status=error is not success. Successful info reports host-supplied origin and
 * zero initial source reads; integers use canonical decimal strings.
 */
char *fcb_reader_open_bytes(uint64_t handle, const char *label,
    const uint8_t *bytes, uint64_t length);

#ifdef __cplusplus
}
#endif
#endif
