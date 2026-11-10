#ifndef FCB_ATLAS_LINKS_H
#define FCB_ATLAS_LINKS_H
#include <stdint.h>
#include "fcb_atlas_index.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Documentation links/backlinks from an explicitly prepared atlas index.
 * This is captured Markdown parser evidence, not a compiler dependency graph.
 * Source and target identity belong to this atlas and this index generation.
 * No operation reads a live source, fetches a URL or grants additional access.
 * All calls run on a host worker. Free every non-null returned string exactly
 * once with fcb_free_string; null is failure, never proof of an empty graph.
 * Success/failure and completeness must be read from each response.
 */
#define FCB_ATLAS_LINKS_MAX_LINKS 4096
#define FCB_ATLAS_LINKS_MAX_PARSE_BYTES UINT64_C(33554432)
#define FCB_ATLAS_LINKS_MAX_PAGE 128
#define FCB_ATLAS_LINKS_MAX_PATH_HEX_BYTES 8192

/* Admit without parsing source. generation is independent of the ordinary
 * search/index attempt sequence and must increase for every attempted build,
 * including a failed/canceled attempt after host admission. index_generation
 * must name the currently retained index. max_links is 1..4096, max_parse_bytes
 * is 0..32 MiB of cumulative Markdown parsing input. Zero is an explicit
 * partial/no-proof inventory, except when no eligible documents need parsing.
 * The previous accepted inventory remains available if replacement fails.
 */
char *fcb_atlas_links_begin(uint64_t handle, uint64_t generation,
    uint64_t index_generation, uint64_t max_links, uint64_t max_parse_bytes);

/* Poll is called only on this worker; zero continues and nonzero cancels.
 * poll/context, when supplied, remain valid through return and are not retained.
 * Poll must not unwind or reenter the atlas. Cancellation is cooperative.
 */
char *fcb_atlas_links_begin_cancelable(uint64_t handle, uint64_t generation,
    uint64_t index_generation, uint64_t max_links, uint64_t max_parse_bytes,
    int32_t (*poll)(void *), void *context);

/* Advance at most one captured Markdown document. Results append with stable
 * link IDs. A terminal retry does not parse again. Running versus complete are
 * separate facts: unavailable, unsupported, limited or undiscovered sources
 * cannot establish a complete relationship inventory.
 */
char *fcb_atlas_links_step(uint64_t handle, uint64_t generation);
char *fcb_atlas_links_step_cancelable(uint64_t handle, uint64_t generation,
    int32_t (*poll)(void *), void *context);

/* direction: 0=all, 1=outgoing, 2=incoming/backlinks. path_hex is null for all
 * documents, or the canonical lowercase hex of an exact relative native path
 * with 1..4096 raw bytes (2..8192 ASCII bytes), NUL-terminated through return.
 * Outgoing/incoming require a path; all plus a path selects either endpoint.
 * Native non-UTF-8 path bytes are preserved. Paths must name this atlas's own
 * catalog member; display labels, normalized aliases and another atlas's IDs
 * cannot select it. The host enforces relative-path component validity.
 * start is a zero-based offset within the filtered result, limit is 1..128.
 * next_offset=null ends the CURRENT rows, not an inventory still being built.
 */
char *fcb_atlas_links_page(uint64_t handle, uint64_t generation, uint64_t direction,
    const char *path_hex, uint64_t start, uint64_t limit);

/* side: 0=whole captured Markdown evidence, 1=resolved captured target file.
 * reader is an existing EMPTY fcb_reader_create handle. The selected link ID
 * and generation, current retained index, exact source/target identity and
 * independent reader receipt are checked before transfer. External, missing,
 * unresolved and uncaptured targets are refused. For side=1, the selected
 * retained target's Markdown heading is qualified through the shared adapter.
 * fragment_status, fragment_slug, fragment_suffix, target_range and
 * initial_original_offset describe the result. Only status=resolved carries a
 * nonempty original-byte target_range and its start as the initial offset;
 * missing/ambiguous/invalid/limited/unsupported fragments open from offset 0.
 * Evidence side=0 always reports fragment_status=none and offset 0. The legacy
 * fragment_resolution describes the unqualified list row, not this navigation.
 * This temporary qualification publishes no reader preview/query generation.
 * Index clear/replacement invalidates old link authority. Readers already
 * installed own their bytes and survive graph/index/atlas retirement.
 * Late cancellation can suppress a reply after receiver installation: inspect
 * or close the known receiver, never silently open the current live file.
 */
char *fcb_atlas_links_open_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t link, uint64_t side);

#ifdef __cplusplus
}
#endif
#endif
