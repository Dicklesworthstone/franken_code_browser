# Retained native reader sessions

The native bridge can now keep one exact source observation open across byte
windows, line navigation, find-in-file, hit previews, and exact byte copies.
Those actions never reopen the working-tree path. This is a SINGLE-FILE reader
session, not a retained workspace/atlas session or an atomic filesystem snapshot.

Public C declarations: `include/fcb_reader_sessions.h`, also included by the
existing `fcb_host_services.h`. Safe Rust service: `fcb_app::host::reader::ReaderSession`.
The C ownership table is safe Rust; only C-string marshaling uses unsafe code.
No dependency, parser, decoder, matcher, runtime or worker pool was added.

## Lifecycle

Call `fcb_reader_create` to reserve an empty opaque integer handle, then call
`fcb_reader_open` on the host's worker. Creating a handle does no source I/O.
Creation returns zero on admission/identity failure. The empty handle already
exists during capture, so another native thread can cancel or close that work.

A successful open pins a complete regular-file observation, up to an explicitly
supplied allowance of 4 MiB. It uses the SAME bounded file reader as the existing
legacy text service, including regular-object checks, read-call limits, and
before/after metadata comparisons. NUL, malformed text and BOM-marked UTF-16 are
retained as original bytes rather than being forced into a text-only C string.
Metadata agreement is not an atomic snapshot guarantee or qualified confinement
against hostile ancestor replacement. Hosts still own current native grants.

A loaded handle cannot be reopened. Refresh is explicit: create a new handle,
prepare its capture, then replace the old view after validating its response.
Failed initialization can be retried while the old handle remains empty.
`fcb_reader_info` exposes capture identity, initial I/O and accepted query state.

Every successful response carries its owner (the C handle), file ID and source
revision as canonical decimal strings. Handles are never recycled within the
loaded library, including when a table slot is reused. They are not durable
cross-process IDs. IDs from old one-shot workspace search or atlas invocations
cannot be used as anchors into this new capture. Retained workspace search and
native frame acknowledgment remain separate integration work.

## Reading and exact selection

`fcb_reader_window(handle, offset, bytes)` returns a logical source window with
original-byte hex and the existing decoder's text, range and replacement flag.
The encoding is the one established from the retained capture's prefix, even
for distant windows. UTF-8 scalars, UTF-16 pairs and CRLF boundaries use the
shared extent reader. Rounding is explicit in `boundaries_adjusted`; omitted
bytes are available through subsequent retained-window requests.

`fcb_reader_lines(handle, first, count, max_bytes)` uses the shared physical-line
scanner. Lines are one-based; CR, LF and CRLF terminate lines, and there is no
phantom row after a final terminator. Empty captures have no physical lines but
remain readable with a byte window. These are NOT rendered visual rows or the
separate source-reader editor-row convention. Locating lines is bounded worker
work over the admitted capture, not a retained per-line index or native shaping.

Byte windows and raw copies admit at most 256 KiB per operation. Line requests
admit at most 1024 physical lines. A giant line produces a byte-limited window
with `range_limited: true` and a continuation offset, never a claim that the
entire line was displayed. Raw copies refuse oversized requests instead of
silently truncating them.

`fcb_reader_find` performs exact case-sensitive literal matching with the shared
streaming engine. The source is already captured: the scan adds ZERO filesystem
reads. A caller-supplied query generation must strictly increase across attempts,
including canceled scans. Up to 4096 occurrences are retained; source-work and
result limits, unsupported decoding, and bounded lookahead remain explicit.
A full row buffer alone is not proof of truncation. Zero rows is not exhaustive
count-only mode. Malformed text cannot establish a successful negative answer.

A replacement query is accepted after its private response is completely
encoded. A failed or canceled unaccepted replacement leaves the older rows
available under their original generation. `fcb_reader_hit` and
`fcb_reader_copy_hit` reject a different generation or missing index instead of
selecting whatever now occupies that row. Hit previews include the whole exact
match plus up to 16 KiB of context per side and bounded scalar padding.

`fcb_reader_copy_range` returns exact original bytes as JSON hex, including
partial-scalar selections, NUL and malformed source. It does not claim decoded
clipboard text. No operation publishes a native clipboard or writes a source
file. The host chooses any subsequent external action explicitly.

## Cancellation, concurrency, admission

All source/query work remains synchronous on the calling host worker. The
bridge creates no executor, thread, global logger, watcher or detached task.
Do not call source work or final source destruction from input/redraw callbacks.

The C table admits at most EIGHT live, initializing or retiring reader cells.
A per-cell operation lock uses try-lock: a competing call receives
`READER_HANDLE_BUSY`, rather than waiting or entering an unbounded queue.
Independent readers can proceed concurrently. The table lock is never held
while opening, decoding, searching or encoding source data.

`fcb_reader_cancel` changes the operation epoch without taking the source/query
lock. Current work sees cooperative cancellation; a subsequent call can use the
same immutable capture after that work drains. Cancellation does not make an
OS read interruptible or roll back state already accepted before cancellation.
A late cancellation may suppress a reply after acceptance; inspect `info` when
reconciling that boundary. Hosts must reject delayed data for obsolete handles
and query generations regardless of when a response was prepared.

`fcb_reader_close` removes the handle and invalidates in-flight work. Its source
and admission permit survive until the last active call releases the cell.
Closing and immediately reopening cannot bypass the eight-cell resource bound
while old operations are still draining. Final source destruction occurs outside
the table lock and belongs on a worker. Poisoned sessions can still be canceled
and closed without poisoning other sessions.

Source, query, decoder, registry and response admissions are separate. Each
reader has a 256 MiB managed accounting domain; source capture is at most 4 MiB,
and output is privately encoded within 8 MiB. Existing returned safe-Rust
responses keep their own leases. Native strings after handoff are separately
host-owned and must be freed; these admissions are NOT a total-process RSS cap.

## Native handoff and verification

Except for create/cancel/close scalar returns, calls return `fcb.reader-session/1`
JSON, including ordinary errors. A non-null pointer alone never means success.
Inspect status, query generation, search coverage and window limits. Null means
no complete handoff (including contained unwinding panics). Invalid foreign
pointers and aborts cannot be recovered by this boundary.

Free every non-null JSON string exactly once with this loaded library's
`fcb_free_string`. Close does NOT free already returned strings. No caller may
mutate a returned allocation, free it with a foreign allocator, or use a closed
handle as a different session. Cancellation/revocation cannot retract bytes
already delivered to an external consumer.

Tests: `fcb-app/tests/retained_host_reader.rs`,
`fcb-bridge/src/reader_sessions_tests.rs`, and `reader_ffi_tests.rs`.
These include live source replacement, both UTF-16 byte orders, BOM/CRLF and
supplementary characters, lossless raw copy, query replacement, simultaneous
readers, cancellation, active-close admission, poisoned-session reclamation,
non-reused identities and public ABI type checks.

Code-first, batch verification pending. Rust compilation/tests, strict RCH,
SwiftUI consumption, native accessibility and physical-Mac ABI/rendering have
NOT been executed by this authoring session. Logical decoded text is not native
shaped/acknowledged pixels, and these commits do not close product gates.
