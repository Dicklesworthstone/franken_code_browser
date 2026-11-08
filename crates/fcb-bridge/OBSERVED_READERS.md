# Observed range readers

The API in `include/fcb_observed_reader.h` opens a current, explicitly selected
root-relative file without capturing its entire contents. It uses the existing
safe `fcb_app::host::observed_reader` adapter, `FileRangeReader`, `ObservedExtent`
and logical extent decoder. It does not change the complete-capture contract of
`fcb_reader_*` or import live ranges into an old search/desk/archive capture.

## Host lifecycle

1. Reserve an empty handle with `fcb_observed_reader_create`.
2. Keep the current project grant alive and call `open` on a worker with the
   absolute root and canonical lowercase hex for the exact native relative path.
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
native root authority; a saved path or an old exact result does not grant it.

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

Failed/canceled observation candidates do not replace retained pages. A late
cancellation or lost bridge reply can suppress delivery after state acceptance;
hosts preserve their last accepted view and use retained/info operations when
reconciling. They never automatically repeat a live read under an old identity.

## Verification boundary

`fcb-app/tests/observed_host_reader.rs` exercises the real source adapter;
`observed_reader_ffi_tests.rs` exercises the exported C route, and
`observed_reader_sessions_tests.rs` covers capacity, cancellation, owner identity
and draining permits. Native decoder/model/ABI fixtures supplement those paths.
The implementation is code-first, batch verification pending. Unexecuted
regressions and source review do not qualify compilation or physical Mac UI.
