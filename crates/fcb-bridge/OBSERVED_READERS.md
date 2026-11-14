# Observed range readers

The API in `include/fcb_observed_reader.h` opens a current, explicitly selected
root-relative file or exact standalone file without capturing its entire
contents. It uses the existing safe `fcb_app::host::observed_reader` adapter,
`FileRangeReader`, `ObservedExtent` and logical extent decoder. It does not
change the complete-capture contract of `fcb_reader_*` or import live ranges
into an old search/desk/archive capture.

## Host lifecycle

1. Reserve an empty handle with `fcb_observed_reader_create`.
2. Keep the selected source's native grant alive. For a project file, call `open`
   on a worker with the absolute root and canonical lowercase hex for the exact
   native relative path. For a standalone file, call `open_file` with lowercase
   hex for its exact absolute native path, including non-UTF8 path components.
   Check the returned `info` identity and raw absolute path. Open reads metadata
   only; a lost reply is not permission to reopen a loaded handle.
3. Call `window` with an absolute original-byte offset, a 4–262144 byte visible
   budget, an explicit encoding and an optional cooperative cancellation poll.
4. Use the returned observation ID with `retained` or `copy`. These operations
   read only retained immutable bytes. An evicted observation is unavailable.
5. Cancel obsolete work, then close on a worker. Keep native access alive until
   active calls and the final close have drained. Free every non-null JSON result
   once with this library's `fcb_free_string`.

There are four admitted handles, including closed handles whose active work is
still draining, and eight retained observations per reader. Identities share the
library's monotone allocator with other retained services. Table locks never
enclose source I/O. Each observation has a separate source revision; revisiting
the same byte offset with `window` creates a new observation.

## Source authority and coverage

The opened regular-file descriptor is retained. Edits to that file object can
affect later observations, while previously captured bytes remain unchanged.
Atomic replacement of its pathname does not retarget the descriptor. Opening a
new handle is the explicit way to follow the current pathname.

Root-relative component validation and the existing observed-symlink and
nonblocking/no-follow leaf checks apply. This does not qualify confinement
against hostile replacement of ancestor directories. The embedding host owns
native source authority; a saved path or an old exact result does not grant it.
The standalone `open_file` route requires only the selected file's grant. It
does not stat, catalog or grant the parent as a project. It validates bounded
absolute path components and admits the leaf with the same regular-file,
no-follow and nonblocking checks. Both routes initialize from the admitted
descriptor, report zero payload reads on open and retain identical range,
search, copy and cancellation behavior. Neither route can reopen a loaded
handle through the other entry point.

Responses use `fcb.observed-reader/1`. Integer fields are canonical decimal
strings; native paths carry reversible Unix bytes. Each window reports its
requested visible/capture ranges, actual captured and visible ranges, fresh
observation ID, extent-only SHA-256, observed/final lengths, consistency,
request-filled state and actual read counters. The common envelope explicitly
sets `whole_file_immutable` to false. Capturing all bytes in a small observation
still does not merge this reader's independent requests into a whole-file
capture identity.

Encoding is declared per observation: 1=UTF-8, 2=UTF-16LE, 3=UTF-16BE. Small
context reads preserve scalar and CRLF boundaries. Returned text is explicitly
logical observed text, not paragraph shaping or an exact global line-number
index. Original-byte copies are distinct from decoded display text.

## Find across the opened file

`fcb_observed_reader_find` runs the existing `FileSearch` streaming engine over
the same opened file object. It starts at byte zero and reads through a bounded
16 KiB buffer. Searching a large file does not capture that whole file, populate
the range-reader cache, create a temporary source copy, or reopen its pathname.
The host runs the call on its worker and can cancel it through the supplied poll
or the handle cancellation API.

The request supplies a strictly increasing query generation, an exact
case-sensitive UTF-8 literal, the declared source encoding, and byte/hit limits.
Supported encodings are UTF-8, UTF-16LE and UTF-16BE. Matches can overlap and cross
read boundaries. Limits are 4096 UTF-8 query bytes, 256 retained hits, 4 GiB scanned
source bytes and 1,048,576 read calls. A host can admit a smaller operation; zero
retained hits still probes for an actual match instead of assuming truncation.

The `find` receipt separates observed input length, bytes actually read/scanned,
matches seen, retained rows, decoder limitations and before/after metadata
consistency. Limit exhaustion is partial coverage, not proof that no later
match exists. `search_complete` requires both a completed scan and unchanged
observed metadata. It never means an atomic filesystem snapshot. Each search
spends a fresh `search_observation` from the same monotone revision domain as
range observations, independently of its query generation.

One accepted search retains only its hit coordinates and a shared exact literal
witness. `fcb_observed_reader_search_hit` resolves a zero-based hit index under
that accepted generation and returns the original matched bytes without I/O.
Those bytes remain exact after edits or pathname replacement. Surrounding source
was not retained. To inspect it, explicitly request a fresh window, identify it
as a new observation, and compare the old literal witness with that window's
original bytes before adding a match highlight. A changed or unavailable range
must not receive the old match's identity.

`fcb_observed_reader_observe_hit` combines that explicit fresh-context request
and witness verification. It takes the accepted query generation and hit index,
a bounded amount of preceding context, and a 4–262144 byte window large enough
for the literal plus eight bytes of boundary slack. The source
adapter checks the retained literal's original bytes, decoded text and exact
original-to-decoded range mapping before publishing or evicting any page. A
changed or unavailable hit returns `OBSERVED_READER_SEARCH_HIT_CHANGED` and
leaves accepted observations intact. Success creates a new range observation
with a verified `find_hit` receipt and page-local UTF-8 selection. Retained
navigation replays that receipt even after a later Find replaces the query;
the page never becomes a result of the replacement query.

Failed/canceled replacement searches preserve the previous accepted query under
its original generation. Once a new query is accepted, older query targets are
unavailable. Hosts revoke obsolete visible targets immediately on query or
encoding changes and fence late replies with their own request identity. Actual
I/O from unsuccessful scans is still counted by `info`; its existing
`retained_payload_bytes` field continues to describe the eight range observations.

Failed/canceled observation candidates do not replace retained pages. A late
cancellation or lost bridge reply can suppress delivery after state acceptance;
hosts preserve their last accepted view and use retained/info operations when
reconciling. They never automatically repeat a live read under an old identity.

## Rescanning later result pages

`fcb_observed_reader_find_page` accepts the same text or whole-token mode,
matching policy, counting option and work limits as `find_mode`, plus
`start_after`: the number of qualified occurrences to skip before retaining
this result page. It exposes the public `FileSearch::new_page` route. Existing
Find entry points keep their first-page behavior.

Every page request scans the explicitly opened file object again from byte zero.
Skipped occurrences still consume the byte, read-call and cancellation budgets;
their source bytes and witnesses are not retained. Each request can retain at
most 256 hits. Exact UTF-8/UTF-16 text, overlapping matches, Unicode lowercase
expansions and whole-token occurrences use the same shared search engine as the
first page. Row indexes start at zero within a page; `occurrence_id` identifies
the occurrence in that request's complete observed sequence, including its
skipped prefix.

The Find receipt includes three independent page fields:

| Field | Meaning |
|---|---|
| `page_start_after` | Requested number of occurrences to skip. |
| `skipped_matches` | Qualified occurrences actually skipped; a bounded scan may stop before reaching the requested page. |
| `page_has_more_matches` | This scan verified at least one additional occurrence after the retained page. A false value proves no later occurrence only when `search_complete` is true. |

`matches_seen` and match-count completeness describe this request's whole scan,
not just its retained page. Deliberately skipped occurrences contribute to
`truncated`; that flag alone does not mean a later page exists. A completed
scan can return an empty page when fewer occurrences exist than `start_after`.
A byte or read-call limit before the requested page instead leaves coverage
partial.

These are **new observations of live source**, not continuation of an immutable
capture. Each request requires a fresh query generation and spends a fresh
observation ID. Edits can change which occurrence falls at a given ordinal.
Hosts must disclose the rescan, replace the accepted result page only after its
receipt is validated, and never combine pages into one exact snapshot or carry
a previous observation's total count forward as current. Hit activation and
copy continue to use only the new page's retained witnesses. The existing stale
generation, cancellation, grant-lifetime and lost-reply rules still apply.

## Verification boundary

`fcb-app/tests/observed_host_reader.rs`, `observed_reader_search.rs` and
`observed_reader_hit_navigation.rs` exercise the real source adapter, streaming
search and fresh-hit verification; `observed_reader_ffi_tests.rs` and
`observed_reader_search_ffi_tests.rs` exercise the exported C routes, and
`observed_reader_sessions_tests.rs` covers capacity, cancellation, owner identity
and draining permits. Native decoder/model/ABI fixtures supplement those paths.
The implementation is code-first, batch verification pending. Unexecuted
regressions and source review do not qualify compilation or physical Mac UI.
