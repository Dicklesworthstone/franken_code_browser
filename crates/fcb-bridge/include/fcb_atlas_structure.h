#ifndef FCB_ATLAS_STRUCTURE_H
#define FCB_ATLAS_STRUCTURE_H

#include <stdint.h>
#include "fcb_atlas_search.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Start a workspace declaration, Markdown-heading or whole-token occurrence query. All source work
 * occurs in the existing fcb_atlas_search_step[_cancelable] worker calls, one
 * file per step. Existing search_page, overlay, focus, open_reader and clear
 * retain their contracts. Activation imports the exact matching capture and
 * recorded original_range, even if the live file changes or disappears.
 *
 * mode: 1=symbol candidates, 2=Markdown headings, 4=whole-token occurrences.
 * Mode 3 is not a structural query; expression search has a separate entry point.
 * name_mode: 0=exact, 1=prefix, 2=contains; case-sensitive Unicode names/titles.
 * Occurrences require name_mode=0 and a single shared-engine token name.
 * needle: nonempty UTF-8, <=1024 bytes, NUL-terminated. No text-search fallback.
 * extensions/filenames/path_kind/path: fcb_atlas_search_path_scoped contract.
 * Scope membership is frozen BEFORE source, file or result quotas.
 * max_matches/max_files: 1..4096; max_source_bytes: 1..32 MiB actual I/O work.
 * max_file_bytes: 1..1 MiB, intersected with the extractor's cap: 64 KiB for
 * symbols, 256 KiB for headings, 512 KiB for occurrences. Oversized/unsupported/refused files are explicit
 * unavailable coverage. Symbol languages currently follow the public library's
 * Rust/Python/JS/TS/Go/C-family candidate extractor. Headings use FrankenMarkdown
 * canonical titles, duplicate-safe slugs and original-source provenance.
 *
 * Occurrences are exact, case-sensitive decoded token matches in UTF-8 or
 * BOM-marked UTF-16. Comments and string literals participate. ASCII letters,
 * digits, underscore and dollar form tokens; non-ASCII non-whitespace,
 * non-control runs stay intact conservatively. A query cannot begin with an
 * ASCII digit. No normalization or compiler binding/reference resolution is
 * performed; spelling and original byte ranges remain authoritative.
 *
 * Admission returns schema fcb.atlas-search/1, command begin-structure. All
 * progress/page responses echo requested_mode, realized_mode, name_mode and
 * evidence_level. mode is symbol-name, markdown-heading or identifier-occurrence. semantic_complete
 * is always false; search_complete covers admitted extraction work, never a
 * complete compiler-definition/reference inventory. Occurrence receipts also
 * state includes_comments_and_literals=true, compiler_resolved=false,
 * unicode_normalization=none, name_syntax=single-token-no-leading-ascii-digit,
 * and token_policy=ascii-alphanumeric-underscore-dollar-plus-nonascii-nonwhitespace-noncontrol-v1.
 * Per-hit structure metadata
 * includes name, kind, language, candidate_id, heading_slug (null for symbols/occurrences),
 * evidence_range and evidence_level. candidate_id is local to its file/capture;
 * activation still uses query_generation + hit_id. Occurrence rows have
 * kind=identifier-occurrence, language=text, evidence_level=whole-token-text-candidate,
 * and evidence_range equal to original_range. Original-byte capture digest,
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

/* Run the same qualified structural-name extraction against the exact sources
 * already retained by index_generation. No file is reopened and no trigram
 * negative removes a candidate: symbols, headings and occurrences inspect
 * every selected complete capture, including gram-quota fallback captures.
 * mode/name_mode/needle/scope follow fcb_atlas_structure_begin above.
 * max_matches: 1..4096; max_scan_bytes: 0..32 MiB of admitted extraction input.
 * The owning extractor's per-file cap still applies. A complete capture that
 * cannot fit the remaining extraction budget stays pending, with terminal
 * stop_reason=verification-byte-limit. Unsupported/oversized/refused extraction
 * is unavailable; files outside the retained capture scope or omitted during
 * index preparation stay pending. Narrowing scope may prove complete coverage.
 *
 * Admission command=begin-indexed-structure, search_strategy=retained-capture-structure.
 * Existing structure metadata and semantic_complete=false remain authoritative.
 * Index generation/manifest/capture scope are frozen in every receipt, even
 * after a replacement index. verification_source_bytes counts admitted full
 * extraction inputs; verified_files=scanned_files, skipped_by_index=0 and
 * fallback_files=0. source_bytes_read/read_calls remain zero. Examined files
 * also include capture-unavailable and unsupported/oversized captured members.
 * The existing worker step/cancel/page/overlay/open_reader route is shared.
 * Accepted hits retain their original source revision and survive index
 * replacement or clearing; unfinished old-index queries cannot resume on new
 * bytes. This is bounded extraction, not exhaustive compiler reference search.
 */
char *fcb_atlas_structure_indexed_begin(uint64_t handle, uint64_t generation,
    uint64_t index_generation, uint64_t mode, uint64_t name_mode,
    const char *needle, const char *extensions, const char *filenames,
    uint64_t path_kind, const char *path, uint64_t max_matches,
    uint64_t max_scan_bytes);

#ifdef __cplusplus
}
#endif
#endif
