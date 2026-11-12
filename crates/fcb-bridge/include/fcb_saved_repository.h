#ifndef FCB_SAVED_REPOSITORY_H
#define FCB_SAVED_REPOSITORY_H
#include <stdint.h>
#include "fcb_reader_sessions.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Retained FCBS archive and optional FCBD demand-paged posting index.
 * This handle kind is distinct from atlas and reader handles. Four live/retiring
 * saved handles are admitted; closing an in-flight handle does not immediately
 * release its capacity. Create reserves an EMPTY cancelable handle; zero fails.
 * All work except create/cancel belongs on a host worker. No runtime is started.
 * Non-null JSON uses fcb.saved-repository/1; inspect status and search_complete.
 * Free every response exactly once with fcb_free_string, even errors. Null is
 * marshaling/panic failure. Full-width IDs, offsets and counts are JSON strings.
 */
uint64_t fcb_saved_create(void);

/* Capture the named archive descriptor and validate the full archive once.
 * Retains its directory, not its complete source body. Default limits: 65536
 * members, 1 MiB per member, 64 MiB captured source, 80 MiB archive, 2048 bytes
 * per relative native name. UTF-8 path must be readable/NUL-terminated until
 * return. No archived pathname confers live-root or execution authority.
 * An opened handle cannot reopen another archive. Failed initial open leaves
 * it empty unless cancellation arrived AFTER acceptance; info reconciles that.
 */
char *fcb_saved_open(uint64_t handle, const char *path);
char *fcb_saved_info(uint64_t handle);

/* Metadata-only page: start/member ordinals are zero-based, limit is 1..128.
 * Captured empty members and unavailable members remain different states.
 */
char *fcb_saved_members(uint64_t handle, uint64_t start, uint64_t limit);

/* Optional FCBD index. The 64-hex trusted_digest MUST come from its original
 * trusted build receipt, NOT be inferred from the input file. It pins the
 * metadata envelope including page hashes, not a whole-file checksum. Pages
 * are verified before use; four 16 KiB pages are cached across queries.
 * Archive-binding/pin failure preserves the preceding accepted index/results.
 * Both strings must be readable/NUL-terminated UTF-8 until return.
 * Attach/detach share an increasing INDEX generation, independent of queries.
 * Info and successful attach/detach expose last_index_attempt for recovery.
 * Attached index receipts include archive-bound indexed/uncovered/unavailable
 * member counts and manifest/page I/O. Coverage is not a whole-body integrity
 * claim; unsupported text formats and short needles can require direct scans.
 * Retain selected-file access through entered work and accepted attachment.
 * Lost/canceled replies require info reconciliation before releasing a grant;
 * neither replay the pathname nor assume cancellation reverted acceptance.
 */
char *fcb_saved_attach_index(uint64_t handle, uint64_t generation,
    const char *path, const char *trusted_digest);
char *fcb_saved_detach_index(uint64_t handle, uint64_t generation);

/* Case-sensitive decoded literal (1..1024 UTF-8 bytes), NOT query syntax.
 * max_matches 1..4096. Uses direct saved-source scanning without an index;
 * attached indexes use rarest posting candidates plus exact source verification.
 * Source residency is one member during verification, not all matching files.
 * Searches run synchronously with cancellation checkpoints; this is NOT the
 * live atlas begin/step API. A QUERY generation increases for each admitted
 * search/clear attempt, including failure. Failed replacement preserves rows.
 * First response has at most 64 hits; page using the same generation afterward.
 * Needle must be readable/NUL-terminated UTF-8 until return.
 */
char *fcb_saved_search(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t max_matches);
/* Same literal semantics and generation namespace, but max_matches 0..4096
 * limits retained witnesses only. Zero counts without retaining navigable rows.
 * Uses the same optional FCBD candidates and exact member verification.
 * The search/results receipts add count_all_matches, matches_seen_exact,
 * max_matches, matching_files and retained_matching_files. Count exactness is
 * search_complete; truncated only means matches_seen exceeds retained_hits.
 * Complete counts can therefore coexist with truncated rows. Missing or
 * unsupported members and incomplete discovery still prevent exact totals.
 */
char *fcb_saved_search_counted(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t max_matches);
/* Match-case selection for the same literal query generation namespace.
 * case_sensitive and count_all_matches are exactly 0 or 1. Match case 1 is the
 * existing exact-decoded-literal policy; 0 uses unicode-lowercase-literal with
 * the shared scalar lowercase mapping (not full folding or normalization).
 * max_matches is 0..4096 when counting, 1..4096 otherwise.
 *
 * Exact FCBD postings are incompatible with lowercase matching. Such queries
 * verify every admitted saved member directly while preserving the attachment.
 * Search/results freeze case_sensitive, mode, index_used and index_policy;
 * a bypassed index has null query index_generation/index_pin and no posting I/O.
 * Count, coverage, cancellation and immutable row identities are unchanged.
 * Open-hit selection receipts repeat the matching policy and include text:
 * the actual decoded source spelling covering the original-byte witness.
 * A lowercase expansion can match part of a scalar, so this text includes the
 * complete contributing scalar and need not equal the needle. original_hex,
 * range and the verified saved-member digest remain authoritative.
 * needle must remain readable/NUL-terminated UTF-8 until return.
 */
char *fcb_saved_search_matching(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t case_sensitive, uint64_t count_all_matches,
    uint64_t max_matches);
char *fcb_saved_results(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);
char *fcb_saved_clear_results(uint64_t handle, uint64_t generation);

/* Explicit saved-source expressions using the shared query grammar:
 * phrases, required terms, -excluded terms, path:/-path: and lang:/-lang: filters.
 * Text predicates apply to the WHOLE captured member, case-sensitively; result
 * rows identify exact occurrences of the first positive term. Query <=1024
 * UTF-8 bytes /64 tokens; regex is not supported. query must remain readable,
 * NUL-terminated UTF-8 until return. Invalid syntax returns a structured error.
 *
 * max_hits 0..4096 (zero probes existence). max_scan_bytes bounds aggregate
 * matching work, including each predicate scan over already loaded bytes.
 * Search uses verified archive members directly; an attached literal posting
 * index is neither used nor detached. Only bounded witness metadata is retained.
 * Inspect complete/state, discovery completeness and omission counters: a
 * work limit or unavailable/unsupported source cannot prove absence.
 *
 * Expression generations are independent of literal/index/relationship state.
 * Every admitted search/clear attempt must increase, even after failure or
 * cancellation; a failed replacement preserves the last accepted expression.
 * info exposes accepted_expression_generation (nullable) and
 * last_expression_attempt for recovery after a lost or late response.
 * First page has at most 64 rows; subsequent pages use the accepted generation,
 * start >= 0 and limit 1..128 without rescanning source. The selection namespace
 * is saved-expression; never reinterpret these hit IDs as literal query IDs.
 */
char *fcb_saved_expression_search(uint64_t handle, uint64_t generation,
    const char *query, uint64_t max_hits, uint64_t max_scan_bytes);
/* Same expression syntax/predicate authority and generation namespace.
 * Counts every primary occurrence within max_scan_bytes after predicates are
 * established; max_hits 0..4096 only bounds retained witnesses. Zero is
 * count-only. Adds count_all_matches, matching_files and retained_matching_files
 * to expression receipts. matches_seen_exact equals complete independently of
 * omitted rows; truncated is matches_seen > retained_hits. A work-limit or
 * unavailable/unsupported member leaves totals inexact, including zero.
 */
char *fcb_saved_expression_search_counted(uint64_t handle, uint64_t generation,
    const char *query, uint64_t max_hits, uint64_t max_scan_bytes);
char *fcb_saved_expression_results(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);
char *fcb_saved_clear_expression(uint64_t handle, uint64_t generation);

/* Same EMPTY-reader admission/lifetime rules as literal activation. Revalidates
 * the full member digest that established the expression predicates, then
 * returns expression-open-reader with exact original selection evidence.
 * Receipt and selection use expression_generation, never query_generation.
 * The resulting retained reader survives query replacement/archive closure.
 */
char *fcb_saved_open_expression_hit_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t hit);

/* Structural names in saved source: mode 1 = qualified symbol candidates,
 * 2 = Markdown headings, 4 = whole-token identifier occurrences. name_mode
 * 0 = exact, 1 = prefix, 2 = contains; matching is case-sensitive. Occurrences
 * require exact mode and a valid shared-engine token name. Compiler resolution
 * is not claimed; inspect evidence_level, realized_mode and semantic_complete.
 * needle must be 1..1024 UTF-8 bytes and readable/NUL-terminated until return.
 * No input pointer is retained. All extraction runs on the calling worker.
 *
 * max_hits is 0..4096 (zero probes existence). max_source_bytes bounds aggregate
 * source reads, at most 64 MiB. Per-member analysis limits are 64 KiB for
 * symbols, 256 KiB for headings and 512 KiB for occurrences, also subject to the
 * archive's capture limits. Unsupported/unavailable sources, source-work limits
 * and retained-result truncation remain explicit; inspect complete and counters.
 * Search does not use or detach the optional literal posting index.
 *
 * A separate increasing STRUCTURE generation owns all three modes. Every
 * admitted search/clear attempt increases it, even on failure or cancellation;
 * failed replacement preserves accepted results. Literal, expression, index
 * and relationship generations remain independent. info exposes nullable
 * accepted_structure_generation and last_structure_attempt for uncertain-result
 * recovery. First page has at most 64 rows; results pages use start >= 0 and
 * limit 1..128 without rereading source. IDs are one-based within the accepted
 * saved-structure namespace; never reinterpret them as another kind of hit.
 */
char *fcb_saved_structure_search(uint64_t handle, uint64_t generation,
    uint64_t mode, uint64_t name_mode, const char *needle,
    uint64_t max_hits, uint64_t max_source_bytes);
char *fcb_saved_structure_results(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);
char *fcb_saved_clear_structure(uint64_t handle, uint64_t generation);

/* Same EMPTY-reader admission and lifetime rules as other saved selections.
 * Revalidates the whole member digest before installing the retained reader.
 * structure-open-reader names structure_generation, exact original-source range
 * and extraction evidence; symbols/headings are not literal-text selections.
 * Large selections retain the exact range with original_hex null and bounded
 * context. A delivered reader survives structural replacement/archive closure.
 */
char *fcb_saved_open_structure_hit_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t hit);

/* Captured Markdown documentation links and reverse backlinks. This separate
 * relationship generation is independent of text-query and posting-index
 * generations. Prepare/clear attempts must increase; failed replacements keep
 * the last accepted projection. Calls run synchronously on a host worker.
 *
 * max_links is 1..4096. max_source_bytes bounds aggregate Markdown member reads
 * up to 32 MiB; each admitted Markdown member is at most 256 KiB. Inspect
 * relationships_complete and all omission counters: missing/unsupported or
 * source-limited documents cannot prove the absence of backlinks. Uses the
 * pinned first-party Markdown parser and archive-relative destination resolver.
 * Evidence spans cover the exact whole captured Markdown document. The pinned
 * parser's block positions are not used as navigation evidence. Link destinations are data; no URL fetch, live source lookup or command
 * execution occurs. Accepted projections retain metadata, not source payloads.
 *
 * direction 0 = all links (member MUST be 0), 1 = outgoing links from member,
 * 2 = incoming backlinks to member. Member ordinals and filtered page starts are
 * zero-based; link IDs are one-based. Page limit is 1..128. Missing, unavailable,
 * external and unresolved destinations remain explicit on link rows. Matching
 * is exact: there is no case folding, extension inference or directory fallback.
 * Result pagination does not read archived member payloads.
 */
char *fcb_saved_prepare_relationships(uint64_t handle, uint64_t generation,
    uint64_t max_links, uint64_t max_source_bytes);
char *fcb_saved_relationships(uint64_t handle, uint64_t generation,
    uint64_t direction, uint64_t member, uint64_t start, uint64_t limit);
char *fcb_saved_clear_relationships(uint64_t handle, uint64_t generation);

/* Existing EMPTY destination from fcb_reader_create. Admission precedes member
 * I/O. hit IDs are one-based and qualified by the accepted query generation;
 * member ordinals are zero-based. Reads verify the saved member digest again;
 * in-place archive corruption is refused, never replaced by live source bytes.
 * Receipt links saved-source and reader identities, exact original selection
 * and decoded context. It does not claim native shaped/UTF-16 selection geometry.
 * Delivered readers use ordinary reader APIs and survive saved-handle closure.
 */
char *fcb_saved_open_hit_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t hit);
char *fcb_saved_open_member_reader(uint64_t handle, uint64_t reader,
    uint64_t member);

/* Same destination admission and lifetime rules as member/hit reader open.
 * side 0 opens the whole original source document; side 1 opens a captured target at
 * byte zero. URL fragments are explicitly unresolved, never silently treated
 * as proven heading/line anchors. A target must resolve to a captured archive
 * member; external, missing or unavailable destinations cannot open.
 * The selected saved member is verified again before reader publication.
 */
char *fcb_saved_open_relationship_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t link_id, uint64_t side);

/* Archive-only Markdown images. The reader must already contain the exact
 * captured source_member and a prepared document display at document_generation.
 * Authorization checks archive member path/length/digest against the reader;
 * acquisition resolves only an upstream image item/request to a captured member.
 * No source label is opened as a native path and no live root grant is created.
 *
 * Success uses fcb.reader-session/1 with saved_archive_owner, saved_archive_digest,
 * saved_source_member, source_relative and live_root_authority=false. Prepared
 * images also identify image_member/image_archive_digest and carry the first
 * bounded encoded chunk. Failure uses fcb.saved-repository/1 for the saved handle.
 * Free every non-null response once with fcb_free_string.
 *
 * Existing limits: 4 MiB per image, 8 MiB/16 encoded images per reader/display,
 * 64 KiB chunks; archive limits may be smaller. PNG/JPEG admission checks only
 * signatures; the host decoder must enforce dimensions/frames/pixel budgets.
 * fcb_reader_document_image_chunk replays admitted bytes after archive close.
 * New authorization/acquisition requires the still-open saved handle.
 * Both functions are synchronous worker calls and cancel with either handle.
 */
char *fcb_saved_document_images_authorize(uint64_t handle, uint64_t reader,
    uint64_t source_member, uint64_t document_generation);
char *fcb_saved_document_image_prepare(uint64_t handle, uint64_t reader,
    uint64_t source_member, uint64_t document_generation,
    uint64_t item_index, uint64_t request_id);

/* Cooperative cancel changes the operation epoch without waiting on source.
 * Close removes authority and cancels work, but does not close delivered readers
 * or free response strings. Close can reclaim memory and must run on a worker.
 * Both return 1 on success, 0 on unknown/busy/closed/exhausted handles.
 * Cancellation after acceptance suppresses delivery, not acceptance; reconcile
 * info/results on the still-open handle. No implicit background work exists.
 */
uint8_t fcb_saved_cancel(uint64_t handle);
uint8_t fcb_saved_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
