# Native host services

The bridge now delegates source I/O, workspace discovery, atlas layout, exact
search and Markdown reading to `fcb_app::host`. The bridge owns C-string
marshaling only; it no longer walks directories, loads whole unbounded files,
rounds parcel coordinates, or invents syntax classes from string heuristics.
The public declarations are in `include/fcb_host_services.h`.

## Source and documentation

Existing `fcb_read_file` returns an exact complete UTF-8 observation up to
4 MiB, including an initial BOM. The size is admitted before reading. Source
extents and read-call counts are bounded, and before/after length/mtime checks
span the operation. A changing working tree is still not an atomic snapshot.
Oversize, malformed UTF-8, embedded NUL, symlink/special-file refusal, and read
failure return null rather than silently truncating, replacing or deleting
source. An empty file returns an allocated empty string, distinct from null.

`fcb_read_file_window(path, offset, bytes)` exposes the existing byte-window
JSON, including original ranges/hex and explicit decoding state.
`fcb_read_file_lines(path, line, lines)` exposes exact one-based source-line
navigation. These routes support the existing UTF-8 and BOM-marked UTF-16
reader semantics without full-file retention. Limits/errors are those of the
shared commands, not a second native-shell implementation.

`fcb_markdown_window(path, line, lines, width)` and
`fcb_markdown_heading(path, slug, lines, width)` expose the FrankenMarkdown-backed
logical document reader. Here lines are rendered logical rows and width is
character cells, NOT native font geometry. The existing 64 KiB UTF-8 document
admission, canonical heading identities, source maps and partial-window status
apply. No link, image, include, network or script is activated.

`fcb_search_workspace` returns the same exact captured-workspace search JSON as
the CLI. File paths are always passed after a positional delimiter; a path that
looks like an option cannot authorize stdin, a wider scope or a different action.
Search text and heading values remain data, not injected command switches.

## Atlas compatibility and versioned plans

`fcb_atlas_plan` is the metadata-only `fcb.cli/1` + `fcb.atlas/1` plan service.
It preserves reversible native path bytes, explicit partial discovery, bounded
visible detail and the selected engine's geometry. No source payload is read.
The default catalog/viewport limits are the ordinary atlas command's limits.

Existing `fcb_atlas_layout` keeps world/files/path/x/y/w/h/bytes/n/tex for the
current shell. Its `fcb.host-atlas/1` projection now uses `WorkspaceCatalog`,
`WorkspaceAtlas` and `AtlasIndex::bounds_in`, with the same static product policy
and capped-log-byte size metric as the library. Coordinates retain full finite
precision in a 4096-by-4096 world; parent-local rectangles are not mistaken for
world coordinates. Empty scopes return an empty file list. The compatibility
route refuses incomplete discovery and non-UTF-8 paths instead of presenting a
partial inventory or a lossy alias; the versioned route exposes those states.
Discovery admits up to 20,000 files, 2 MiB of catalog path bytes and the shared
bounded traversal. Only catalogued regular files and their ancestors are mapped.
Repository ignore configuration is not implicitly applied as policy.

Compatibility profiles explicitly authorize bounded source observations:
64 KiB per file, 16 MiB global source I/O, the shared workspace read-call cap,
and up to 4000 packed rows per file. Source/capture/profile/JSON/native-handoff
storage is admitted, including temporary overlap. The encoded document has an
8 MiB cap; it is never handed off half-written after an encoding failure.
The safe Rust options can select lower or separately bounded profile allowances.

`n` always means encoded rows, so old hosts can decode exactly 2*n bytes from
`tex`. `source_lines` is a separate canonical string or null when unknown.
`profile_state` distinguishes complete, row/file/global limits, disabled,
unsupported text and unavailable/changed input. File parcels survive missing
profiles. The global I/O counters include failed attempts. Profiles use the
shared CR/LF/CRLF scanner; class bytes are neutral zero, explicitly line density
rather than a substitute for the upstream syntax engine. A profile is a summary,
not retained source for future exact reading. Its source revision/observed byte
count remain separate from discovery's file-size observation.

## Ownership and verification

All calls are synchronous worker operations. Hosts own scheduling, current
native grants, cancellation policy and presentation. The safe Rust services
accept cancellation callbacks; these string-only C entrypoints do not yet expose
an in-flight cancellation handle. No renderer, event loop or global cache is
created. Do not issue fresh discovery or document preparation on each camera
input/redraw. Retained Rust objects remain the route for interactive sessions.

Each call makes a new observation. IDs and hit indices from separate calls are
not durable anchors, and new named-file reads do not resolve old search captures.
Every non-null result must be released exactly once by `fcb_free_string` from
the same loaded library. Do not mutate it or use a foreign allocator. Rust callers
now acknowledge these pointer contracts with unsafe calls; C symbol signatures
are unchanged. Structured calls preserve complete error/partial JSON; null means
no complete handoff. Unwinding service panics are contained, but aborts and invalid
foreign pointers cannot be recovered by this boundary.

New tests: `fcb-app/tests/host_services.rs`, `fcb-app/tests/host_atlas.rs`, the
profile unit test, and `fcb-bridge/src/tests.rs`. These cover real files, legacy
compatibility, shared-engine results, exact data, limits and negative cases.
Code-first, batch verification pending: Rust compilation/tests, strict RCH,
SwiftUI integration, ABI execution on a physical Mac and native presentation
qualification were not executed by this authoring session. No product gate or
bead is closed by this integration.
