#ifndef FCB_READER_SESSIONS_H
#define FCB_READER_SESSIONS_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FCB_READER_MAX_SOURCE_BYTES UINT64_C(4194304)
#define FCB_READER_MAX_WINDOW_BYTES UINT64_C(262144)
#define FCB_READER_MAX_CONTEXT_BYTES UINT64_C(16384)
#define FCB_READER_MAX_MATCHES UINT64_C(4096)

/*
 * Retained SINGLE-FILE reader sessions; no runtime or worker is created.
 * create reserves one of eight live/retiring handles and performs no I/O.
 * open captures one complete regular file on the calling host worker. A loaded
 * handle never reopens or replaces that source, even after working-tree edits.
 * Failed initialization can be retried while the handle remains empty.
 *
 * Handles are opaque integers, not pointers. Never reuse a handle after close
 * or move it between processes/loaded library instances. JSON's owner is this
 * handle; match it and the query generation before accepting delayed output.
 * Separate one-shot workspace/atlas calls still have independent observations.
 *
 * Every non-null string argument must be readable NUL-terminated UTF-8, stable
 * until return. Null or invalid UTF-8 arguments produce structured errors when
 * a response can be handed off. Invalid/dangling foreign pointers are NOT safe.
 * Every non-null returned string must be released once by fcb_free_string from
 * the same library. Closing a handle does not free previously returned strings.
 *
 * Responses use fcb.reader-session/1. status=error, search_complete, range_limited
 * and boundaries_adjusted must be inspected; non-null alone is not success.
 * Unwinding service panics return NULL. Aborts cannot be recovered here.
 * All source, search, copy and close operations belong on a worker, not redraw.
 * cancel never waits on source/query work; concurrent operations on the same
 * handle return READER_HANDLE_BUSY instead of queuing or blocking.
 */
uint64_t fcb_reader_create(void); /* 0 on admission/identity failure. */
char *fcb_reader_open(uint64_t handle, const char *path, uint64_t max_source_bytes);
char *fcb_reader_info(uint64_t handle);

/* Original-byte windows: 4 <= bytes <= MAX_WINDOW_BYTES. Text uses the shared
 * UTF-8/BOM-UTF-16 decoder with exact raw hex and explicit replacement flags.
 * This is logical text, NOT measured/shaped native font or bidi geometry. */
char *fcb_reader_window(uint64_t handle, uint64_t offset, uint64_t bytes);

/* Physical source lines: first >= 1, 1 <= count <= 1024, bounded by max_bytes.
 * CR/LF/CRLF are terminators; a trailing terminator creates no phantom line.
 * Oversized lines return a range_limited window, not a full-line claim. */
char *fcb_reader_lines(uint64_t handle, uint64_t first, uint64_t count, uint64_t max_bytes);

/* Exact case-sensitive literal matching. Generations must strictly increase
 * across attempts, including canceled scans. Retained rows are indexed from 0.
 * max_matches <= MAX_MATCHES; zero is bounded lookahead, not exhaustive counting.
 * max_scan_bytes is original input work over the already captured source. */
char *fcb_reader_find(uint64_t handle, uint64_t generation, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes);
char *fcb_reader_hit(uint64_t handle, uint64_t generation, uint64_t index, uint64_t context_bytes);

/* Exact byte exports as JSON hex; no clipboard or filesystem write. The range
 * is half-open and cannot exceed MAX_WINDOW_BYTES. NUL/malformed/scalar-partial
 * source remains lossless. A hit uses its accepted generation, never newer rows. */
char *fcb_reader_copy_range(uint64_t handle, uint64_t start, uint64_t end);
char *fcb_reader_copy_hit(uint64_t handle, uint64_t generation, uint64_t index);

/* cancel invalidates in-flight work, leaving the capture available afterward.
 * It is cooperative, not interruption of an OS syscall or rollback of already
 * accepted state. Retry only after the current worker call has returned.
 * close immediately invalidates the handle, retaining its admission until all
 * in-flight work releases it. Neither function frees returned JSON strings.
 * 1 = effect performed; 0 = invalid/busy/exhausted handle, no success claim. */
uint8_t fcb_reader_cancel(uint64_t handle);
uint8_t fcb_reader_close(uint64_t handle);
void fcb_free_string(char *result);

#ifdef __cplusplus
}
#endif
#endif
