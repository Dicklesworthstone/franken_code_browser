#ifndef FCB_SNAPSHOT_COMPARISON_H
#define FCB_SNAPSHOT_COMPARISON_H
#include <stdint.h>
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/*
 * Compare two explicitly chosen FCBS archives. This is local captured-source
 * comparison, not Git history, rename detection or compiler analysis.
 *
 * The registry admits at most two comparison owners, including retiring owners
 * still held by entered calls. Handles share the process/library allocator with
 * readers and saved repositories; handles from other registries cannot alias.
 * create returns zero on refusal. Each owner opens both archives exactly once
 * under the host's default SnapshotLimits: at most 65,536 members, 2,048 path
 * bytes per member, 8 MiB total path bytes, 1 MiB per captured member, 64 MiB
 * total source bytes and 80 MiB per archive. A failed open leaves it retryable.
 *
 * All source work/reclamation belongs on a worker. Operations on one busy owner
 * fail without waiting. Returned JSON owns its bytes independently: release
 * every non-null pointer, including errors, once with fcb_free_string(). Null
 * indicates panic or output allocation/encoding failure, never an empty result.
 * Wire schema: fcb.snapshot-comparison/1. IDs/offsets/counts are decimal strings.
 *
 * Paths are readable NUL-terminated UTF-8 with 1..4096 bytes before NUL, stable
 * through return. Member paths in responses retain reversible raw-byte payloads;
 * escaped display strings are never source-opening authority.
 */
typedef int32_t (*FcbSnapshotComparisonCancelPoll)(void *context);
uint64_t fcb_snapshot_comparison_create(void);
char *fcb_snapshot_comparison_open(uint64_t handle, const char *before_path,
                                  const char *after_path);
/*
 * A poll returns nonzero to cancel. It and context are borrowed only until the
 * synchronous call returns; never retained or called from a detached worker.
 * Poll must not unwind, destroy its context, or reenter the same comparison.
 */
char *fcb_snapshot_comparison_open_cancelable(uint64_t handle, const char *before_path,
                                             const char *after_path,
                                             FcbSnapshotComparisonCancelPoll poll, void *context);
char *fcb_snapshot_comparison_info(uint64_t handle);

/* Open does not begin classification. Generations are nonzero and strictly
 * increasing across attempts. Failed/canceled attempts preserve accepted rows.
 * Step admits 1..1024 merged paths; begin/step/page use metadata only. */
char *fcb_snapshot_comparison_begin(uint64_t handle, uint64_t generation);
char *fcb_snapshot_comparison_begin_cancelable(uint64_t handle, uint64_t generation,
                                              FcbSnapshotComparisonCancelPoll poll, void *context);
char *fcb_snapshot_comparison_step(uint64_t handle, uint64_t generation, uint64_t max_entries);
char *fcb_snapshot_comparison_step_cancelable(uint64_t handle, uint64_t generation,
                                             uint64_t max_entries,
                                             FcbSnapshotComparisonCancelPoll poll, void *context);

/* Filter: 0 all, 1 differences, 2 unchanged, 3 changed, 4 added, 5 removed,
 * 6 only-before, 7 only-after, 8 unavailable. Page limit is 1..128.
 * IDs are stable across filters. Partial catalogs use only-before/only-after
 * instead of claiming an addition/removal from an unobserved opposite side. */
char *fcb_snapshot_comparison_page(uint64_t handle, uint64_t generation, uint64_t filter,
                                  uint64_t start, uint64_t limit);

/* reader is an existing EMPTY handle from fcb_reader_create() in this library
 * instance. Side: 0 before, 1 after. Entry and archive identities are rechecked;
 * absent/unavailable/corrupt sides fail without replacing destination state.
 * Exact selected member bytes are verified from the open archive, never the
 * original source path. Success creates an independent retained reader; closing
 * either archive/comparison later cannot invalidate bytes already transferred.
 * Cancellation after installation does not roll accepted reader state back;
 * reader_info can reconcile acceptance, or the caller can close its handle. */
char *fcb_snapshot_comparison_open_reader(uint64_t handle, uint64_t reader,
                                         uint64_t generation, uint64_t entry_id, uint64_t side);
/* Cancel is nonblocking and preserves accepted comparison results. Close
 * revokes this owner only. Existing readers and returned strings are separate.
 * Both return 1 on success, 0 for unknown/closed/refused handles. */
uint8_t fcb_snapshot_comparison_cancel(uint64_t handle);
uint8_t fcb_snapshot_comparison_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
