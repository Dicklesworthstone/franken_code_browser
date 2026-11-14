#ifndef FCB_ATLAS_STREAM_SEARCH_H
#define FCB_ATLAS_STREAM_SEARCH_H
#include <stdint.h>
#include "fcb_atlas_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Bounded streaming literal search over ALL files in an atlas's frozen base
 * catalog, independent of its display scope. Open the atlas first; native
 * callers may admit up to 20000 metadata entries, subject to path/page limits.
 * Exclusions and incomplete discovery remain explicit. This never claims an
 * atomic workspace snapshot or unlimited repository coverage.
 *
 * Exact decoded spelling in UTF-8 or BOM-marked UTF-16 LE/BE. No complete-file
 * capture size limit. needle is 1..1024 UTF-8 bytes; max_matches is 1..256;
 * max_scan_bytes is 1..4294967296; max_read_calls is 1..1048576. Recommended
 * defaults: 256 matches, 268435456 scan bytes, 1048576 read calls. Failed work
 * consumes I/O too. Source syscalls cannot be forcibly interrupted.
 *
 * The application assigns each attempt a fresh stream query generation and
 * per-file observation revisions. These identities are separate from captured
 * atlas searches. Returned fcb.atlas-stream-search/1 JSON labels this distinction.
 * Session failures use fcb.atlas-session/1. Inspect status, search_complete,
 * discovery_complete, file outcomes and stop_reason; non-null is not success.
 * Initial search returns at most 32 hits; page returns remaining retained rows.
 *
 * All calls are synchronous host-worker operations, never input/draw callbacks.
 * An entered search holds its session permit and the HOST must retain native
 * access until that call drains, including after cancel/close. poll/context are
 * borrowed until return only; no unwinding, reentry or callback retention.
 * needle must be readable until NUL or 1025 bytes, whichever comes first.
 * Each non-null response must be freed once with the same fcb_free_string.
 * Null means no complete handoff. Late cancellation or a lost reply may follow
 * committed replacement: revoke previous result actions or reconcile by query.
 */
char *fcb_atlas_stream_search_cancelable(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    int32_t (*poll)(void *context), void *context);

/* Additive scoped search. Legacy entry points above/below still select all
 * catalogued files. extensions/filenames follow captured-search scope syntax:
 * comma-separated ASCII tokens, case-insensitive; extensions omit the dot.
 * Both NULL means all types; either present (including "") means the union
 * of its extensions and exact basenames. Lists are at most 1024 bytes each.
 *
 * path_kind: 0 workspace (NULL or empty path), 1 directory, 2 exact file,
 * 3 frozen visible region. Directory/file paths are exact UTF-8 relative paths
 * up to 4096 bytes, without dot/repeated/leading/trailing components. Directory
 * matching respects component boundaries. Visible path is canonical sorted,
 * unique lowercase hex CSV of raw relative paths (up to 20000 paths and 1 MiB
 * encoded bytes); "" is an explicitly empty region. Only workspace accepts
 * NULL path. Invalid scope never widens membership or starts source I/O.
 * All pointers are readable through NUL or the appropriate byte limit plus
 * one, whichever comes first, and remain stable through the entered call.
 *
 * File types and location intersect BEFORE opening files or spending scan
 * limits. catalogued_files remains the original base catalog; eligible_files
 * + excluded_files equals it, and files_examined + files_unexamined equals
 * eligible_files. Original file/node IDs survive. search/progress/page/hit
 * echo search_scope, scope_extensions/filenames, scope_path_kind/path/path_hex,
 * case_sensitive=true and both file counts. Visible receipts also echo their
 * scope_region_sha256/files/path_bytes/catalogued_files/missing_files; missing
 * requested paths prevent search_complete even if all admitted files finished.
 */
char *fcb_atlas_stream_search_scoped_cancelable(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    const char *extensions, const char *filenames, uint64_t path_kind, const char *path,
    int32_t (*poll)(void *context), void *context);

/* Progressive variant of the same search. Each progress callback carries a
 * provisional first page (at most 32 finalized-file hits) with command=progress,
 * terminal=false, selection_available=false and a monotonically increasing
 * decimal-string progress_sequence. Active-file matches are never exposed.
 * Callbacks are bounded by scanner work and run on the entered worker.
 *
 * json is borrowed for the callback ONLY, has exactly length UTF-8 bytes, and
 * is NOT NUL terminated. Copy it before returning; do NOT call fcb_free_string
 * on it. Do not unwind, reenter the session or retain the pointer/context.
 *
 * poll requests hard cancellation; stop requests Stop and keep results. Stop
 * abandons the active file, preserves spent I/O and accepts finalized-file
 * evidence with stop_reason=user-stopped and search_complete=false. A hard
 * cancellation takes precedence. Provisional rows cannot authorize page/hit
 * actions; use the returned terminal generation after successful acceptance.
 * Terminal search/page reports have terminal=true, selection_available=true.
 * All callbacks are optional and borrowed until return. The final non-null
 * returned string alone is owned and must be freed once with fcb_free_string.
 */
typedef void (*fcb_atlas_stream_progress_callback)(
    const uint8_t *json, uint64_t length, void *context);
char *fcb_atlas_stream_search_progress(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    int32_t (*poll)(void *context), int32_t (*stop)(void *context),
    fcb_atlas_stream_progress_callback progress, void *context);

/* Same progress/Stop/cancellation contract, with the frozen scope above. */
char *fcb_atlas_stream_search_scoped_progress(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    const char *extensions, const char *filenames, uint64_t path_kind, const char *path,
    int32_t (*poll)(void *context), int32_t (*stop)(void *context),
    fcb_atlas_stream_progress_callback progress, void *context);

/* Opt-in counting variants. Reaching max_matches limits retained rows and
 * witness bytes only; scanning continues through eligible files until EOF,
 * an I/O budget, Stop or cancellation. Legacy entry points retain their
 * stop-at-first-overflow behavior. All callback pointers may be NULL.
 *
 * Summaries echo count_all_matches=true, matches_seen and matching_files for
 * finalized file observations, plus retained_matching_files for files with
 * retained rows. match_count_complete equals search_complete and is false in
 * progress. A complete scan may have truncated=true (matches_seen exceeds
 * retained_hits) with stop_reason=null. Partial coverage never asserts an
 * exhaustive total. Hit receipts echo count_all_matches, match_count_complete
 * and retained_matching_files from the accepted immutable report as well.
 * The same scope, pointer, progress, Stop and cancellation contracts apply.
 */
char *fcb_atlas_stream_search_counted_progress(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    int32_t (*poll)(void *context), int32_t (*stop)(void *context),
    fcb_atlas_stream_progress_callback progress, void *context);
char *fcb_atlas_stream_search_scoped_counted_progress(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    const char *extensions, const char *filenames, uint64_t path_kind, const char *path,
    int32_t (*poll)(void *context), int32_t (*stop)(void *context),
    fcb_atlas_stream_progress_callback progress, void *context);

/* Explicit Unicode lowercase or exact text search under frozen scope. Both
 * flags are exactly 0 or 1; all callbacks may be NULL. Legacy APIs remain exact.
 * mode is exact-decoded-literal or unicode-lowercase-literal and case_sensitive
 * echoes the policy on search/progress/page/hit. This is scalar lowercase,
 * without canonical normalization or full case folding. Scope exclusions still
 * happen before I/O and incomplete/malformed files never establish absence.
 *
 * Lowercase rows retain individual witness_sha256 and original ranges. The hit
 * endpoint returns each original spelling as text and its exact original_hex.
 * retained_witness_bytes sums those ranges, bounded by 12*needle UTF-8 bytes per
 * retained occurrence. Exact search preserves per-file shared literal storage.
 * Overlapping/equal source ranges can represent distinct normalized occurrences;
 * occurrence IDs, never deduplicated original ranges, define result identity.
 * Current-context activation must compare the selected original bytes/spelling.
 * Counting, Stop, cancellation and callback lifetimes match the APIs above.
 */
char *fcb_atlas_stream_search_scoped_matching_progress(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    const char *extensions, const char *filenames, uint64_t path_kind, const char *path,
    uint64_t case_sensitive, uint64_t count_all_matches,
    int32_t (*poll)(void *context), int32_t (*stop)(void *context),
    fcb_atlas_stream_progress_callback progress, void *context);

/* Bounded expression search over the SAME observed file stream. expression is
 * at most 1024 UTF-8 bytes, readable through NUL or 1025 bytes, whichever comes
 * first. The shared parser supports phrases, required/excluded text, path: and
 * lang:/type: filters; unsupported syntax returns its stable QUERY_* error.
 * Empty or malformed expressions never replace an accepted report. Foreign
 * NULL, invalid UTF-8, overlong strings and a case_sensitive value other than
 * 0 or 1 are ATLAS_HANDLE_INVALID_ARGUMENT. Scope/callback lifetimes match above.
 * All callbacks may be NULL for a synchronous terminal-only response.
 *
 * Metadata filters intersect the frozen native scope before source I/O. All
 * required/excluded content predicates must be decided from one fully decoded
 * observed stream before its primary-literal hits can appear. A read limit,
 * malformed encoding or stop cannot prove that excluded text was absent.
 * No complete-file capture or atomic snapshot guarantee is implied.
 *
 * All qualified primary-literal occurrences are counted within the I/O budget;
 * max_matches bounds retained rows, never expression evaluation. Summaries and
 * hit receipts echo count_all_matches=true, mode=decoded-text-expression,
 * expression (the original query), needle and primary_needle (the first required
 * literal), query_syntax=fcb-bounded-expression/1,
 * predicate_scope=same-observed-stream and
 * path_filter_domain=root-relative-utf8-or-escaped-search-key.
 * expression_scope_files + expression_excluded_files equals eligible_files;
 * files_examined + files_unexamined equals expression_scope_files. Native scope
 * eligible_files + excluded_files still equals catalogued_files. Progress,
 * terminal search, pages and retained witnesses preserve this immutable policy.
 * Exact/lowercase matching, retained original bytes, Stop and hard cancellation
 * retain their existing semantics. A retained witness proves only the matching
 * literal; opening current context cannot revalidate all expression predicates.
 */
char *fcb_atlas_stream_search_scoped_expression_progress(uint64_t handle, const char *expression,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    const char *extensions, const char *filenames, uint64_t path_kind, const char *path,
    uint64_t case_sensitive,
    int32_t (*poll)(void *context), int32_t (*stop)(void *context),
    fcb_atlas_stream_progress_callback progress, void *context);

/* Exact whole-identifier occurrences through the same bounded streaming
 * decoder, with no per-file capture limit. count_all_matches is exactly 0 or 1;
 * all callbacks may be NULL. Scope and callback ownership match the APIs above.
 *
 * The shared captured/streaming token policy admits ASCII letters, digits,
 * underscore and dollar; an identifier cannot start with an ASCII digit.
 * Non-ASCII non-whitespace/non-control runs are conservative token content.
 * needle must be one complete identifier. Matching is case-sensitive, includes
 * comments and literals, and performs no compiler or language-server resolution.
 * A delimiter or actual observed end of source completes a token; a decoder
 * window, work quantum or scan-budget boundary cannot manufacture a match.
 *
 * search/progress/page/hit echo mode=whole-token-occurrences, case_sensitive=true,
 * evidence_level=whole-token-text-candidate, includes_comments_and_literals=true,
 * compiler_resolved=false and semantic_complete=false. After ordinary needle
 * admission, invalid identifier syntax returns STREAM_READ_INVALID_IDENTIFIER
 * and preserves the accepted inventory.
 * Retained witnesses are exact encoded token bytes with shared per-file storage.
 * Opening current context verifies those literal bytes, not unchanged neighboring
 * token boundaries or symbol identity. Coverage and count completeness retain
 * their separate existing meanings.
 */
char *fcb_atlas_stream_search_scoped_occurrences_progress(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    const char *extensions, const char *filenames, uint64_t path_kind, const char *path,
    uint64_t count_all_matches,
    int32_t (*poll)(void *context), int32_t (*stop)(void *context),
    fcb_atlas_stream_progress_callback progress, void *context);

/* NEW live observation retaining a match window beyond the initial result cap.
 * Scan eligible files again in catalog order, discard the first start_after
 * observed occurrences, and retain up to max_matches thereafter. Every skipped
 * byte/read call consumes the same total limits. No source-sized cache is built.
 *
 * case_sensitive, count_all_matches and occurrences are exactly 0 or 1.
 * occurrences=1 selects whole-identifier text and requires case_sensitive=1.
 * start_after + max_matches + 1 must fit uint64_t. All callbacks may be NULL.
 *
 * This is NOT a cursor into an immutable workspace snapshot. Files may change
 * between calls, and every attempt receives new query/source revision identities.
 * Replace the previous result set; do not append rows as one coherent capture.
 * Progress and terminal replies echo page_start_after (the requested offset),
 * skipped_matches=min(page_start_after,matches_seen), and page_has_more_matches.
 * truncated means ANY observed rows were omitted, including the skipped prefix;
 * page_has_more_matches means a later occurrence was actually observed. Neither
 * false nor an empty page proves absence when search_complete=false. Counts cover
 * this attempt's finalized file observations, including skipped occurrences/files.
 * Later requests must repeat the same intended scope/matching policies. Existing
 * retained-row page/hit calls still perform zero I/O; hit echoes page_start_after.
 */
char *fcb_atlas_stream_search_scoped_page_progress(uint64_t handle, const char *needle,
    uint64_t max_matches, uint64_t max_scan_bytes, uint64_t max_read_calls,
    const char *extensions, const char *filenames, uint64_t path_kind, const char *path,
    uint64_t case_sensitive, uint64_t count_all_matches, uint64_t occurrences, uint64_t start_after,
    int32_t (*poll)(void *context), int32_t (*stop)(void *context),
    fcb_atlas_stream_progress_callback progress, void *context);

/* No source I/O. generation must identify the accepted report; start is a
 * zero-based retained-row position and count is 1..32. next_offset=null means
 * the end of the terminal retained rows, not proof of full source coverage.
 */
char *fcb_atlas_stream_search_page(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t count);

/* Exact retained text/original_hex and provenance for one zero-based position,
 * with zero additional source I/O. Descriptors closed when their scans ended.
 * This is only the matching literal, not a capture or current context. Opening
 * its pathname later requires a current grant, a NEW owner/observation and
 * exact original-byte comparison before any old-match highlight is installed.
 */
char *fcb_atlas_stream_search_hit(uint64_t handle, uint64_t generation,
    uint64_t position);

/* Release the accepted report and advance its query identity. This does not
 * affect independent captured-search results, indexes or opened readers.
 */
char *fcb_atlas_stream_search_clear(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
