#ifndef FCB_ATLAS_STRUCTURE_H
#define FCB_ATLAS_STRUCTURE_H

#include <stdint.h>
#include "fcb_atlas_search.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Start a workspace symbol-name or Markdown-heading query. All source work
 * occurs in the existing fcb_atlas_search_step[_cancelable] worker calls, one
 * file per step. Existing search_page, overlay, focus, open_reader and clear
 * retain their contracts. Activation imports the exact matching capture and
 * recorded original_range, even if the live file changes or disappears.
 *
 * mode: 1=symbol candidates, 2=Markdown headings.
 * name_mode: 0=exact, 1=prefix, 2=contains; case-sensitive Unicode names/titles.
 * needle: nonempty UTF-8, <=1024 bytes, NUL-terminated. No text-search fallback.
 * extensions/filenames/path_kind/path: fcb_atlas_search_path_scoped contract.
 * Scope membership is frozen BEFORE source, file or result quotas.
 * max_matches/max_files: 1..4096; max_source_bytes: 1..32 MiB actual I/O work.
 * max_file_bytes: 1..1 MiB, intersected with the extractor's cap: 64 KiB for
 * symbols, 256 KiB for headings. Oversized/unsupported/refused files are explicit
 * unavailable coverage. Symbol languages currently follow the public library's
 * Rust/Python/JS/TS/Go/C-family candidate extractor. Headings use FrankenMarkdown
 * canonical titles, duplicate-safe slugs and original-source provenance.
 *
 * Admission returns schema fcb.atlas-search/1, command begin-structure. All
 * progress/page responses echo requested_mode, realized_mode, name_mode and
 * evidence_level. mode is symbol-name or markdown-heading. semantic_complete
 * is always false; search_complete covers admitted extraction work, never a
 * complete compiler-definition/reference inventory. Per-hit structure metadata
 * includes name, kind, language, candidate_id, heading_slug (null for symbols),
 * evidence_range and evidence_level. candidate_id is local to its file/capture;
 * activation still uses query_generation + hit_id. Original-byte capture digest,
 * length and path identity remain mandatory. Names/slugs are bounded to 4096
 * bytes each and total retained strings to 8 MiB; omissions make coverage
 * partial and are counted separately from stored rows.
 *
 * Every non-null string is owned by this library; call fcb_free_string once.
 * Inputs remain valid through return and are never retained. The C API creates
 * no worker/runtime and cannot establish native hardware qualification.
 */
char *fcb_atlas_structure_begin(uint64_t handle, uint64_t generation,
    uint64_t mode, uint64_t name_mode, const char *needle,
    const char *extensions, const char *filenames, uint64_t path_kind,
    const char *path, uint64_t max_matches, uint64_t max_files,
    uint64_t max_file_bytes, uint64_t max_source_bytes);

#ifdef __cplusplus
}
#endif
#endif
