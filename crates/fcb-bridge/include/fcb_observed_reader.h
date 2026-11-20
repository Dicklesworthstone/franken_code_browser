#ifndef FCB_OBSERVED_READER_H
#define FCB_OBSERVED_READER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bounded observations of one explicitly opened regular file, including files
 * too large for a complete retained reader capture. These are synchronous
 * worker operations: never perform source I/O or final close on a redraw/input
 * callback. Opening is metadata-only. A byte-window request reads at most
 * 256 KiB plus 16 bytes of scalar/CRLF context and receives a fresh observation
 * identity. Line requests additionally scan a separately bounded prefix.
 * Separate windows are NOT a coherent whole-file snapshot.
 *
 * Four handles, including retiring in-flight handles, are admitted per loaded
 * library. Each retains at most eight observations. Retained/copy operations
 * use only those immutable bytes; eviction is an explicit unavailable error,
 * never permission to reread a live path under an old identity.
 *
 * Keep the selected file's or current root's native access grant alive through
 * close/drain. For project-relative open, root is absolute NUL-terminated UTF-8
 * (<=16384 bytes). relative_path_hex is bounded
 * lowercase hex (<=32768 chars) encoding exact native root-relative components,
 * including non-UTF8 names. Absolute paths, NUL, empty/dot/dotdot components and
 * observed symlinks are refused. Leaf no-follow/nonblocking open is enforced;
 * hostile ancestor replacement confinement is NOT claimed.
 *
 * An open descriptor continues to name its original file object after pathname
 * replacement. Explicitly open a NEW handle to follow the current pathname.
 * Source grants and filename labels do not establish old exact-anchor authority.
 *
 * Every non-null result is independently owned fcb.observed-reader/1 JSON
 * (<=8 MiB), released once by fcb_free_string from this library. A null result
 * means no response was delivered; it does not revoke previously retained
 * observations. Do not infer successful open or observation from non-null alone.
 */
typedef int32_t (*fcb_observed_reader_poll)(void *context);

/* No I/O. Zero means capacity/identity admission failed. */
uint64_t fcb_observed_reader_create(void);

/* poll/context, when supplied, remain valid through return. Poll executes only
 * on this calling worker, returns nonzero to cancel, and must not block, unwind,
 * mutate inputs or reenter the active reader. Input strings stay valid through
 * return. A loaded handle cannot be reopened. */
char *fcb_observed_reader_open(uint64_t handle, const char *root,
    const char *relative_path_hex, fcb_observed_reader_poll poll, void *context);

/* Metadata-only open of ONE explicitly selected file without a project grant.
 * absolute_path_hex is NUL-terminated lowercase hex (<=32768 chars) of the exact
 * absolute native path (<=16384 decoded bytes, <=256 components), including
 * non-UTF8 names. NUL, empty/dot/dotdot components and a final symlink or special
 * file are refused. The parent is neither inspected nor granted as a project.
 * Ancestor replacement confinement is not claimed. Keep this selected file's
 * native grant through close/drain. Inputs/poll obey the open contract above;
 * success has the same info schema, exact absolute path and zero payload I/O.
 * Either opener loads the handle once; callers cannot switch its source. */
char *fcb_observed_reader_open_file(uint64_t handle, const char *absolute_path_hex,
    fcb_observed_reader_poll poll, void *context);
char *fcb_observed_reader_info(uint64_t handle);

/* bytes is 4..262144; offset is an absolute original-byte position. Explicit
 * decoding: 1=UTF-8, 2=UTF-16LE, 3=UTF-16BE. This is bounded logical text, not
 * whole-paragraph shaping, global line numbering or a new whole-file digest. */
char *fcb_observed_reader_window(uint64_t handle, uint64_t offset,
    uint64_t bytes, uint64_t encoding, fcb_observed_reader_poll poll, void *context);

/* Locate physical lines by scanning from byte zero of the SAME opened file
 * object. Retain the selected bytes during that scan; never reread the located
 * range or reuse line checkpoints from an earlier observation. CR, LF and CRLF
 * terminate physical lines; a final terminator does not add a phantom line.
 * Explicit encoding values are the same as window. No whole-file capture or
 * per-line index is allocated.
 *
 * first_line is one-based; line_count is 1..4096. max_bytes is 4..262144 and
 * bounds retained selected source, with bounded decoder context in addition.
 * max_scan_bytes is 1..4294967296 (4 GiB); a separate read-call bound applies.
 * Success returns command "lines", a fresh retained observation and its
 * line_navigation provenance. Retained replay preserves that provenance
 * without source I/O. Metadata consistency is not an atomic snapshot promise.
 * Missing-at-EOF, scan/call/output limits, changed source and cancellation are
 * explicit errors that leave previously accepted observations intact. A limit
 * is never proof that the requested line does not exist.
 */
char *fcb_observed_reader_lines(uint64_t handle, uint64_t first_line,
    uint64_t line_count, uint64_t max_bytes, uint64_t max_scan_bytes,
    uint64_t encoding, fcb_observed_reader_poll poll, void *context);

/* No payload reread. Both commands require an exact retained observation ID.
 * Copy returns original bytes as hex, never silently truncated or decoded. */
char *fcb_observed_reader_retained(uint64_t handle, uint64_t observation);
char *fcb_observed_reader_copy(uint64_t handle, uint64_t observation,
    uint64_t start, uint64_t end);

/* Resolve one nonempty selection in the accepted observation's displayed
 * logical text. utf8_start/end are half-open UTF-8 byte offsets LOCAL to that
 * text, not native UTF-16 units or absolute source offsets. The native host
 * supplies whole grapheme boundaries and verifies the reply against the
 * currently displayed observation before permitting copy.
 *
 * Returns command "resolve-selection", selection_namespace "native-text",
 * window_original_range, window_utf8_range, exact absolute original_range,
 * decoded text, original_hex and contains_replacements. All observation and
 * extent digest fields are preserved; additional source I/O counters are zero.
 * At most 256 KiB of selected original bytes is admitted. Empty/reversed,
 * split-scalar, past-end and evicted-observation selections fail explicitly.
 * A malformed-byte replacement is disclosed; exact hex keeps those bytes.
 * No clipboard side effect, source reread or whole-file capture is implied.
 */
char *fcb_observed_reader_resolve_selection(uint64_t handle, uint64_t observation,
    uint64_t utf8_start, uint64_t utf8_end);

/* Exact case-sensitive decoded-text search from byte zero of the SAME opened
 * file object. Runs on a worker, polling cooperatively between bounded reads.
 * generation is nonzero and strictly increases on each valid query attempt.
 * needle is NUL-terminated UTF-8, 1..4096 bytes; encoding has the window values.
 * max_matches is 0..256; max_scan_bytes is 0..4294967296 (4 GiB). A separate
 * 1048576-call bound also applies. A zero hit limit probes for one actual match;
 * a byte/call/hit limit or unsupported decoding reports incomplete coverage.
 *
 * A fresh search_observation identifies the continuous observed sequence, NOT
 * an atomic snapshot. Before/after metadata, scanned bytes, matches seen and
 * retained hits are separate fields. One accepted query and its literal
 * witness are retained independently of the eight window observations. Failed
 * or canceled replacements preserve the previous accepted query under its
 * original generation. A null/late reply does not prove state was unchanged.
 */
char *fcb_observed_reader_find(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t encoding, uint64_t max_matches,
    uint64_t max_scan_bytes, fcb_observed_reader_poll poll, void *context);

/* Opt-in complete counting under the same scan/read-call limits. Reaching
 * max_matches limits retained rows/witness bytes only; scanning continues.
 * count_all_matches=true and match_count_complete identify these receipts.
 * A complete count may have truncated=true with state=complete-observed-input.
 * Partial scans never claim an exhaustive count. Legacy find above preserves
 * its stop-at-first-overflow behavior.
 */
char *fcb_observed_reader_find_counted(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t encoding, uint64_t max_matches,
    uint64_t max_scan_bytes, fcb_observed_reader_poll poll, void *context);

/* Explicit matching/count policy. Both flags are exactly 0 or 1. case_sensitive
 * zero uses the shared Unicode lowercase semantics, without canonical
 * normalization/full case folding. Existing find calls remain case-sensitive.
 * Limits, cancellation, generation consumption and observation honesty match
 * find above; max_matches=0 supports count-only scans when count_all_matches=1.
 *
 * find and search-hit echo mode=exact-decoded-text or unicode-lowercase-text,
 * plus case_sensitive. Lowercase find has null literal_original_hex/SHA because
 * no one literal proves every spelling; each hit row carries witness_sha256.
 * The selected search-hit supplies its exact original text/original_hex/SHA.
 * retained_witness_bytes is one shared witness for exact search or the sum of
 * retained original ranges for lowercase search, at most 12*needle UTF-8 bytes
 * per retained occurrence. Expansions retain whole contributing source scalars;
 * occurrence IDs are distinct even when original ranges overlap or coincide.
 * observe_hit verifies the selected bytes and spelling in a NEW observation.
 */
char *fcb_observed_reader_find_matching(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t encoding, uint64_t case_sensitive,
    uint64_t count_all_matches, uint64_t max_matches, uint64_t max_scan_bytes,
    fcb_observed_reader_poll poll, void *context);

/* Additive search mode: 0=literal text, 1=whole-token occurrences, 2=bounded
 * content expression. mode and
 * boolean flags are checked; unknown values are never coerced. Text preserves
 * the matching/count semantics above. Occurrences requires case_sensitive=1
 * and a valid shared identifier of at most 1024 UTF-8 bytes. ASCII letters,
 * digits, '_' and '$' plus conservative non-ASCII runs define token boundaries;
 * names cannot begin with an ASCII digit. No Unicode normalization, compiler
 * binding or exclusion of comments/string literals is implied.
 *
 * Occurrence receipts carry mode/realized_mode="whole-token-occurrences",
 * requested_mode="occurrences", evidence_level="whole-token-text-candidate",
 * includes_comments_and_literals=true and compiler_resolved/semantic_complete
 * false. Count-only and bounded partial coverage remain separate from rows.
 * Read boundaries, byte limits and malformed input are not inferred EOF.
 *
 * Expression mode uses the shared bounded query syntax: quoted phrases,
 * conjunctions and exclusions, at most 1024 UTF-8 bytes / 64 tokens. Metadata
 * filters (path:/lang:/type:, including exclusions) are refused before payload
 * I/O with OBSERVED_READER_EXPRESSION_SCOPE_UNSUPPORTED. Parser errors retain
 * QUERY_* codes. Both exact and Unicode-lowercase text policies are supported.
 * All predicates must qualify on the SAME complete observed sequence before
 * any primary matches publish. Even count_all_matches=0 scans/counts the full
 * bounded input; max_matches limits only retained rows. Zero supports counting.
 *
 * Expression find/search-hit carry requested_mode="expression",
 * mode/realized_mode="decoded-text-expression", expression (the full raw query),
 * primary_needle, query_syntax="fcb-bounded-expression/1" and predicate_scope=
 * "same-observed-stream". expression_qualified is null on incomplete/unsupported
 * input, false on a completed sequence with no qualifying primary matches, or
 * true with a positive total (possibly zero retained rows after limits/skips).
 * Null/false never exposes primary candidates or tentative counts.
 * Find needle echoes the FULL raw expression; literal_text, literal witnesses
 * and hit ranges describe only the PRIMARY. Lowercase per-hit witnesses retain
 * exact original spelling/bytes as above. Completion remains observed metadata
 * consistency, never an atomic-snapshot claim.
 */
char *fcb_observed_reader_find_mode(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t encoding, uint64_t mode, uint64_t case_sensitive,
    uint64_t count_all_matches, uint64_t max_matches, uint64_t max_scan_bytes,
    fcb_observed_reader_poll poll, void *context);

/* Retain a later result page by rescanning from byte zero and skipping the
 * first start_after matching occurrences. Modes/flags and all limits match
 * find_mode. Skipped occurrences spend the SAME byte/read-call budgets, so a
 * large offset cannot create unbounded work. Zero selects the first page;
 * offsets beyond observed matches return no rows and preserve honest coverage.
 *
 * Every call requires a fresh generation and creates a NEW search_observation
 * of the live opened file. Pages from separate calls are not one immutable
 * capture: source edits may change counts, order and positions. The host must
 * label this as a rescan and must not combine results under an old identity.
 * Failed/canceled attempts preserve the previously accepted query as above.
 *
 * All find/search-hit receipts add page_start_after, skipped_matches (the lesser
 * of the requested skip and matches_seen), and page_has_more_matches (a known
 * match AFTER the retained page). Existing entry points use page_start_after=0.
 * A false page_has_more_matches does not prove EOF on an incomplete scan.
 * matches_seen counts from byte zero; retained row index/hit_index is local to
 * this page, while occurrence_id remains the one-based whole-input ordinal.
 * truncated includes skipped rows; completion/count/retention remain separate.
 * observe_hit and retained history preserve page_start_after in find_hit.
 */
char *fcb_observed_reader_find_page(uint64_t handle, uint64_t generation,
    const char *needle, uint64_t encoding, uint64_t mode, uint64_t case_sensitive,
    uint64_t count_all_matches, uint64_t start_after, uint64_t max_matches,
    uint64_t max_scan_bytes, fcb_observed_reader_poll poll, void *context);

/* Zero-based hit_index must belong to the current accepted query generation.
 * Returns the retained literal text/bytes and original-byte range, plus bounded
 * context captured by the same search, with no source reread. Each result admits
 * up to 160 original bytes before/after the match. Context never extends scan
 * limits; requested/actual ranges and complete report a clipped suffix. The
 * context object includes original_hex and, when scalar boundaries permit,
 * text, text_original_range, match_decoded_range and text_has_replacements.
 * Unavailable text carries text_unavailable_reason; original bytes remain exact.
 * Find receipts declare literal-witnesses-and-bounded-context retention and
 * retained_context_bytes independently of witnesses and observation history.
 * A host may explicitly request a NEW window and verify the witness against
 * those bytes; it must never label that window as the old search observation.
 */
char *fcb_observed_reader_search_hit(uint64_t handle, uint64_t generation,
    uint64_t hit_index);

/* Open the accepted hit's captured excerpt in the eight-page reader history.
 * This performs no stat or source I/O. The returned search-excerpt page has a
 * fresh page identity, host-supplied-sequence consistency, zero observation and
 * additional I/O counters, and retained_search_context provenance naming the
 * original query/observation/occurrence plus requested/actual excerpt ranges.
 * find_hit.verification is retained-original-search-bytes; this does not verify
 * the current file. Selection/copy and retained-window replay work after query
 * replacement until the page is evicted. Incomplete scalar context is refused
 * without reading missing bytes or changing retained history. Info reports
 * retained_copied_bytes separately because page copies can exceed source I/O.
 * poll/context follow the same worker lifetime contract as observe_hit.
 */
char *fcb_observed_reader_search_excerpt(uint64_t handle, uint64_t generation,
    uint64_t hit_index, fcb_observed_reader_poll poll, void *context);

/* Activate an accepted search hit by zero-based index. Context is freshly read
 * from the opened descriptor and its original hit bytes must equal the retained
 * literal witness. Changed/unavailable bytes return an error; old observations
 * remain readable. Success returns a NEW observation plus verified find_hit
 * provenance and page-local decoded UTF-8 selection. It never reuses the old
 * search observation's identity. Context and window sizes are bounded by the
 * source service; window_bytes is 4..262144 and must contain the full hit plus
 * eight bytes of scalar/CRLF boundary slack for text queries. Occurrences
 * require 24 bytes of slack and always obtain adjacent scalar context even if
 * context_bytes=0. Both current token boundaries must also verify; a matching
 * old witness inside a now-longer identifier is refused. The retained find_hit
 * records its original mode and token_boundaries_verified, so history replay
 * remains qualified after another query replaces the active search.
 *
 * Expression activation verifies only the retained PRIMARY witness in the new
 * context, not the whole-file predicates. find_hit retains the original raw
 * expression, primary_needle and query_syntax with predicate_scope=
 * "original-search-observation", expression_qualified=true and
 * expression_revalidated=false. Its mode/case flags remain frozen. Retained
 * history keeps that small query provenance independently of active-query
 * replacement; it does not retain old full search tables or reread predicates.
 * Retained-window navigation afterward performs no file I/O.
 */
char *fcb_observed_reader_observe_hit(uint64_t handle, uint64_t generation,
    uint64_t hit_index, uint64_t context_bytes, uint64_t window_bytes,
    fcb_observed_reader_poll poll, void *context);

/* Cancellation revokes in-flight delivery without waiting on source work.
 * Close removes authority immediately; its admission survives in-flight calls.
 * Returns 1 on success, 0 for unknown/busy/exhausted handles. */
uint8_t fcb_observed_reader_cancel(uint64_t handle);
uint8_t fcb_observed_reader_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
