#ifndef FCB_DESK_H
#define FCB_DESK_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

/* All work except create/cancel belongs on a host worker. No runtime or native
 * event loop is acquired. Two live/retiring desks are admitted. Each desk uses
 * the existing public defaults: 8 panes, 32 retained sources, 32 MiB aggregate
 * original bytes, 4 MiB per source, 64 history positions and 32 named bookmarks.
 * Every non-null response is fcb.reading-desk/1 JSON; free it exactly once with
 * fcb_free_string. Integers are canonical decimal STRINGS. Check status.
 *
 * create returns zero on admission failure; start initializes its empty handle.
 * Mutations require current model_revision and a fresh attempt > last_attempt.
 * They return the full new state; a lost/canceled reply is NOT rollback. Recover
 * with state before trying another action. Old attempts never add a second note.
 * Captures stay exact across edits/deletes, pane closure, history and bookmarks.
 */
uint64_t fcb_desk_create(void);
char *fcb_desk_start(uint64_t handle);
char *fcb_desk_state(uint64_t handle);
/* First-class observed passages retain a bounded extent, not a complete file.
 * State has separate observations and observed_passages arrays. Original byte
 * offsets remain full-width absolute coordinates; paths are provenance only.
 * Observations share the source/byte limits, passages share the bookmark limit.
 * Checkpoints preserve their bytes, decoder and consistency evidence offline.
 * Import binds a retained observed-reader handle plus observation identity and
 * nonempty visible selection. Keep both handles alive until the worker returns.
 * Repeated selections share immutable observation backing, with distinct notes.
 * Read returns command observed-passage, nested observation/passage, logical
 * text, exact selected original_hex and contains_replacements. It performs no
 * live source reads and never claims the unseen file is immutable.
 * Edit/forget additionally require the accepted passage update_sequence.
 * Label/body contracts match fcb_desk_annotate; late cancel is not rollback. */
char *fcb_desk_import_observation(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t reader, uint64_t observation, uint64_t start, uint64_t end);
/* Keep a search result's full lossless captured excerpt as an observed passage.
 * family=0: frozen atlas stream, revision MUST be 0. family=1: progressive
 * catalog stream, revision MUST be the accepted nonzero publication revision.
 * generation/index bind the accepted result; engine-owned bytes, decoder and
 * source observation supply all provenance. No current source path is read.
 * Both handles stay alive through return. Unsupported partial evidence or
 * malformed text fails without importing. Reconcile a lost reply with state;
 * never blindly repeat an attempt. Return/free ownership matches other calls. */
char *fcb_desk_import_stream_excerpt(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t stream, uint64_t family, uint64_t generation, uint64_t revision, uint64_t index);
char *fcb_desk_observed_passage(uint64_t handle, uint64_t expected, uint64_t passage);
char *fcb_desk_edit_observed_passage(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t passage, uint64_t sequence, const char *label, const uint8_t *body, uint64_t body_bytes);
char *fcb_desk_forget_observed_passage(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t passage, uint64_t sequence);
/* Explicit current-file observation. UTF-8 path is host authority; no implicit
 * root access is conferred. Regular source files only, no source writes. */
char *fcb_desk_open(uint64_t handle, uint64_t expected, uint64_t attempt, const char *path);
/* kind: 0 focus pane; 1 navigate pane/start; 2 select pane/[start,end);
 * 3 bookmark pane/label; 4 recall bookmark; 5 forget bookmark; 6 pin pane;
 * 7 unpin pane; 8 close pane; 9 back; 10 forward; 11 clear history;
 * 12 deliberately reattach bookmark: subject=bookmark, start=target pane,
 * end=expected bookmark update_sequence. The current model_revision binds both
 * captured locations. Both selections must be nonempty and byte-identical.
 * This explicit correspondence preserves label/body/ID, increments the note
 * sequence, and grants no file continuity or live-path access. Stale or
 * mismatching selections leave the original note and anchor unchanged.
 * subject is pane or bookmark as appropriate. Label is <=256 UTF-8 bytes;
 * null is empty and accepted for commands that do not use a label. */
char *fcb_desk_apply(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t kind, uint64_t subject, uint64_t start, uint64_t end, const char *label);
/* Personal annotations share the bookmark IDs/retention/recall/forget commands.
 * Create uses the pane's exact current capture/range. Edit preserves that anchor
 * and ID, updating label/body and update_sequence only. Empty body is allowed.
 * Labels are nonempty UTF-8 C strings <=256 bytes. Bodies are length-prefixed
 * UTF-8 <=16384 bytes, including embedded NUL/newline; null is valid only when
 * body_bytes==0. Borrowed inputs remain readable through return and are copied.
 * Source files are never changed. Full state replies include body and canonical
 * decimal update_sequence on every bookmark. Lost replies require state
 * reconciliation, never replay of the same attempt. Explicit checkpoints retain
 * note text; old checkpoints restore empty bodies without reopening live paths. */
char *fcb_desk_annotate(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t pane, const char *label, const uint8_t *body, uint64_t body_bytes);
char *fcb_desk_edit_bookmark(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t bookmark, const char *label, const uint8_t *body, uint64_t body_bytes);
/* Read-only guided recovery of a bookmark with a nonempty saved selection. Its
 * original capture survives pane closure. Compare against an explicitly chosen
 * retained pane, with <=67108864 work units and <=512 edit-distance allowance.
 * Each source is <=4 MiB; zero work/edits are valid partial-comparison budgets.
 * command="suggest-reattachment" reports mapped/unchanged/changed/unresolved,
 * original/target byte ranges, both capture identities, bounded logical-text
 * previews and exact/work-limit/edit-limit comparison quality. A mapped range
 * lies wholly in a byte-verified equal span even if other spans are unresolved.
 * This deterministic alignment is NOT identity continuity. Repeated text may
 * have other correspondences. No note, pane, revision or attempt changes and
 * no source is reopened. Preview text never grants source coordinates. */
char *fcb_desk_suggest_reattachment(uint64_t handle, uint64_t expected,
    uint64_t bookmark, uint64_t sequence, uint64_t pane, uint64_t max_work, uint64_t max_edits);
/* Explicit confirmation, independent of the proposal receipt: validates current
 * desk revision, bookmark sequence and target pane/file/source_revision, then
 * re-compares the actual retained original bytes. Moves only this note's anchor
 * to [start,end), with offset=start; preserves ID/label/body and increments its
 * sequence. Pane selections, active focus and history remain unchanged. Empty,
 * mismatching, stale or canceled mappings leave the note unchanged. As with all
 * mutations, a lost reply requires a state read before another action. */
char *fcb_desk_reattach_range(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t bookmark, uint64_t sequence, uint64_t pane, uint64_t file,
    uint64_t source_revision, uint64_t start, uint64_t end);
/* SourceReader logical text with original byte ranges. 4..65536 requested
 * bytes, <=1024 rows. Page text is not copied back into source storage. */
char *fcb_desk_window(uint64_t handle, uint64_t expected, uint64_t pane,
    uint64_t offset, uint64_t bytes);
/* Import exact retained reader bytes across owner domains without reopening a
 * path. Caller holds reader+native grant through return. start/end select the
 * original range. Repeated source identities reuse the receiving capture. */
char *fcb_desk_import_reader(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t reader, uint64_t start, uint64_t end);
/* Keep [start,end) from a retained fcb_reader as a saved passage. Nonempty,
 * <=256 KiB selection; UTF-8/UTF-16 scalar and CRLF boundaries must be exact.
 * Retains only the selection plus <=8 context bytes per side, with original
 * owner/file/source_revision, raw path, encoding and capture length. Supports
 * the reader's full 32 MiB capture limit without a whole-capture desk import.
 * Evidence is an observed-extent with consistency host-supplied-sequence:
 * immutable captured bytes, no new live observation or whole-file desk claim.
 * Notes, recovery checkpoints and source-pack export use existing passage APIs.
 * Both handles must remain alive through return. Reconcile a canceled/lost
 * mutation reply through desk state before another action; never replay it.
 * Free every non-null response with fcb_free_string. */
char *fcb_desk_import_reader_passage(uint64_t handle, uint64_t expected, uint64_t attempt,
    uint64_t reader, uint64_t start, uint64_t end);
/* Open a retained pane in an ALREADY CREATED EMPTY fcb_reader destination.
 * Copies <=4 MiB of exact original bytes under independent reader admission.
 * No path lookup, disk refresh, source grant or desk mutation is involved.
 * Success is desk/1 command "open-reader" with original pane/location/label,
 * captured_bytes and nested "reader" containing ordinary reader-session/1 info.
 * The nested owner is destination, capture_origin is host-supplied, initial read
 * counters are zero and the digest covers all imported original bytes. The new
 * reader supports the usual find/outline/Markdown/copy/fork/compare operations.
 * Both handles may be canceled during transfer. Keep both until return; retire
 * destination on any rejected/lost receipt (late cancellation is not rollback).
 * After accepted handoff, destination survives source desk closure or restore.
 * Saved labels never authorize fcb_reader_open or a native Refresh from disk. */
char *fcb_desk_open_reader(uint64_t handle, uint64_t expected, uint64_t pane,
    uint64_t destination);
/* Checkpoints contain ALL retained source, personal labels and notes, unencrypted.
 * User selects an explicit new destination and disclosure cap. Existing files
 * are not overwritten. Save returns effect/bytes_written even on failure: a
 * late cancel/close is never a no-effects claim. Preserve/inspect partial files.
 * Restore imports verified bytes with fresh local identities and no live-path
 * reopening. Invalid/canceled restore keeps the prior desk state. */
char *fcb_desk_save(uint64_t handle, uint64_t expected, const char *path, uint64_t max_source_bytes);
char *fcb_desk_restore(uint64_t handle, uint64_t expected, uint64_t attempt, const char *path);
/* Explicit bounded JSON source pack of ALL bookmark labels/notes and whole
 * saved selections, encoded reversibly as original-byte hex. No live reads or
 * observed-passage payloads/notes: excluded_observed_passages reports their
 * count explicitly. Use a checkpoint to preserve the entire Reading Desk.
 * inferred selection from a caret. Missing selections and budget omissions are
 * reported per entry; metadata refusal creates no file. Source limit includes
 * repeated selections; output bytes/scalars cover the complete escaped JSON.
 * output_bytes and output_scalars must each be in 1..16777216. Choose a NEW file
 * and review unredacted source, labels and notes before calling. No overwrite.
 * Returns command "export" with effect, bytes_written, digest, budgets and
 * coverage even after late cancel/close. Status "ok" proves publication, while
 * complete describes selection coverage independently. A lost response is an
 * uncertain effect: inspect the destination, never automatically replay. */
char *fcb_desk_export(uint64_t handle, uint64_t expected, const char *path,
    uint64_t source_bytes, uint64_t output_bytes, uint64_t output_scalars);
/* Read-only integrity check against a prior successful save receipt. The host
 * holds a valid readable regular-file descriptor and a 32-byte SHA-256 digest
 * through return; it must not close/reuse the descriptor concurrently. Streaming
 * uses <=64 KiB scratch and <=64 MiB input; body, header, trailer and final file
 * metadata are verified. The shared file cursor and caller's ownership are
 * unchanged. No source path, desk mutation, restore, or registry slot is used.
 * Call on a worker. Returns 1 only for full verification, 0 on every failure.
 * Does not qualify semantic invariants of a new, untrusted checkpoint: use the
 * normal restore path for that. No deletion may rely on a failed check. */
uint8_t fcb_desk_checkpoint_verify_fd(int32_t descriptor, uint64_t expected_bytes,
    const uint8_t *expected_digest);
uint8_t fcb_desk_cancel(uint64_t handle);
uint8_t fcb_desk_close(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
