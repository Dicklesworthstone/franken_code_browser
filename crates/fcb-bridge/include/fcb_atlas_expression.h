#ifndef FCB_ATLAS_EXPRESSION_H
#define FCB_ATLAS_EXPRESSION_H
#include <stdint.h>
#include "fcb_atlas_search.h"
#include "fcb_atlas_index.h"
#ifdef __cplusplus
extern "C" {
#endif

#define FCB_ATLAS_EXPRESSION_MAX_BYTES UINT64_C(1024)
#define FCB_ATLAS_EXPRESSION_MAX_TOKENS UINT64_C(64)
#define FCB_ATLAS_EXPRESSION_MAX_SCAN_BYTES UINT64_C(268435456)

/* Explicit advanced expressions through the shared first-party query parser:
 * whitespace-separated terms are document-level AND; -term excludes a document;
 * double quotes preserve exact phrases (with quote/backslash escapes). path:
 * and lang:/type: predicates restrict membership; prefixed '-' excludes them.
 * A positive content term is required. Regex constructs are rejected explicitly.
 * Every result selects an occurrence of the primary term only after all whole-
 * document predicates have been verified in the SAME retained source capture.
 *
 * The existing file-type/path/visible scope INTERSECTS expression predicates;
 * expressions never expand granted membership or provide execution authority.
 * case_sensitive is 0/1 with the existing declared Unicode lowercase semantics.
 * All limits, nullable scope CSVs, raw visible-path encoding and path_kind codes
 * match fcb_atlas_search_begin_matching. expression is UTF-8 <=1024 bytes and
 * readable until NUL or 1025 bytes, whichever is first. Other strings are
 * NUL-terminated UTF-8 stable until return. No foreign borrow is retained.
 *
 * These admit worker-side resumable work, with zero source I/O at begin. Resume
 * through fcb_atlas_search_step and use ordinary page/overlay/focus/open_reader.
 * Returned fcb.atlas-search/1 rows carry primary needle plus exact expression
 * and predicate evidence. Inspect search_in_progress, search_complete, omission
 * counts and stop_reason independently; an incomplete scan cannot prove NOT.
 * Generation, cancel/close, owned strings and lost-reply reconciliation follow
 * ordinary atlas search. Free every non-null response once with fcb_free_string.
 */

/* Capture I/O and predicate verification have separate quotas. max_scan_bytes
 * is 0..256 MiB; zero is an explicit no-proof/partial request. Repeated predicate
 * scans consume verification work. They do not silently borrow source-read
 * capacity or publish a prefix as a complete document.
 */
char *fcb_atlas_search_begin_expression(uint64_t handle, uint64_t generation,
    const char *expression, const char *extensions, const char *filenames,
    uint64_t path_kind, const char *path, uint64_t case_sensitive,
    uint64_t max_matches, uint64_t max_files, uint64_t max_file_bytes,
    uint64_t max_source_bytes, uint64_t max_scan_bytes);

/* Uses only the accepted index generation's retained captures. Source changes
 * after index preparation do not alter results or later reader activation.
 * max_scan_bytes has the same independent verification-work meaning as above.
 */
char *fcb_atlas_search_indexed_begin_expression(uint64_t handle, uint64_t generation,
    uint64_t index_generation, const char *expression, const char *extensions,
    const char *filenames, uint64_t path_kind, const char *path,
    uint64_t case_sensitive, uint64_t max_matches, uint64_t max_scan_bytes);

#ifdef __cplusplus
}
#endif
#endif
