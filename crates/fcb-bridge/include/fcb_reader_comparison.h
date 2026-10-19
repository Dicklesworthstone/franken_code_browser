#ifndef FCB_READER_COMPARISON_H
#define FCB_READER_COMPARISON_H

#include "fcb_reader_sessions.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Explicit comparison of two different retained reader handles in this loaded
 * library. Both source captures and their byte reservations survive the call;
 * no pathname is reopened and no query or selection is transferred. Invoke on
 * a worker. Concurrent reader use fails Busy instead of blocking; cancellation
 * or closure of either handle invalidates in-flight output.
 *
 * generation must strictly increase across admitted attempts on before_handle,
 * including canceled attempts. max_edit_distance <= 512; max_work <= 67108864.
 * Each source remains bounded by FCB_READER_MAX_SOURCE_BYTES. Zero work/edit
 * allowances are valid and can produce explicit unresolved regions.
 *
 * fcb.reader-session/1 command=compare returns BOTH original owner/file/revision,
 * raw native path, capture length, encoding and SHA-256 receipts. comparison_owner
 * is an isolated analysis identity, never a usable reader handle. Inspect
 * comparison_complete, comparison_quality and relation. spans partition both
 * complete captures, including unresolved spans; original-byte ranges may split
 * encoded text. Equal spans are byte verified. Caller-chosen before/after roles
 * imply no file continuity, Git history, rename or annotation reattachment.
 *
 * The complete bounded partition has at most 1029 spans. No source payload is
 * embedded; navigate/copy via the still-retained matching reader identities.
 * A range must never be applied to a replacement reader with the same path.
 * Every non-null result is released exactly once with fcb_free_string. */
char *fcb_reader_compare(uint64_t before_handle, uint64_t after_handle,
    uint64_t generation, uint64_t max_edit_distance, uint64_t max_work);

#ifdef __cplusplus
}
#endif
#endif
