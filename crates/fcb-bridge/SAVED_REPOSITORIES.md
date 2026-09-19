# Retained saved-repository browsing

`include/fcb_saved_repository.h` connects the existing FCBS source archive and
FCBD demand-paged posting engines to retained host handles. This is a saved
repository, not a reopened live root or another atlas. The safe implementation
is `fcb_app::host::saved_repository::SavedRepositorySession`; the bridge owns
only handles, cancellation and pointer conversion.

## Open, search, read

Reserve with `fcb_saved_create`, then call `fcb_saved_open` on a host worker.
Opening validates the full bounded archive checksum and every member digest
once while retaining metadata and its open descriptor, not the archive body.
Subsequent pathname replacement cannot redirect this descriptor. Member loads
seek to recorded offsets and revalidate original bytes against their SHA-256.
Archived native names are labels only and never acquire live filesystem grants.

Use `fcb_saved_members` to page the saved inventory without member payload I/O.
Unavailable members remain distinct from captured empty files. Member ordinals
are zero-based; file/revision values are qualified by this saved-session owner.
They are not persistent process IDs imported from untrusted archive metadata.

`fcb_saved_search` performs exact case-sensitive decoded-literal search. It uses
the existing paged-source scanner; no second matcher or decoder is introduced.
The direct route visits saved members. An attached index instead drives the
existing rarest-posting candidate intersection with exact verification and the
existing UTF-16/short-query/uncovered fallback. Verification keeps at most one
member resident. Accepted queries keep bounded hit metadata, not the complete
source of every matching file. Result pagination performs no archive/index I/O.

Pass the accepted query generation and one-based hit ID to
`fcb_saved_open_hit_reader`, with an EXISTING EMPTY destination created using
`fcb_reader_create`. Or use `fcb_saved_open_member_reader` with a member ordinal.
Destination admission occurs before source I/O. The saved member is verified
again before the ordinary ReaderSession acquires an independent source capture.
There is bounded copy overlap during this handoff; it is not a zero-copy claim.

The receipt links saved file/revision/digests to the new reader owner. Hit
activation includes exact original-byte selection/hex and decoded context; it
does not invent native glyph, UTF-16, bidi or presented-frame selection geometry.
The existing reader window, line, search, outline and document APIs then work
without returning to the archive or original live root. Already-delivered
readers survive result clearing, index replacement and saved-handle closure.

## Optional demand-paged index

Build the FCBD file through the existing paged snapshot-index build workflow
and retain its trusted build receipt separately. Supply that receipt's
64-hex digest to `fcb_saved_attach_index`; never derive the expected pin from the
untrusted file being opened. The pin covers the metadata envelope and page
hashes, not a freshly hashed whole index body. Opening verifies its archive
binding and metadata. Loaded pages are SHA-256 verified before use; unread page
integrity remains unknown. Four 16 KiB pages are cached across repeated queries.

Attachment is optional and explicit. This API neither builds nor writes an
index. A wrong pin or wrong-archive attachment preserves the old accepted index
and results. Detaching switches future searches to direct saved-source scanning
without invalidating existing hits. An index page corruption failure preserves
completed rows; detach or explicitly attach a healthy trusted index to continue
searching. Corrupt archived member bytes cannot become a new exact reader.

## Replacement, cancellation and ownership

Query search/clear operations share one non-reusing increasing generation.
Index attach/detach use a separate increasing generation. Admitted failed
attempts consume their numbers. Both replacement routes prepare complete output
before publishing accepted state; failure/cancellation preserves its predecessor.
An index change cannot change the archive identity or the meaning of an old hit.

Four live/retiring saved handles are admitted. Their identities use the same
non-reusing allocator as atlas/reader handles, so wrong-kind handles cannot
alias a live object. Every operation uses a nonblocking session lock; a busy
saved session does not block another handle. Table locks never enclose source
work. Reader activation locks saved source before reader, both nonblocking.

`fcb_saved_cancel` changes the current operation epoch without waiting for I/O.
Subsequent requests may run after the current operation drains. Reader-only
cancellation does not cancel the shared saved repository. Closing removes its
authority immediately, while in-flight work retains the admission slot until
actual release. Final close/destruction belongs on a worker, not redraw/input.

Late cancellation can suppress delivery after acceptance without rolling back
accepted state. Reconcile using info or the known result generation. All returned
JSON strings are independently owned; close never frees a returned string.
Release each non-null response exactly once with `fcb_free_string`, including
errors. Full-width numbers use canonical decimal JSON strings. Ordinary errors
are bounded and redacted rather than including queries or source payloads.

## Limits and verification boundary

The C open route uses existing defaults: 65,536 members, 1 MiB per captured
member, 64 MiB captured source, 80 MiB archive, 2,048 bytes per raw relative path.
The Rust host accepts explicit SnapshotLimits, with member size additionally
bounded by the existing 4 MiB retained-reader ceiling. Queries retain at most
4,096 hits; pages contain 1..128 rows and literal needles at most 1,024 UTF-8
bytes. Source, metadata, old/new state and response buffers keep distinct leases.
Managed reservations do not claim measured RSS, zero-copy handoff or speedup.

Open/search/activation are synchronous host-worker operations with existing
engine cancellation checkpoints. This host route does not yet expose native
begin/step continuations or trusted-catalog cold opens. It does not implement
unlimited-size archives, out-of-core index construction, live watchers, native
widgets, Metal presentation, or physical-Mac qualification. The existing CLI
formats/engines are reused unchanged apart from the shared query-limit repair.

Production tests are in `fcb-app/tests/retained_saved_repository.rs` and the
bridge's `saved_sessions_tests.rs` / `saved_ffi_tests.rs`. The shared query-limit
regression in `fcb/tests/paged_query_limits.rs` checks four actual scan/index
routes. These changes are code-first, batch verification pending: compilation,
test execution and strict RCH have not run in the authoring environment. No
bead or product gate is claimed complete. The bridge explicitly selects the
already-transitive snapshot feature; no new package or runtime is introduced.
