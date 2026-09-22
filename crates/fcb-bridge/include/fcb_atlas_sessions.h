#ifndef FCB_ATLAS_SESSIONS_H
#define FCB_ATLAS_SESSIONS_H
#include <stdint.h>
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Retained metadata atlas, not a source snapshot. Explicit synchronous worker
 * services: do not discover, encode, activate source, or finally destroy large
 * geometry on the native input/redraw thread. No executor is created here.
 * All non-null returned strings are complete fcb.atlas-session/1 JSON; inspect
 * status, discovery_complete and detail_limited rather than treating non-null
 * as success. Free strings exactly once using this library's fcb_free_string.
 * Null means no complete handoff. Foreign pointers must be valid UTF-8 C strings
 * stable until return; invalid pointer memory is not recoverable.
 *
 * At most four live/initializing/retiring atlas cells. Handles are opaque, not
 * pointers, never reused in this loaded library, and distinct from reader IDs.
 * Zero from create means refusal. Open is metadata-only, once per handle;
 * max_files is 1..20000 (4096 recommended), with separate shared path/page caps.
 * Partial discovery remains usable and explicitly incomplete. Initial viewport
 * is 1024x768 points at scale 1. Resize creates a new display generation.
 */
uint64_t fcb_atlas_create(void);
char *fcb_atlas_open(uint64_t handle, const char *root, uint64_t max_files);
char *fcb_atlas_info(uint64_t handle);

/* Plan generations strictly increase across accepted or failed admitted
 * attempts. A returned plan is a CANDIDATE, not acknowledgment of native pixels.
 * Coordinates/deltas are logical viewport points. Focus uses a layout ordinal
 * from this session's plan/tree. Back restores a bounded focus/camera history.
 * Pan/zoom do not append history, rediscover files or repack geometry.
 */
char *fcb_atlas_view(uint64_t handle, uint64_t generation);
char *fcb_atlas_pan(uint64_t handle, uint64_t generation, double dx, double dy);
char *fcb_atlas_zoom(uint64_t handle, uint64_t generation, double x, double y, double factor);
char *fcb_atlas_focus(uint64_t handle, uint64_t generation, uint64_t node);
char *fcb_atlas_back(uint64_t handle, uint64_t generation);
char *fcb_atlas_resize(uint64_t handle, uint64_t generation, double width, double height, double scale);

/* Call only when the host conservatively knows this plan was displayed. Frame
 * IDs strictly increase; display is the generation of the actual host surface.
 * This is a HOST DECLARATION, not OS/GPU observation by the bridge. Preparing a
 * newer candidate never replaces old picking geometry before acknowledgment.
 * A superseded, never-acknowledged candidate cannot be presented later.
 */
char *fcb_atlas_present(uint64_t handle, uint64_t generation, uint64_t frame, uint64_t display);
char *fcb_atlas_pick(uint64_t handle, uint64_t frame, uint64_t display, double x, double y);

/* Metadata-only conventional tree, limit 1..1024 rows, raw reversible paths.
 * Root ordinal is returned by initial info. next_offset is a string or null.
 */
char *fcb_atlas_children(uint64_t handle, uint64_t parent, uint64_t start, uint64_t limit);

/* Explicit source action on the ACKNOWLEDGED frame; aggregate hits are refused.
 * reader is an already-created EMPTY fcb_reader_create handle. Its admission
 * and operation lock are acquired before capture. Uses the existing bounded
 * regular-file reader, up to 4 MiB. On success all fcb_reader_* methods operate
 * on that retained source, without reopening the path. This is a new source
 * observation after metadata selection, not bytes pinned by atlas discovery.
 * Late cancellation/handoff failure can follow reader installation; reconcile
 * the known destination with fcb_reader_info/close, never assume rollback.
 */
char *fcb_atlas_open_reader(uint64_t handle, uint64_t reader, uint64_t frame,
    uint64_t display, double x, double y, uint64_t max_source_bytes);

/* Explicit file-type display scope: "all", "markdown", "python", "rust", or
 * "extensions" with a comma-separated custom list in extensions (for example
 * "md,toml"; ASCII case-insensitive; 1..16 tokens of 1..16 bytes; no dots or
 * separators inside a token). The NEXT plan may repack geometry from the
 * frozen catalog on this worker; pan/zoom never repack, and no source is read
 * or parsed for any scope change. Restoring "all" reuses the retained original
 * layout, so earlier All-frame geometry and node identity stay valid. A scope
 * change resets focus/history/selection to the new layout's root. Search and
 * path queries remain workspace-wide; their responses label the scope, and
 * paging/overlay encode only in-scope rows while counts stay workspace-wide.
 * Focus of a retained hit whose file is out of scope is refused.
 */
char *fcb_atlas_scope(uint64_t handle, uint64_t generation,
    const char *scope, const char *extensions);

/* Return 1 if changed/removed, 0 if unknown/busy. Cancellation advances an epoch
 * without locking source work. Close invalidates the atlas while active calls
 * retain admission until drained. It does not close independent readers or free
 * returned strings. Hosts retain responsibility for current native root grants.
 */
uint8_t fcb_atlas_cancel(uint64_t handle);
uint8_t fcb_atlas_close(uint64_t handle);
#ifdef __cplusplus
}
#endif
#endif
