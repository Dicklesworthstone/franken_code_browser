#ifndef FCB_READER_REFERENCES_H
#define FCB_READER_REFERENCES_H
#include <stdint.h>
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

#define FCB_READER_REFERENCE_SOURCE_BYTES UINT64_C(524288)
#define FCB_READER_REFERENCE_NAME_BYTES UINT64_C(1024)
#define FCB_READER_REFERENCE_ITEMS UINT64_C(4096)
#define FCB_READER_REFERENCE_PAGE UINT64_C(128)

/* Whole-token TEXT occurrences in an existing OPEN reader's retained capture.
 * This is not compiler name/reference resolution. Comments and string literals
 * participate. Matching is exact and case-sensitive; no normalization or full
 * Unicode identifier claim is made. The shared Rust token policy groups ASCII
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
 * Sources larger than 512 KiB or malformed/unsupported text fail explicitly;
 * they remain readable without an occurrence inventory.
 *
 * Each attempt uses a strictly increasing reference generation independent of
 * ordinary Find, outline and document generations. Failed/canceled attempts
 * preserve the previous accepted inventory but consume an admitted generation.
 * fcb_reader_info exposes accepted_reference_generation and last_reference_attempt
 * for reconciliation if cancellation or delivery failure races acceptance.
 */
char *fcb_reader_references(uint64_t handle, uint64_t generation,
    const char *name, uint64_t max_items);

/* Page the accepted inventory; limit is 1..128. start is a zero-based offset
 * and next_offset continues that inventory. reference_id is one-based within
 * the accepted generation and is NOT a row offset or a symbol/search-hit ID.
 * A limited inventory's matches_counted includes at most one unstored lookahead,
 * never an exhaustive total. line values count LF, including CRLF.
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
