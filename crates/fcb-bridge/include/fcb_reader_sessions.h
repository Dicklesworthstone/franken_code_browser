#ifndef FCB_READER_SESSIONS_H
#define FCB_READER_SESSIONS_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FCB_READER_MAX_SOURCE_BYTES UINT64_C(33554432)
#define FCB_READER_MAX_WINDOW_BYTES UINT64_C(262144)
#define FCB_READER_MAX_SELECTION_WINDOW_BYTES UINT64_C(262160)
#define FCB_READER_MAX_CONTEXT_BYTES UINT64_C(16384)
#define FCB_READER_MAX_MATCHES UINT64_C(4096)
#define FCB_READER_MAX_NEEDLE_BYTES UINT64_C(4096)

/*
 * Retained SINGLE-FILE reader sessions; no runtime or worker is created.
 * create reserves one of eight live/retiring handles and performs no I/O.
 * open captures one complete regular file on the calling host worker. A loaded
 * handle never reopens or replaces that source, even after working-tree edits.
 * Failed initialization can be retried while the handle remains empty.
 * max_source_bytes is 1..32 MiB; larger files are refused without publishing
 * a prefix. This retains a bounded complete capture, not out-of-core storage.
 * Legacy whole-text and supplied-buffer transfers remain capped at 4 MiB;
 * comparison, desk, outline and document services retain their own limits.
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
 * Info includes capture_sha256: lowercase SHA-256 of the complete original
 * bytes, computed once at capture admission and retained across forks. The
 * digest is an integrity witness, not a file ID or an access grant.
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
 * The additive highlight object carries capture-derived, page-local UTF-16
 * token runs (at most 8192) or an explicit plain-text fallback. Validate the
 * enclosing owner/capture and full run coverage before styling; token offsets
 * are NOT original-byte copy positions. Lexing uses the complete retained
 * capture once, never an arbitrary page as a new file. No C ABI change.
 * This is logical text, NOT measured/shaped native font or bidi geometry. */
char *fcb_reader_window(uint64_t handle, uint64_t offset, uint64_t bytes);

/* Physical source lines: first >= 1, 1 <= count <= 1024, bounded by max_bytes.
 * CR/LF/CRLF are terminators; a trailing terminator creates no phantom line.
 * Oversized lines return a range_limited window, not a full-line claim. */
char *fcb_reader_lines(uint64_t handle, uint64_t first, uint64_t count, uint64_t max_bytes);

/* Exact one-based physical line and logical column. column_kind is 1 for
 * Unicode scalars, 2 for UTF-16 code units, or 3 for decoded UTF-8 bytes.
 * Tabs count as one logical unit; these are not visual or grapheme columns.
 * Leading BOM and line terminators are excluded. The content-end caret is
 * valid; missing lines, past-end columns and interior scalar/surrogate offsets
 * fail without clamping. max_bytes has the same bounds as a byte window.
 * The capture-bound page has command "position", selection null and a position
 * receipt containing line, column, column_kind, original_offset and
 * window_utf8_offset. Integer fields are canonical decimal strings. The
 * receipt proves a logical source position, not a native shaped caret. */
char *fcb_reader_position(uint64_t handle, uint64_t line, uint64_t column,
    uint64_t column_kind, uint64_t max_bytes);

/* Exact case-sensitive literal matching. Generations must strictly increase
 * across attempts, including canceled scans. Retained rows are indexed from 0.
 * needle is nonempty UTF-8, at most MAX_NEEDLE_BYTES. The allowance includes
 * original spellings of case-insensitive workspace hits; Find remains exact.
 * max_matches <= MAX_MATCHES; zero is bounded lookahead, not exhaustive counting.
 * max_scan_bytes is original input work over the already captured source. */
char *fcb_reader_find(uint64_t handle, uint64_t generation, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes);

#define FCB_READER_FIND_IGNORE_CASE UINT64_C(1)
#define FCB_READER_FIND_COUNT_ALL UINT64_C(2)
/* Additive Find options; flags=0 preserves fcb_reader_find exactly. Rejects
 * unknown flag bits. IGNORE_CASE uses Rust Unicode lowercase comparison, not
 * locale rules, full case folding or canonical normalization. Each insensitive
 * row includes matched_text and original_hex for its own original source range;
 * the query text is never substituted for that spelling. literal_original_hex
 * is null for insensitive results. Overlapping occurrences remain distinct.
 * COUNT_ALL continues within max_scan_bytes after retained rows fill, including
 * zero-row counting. match_count_complete/search_complete remain false after
 * byte exhaustion or unsupported decoding. rows_truncated is independent.
 * Responses name case_sensitive, count_all_matches and retained_limit. Long
 * needles may lower retained_limit to bound the complete response and witness
 * storage. max_matches=0 requires COUNT_ALL on the IGNORE_CASE route.
 * Cancellation/failure preserves the previously accepted query and capture. */
char *fcb_reader_find_with_options(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t max_matches, uint64_t max_scan_bytes, uint64_t flags);
/* Page through literal occurrences without retaining every earlier row.
 * start_after is an occurrence count (0 selects the first page), NOT a source
 * byte offset; it must not exceed captured_bytes. Overlapping matches keep
 * their ordinary stream order. Each request replays the same immutable capture
 * from byte 0; max_scan_bytes includes the work spent skipping earlier matches.
 * No live-path read or source-byte cursor can replace this capture replay.
 *
 * flags and upper row/needle limits match find_with_options; max_matches=0
 * requires COUNT_ALL for every paged query. Each attempted page requires a
 * strictly newer generation, even when needle/options are unchanged.
 * Acceptance replaces retained rows: index 0 now means this page's
 * first row, and old generations cannot select or copy any of the new rows.
 * Failed/canceled pages preserve previously accepted rows.
 *
 * Responses add page_start_after (requested ordinal), skipped_matches (actual
 * occurrences skipped) and has_more_matches (a later occurrence was observed).
 * matches_seen remains GLOBAL from capture start, including skipped matches;
 * retained_hits counts only this page. COUNT_ALL may finish counting beyond it.
 * has_more_matches=false alone is not an exhaustive-search claim: always inspect
 * search_complete/match_count_complete and the scan state. An ordinal beyond
 * the final occurrence yields an empty page when the scan reaches the end. */
char *fcb_reader_find_page(uint64_t handle, uint64_t generation, const char *needle,
    uint64_t start_after, uint64_t max_matches, uint64_t max_scan_bytes, uint64_t flags);
char *fcb_reader_hit(uint64_t handle, uint64_t generation, uint64_t index, uint64_t context_bytes);

/* Exact byte exports as JSON hex; no clipboard or filesystem write. The range
 * is half-open and cannot exceed MAX_WINDOW_BYTES. NUL/malformed/scalar-partial
 * source remains lossless. A hit uses its accepted generation, never newer rows. */
char *fcb_reader_copy_range(uint64_t handle, uint64_t start, uint64_t end);
char *fcb_reader_copy_hit(uint64_t handle, uint64_t generation, uint64_t index);

/* Resolve one nonempty native text selection within a retained reader page.
 * window_start/end are its accepted visible ORIGINAL-byte bounds, at most
 * MAX_SELECTION_WINDOW_BYTES; utf8_start/end are half-open DECODED UTF-8
 * offsets in that page's logical text, not original bytes or UTF-16 units.
 * Native hosts must first validate their grapheme/UTF-16 selection and bind the
 * receipt to the exact presented page, source identity and navigation request.
 * A reconstructed page must have exactly these visible bounds. Split scalars,
 * empty/reversed/past-end ranges and selected source > MAX_WINDOW_BYTES fail.
 * No new source I/O or query/outline/reference generation mutation occurs.
 *
 * Success uses command "resolve-selection", selection_namespace "native-text",
 * window_original_range, window_utf8_range, original_range, selected decoded
 * text, exact original_hex, and contains_replacements (selected text only).
 * The usual owner/file_id/source_revision/path/captured_bytes/encoding identify
 * the immutable capture; all integer coordinates are canonical decimal strings.
 * A real U+FFFD is not a malformed replacement. Neither Unicode normalization
 * nor line-ending normalization is applied. This returns data only: it never
 * writes the clipboard, creates an annotation or exports a file. */
char *fcb_reader_resolve_selection(uint64_t handle, uint64_t window_start,
    uint64_t window_end, uint64_t utf8_start, uint64_t utf8_end);

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
