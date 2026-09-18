# Retained native atlas-to-reader navigation

Native callers can now discover a repository once, retain its spatial index,
move the camera, focus a subtree, return through history, navigate a conventional
tree, and open a displayed file into the existing retained reader.

Public C declarations: `include/fcb_atlas_sessions.h`, also included by
`fcb_host_services.h`. Safe application API:
`fcb_app::host::atlas_session::AtlasSession`. Public engine ownership:
`fcb::map::workspace::retained::RetainedWorkspaceAtlas` and
`fcb_map::atlas::retained::RetainedAtlasIndex`.

## Native workflow

Create an atlas with `fcb_atlas_create`, then call `fcb_atlas_open` on a host
worker. Opening performs bounded metadata discovery, shared partition layout,
and spatial-index preparation, but does not read source payloads or repository
rule files. It does not create a worker, runtime, window, renderer, or watcher.
The scope uses the existing static product exclusion policy.

Request a candidate with `fcb_atlas_view`. Pan, anchored zoom, subtree focus,
back, and resize use the same retained catalog, geometry and spatial-index
allocations. Index views share ownership without sorting paths, allocating
another spatial index or reconstructing the repository. Visible queries remain
bounded by parcel and traversal allowances. They can emit explicitly labeled
aggregates, not invented visible files. File IDs and raw path identities remain
those of the frozen catalog.

The response is `fcb.atlas-session/1` JSON, with finite logical-point rectangles,
canonical string IDs/counts, and reversible native byte paths. Capped-log-byte
layout weights and parent-local/focus-island geometry use the existing engine.
`fcb_atlas_children` exposes bounded direct-child pages for conventional tree
and keyboard navigation. `next_offset` is null only at the end of that page's
parent membership. Selecting a file and focusing its directory remain distinct.
Focus navigation saves at most 64 camera/focus endpoints; pan/zoom do not fill
history with gesture ticks. Back restores the transform under a fresh camera
identity, adapting to the current display rather than restoring stale pixels.

## Prepared geometry is not presented geometry

`fcb_atlas_present` explicitly declares the plan the host conservatively knows
was displayed. The bridge does not observe AppKit/Metal presentation itself.
A completed candidate does not replace the acknowledged picking plan. While a
new camera candidate waits, `fcb_atlas_pick` uses the previous acknowledged
geometry. Picking reuses the existing half-open rectangle and aggregate rules;
it does not inspect a newer model just because its worker finished.

Plan generations strictly increase across admitted attempts, including failed
or canceled work. Frame IDs strictly increase across acknowledgment. Display
generations change independently on resize. Wrong frames, display generations,
and superseded never-acknowledged candidates are refused. A failed preparation
preserves the previous camera, history and candidate; an accepted earlier frame
can be re-presented explicitly while a newer candidate remains pending.
The host must reject delayed responses for obsolete handles or generations.

## Opening source and returning

Create an empty destination using `fcb_reader_create`, then call
`fcb_atlas_open_reader(atlas, reader, frame, display, x, y, max_source_bytes)`.
Only an exact file parcel in the acknowledged frame may open source. The path
comes from its retained file binding, not a display string supplied by the UI.
The inherited path-check and regular-file read policies are revalidated; this
is not qualified as confinement against hostile ancestor replacement.

The operation admits/locks the empty reader before capturing source. The safe
application service prepares both the retained reader and its linked receipt
before the registry installs it. The receipt names the atlas node/FileId and
the separate reader owner: this is a NEW source observation after metadata
selection, not an old source capture supposedly taken during discovery.
Afterward, byte/line windows, find-in-file, hit previews and exact copies use
that same reader capture even after the live file changes. No existing reader
is overwritten. Closing the atlas does not destroy an independently delivered
reader. Hosts must enforce grant revocation across their own retained consumers.

## Bounds, cancellation and honest coverage

The native table admits four live, initializing or retiring atlases, separately
from the existing eight-reader bound. Both tables share one non-recycling handle
allocator; an atlas number cannot accidentally select a reader with that number.
Table locks never cover engine work. Per-session operations use try-lock and
report busy rather than queueing or blocking. Joint source activation takes the
atlas then reader operation lock, both nonblocking. Cancellation advances an
epoch without waiting on those locks. A closed cell retains its admission until
active calls release it; closing/reopening cannot bypass retiring-cell limits.

Default discovery is 4096 files; the C open call allows up to 20000, still under
shared 512 KiB path, 4096-page and 32-level discovery bounds. Partial discovery
is usable but remains explicit; no refresh or live reconciliation is implied.
The C viewport defaults to 1024 parcels/32768 visits. Safe Rust options admit
up to 4096 parcels/131072 visits. Responses fit the existing 8 MiB cap. Each
atlas has a 256 MiB managed accounting domain, not a process-footprint guarantee.
Reader capture remains separately admitted up to 4 MiB.

All operations are synchronous worker services, including JSON encoding and
final large-object destruction. Do not call these operations directly in input
or redraw callbacks. A native host can schedule them through its owned runtime.
There is no claim of a wall-clock bound on OS reads or hard cancellation.
Cancellation after state acceptance can suppress a receipt without rolling back
that state. In particular, inspect or close the already-known destination
reader after an uncertain activation; do not assume it was never installed.
Free every non-null string once with this loaded library's `fcb_free_string`.
Neither cancel nor close frees strings already handed to the host.

This slice retains metadata geometry, not a workspace text-search result set.
Existing one-shot workspace search and atlas text/streaming CLI routes remain
separate. Automatic file reconciliation, retained workspace content search,
native drawing, native accessibility and actual shell consumption are not
represented as completed by these APIs.

Tests: `fcb-map/src/atlas/retained.rs`,
`fcb/tests/retained_workspace_atlas.rs`, `fcb-app/tests/retained_host_atlas.rs`,
`fcb-bridge/src/atlas_sessions_tests.rs`, and `atlas_ffi_tests.rs`. The existing
reader registry tests also cover the shared initialization/identity path.
Code-first, batch verification pending: this session did not compile or execute
Rust tests, strict RCH, SwiftUI integration or physical-Mac ABI/rendering checks.
Host-declared frame acknowledgment is not native presentation qualification.
