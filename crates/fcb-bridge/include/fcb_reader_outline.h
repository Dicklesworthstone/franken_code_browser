#ifndef FCB_READER_OUTLINE_H
#define FCB_READER_OUTLINE_H
#include <stdint.h>
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* All operations use an existing OPEN reader handle, including a reader opened
 * from a retained atlas search hit. They never reopen source or change capture.
 * Call on a host worker; extraction is not an input/redraw callback operation.
 * Inspect every response's status. Null means marshaling/panic failure; non-null
 * may be an error. Free each non-null string once with fcb_free_string.
 * Schema: fcb.reader-session/1. Full-width integers are decimal JSON strings.
 */

/* Build once; first page contains at most 64 candidates. max_items: 1..4096.
 * language: null to infer the retained label suffix, or a supported UTF-8 name:
 * rust, python, javascript, typescript, go, cpp (existing aliases also accepted).
 * The supplied NUL-terminated string must remain readable until return.
 * Source AND decoded text are capped at 64 KiB by the existing extractor;
 * unsupported/oversized/too-complex sources remain readable without an outline.
 * Failed/canceled replacement preserves prior outline and content-search state.
 * Use a strictly increasing outline generation for each admitted attempt.
 * This generation is INDEPENDENT of content-query generations.
 * Candidates are heuristic, never compiler definitions or exhaustive semantics.
 */
char *fcb_reader_outline(uint64_t handle, uint64_t generation,
    const char *language, uint64_t max_items);

#define FCB_SYMBOL_NAME_EXACT 0
#define FCB_SYMBOL_NAME_PREFIX 1
#define FCB_SYMBOL_NAME_CONTAINS 2

/* Filter the already-extracted inventory; no parsing or source I/O. Names are
 * case-sensitive. needle: null/empty lists all; otherwise UTF-8 <=256 bytes,
 * NUL-terminated and stable until return. name_mode must be one of the above.
 * limit: 1..128. start is a zero-based offset in the FILTERED view; next_offset
 * continues that view. Activate by returned symbol_id, never by a row index.
 * output_limited and semantic_complete remain distinct from page continuation.
 */
char *fcb_reader_symbols(uint64_t handle, uint64_t generation,
    const char *needle, uint8_t name_mode, uint64_t start, uint64_t limit);

/* Exact original-byte AND decoded window-UTF-8 selection with source context.
 * context_bytes: 0..16384. IDs are one-based within the accepted outline.
 * Selection carries outline_generation and symbol_id, not query_generation.
 * A missing name_range selects the explicitly recorded declaration evidence.
 * Logical text/selection is not a claim of native shaping or presentation.
 */
char *fcb_reader_symbol(uint64_t handle, uint64_t generation,
    uint64_t symbol_id, uint64_t context_bytes);

/* Exact original bytes as hex, NOT a native clipboard write. whole_declaration:
 * 0 = exact name (or disclosed evidence fallback), 1 = declaration evidence.
 * Other values are refused. The evidence span is not a semantic function body.
 */
char *fcb_reader_copy_symbol(uint64_t handle, uint64_t generation,
    uint64_t symbol_id, uint8_t whole_declaration);

/* Release derived outline only, consuming a fresh outline generation. Literal
 * search results, captured bytes and previously returned strings survive.
 * Existing fcb_reader_cancel/close apply. Cancellation after acceptance suppresses
 * delivery but does not roll back state; fcb_reader_info exposes accepted outline
 * generation for reconciliation. Closing must happen on a host worker.
 */
char *fcb_reader_outline_clear(uint64_t handle, uint64_t generation);

#ifdef __cplusplus
}
#endif
#endif
