#ifndef FCB_CITY_H
#define FCB_CITY_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

/* Retained, metadata-only City geometry over the EXISTING atlas footprints.
 * All calls are synchronous WORKER operations, including final close. The host
 * owns its event loop, Metal device, retained layout and source-open grants.
 * No source, filesystem or network access occurs in these services.
 * Every non-null result is complete fcb.city/1 JSON. Inspect status; free it
 * exactly once with this library's fcb_free_string. Null means no handoff.
 * All integer wire fields are canonical decimal STRINGS. Float geometry and
 * the matrix are JSON numbers. Matrix is column-major Metal clip space, z0..1.
 * There are at most four live/initializing/retiring cells, with <=20,000 files
 * per scene; handles are opaque and never reused. Zero from create is refusal.
 */
uint64_t fcb_city_create(void);

/* Open once. layout is nonzero and meaningful only in this owner. For count>0,
 * ids/observed_bytes/known must contain count initialized aligned elements;
 * rects contains count*4 doubles [x,y,width,height]. Keep them immutable/live
 * through return. All pointers may be null for count0. IDs must be unique and
 * nonzero, rectangles finite/nonempty. known[i] and discovery_complete are 0/1.
 * A known zero is an empty-file observation; unknown is not silently zero.
 * The scene response contains instances [file_id,rect,height,observed_bytes],
 * the named capped-log-observed-bytes metric/cap and explicit coverage counts.
 * GPU rects are rebased around the returned origin; the original footprints
 * and IDs remain authoritative. This transform never repacks the atlas.
 * info replays the scene if the open handoff was lost. It does not open again.
 */
char *fcb_city_open(uint64_t handle, uint64_t layout, const uint64_t *ids,
    const double *rects, const uint64_t *observed_bytes, const uint8_t *known,
    uint64_t count, uint32_t discovery_complete);
char *fcb_city_info(uint64_t handle);

/* Candidate generations strictly increase, including failed attempts. Camera
 * centers use ORIGINAL world coordinates; scale is logical points/world unit;
 * yaw/tilt are radians. A view contains the checked Metal matrix, viewport and
 * camera. It is not a displayed frame. Pan/zoom/orbit/resize reference a retained
 * generation (last8 candidates or current presentation); there is no native
 * inverse projection to duplicate. Fit includes elevated extents with padding.
 */
char *fcb_city_view(uint64_t handle, uint64_t generation, double width, double height,
    double center_x, double center_y, double scale, double yaw, double tilt);
char *fcb_city_fit(uint64_t handle, uint64_t generation, double width, double height);
char *fcb_city_pan(uint64_t handle, uint64_t generation, uint64_t base, double dx, double dy);
char *fcb_city_zoom(uint64_t handle, uint64_t generation, uint64_t base, double x, double y, double factor);
char *fcb_city_orbit(uint64_t handle, uint64_t generation, uint64_t base, double yaw, double tilt);
char *fcb_city_resize(uint64_t handle, uint64_t generation, uint64_t base, double width, double height);

/* Acknowledge only after the actual native presentation callback. frame is
 * positive/monotonic; display is the positive current surface generation.
 * Repeating the identical acknowledgment is idempotent. Older generations
 * cannot replace a newer presentation. native_presented:false in bridge JSON
 * means the bridge cannot itself observe the GPU; acknowledgment:host-declared
 * records the host's assertion. Preparing another view never changes picking.
 * pick uses EXACTLY the acknowledged frame/display and logical viewport points.
 * A hit is a frontmost cuboid ID, not a text position or filesystem grant.
 * Resolve it only through that layout's retained metadata and existing reader.
 */
char *fcb_city_present(uint64_t handle, uint64_t generation, uint64_t frame, uint64_t display);
char *fcb_city_pick(uint64_t handle, uint64_t frame, uint64_t display, double x, double y);

/* Cancel revokes pending candidates while preserving accepted visible geometry.
 * Close revokes authority immediately; in-flight calls keep their admission
 * until drained. Neither invalidates returned strings nor independent readers.
 * Return1 on success,0 when unknown/busy. Close on a host worker.
 */
uint32_t fcb_city_cancel(uint64_t handle);
uint32_t fcb_city_close(uint64_t handle);
#ifdef __cplusplus
}
#endif
#endif
