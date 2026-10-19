#ifndef FCB_DESK_H
#define FCB_DESK_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

/* All work except create/cancel belongs on a host worker. No runtime or native
 * event loop is acquired. Two live/retiring desks are admitted. Each desk uses
 * the existing public defaults: 8 panes, 32 retained sources, 32 MiB aggregate
 * original bytes, 4 MiB per source, 64 history positions and 32 named bookmarks.
 * Every non-null response is fcb.reading-desk/1 JSON; free it exactly once with
 * fcb_free_string. Integers are canonical decimal STRINGS. Check status.
 *
 * create returns zero on admission failure; start initializes its empty handle.
 * Mutations require current model_revision and a fresh attempt > last_attempt.
 * They return the full new state; a lost/canceled reply is NOT rollback. Recover
 * with state before trying another action. Old attempts never add a second note.
 * Captures stay exact across edits/deletes, pane closure, history and bookmarks.
 */
uint64_t fcb_desk_create(void);
char *fcb_desk_start(uint64_t handle);
char *fcb_desk_state(uint64_t handle);
/* Explicit current-file observation. UTF-8 path is host authority; no implicit
 * root access is conferred. Regular source files only, no source writes. */
char *fcb_desk_open(uint64_t handle, uint64_t expected, uint64_t attempt, const char *path);
/* kind: 0 focus pane; 1 navigate pane/start; 2 select pane/[start,end);
 * 3 bookmark pane/label; 4 recall bookmark; 5 forget bookmark; 6 pin pane;
 * 7 unpin pane; 8 close pane; 9 back; 10 forward; 11 clear history.
 * subject is pane or bookmark as appropriate. Label is <=256 UTF-8 bytes;
 * null is empty and accepted for commands that do not use a label. */
char *fcb_desk_apply(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t kind, uint64_t subject, uint64_t start, uint64_t end, const char *label);
/* SourceReader logical text with original byte ranges. 4..65536 requested
 * bytes, <=1024 rows. Page text is not copied back into source storage. */
char *fcb_desk_window(uint64_t handle, uint64_t expected, uint64_t pane,
    uint64_t offset, uint64_t bytes);
/* Import exact retained reader bytes across owner domains without reopening a
 * path. Caller holds reader+native grant through return. start/end select the
 * original range. Repeated source identities reuse the receiving capture. */
char *fcb_desk_import_reader(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t reader, uint64_t start, uint64_t end);
/* Checkpoints contain ALL retained source and personal labels, unencrypted.
 * User selects an explicit new destination and disclosure cap. Existing files
 * are not overwritten. Save returns effect/bytes_written even on failure: a
 * late cancel/close is never a no-effects claim. Preserve/inspect partial files.
 * Restore imports verified bytes with fresh local identities and no live-path
 * reopening. Invalid/canceled restore keeps the prior desk state. */
char *fcb_desk_save(uint64_t handle, uint64_t expected, const char *path, uint64_t max_source_bytes);
char *fcb_desk_restore(uint64_t handle, uint64_t expected, uint64_t attempt, const char *path);
uint8_t fcb_desk_cancel(uint64_t handle);
uint8_t fcb_desk_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
