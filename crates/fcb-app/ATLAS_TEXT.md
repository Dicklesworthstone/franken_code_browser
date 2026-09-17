# Exact text search on the repository atlas

`fcb atlas --text` connects real workspace capture, the existing exact decoded
search engine, retained map parcels, and source-byte selections in one response.
It is a renderer-neutral integration route, not a native presentation claim.

```sh
fcb atlas /path/to/repository --text 'cancel' --json
fcb atlas /path/to/repository --text 'pub fn' --focus crates/fcb --json
fcb atlas /path/to/repository --text 'needle' --match-limit 50 --json
fcb atlas /path/to/repository --text 'needle' --max-file-bytes 1048576 \
  --max-total-bytes 33554432 --max-scan-bytes 33554432 --json
fcb atlas /path/to/repository --text 'needle' --respect-ignores --json
```

## Source and search scope

`--text` explicitly authorizes source payload reads for the frozen workspace
catalog. Without it, ordinary atlas and `--path` remain metadata-only. The path
and text switches are alternatives; combining them is rejected before I/O.
Repository configuration reads still require `--respect-ignores` separately.
No build, script, source instruction, network request, cache write, or implicit
stdin read is performed.

Text is literal, case-sensitive decoded text with no normalization. A string
such as `needle -path:src` is not reinterpreted as an expression. UTF-8 and
BOM-marked UTF-16LE/BE use the shared search decoder and exact original-byte
maps. Unsupported text stays explicit, not a complete negative result. Index
segments that cannot safely prefilter the encoding use the shared direct scan.

The query covers the admitted catalog, not just the camera focus or the visible
parcels. Offscreen and out-of-focus results retain their file identities and
source ranges with null projected rectangles. Budget aggregation cannot erase
known text matches or manufacture an exact hit on an unseen file.

Capture is an observation, not an atomic filesystem instant or a cross-file
snapshot. The source provider checks regular files, leaf no-follow behavior,
and read consistency using the existing workspace route. Path confinement is
not qualified as race-safe against hostile ancestor replacement.

## Bounds and completeness

Default capture limits are 1 MiB per file and 32 MiB total retained source.
`--max-file-bytes` admits up to 1 MiB; `--max-total-bytes` admits up to 64 MiB.
Files beyond those limits remain unavailable, never truncated CompleteCaptures.
The source I/O allowance includes bytes spent on failed capture attempts and
uses the existing independent read-call cap.

`--max-scan-bytes` is the exact-verification allowance, not a filesystem I/O
counter. It defaults to 32 MiB and accepts up to 1 TiB. Index preparation and
source capture are distinct stages; their work does not disappear merely
because verification can skip a file using a compatible index segment.

`--match-limit` defaults to 100 and accepts 0 through 4096 retained occurrences.
Zero is not text count-only mode: the underlying exact scanner can decline a
nonempty scan and return partial coverage. This intentionally differs from
path search's count-without-rows behavior. `--limit` continues to bound map
parcels independently from text results.

A complete atlas plan is not a complete search. The text report separately
exposes unavailable captures, unsupported text, unexamined captured files,
verification-byte exhaustion, result truncation, and catalog completeness.
`matches_counted` can include one unstored lookahead occurrence and is not an
exhaustive total when `counts_complete` is false. A complete no-match atlas
response exits 0 (the atlas command's contract); partial search/detail/discovery
exits 3, errors 2, and cancellation 130.

## Wire additions

The existing `fcb.cli/1` and `fcb.atlas/1` response receives `text_search` (null
without `--text`). The outer `payload_bytes_read` reports actual capture I/O
rather than the metadata route's constant zero.

Each retained hit carries its response-local `hit_index`, atlas node, FileId,
SourceRevision, `original_range`, exact `matched_text`, `original_hex`, and
optional clipped logical rectangle. UTF-16 offsets and hex describe original
UTF-16 bytes, not UTF-8 needle bytes. IDs, counts and offsets remain canonical
decimal strings; geometry remains finite JSON numbers. Native paths retain
reversible byte payloads and secondary escaped display labels.

`retained_text_matches` on a file/directory parcel has separate `occurrences`
and `files` counts. Two occurrences in one file are not two matching files.
Directory counts use component-aware raw-path intervals, so `src-old` is not
included in `src`. Sibling-group boxes receive null, because a parent's count
would incorrectly describe only the subset represented by that rectangle.
These are retained-result counts, not unsampled repository totals.

Output is one privately encoded document within the existing 8 MiB response
cap. Encoding failure discards the private candidate. A broken output stream
can leave an incomplete document, but never appends a second JSON document as
an attempted repair. Human source text escapes terminal/bidi controls.

## Retained library use

Enable `map` and `search`, and use
`fcb::map::workspace::text_search::WorkspaceTextSource`. It accepts a retained
WorkspaceAtlas and completed WorkspaceCaptures from the SAME actual catalog.
Separately prepare its WorkspaceTextIndex and reuse that index for queries.
Each AtlasTextOverlay owns its output reservations and borrows its exact source.
The index can be dropped while the source and returned overlay remain retained.

`validate_delivery` compares the actual WorkspaceTextSource allocation as well
as the query generation. Equal numeric IDs from a replacement source set do
not make an old result current. `select_hit` returns an AtlasTextSelection
borrowing the searched CompleteCapture and exact original match bytes; it never
reopens the live path. A new action validates the grant. Revocation cannot
retract bytes already delivered to an external consumer.

Layout and text generations remain independent. New queries do not repack the
base map or mutate an older overlay. A canceled/failed replacement can be
abandoned while the old accepted results remain retained. These are worker
operations, not work for an input or redraw callback. There is no implicit
runtime, filesystem provider, or native view.

## Verification boundary

Production-consumer tests: `crates/fcb/tests/workspace_atlas_text.rs`.
CLI tests: `crates/fcb-app/tests/atlas_text_cli.rs` and parser cases in
`crates/fcb-app/src/atlas.rs`. They cover real source capture, UTF-16, exact
old-source selection, overlap/counting, partial scopes, quota fallback,
revocation, stale sources/generations, cancellation and output interruption.

Implementation is code-first, batch verification pending. The authoring
session did not execute Rust compilation, Rust tests, strict RCH, or native
hardware qualification. No product gate is closed by these additions.
