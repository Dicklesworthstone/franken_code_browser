#ifndef FCB_ATLAS_IMPORTS_H
#define FCB_ATLAS_IMPORTS_H
#include <stdint.h>
#include "fcb_atlas_index.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Lexical source-import navigation over an explicitly prepared captured index.
 * This projection is separate from Markdown links and compiler semantics.
 * Source ranges are exact; target resolution uses a declared bounded
 * closed-catalog policy. Read each row's resolution and resolution_basis.
 * Package tooling, network and live source reopening are never invoked.
 *
 * All calls run on a host worker. Free every non-null reply exactly once with
 * fcb_free_string. Null or an error is never proof of an empty/complete graph.
 * Success schema: fcb.atlas-imports/1. Full-width numbers are decimal strings.
 */
#define FCB_ATLAS_IMPORTS_MAX_IMPORTS 4096
#define FCB_ATLAS_IMPORTS_MAX_PARSE_BYTES UINT64_C(33554432)
#define FCB_ATLAS_IMPORTS_MAX_PAGE 128
#define FCB_ATLAS_IMPORTS_MAX_PATH_HEX_BYTES 8192

/* Import generations must increase for each attempted admitted build. They are
 * independent from text queries, index builds and Markdown-link generations.
 * index_generation must name the currently accepted retained index.
 * max_imports is 1..4096; max_parse_bytes is 0..32 MiB of aggregate source input.
 * Zero input allowance cannot establish absence in unexamined source.
 * A failed/canceled replacement preserves the previous accepted inventory.
 */
char *fcb_atlas_imports_begin(uint64_t handle, uint64_t generation,
    uint64_t index_generation, uint64_t max_imports, uint64_t max_parse_bytes);

/* poll/context are borrowed through return, never retained. Zero continues;
 * nonzero cancels. poll must not unwind or reenter this atlas. */
char *fcb_atlas_imports_begin_cancelable(uint64_t handle, uint64_t generation,
    uint64_t index_generation, uint64_t max_imports, uint64_t max_parse_bytes,
    int32_t (*poll)(void *), void *context);

/* Advance at most one admitted source member. Results preserve occurrence IDs.
 * The shared upstream highlighter has bounded whole-file work; callback
 * cancellation cannot interrupt that synchronous call. In-progress,
 * completeness, unsupported/limited sources and row truncation are distinct.
 * A terminal retry does not repeat extraction. */
char *fcb_atlas_imports_step(uint64_t handle, uint64_t generation);
char *fcb_atlas_imports_step_cancelable(uint64_t handle, uint64_t generation,
    int32_t (*poll)(void *), void *context);

/* direction: 0=all, 1=outgoing imports, 2=incoming imports.
 * path_hex is null for all source files, or canonical lowercase hex of this
 * atlas's exact relative native path (1..4096 raw bytes), NUL-terminated.
 * Outgoing/incoming require a path; all plus path selects either endpoint.
 * No Unicode normalization, case substitution or lossy path conversion occurs.
 * start is the zero-based offset in the filtered inventory; limit is 1..128.
 * next_offset=null ends currently retained rows, not unexamined source.
 * Pages read retained metadata without parsing or source I/O.
 */
char *fcb_atlas_imports_page(uint64_t handle, uint64_t generation,
    uint64_t direction, const char *path_hex, uint64_t start, uint64_t limit);

/* side: 0=captured import evidence, 1=available captured target file.
 * reader must be an existing EMPTY fcb_reader_create handle. Accepted inventory,
 * retained index, source/target proof and destination ownership are checked.
 * initial_original_offset is the exact evidence start for side 0 and zero for
 * side 1. Evidence names lexical source, not a resolved compiler reference.
 * Missing/ambiguous/external/unsupported/uncaptured targets cannot be opened.
 * Index clear/replacement invalidates old import authority. Captured readers
 * already installed survive inventory/index/atlas retirement.
 * Cancellation may suppress the reply after receiver installation; inspect or
 * close that receiver rather than replaying the transfer into another reader.
 */
char *fcb_atlas_imports_open_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t import_id, uint64_t side);

#ifdef __cplusplus
}
#endif
#endif
