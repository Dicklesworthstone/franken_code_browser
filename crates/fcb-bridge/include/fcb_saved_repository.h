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
 * Retains its directory, not its complete source body. Paged limits: 65536
 * members, 32 MiB per member, 1 GiB captured source, 1 GiB + 16 MiB archive, 2048 bytes
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

/* Explicit user-owned FCBT reading trails. Attach reads and validates the whole
 * selected regular file (at most 4 MiB / 1024 ordered visits), retaining only
 * reference/rationale bytes. It never opens any archive or live path named by
 * a visit. Repeated visits and mixed-archive references remain separate rows.
 * path is borrowed NUL-terminated UTF-8 valid until return. Retain selected-file
 * access through entered attachment work/drain; no path is reopened afterward.
 *
 * Attach/detach attempts use their own strictly increasing nonzero generation.
 * Failed/canceled replacement preserves the preceding accepted trail. A late
 * cancellation can suppress delivery after acceptance; fcb_saved_info exposes
 * accepted_trail_generation and last_trail_attempt, and page recovers the
 * accepted state. Do not infer rollback or replay a pathname from a lost reply.
 *
 * Attach returns the first page. Page start/item ordinals are zero-based and
 * limit is 1..32. Commands are trail-attach/trail-page/trail-detach in the shared
 * fcb.saved-repository/1 schema. Pages expose trail digest/title/parent, exact
 * original ranges, rationale, raw paths, total/ready/unavailable counts and
 * next_offset. Readiness is relative to the opened archive metadata: ready,
 * other-archive, missing-member, unavailable-member, changed-source or
 * invalid-range. It is not a proof of a later payload read. No source payload
 * is loaded by attachment or pagination; no entry is silently omitted.
 */
char *fcb_saved_trail_attach(uint64_t handle, uint64_t generation,
    const char *path);
char *fcb_saved_trail_page(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);
char *fcb_saved_trail_detach(uint64_t handle, uint64_t generation);

/* Admit an existing EMPTY reader before source I/O, resolve the immutable
 * generation-bound visit, then reverify the saved member digest/length/range.
 * Unready visits fail without installing a reader or reading live source.
 * The trail-open-reader receipt preserves the entire exact selection, including
 * empty point bookmarks and ranges larger than the initial 16 KiB window.
 * Initial window starts at max(0, selection.start - 132); it is not a claim
 * that the whole selection is visible. The independently admitted reader keeps
 * its exact source after the trail, selected file, or saved session is closed.
 */
char *fcb_saved_open_trail_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t item);

/* Open one other-archive trail visit from an explicitly selected FCBS file.
 * The generation AND expected trail_digest pin the accepted immutable trail.
 * archive_path is a borrowed NUL-terminated UTF-8 absolute path (1..16384 bytes),
 * never an entry label. validation_bytes <=268435456 independently bounds full
 * envelope validation (observed file length plus one EOF probe); directories
 * share the existing 67108864-byte admission. A wrong archive digest, member
 * digest/length/path/range, stale request, budget refusal or cancellation during
 * preparation leaves the reader destination empty. Admission of that empty destination precedes
 * archive I/O. The original saved archive and trail are preserved.
 *
 * Success command is trail-archive-open-reader. Its envelope identifies the
 * primary session; entry is the unchanged original other-archive reference.
 * source_archive identifies the explicitly validated supplemental digest/path
 * and catalog counts. resolved_member is local to that digest and uses ordinary
 * catalog member/file=ordinal+1; top-level file_id/source_revision instead name
 * fresh staging identities above the primary member interval. The nested
 * reader has its own destination owner and capture identities. Never route the
 * supplemental catalog member through a primary archive-local API.
 *
 * The reader owns copied, digest-verified bytes and survives releasing the
 * supplemental grant, moving either archive, or detaching/closing the trail.
 * Keep the grant and optional callback/context alive until the worker returns.
 * Callback rules, owned reply/freeing, and late reader-acceptance reconciliation
 * follow the existing reader contract; cancellation cannot imply rollback.
 */
char *fcb_saved_open_trail_reader_with_archive_cancelable(uint64_t handle,
    uint64_t reader, uint64_t generation, uint64_t item, const char *trail_digest,
    const char *archive_path, uint64_t validation_bytes,
    int32_t (*poll)(void *), void *context);


/* Author a NEW immutable FCBT from an exact saved-member byte range. The host
 * verifies bytes using the retained archive descriptor, never its pathname or
 * a live source file. member is zero-based; start <= end <= captured length.
 * Empty ranges are point bookmarks. Text is explicit UTF-8 bytes, including
 * embedded NUL: title <=256 bytes, rationale <=4096 bytes; null is permitted
 * only when the corresponding length is zero. All inputs remain valid until
 * return. destination/digest are borrowed NUL-terminated UTF-8.
 *
 * New: expected_trail_generation=0 and expected_trail_digest="". Append:
 * current accepted attachment generation plus its 64-hex digest, and title
 * equal to its exact existing title. Preserves every parent visit/rationale,
 * including repeats and other archives; records the parent's digest. Limits
 * remain 1024 visits / 4 MiB and never evict or truncate earlier user notes.
 * Never attaches the output or changes accepted trail/query/reader state.
 *
 * Each host-entered operation consumes a fresh strictly increasing nonzero ID,
 * independent of attach/search generations. Creation is exclusive (Unix 0600),
 * never overwrite; hold destination write access through worker drain. Receipt
 * capacity is admitted before source verification/publication. A cancel/close
 * after create cannot erase the effect-bearing response. Cancellation never
 * deletes a created file or rolls back its bytes. Inspect status, effect and
 * bytes_written; complete_artifact/attach_eligible are true only after all
 * encoded bytes were written. Digest/counts can describe an encoded artifact
 * even when destination creation/write failed. No power-loss guarantee.
 *
 * command=trail-write in fcb.saved-repository/1 binds exact archive/member/path,
 * source digest/length/range, reviewed title/rationale, parent, destination,
 * produced artifact digest/counts, source read counters and terminal effect.
 * info exposes last_trail_write_attempt and nullable last_trail_write_operation.
 * Missing/malformed/null delivery is uncertain: recover the exact operation's
 * immutable receipt without I/O or replay. Never auto-retry a lost write reply.
 */
char *fcb_saved_trail_write(uint64_t handle, uint64_t operation,
    uint64_t expected_trail_generation, const char *expected_trail_digest,
    uint64_t member, uint64_t start, uint64_t end,
    const uint8_t *title, uint64_t title_length,
    const uint8_t *rationale, uint64_t rationale_length, const char *destination);
char *fcb_saved_trail_write_cancelable(uint64_t handle, uint64_t operation,
    uint64_t expected_trail_generation, const char *expected_trail_digest,
    uint64_t member, uint64_t start, uint64_t end,
    const uint8_t *title, uint64_t title_length,
    const uint8_t *rationale, uint64_t rationale_length, const char *destination,
    int32_t (*poll)(void *), void *context);
/* Edit one exact accepted trail, writing a NEW immutable FCBT. Parent generation
 * must be nonzero and the digest must match the current attachment. Kinds:
 *   0 = rename: text is the new title (<=256 UTF-8 bytes); item=position=0.
 *   1 = set rationale: item is the visit; text <=4096 bytes; position=0.
 *   2 = move: item moves to final position, both zero-based and < visit count.
 *   3 = remove: remove item; position=0. Removing the last visit is valid.
 * Move/remove require text_length=0. Unknown kinds/reserved nonzero arguments
 * are rejected. Text may be empty and include embedded NUL; null is valid only
 * with zero length. All pointers obey the write contract above.
 *
 * Retained source digests, lengths, raw paths and original-byte ranges are
 * unchanged. Other-archive/unavailable/repeated visits need no source access.
 * Edits never open source payloads or the parent pathname. Every edit records
 * the exact parent digest, including an edit that leaves metadata unchanged.
 * The output remains separate until explicitly attached.
 *
 * Shares write's strictly increasing operation IDs, create-only effects,
 * cancellation/drain rules and fcb_saved_trail_write_receipt recovery. The
 * receipt has command=trail-edit, mode=edit, common parent/artifact/effect fields
 * and edit={kind,ordinal,destination_ordinal,title,rationale}; ordinals are
 * zero-based decimal strings and unused fields are null. kind is rename,
 * set-rationale, move or remove. source_anchors_unchanged=true,
 * source_verified=false and member read counters=0. Never retry an uncertain
 * delivery automatically; recover the exact operation first.
 */
char *fcb_saved_trail_edit(uint64_t handle, uint64_t operation,
    uint64_t expected_trail_generation, const char *expected_trail_digest,
    uint32_t kind, uint64_t item, uint64_t position,
    const uint8_t *text, uint64_t text_length, const char *destination);
char *fcb_saved_trail_edit_cancelable(uint64_t handle, uint64_t operation,
    uint64_t expected_trail_generation, const char *expected_trail_digest,
    uint32_t kind, uint64_t item, uint64_t position,
    const uint8_t *text, uint64_t text_length, const char *destination,
    int32_t (*poll)(void *), void *context);
/* Recover the last admitted create, append or edit without source/output I/O. */
char *fcb_saved_trail_write_receipt(uint64_t handle, uint64_t operation);

/* Export the exact accepted FCBT generation/digest against this retained FCBS
 * descriptor only. No archive/trail/live source pathname is reopened. Other
 * archives, missing members and changed captures remain explicit unavailable
 * entries, retaining ordered notes and provenance. The output is self-contained
 * fcb.trail-source-pack/1 JSON with exact original source bytes encoded as hex.
 *
 * Limits: entries 0..1024; whole-member verification and selected original bytes
 * independently 0..64 MiB; complete output bytes and Unicode scalars independently
 * 1..16 MiB. All admitted reference metadata is reserved before source loads.
 * No truncation inside a selection. Missing/limited evidence is represented in
 * a valid partial pack; complete_artifact and coverage_complete are independent.
 *
 * Destination must name a NEW file: create-only, no overwrite. Paths/notes/source
 * may be sensitive; no redaction/encryption is claimed. Destination/digest are
 * readable NUL-terminated UTF-8 until return. Keep destination permission alive
 * through worker drain. Cancellation may follow a filesystem effect; inspect
 * the retained effect/bytes_written and never automatically replay a lost reply.
 * Operations increase independently of attachment/write/search generations.
 * Info reports last_trail_export_attempt and last_trail_export_operation.
 */
char *fcb_saved_trail_export(uint64_t handle, uint64_t operation,
    uint64_t generation, const char *digest, const char *destination,
    uint64_t entries, uint64_t verification_bytes, uint64_t selected_bytes,
    uint64_t output_bytes, uint64_t output_scalars);
char *fcb_saved_trail_export_cancelable(uint64_t handle, uint64_t operation,
    uint64_t generation, const char *digest, const char *destination,
    uint64_t entries, uint64_t verification_bytes, uint64_t selected_bytes,
    uint64_t output_bytes, uint64_t output_scalars,
    int32_t (*poll)(void *), void *context);
/* Add up to seven explicitly selected supplemental FCBS archives to the
 * retained primary. archive_paths is a borrowed array of archive_count borrowed
 * NUL-terminated UTF-8 absolute paths, each 1..16384 bytes. Count zero permits a
 * null array and uses the compatible single-archive receipt. Keep every input
 * read grant plus the output write grant until entered work drains.
 *
 * validation_bytes <=268435456 (recommended default 268435456) is separate from
 * member verification/selection budgets. Every supplied path, including copies
 * of the primary or another supplemental file, pays its observed length plus
 * one EOF probe before validation. A shared 67108864-byte directory allowance
 * is backed by the host budget. Fully verified digests route visits; the first
 * verified handle wins a duplicate digest and the retained primary wins its
 * digest. Missing digests remain explicit unavailable selections.
 *
 * No source member reads or output creation occur until all supplied envelopes
 * validate. Mixed receipts carry ordered additional_archives path/opened/digest
 * records and archive_validation counters, including partial failed-validation
 * work. validation_complete is separate from source coverage and artifact
 * completeness. archive_path_reopened:false refers to the retained primary;
 * opened_archive_paths counts explicitly selected supplemental inputs.
 *
 * Cancellation/close, create-only effects and receipt recovery follow the
 * original export contract above. Recovery never reopens supplemental files.
 * Optional callback/context must remain callable until return, must not block,
 * unwind or reenter this handle, and returns nonzero to cancel.
 */
char *fcb_saved_trail_export_with_archives_cancelable(uint64_t handle, uint64_t operation,
    uint64_t generation, const char *digest, const char *destination,
    uint64_t entries, uint64_t verification_bytes, uint64_t selected_bytes,
    uint64_t output_bytes, uint64_t output_scalars,
    const char *const *archive_paths, uint64_t archive_count, uint64_t validation_bytes,
    int32_t (*poll)(void *), void *context);
char *fcb_saved_trail_export_receipt(uint64_t handle, uint64_t operation);

/* Quick Open searches the already validated archive directory with the shared
 * exact/prefix/fuzzy path ranker, including members without captured contents.
 * query is a borrowed 1..256-byte native path query, with no NUL. Raw non-UTF-8
 * names are supported. mode: 0 exact, 1 prefix, 2 fuzzy; match_case: 0 Unicode
 * scalar lowercase, 1 sensitive. No canonical normalization is applied.
 * max_results is 0..256; max_work_units is 0..2^32 and bounds ranker work, not
 * elapsed time. Metadata index construction has separate archive/memory bounds.
 *
 * Search and page read no member payload, live source, or persistent index.
 * Directory search completeness is independent of historical discovery and
 * capture completeness: unavailable members still have searchable filenames.
 * search_complete/matches_seen_exact describe the examined archive metadata;
 * truncated describes retained rows, and work_limited keeps global top-k and
 * absence claims incomplete. Pages may shorten to fit response bounds; follow
 * next_offset rather than adding the requested limit. Page limit is 1..128.
 *
 * Path generations are independent of text/import/document searches. Search
 * and clear attempts must increase; failed/canceled replacement preserves old
 * accepted rows. These calls run synchronously on a host worker. Query bytes
 * remain valid until return; free every owned response with fcb_free_string.
 */
char *fcb_saved_path_search(uint64_t handle, uint64_t generation,
    const uint8_t *query, uint64_t query_bytes, uint64_t mode, uint64_t match_case,
    uint64_t max_results, uint64_t max_work_units);
char *fcb_saved_path_results(uint64_t handle, uint64_t generation,
    uint64_t start, uint64_t limit);
char *fcb_saved_clear_paths(uint64_t handle, uint64_t generation);

/* Existing EMPTY reader destination. Validates the accepted path generation
 * and one-based hit before resolving its member ordinal; then verifies the
 * selected captured member from the retained archive. Returns the ordinary
 * open-reader receipt with selection:null. Unavailable rows cannot open.
 * Reader admission precedes source I/O, and accepted readers remain usable
 * after path results or the saved archive are closed.
 */
char *fcb_saved_open_path_hit_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t hit);

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

/* Build FCBD from the already opened archive using the shared index engine.
 * destination is an explicitly chosen, borrowed NUL-terminated UTF-8 path.
 * Creation is exclusive: no overwrite, rename, retry or deletion of output.
 * Retain the selected output write grant until the entered worker call drains.
 * Limits (also defaults): max_file_bytes <=262144, max_source_bytes <=67108864,
 * max_grams <=2097152. Zero is allowed and can leave explicit uncovered members.
 * Per-member grams <=65536, scratch <=1048576, encoded FCBD <=25165824 bytes.
 * A separate increasing build operation is consumed before admission, even on
 * preadmission failure. Building does not change accepted index/query state.
 *
 * A terminal index-build receipt is retained before delivery, including write
 * failures/cancellation. effect distinguishes none, destination-created-incomplete,
 * complete-file-sync-unconfirmed, complete-file-sync-requested, and
 * complete-file-and-parent-sync-requested. bytes_written and index_bytes are
 * separate; success alone does not certify whole-source coverage or power-loss
 * durability. The output is source-derived sensitive data and is not encrypted.
 *
 * A complete artifact can be attach_eligible even after cancellation or a sync
 * error. index_pin is the trusted build-time page-manifest-envelope digest;
 * attach explicitly with fcb_saved_attach_index to revalidate the chosen file.
 * No automatic attachment occurs. Never infer a trusted pin from input bytes.
 *
 * Cancellation/close after a write must not hide the receipt returned by the
 * entered call. Null or an unparseable reply does not prove no write occurred.
 * Keep the saved handle open and recover the same operation with receipt();
 * do not retry the write. Only the latest admitted terminal receipt is kept.
 * info exposes last_index_build_attempt and nullable last_index_build_operation.
 * Receipt replay is immutable and reads neither member nor destination bytes.
 * The optional poll runs only on the calling thread, must not block/unwind or
 * reenter the bridge, and returns nonzero to cancel. Null means no callback.
 * context and all borrowed input must remain valid until the call returns.
 */
char *fcb_saved_build_index(uint64_t handle, uint64_t operation,
    const char *destination, uint64_t max_file_bytes,
    uint64_t max_source_bytes, uint64_t max_grams);
char *fcb_saved_build_index_cancelable(uint64_t handle, uint64_t operation,
    const char *destination, uint64_t max_file_bytes,
    uint64_t max_source_bytes, uint64_t max_grams,
    int32_t (*poll)(void *), void *context);
char *fcb_saved_index_build_receipt(uint64_t handle, uint64_t operation);

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
/* A bounded later literal window over the same immutable archive. This is a
 * new search with a fresh increasing query generation, not an extension of the
 * old retained table. skip_matches is a zero-based count of verified qualified
 * occurrences; index candidates, negative proofs and unavailable members do not
 * consume it. All skipped source work still counts against existing budgets.
 *
 * All search/results receipts expose result_offset (requested skip),
 * skipped_matches (the admitted prefix actually seen), has_more_matches and
 * nullable next_match_offset. matches_seen includes skipped matches; retained_hits
 * counts only this window. truncated continues to mean omitted rows, including
 * the skipped prefix. Use has_more_matches, not truncated, to enable Later.
 * next_offset remains metadata paging within this window. Hit IDs remain local
 * to the accepted generation. An incomplete scan is not a completed inventory;
 * never concatenate independent windows into one accepted query.
 *
 * Flags, limits, index policy and pointer ownership match search_matching.
 * Failed/canceled replacement preserves accepted rows. Existing retained readers
 * survive replacement; opening a new hit revalidates its immutable member digest.
 */
char *fcb_saved_search_page(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t case_sensitive, uint64_t count_all_matches,
    uint64_t max_matches, uint64_t skip_matches);
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
/* Explicit matching/count policy for every primary/required/excluded term.
 * Both flags are exactly 0 or 1. case_sensitive=0 applies Unicode scalar
 * lowercase, not full case folding or canonical accent normalization. Original
 * byte ranges include complete contributing scalars (e.g. i may match U+0130).
 * max_hits and aggregate matching-work bounds are unchanged, including count
 * only. Attached exact-byte indexes are bypassed without detaching them.
 * Receipts/pages/readers freeze case_sensitive, mode (decoded-text-expression
 * or unicode-lowercase-expression), and normalization (exact). Reader selection
 * original_hex is the verified source spelling, which may differ from the
 * primary query text. Old search/count endpoints remain case-sensitive.
 */
char *fcb_saved_expression_search_matching(uint64_t handle, uint64_t generation,
    const char *query, uint64_t case_sensitive, uint64_t count_all_matches,
    uint64_t max_hits, uint64_t max_scan_bytes);
/* Later primary-hit window with the same window receipt fields and ownership
 * rules as saved_search_page. Predicates are reestablished over the entire
 * verified member before its primary occurrences consume skip_matches.
 * Uses a fresh expression generation; max_scan_bytes includes skipped work.
 * All old expression entrypoints keep result_offset zero.
 */
char *fcb_saved_expression_search_page(uint64_t handle, uint64_t generation,
    const char *query, uint64_t case_sensitive, uint64_t count_all_matches,
    uint64_t max_hits, uint64_t max_scan_bytes, uint64_t skip_matches);
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
 * symbols, 256 KiB for headings and 32 MiB for occurrences, also subject to the
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
/* Later symbols/headings/whole-token window under a fresh structure generation.
 * Same admission, source budgets, verification and modes as structure_search.
 * result_offset/skipped_matches/has_more_matches/next_match_offset describe the
 * window; skipped and metadata-omitted qualified candidates count in matches_seen.
 * Follow next_match_offset because metadata omissions also consume positions.
 * Structure truncated retains its existing later-lookahead meaning; a complete
 * final window may have a skipped prefix. No semantic resolution is implied.
 */
char *fcb_saved_structure_search_page(uint64_t handle, uint64_t generation,
    uint64_t mode, uint64_t name_mode, const char *needle,
    uint64_t max_hits, uint64_t max_source_bytes, uint64_t skip_matches);
/* Matching/count variants preserve the legacy functions and generation space.
 * Both flags are exactly 0 or 1. case_sensitive=0 compares symbol and heading
 * names with Unicode scalar lowercase, not full folding or normalization.
 * Occurrences require case_sensitive=1 and exact token-name matching.
 *
 * count_all_matches=1 continues admitted extraction after max_hits is reached;
 * the limit bounds retained witnesses only. max_hits=0 counts without rows or a
 * continuation. All existing source, metadata, cancellation and work bounds
 * remain in force. Counts include the skipped prefix and metadata omissions.
 * matches_seen_exact certifies completed admitted extraction and source coverage
 * independently of metadata omissions; complete also requires no metadata
 * omissions. Neither field claims compiler-resolved semantic completeness.
 * truncated reports later rows omitted by the retained cap, independently of
 * state and complete. has_more_matches additionally requires a nonzero row
 * limit; next_match_offset is the FIRST later candidate, never the final count.
 * Use next_match_offset, not matches_seen, to request the next bounded window.
 * Receipts and reader provenance freeze case_sensitive, count_all_matches and
 * name_case (sensitive or unicode-lowercase); original-source ranges remain exact.
 */
char *fcb_saved_structure_search_matching(uint64_t handle, uint64_t generation,
    uint64_t mode, uint64_t name_mode, const char *needle,
    uint64_t case_sensitive, uint64_t count_all_matches,
    uint64_t max_hits, uint64_t max_source_bytes);
char *fcb_saved_structure_search_matching_page(uint64_t handle, uint64_t generation,
    uint64_t mode, uint64_t name_mode, const char *needle,
    uint64_t case_sensitive, uint64_t count_all_matches,
    uint64_t max_hits, uint64_t max_source_bytes, uint64_t skip_matches);
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

/* Lexical import navigation over the same closed saved manifest. This inventory
 * has its own increasing prepare/clear generation, independent of documentation
 * relationships and text/structure searches. Failed or canceled replacement
 * preserves the previously accepted inventory. info exposes the accepted import
 * generation and last attempted generation for reconciliation after lost replies.
 *
 * Reuses the shared Rust, JS/TS and Python import extractor and declared finite
 * catalog candidate policies. Import statements preserve original byte evidence;
 * candidates do not establish compiler or runtime bindings. No live source path,
 * package execution, sys.path or network access is used. Prepare admits 1..4096
 * rows, up to 32 MiB aggregate source reads and at most 1 MiB of original bytes
 * per source member. Combined source and Go module metadata payloads also stay
 * within 1 MiB; the separate module metadata limit remains 64 KiB.
 * Inspect imports_complete, truncation, diagnostics and omission counters before
 * interpreting an empty incoming/outgoing page. Accepted rows retain metadata;
 * source payloads are loaded one member at a time during extraction.
 *
 * direction 0 = all (member MUST be 0), 1 = outgoing, 2 = incoming. Ordinals and
 * filtered offsets are zero-based; link IDs are one-based. Page limit is 1..128.
 * Pagination reads no member payload. Missing, unavailable, ambiguous, external
 * and unsupported destinations remain explicit; only a unique captured target
 * can authorize target navigation. Calls execute synchronously on a host worker.
 */
char *fcb_saved_prepare_imports(uint64_t handle, uint64_t generation,
    uint64_t max_links, uint64_t max_source_bytes);
char *fcb_saved_imports(uint64_t handle, uint64_t generation,
    uint64_t direction, uint64_t member, uint64_t start, uint64_t limit);
char *fcb_saved_clear_imports(uint64_t handle, uint64_t generation);

/* Existing EMPTY reader destination. side 0 opens the exact original import
 * statement range; side 1 opens a unique captured catalog candidate at byte 0.
 * The selected member digest is reverified from the retained archive before
 * installation. The receipt qualifies import generation/link/side, archive,
 * member and reader identities, with original byte selection for evidence.
 * Delivered readers survive import replacement and saved-session closure.
 * Responses, including failures, are owned strings freed with fcb_free_string.
 */
char *fcb_saved_open_import_reader(uint64_t handle, uint64_t reader,
    uint64_t generation, uint64_t link_id, uint64_t side);

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
