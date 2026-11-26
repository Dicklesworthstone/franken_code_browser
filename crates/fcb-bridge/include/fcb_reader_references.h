#ifndef FCB_READER_REFERENCES_H
#define FCB_READER_REFERENCES_H
#include <stdint.h>
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

#define FCB_READER_REFERENCE_SOURCE_BYTES UINT64_C(33554432)
#define FCB_READER_REFERENCE_NAME_BYTES UINT64_C(1024)
#define FCB_READER_REFERENCE_ITEMS UINT64_C(4096)
#define FCB_READER_REFERENCE_PAGE UINT64_C(128)

/* Whole-token TEXT occurrences in an existing OPEN reader's retained capture.
 * This is not compiler name/reference resolution. Comments and string literals
 * participate. Legacy entry points match exact case-sensitive spelling; the
 * matching-window entry point also supports Unicode scalar lowercase equality.
 * No normalization or full Unicode identifier claim is made. The token policy groups ASCII
 * letters/digits, '_' and '$', and conservatively keeps non-ASCII runs intact.
 * It never rescans or reopens a working-tree pathname during navigation.
 *
 * Every operation is worker work, not an input/redraw callback. The existing
 * fcb_reader_cancel/close apply. Strings use schema fcb.reader-session/1, with
 * canonical decimal strings for full-width integers. Inspect status and the
 * independent search_complete/count_complete/output_limited fields. Null means
 * marshaling/panic failure, not an empty result. Every non-null returned string
 * is owned and must be freed exactly once with fcb_free_string.
 */

/* Build a bounded inventory and return its first page of at most 64 records.
 * name must be nonempty NUL-terminated UTF-8 <=1024 bytes, readable and stable
 * until return. It must be one admitted token and cannot begin with an ASCII
 * digit. max_items is 0..4096; zero permits a one-occurrence lookahead only.
 * Complete retained sources up to the reader's 32 MiB capture ceiling are
 * supported. Decoder scratch is bounded independently of source size;
 * original bytes remain borrowed from the reader. Source capture/admission
 * limits still apply, and other analysis services retain their own limits.
 * Malformed/unsupported text fails explicitly and remains readable without
 * an occurrence inventory.
 *
 * Each attempt uses a strictly increasing reference generation independent of
 * ordinary Find, outline and document generations. Failed/canceled attempts
 * preserve the previous accepted inventory but consume an admitted generation.
 * fcb_reader_info exposes accepted_reference_generation and last_reference_attempt
 * for reconciliation if cancellation or delivery failure races acceptance.
 */
char *fcb_reader_references(uint64_t handle, uint64_t generation,
    const char *name, uint64_t max_items);

/* Retain a later occurrence window from the SAME immutable reader capture.
 * All existing source/name/generation/cancellation limits apply. result_offset
 * is a full-width count of preceding matches, not a byte or local page offset.
 * count_all_matches is strictly 0 or 1. With 1, scan the complete admitted source
 * and count every occurrence independently of the 0..4096 retained-row cap.
 * With 0, count skipped/retained matches plus at most one later lookahead.
 * No skipped prefix rows are stored, and no live source pathname is opened.
 *
 * The normal references response adds result_offset, count_all_matches,
 * skipped_matches, rows_truncated, has_more_matches and next_match_offset.
 * count_complete/search_complete can be true while output_limited is true;
 * output_limited and has_more_matches mean a match follows this retained window.
 * next_match_offset is result_offset + retained_references when more exist and
 * at least one row was retained, otherwise null (including zero-row counting).
 * Each row adds its global one-based occurrence_id. Activation and exact copy
 * still require the returned window-local reference_id and reference generation.
 * fcb_reader_reference_page pages this accepted window with its existing local
 * next_offset; a later match window requires a fresh generation and this call.
 * The legacy fcb_reader_references response shape and behavior are unchanged.
 */
char *fcb_reader_references_window(uint64_t handle, uint64_t generation,
    const char *name, uint64_t max_items, uint64_t count_all_matches,
    uint64_t result_offset);

/* Choose the matching policy for a bounded occurrence window. case_sensitive
 * is strictly 0 or 1: 1 compares exact spelling; 0 compares each Unicode
 * scalar's lowercase expansion across complete original tokens. This is not
 * full case folding or normalization. All window/count/generation rules above
 * apply, including rejection of invalid boolean flags before reading name.
 *
 * name_case is "sensitive" or "unicode-lowercase" on the initial response and
 * every metadata page. Reference ranges, navigation and copied bytes always
 * identify the original source spelling, which can differ in UTF-8 byte length
 * from name. Existing exact entry points and their response shapes are unchanged.
 */
char *fcb_reader_references_window_matching(uint64_t handle, uint64_t generation,
    const char *name, uint64_t max_items, uint64_t case_sensitive,
    uint64_t count_all_matches, uint64_t result_offset);

/* Page the accepted inventory; limit is 1..128. start is a zero-based offset
 * and next_offset continues that inventory. reference_id is one-based within
 * the accepted generation and is NOT a row offset or a symbol/search-hit ID.
 * Without count_all_matches, a limited inventory's matches_counted includes at
 * most one unstored lookahead, never an exhaustive total. line values count LF,
 * including CRLF. Window count metadata and occurrence_id survive paging.
 */
char *fcb_reader_reference_page(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);

/* Bounded original-byte and window-UTF-8 selection in the SAME retained source.
 * context_bytes is 0..16384. Selection has namespace "reference", generation,
 * reference_id and reference_line, plus original_range/window_utf8_range/hex.
 * These are exact logical source coordinates, not native shaped geometry.
 */
char *fcb_reader_reference(uint64_t handle, uint64_t generation,
    uint64_t reference_id, uint64_t context_bytes);

/* Original occurrence bytes as hex. No clipboard or filesystem effect. */
char *fcb_reader_copy_reference(uint64_t handle, uint64_t generation,
    uint64_t reference_id);

/* Consume a fresh reference generation and release only the derived inventory.
 * Captured source, Find/outline/document state and returned strings survive.
 */
char *fcb_reader_references_clear(uint64_t handle, uint64_t generation);

#ifdef __cplusplus
}
#endif
#endif
