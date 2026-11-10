#ifndef FCB_OBSERVED_READER_H
#define FCB_OBSERVED_READER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bounded observations of one explicitly opened regular file, including files
 * too large for a complete retained reader capture. These are synchronous
 * worker operations: never perform source I/O or final close on a redraw/input
 * callback. Opening is metadata-only. A byte-window request reads at most
 * 256 KiB plus 16 bytes of scalar/CRLF context and receives a fresh observation
 * identity. Line requests additionally scan a separately bounded prefix.
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

/* Locate physical lines by scanning from byte zero of the SAME opened file
 * object. Retain the selected bytes during that scan; never reread the located
 * range or reuse line checkpoints from an earlier observation. CR, LF and CRLF
 * terminate physical lines; a final terminator does not add a phantom line.
 * Explicit encoding values are the same as window. No whole-file capture or
 * per-line index is allocated.
 *
 * first_line is one-based; line_count is 1..4096. max_bytes is 4..262144 and
 * bounds retained selected source, with bounded decoder context in addition.
 * max_scan_bytes is 1..4294967296 (4 GiB); a separate read-call bound applies.
 * Success returns command "lines", a fresh retained observation and its
 * line_navigation provenance. Retained replay preserves that provenance
 * without source I/O. Metadata consistency is not an atomic snapshot promise.
 * Missing-at-EOF, scan/call/output limits, changed source and cancellation are
 * explicit errors that leave previously accepted observations intact. A limit
 * is never proof that the requested line does not exist.
 */
char *fcb_observed_reader_lines(uint64_t handle, uint64_t first_line,
    uint64_t line_count, uint64_t max_bytes, uint64_t max_scan_bytes,
    uint64_t encoding, fcb_observed_reader_poll poll, void *context);

/* No payload reread. Both commands require an exact retained observation ID.
 * Copy returns original bytes as hex, never silently truncated or decoded. */
char *fcb_observed_reader_retained(uint64_t handle, uint64_t observation);
char *fcb_observed_reader_copy(uint64_t handle, uint64_t observation,
    uint64_t start, uint64_t end);

/* Exact case-sensitive decoded-text search from byte zero of the SAME opened
 * file object. Runs on a worker, polling cooperatively between bounded reads.
 * generation is nonzero and strictly increases on each valid query attempt.
 * needle is NUL-terminated UTF-8, 1..4096 bytes; encoding has the window values.
 * max_matches is 0..256; max_scan_bytes is 0..4294967296 (4 GiB). A separate
 * 1048576-call bound also applies. A zero hit limit probes for one actual match;
 * a byte/call/hit limit or unsupported decoding reports incomplete coverage.
 *
 * A fresh search_observation identifies the continuous observed sequence, NOT
 * an atomic snapshot. Before/after metadata, scanned bytes, matches seen and
 * retained hits are separate fields. One accepted query and its literal
 * witness are retained independently of the eight window observations. Failed
 * or canceled replacements preserve the previous accepted query under its
 * original generation. A null/late reply does not prove state was unchanged.
 */
char *fcb_observed_reader_find(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t encoding, uint64_t max_matches,
    uint64_t max_scan_bytes, fcb_observed_reader_poll poll, void *context);

/* Zero-based hit_index must belong to the current accepted query generation.
 * Returns exactly the retained literal text/bytes and original-byte range,
 * with no source reread. Surrounding context was NOT retained. A host may
 * explicitly request a new window and verify the witness against those new
 * bytes; it must never label that window as the original search observation.
 */
char *fcb_observed_reader_search_hit(uint64_t handle, uint64_t generation,
    uint64_t hit_index);

/* Activate an accepted search hit by zero-based index. Context is freshly read
 * from the opened descriptor and its original hit bytes must equal the retained
 * literal witness. Changed/unavailable bytes return an error; old observations
 * remain readable. Success returns a NEW observation plus verified find_hit
 * provenance and page-local decoded UTF-8 selection. It never reuses the old
 * search observation's identity. Context and window sizes are bounded by the
 * source service; window_bytes is 4..262144 and must contain the full hit plus
 * eight bytes of scalar/CRLF boundary slack.
 * Retained-window navigation afterward performs no file I/O.
 */
char *fcb_observed_reader_observe_hit(uint64_t handle, uint64_t generation,
    uint64_t hit_index, uint64_t context_bytes, uint64_t window_bytes,
    fcb_observed_reader_poll poll, void *context);

/* Cancellation revokes in-flight delivery without waiting on source work.
 * Close removes authority immediately; its admission survives in-flight calls.
 * Returns 1 on success, 0 for unknown/busy/exhausted handles. */
uint8_t fcb_observed_reader_cancel(uint64_t handle);
uint8_t fcb_observed_reader_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
