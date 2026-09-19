#ifndef FCB_READER_DOCUMENT_H
#define FCB_READER_DOCUMENT_H
#include <stdint.h>
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Retained Markdown on an existing OPEN reader, including a captured atlas hit.
 * Every call is synchronous HOST-WORKER work. Preparation/reflow calls the shared
 * FrankenMarkdown engine; viewport/heading/copy/navigation calls reuse its output.
 * No source path is reopened, no asset is fetched, and no link is activated.
 * No native font shaping, glyph-exact mapping or actual presentation is claimed.
 * Returned JSON uses fcb.reader-session/1. Check status; non-null can be an error.
 * Null means panic/marshaling refusal. Free each non-null string exactly once with
 * fcb_free_string, independently of reader lifetime. All integer JSON fields are
 * canonical decimal STRINGS. Existing reader cancel/close/ownership rules apply.
 */

/* Fresh independent document generation for each admitted prepare/reflow/clear.
 * Equal text-query and outline generations do not name this document's layout.
 * Failed/canceled preparation preserves the last accepted document and source.
 * Limits: width_columns 4..512; max_source_bytes 0..262144 (UTF-8, optional BOM);
 * max_flow_lines/max_flow_items 1..32768; max_blocks 1..8192. Zero source allowance
 * admits only an empty capture. These limits cannot bypass memory admission.
 * Defaults: 100, 65536, 8192, 8192, 4096. Unsupported UTF-16/malformed source is an
 * explicit preview refusal, not an empty document; the ordinary reader still works.
 * Returns the first <=64 flow rows and <=64 headings only after complete layout.
 * Reflow reparses the bounded retained source. Keep the old preview until success;
 * then restore place with an ORIGINAL source anchor, not an old rendered offset.
 */
char *fcb_reader_document(uint64_t handle, uint64_t generation,
    uint64_t width_columns, uint64_t max_source_bytes, uint64_t max_flow_lines,
    uint64_t max_flow_items, uint64_t max_blocks);

/* First is zero-based logical flow row/heading offset; count is 1..128. EOF is
 * a valid empty page. next_flow_line/next_heading are pagination, not incomplete
 * source coverage. Headings contain canonical upstream slugs and source regions.
 * Rows contain global rendered UTF-8 ranges and enclosing original regions;
 * rendered row ranges can include separators omitted from their trimmed text.
 */
char *fcb_reader_document_window(uint64_t handle, uint64_t generation,
    uint64_t first, uint64_t count);
char *fcb_reader_document_headings(uint64_t handle, uint64_t generation,
    uint64_t first, uint64_t count);

/* slug is null or valid NUL-terminated UTF-8, stable throughout this call.
 * Null/empty/unknown or >4096-byte slugs are refused. Pass the literal canonical
 * slug from this generation, without leading '#' or URL decoding. count 1..128.
 */
char *fcb_reader_document_heading(uint64_t handle, uint64_t generation,
    const char *slug, uint64_t count);

/* Original-byte source/search anchor -> first flow row of its enclosing region.
 * This is not an exact glyph/column. Unmapped bytes, BOM bytes and interior UTF-8
 * positions are refused; original EOF maps to the empty document window.
 */
char *fcb_reader_document_from_source(uint64_t handle, uint64_t generation,
    uint64_t original_offset, uint64_t count);

/* Preview selection -> original enclosing Markdown, with ordinary reader context
 * and an exact original-byte/decoded-source selection round trip. Start/end are
 * global rendered UTF-8 BYTES in this generation, not glyph/UTF-16/source offsets.
 * Nonempty range <=262144 bytes; context 0..16384. Selection is explicitly in the
 * document namespace and describes enclosing source, not per-glyph provenance.
 */
char *fcb_reader_document_source(uint64_t handle, uint64_t generation,
    uint64_t rendered_start, uint64_t rendered_end, uint64_t context_bytes);

/* Explicit copy DATA, never a native clipboard write or export file operation.
 * Mode 0 returns selected rendered UTF-8 text. Mode 1 returns the enclosing
 * original Markdown bytes as hex, including syntax and possibly other text.
 * Other modes are refused. Nonempty range <=262144 bytes; same offset domain as
 * document_source. Never silently truncates. Old layout generations are rejected.
 */
char *fcb_reader_document_copy(uint64_t handle, uint64_t generation,
    uint64_t rendered_start, uint64_t rendered_end, uint8_t mode);

/* Drop derived preview only on a worker. Source, search and outline survive.
 * A post-acceptance cancellation may suppress delivery without rolling it back;
 * fcb_reader_info reports accepted_document_generation for reconciliation.
 */
char *fcb_reader_document_clear(uint64_t handle, uint64_t generation);

#ifdef __cplusplus
}
#endif
#endif
