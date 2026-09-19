# Markdown previews on retained reader captures

`include/fcb_reader_document.h` adds source/preview navigation to an existing open
reader. It works with ordinary reader opens, atlas picks, filename results and
captured content-search hits. It never calls the old path-based Markdown service:
preview, search, outline and source copying stay on the reader's same immutable
capture, even after the working-tree path changes or the atlas is closed.

## Host workflow

On a host-owned worker, call `fcb_reader_document` with a fresh document generation
and explicit source/flow limits. The shared `fcb::document::reader::DocumentReader`
uses FrankenMarkdown's existing parser, logical flow, heading and source-map APIs.
The new owning route shares the exact immutable source backing and retains its
own source admission; it neither copies that backing nor reparses during transfer.
There is no self-reference, separate decoder, Markdown engine or native renderer.

Successful preparation returns the first 64 flow rows and headings. Use
`fcb_reader_document_window` and `fcb_reader_document_headings` to page them.
`next_flow_line` and `next_heading` are independent continuations. They do not
mean incomplete parsing: publication requires a complete, validated layout.
Rows use zero-based logical flow indexes, not physical source line numbers.
Their global rendered UTF-8 ranges can include separators omitted from trimmed
row text. Headings use canonical upstream slugs; pass the literal slug to
`fcb_reader_document_heading` without adding '#' or URL decoding.

For source/search-to-preview navigation, send the original source offset to
`fcb_reader_document_from_source`. It locates the first logical row of the
upstream enclosing region. This is block-level correspondence, not an exact
rendered glyph/column. Unmapped source syntax, BOM bytes and interior UTF-8
positions are refused rather than guessed; source EOF is a valid empty window.

For preview-to-source navigation, pass a nonempty global rendered UTF-8 selection
to `fcb_reader_document_source`. It resolves the enclosing original Markdown and
uses the ordinary reader's scalar/CRLF-aware decoder and exact selection round
trip. The response identifies its document namespace/generation, original source
range and window-local UTF-8 selection. Native layout still owns any subsequent
UTF-16, bidi, glyph, accessibility or display-frame mapping.

`fcb_reader_document_copy` mode 0 returns selected rendered UTF-8 text. Mode 1
returns exact original bytes of the enclosing Markdown region as hex. These are
intentionally different: selecting the displayed word `bold` can copy `bold` in
mode 0 but a paragraph containing `**bold**` and other text in mode 1. Enclosing
source is never labeled per-glyph provenance. Copy returns data; it does not write
a clipboard, create an export file, follow a link or acquire new authority.

All non-null C responses may be errors and must be inspected. Free them exactly
once with `fcb_free_string`. Closing a reader never frees an already-returned
string. Full-width IDs, offsets and counts use canonical decimal JSON strings.

## Reflow, cancellation and ownership

Preparing again at a new width is explicit worker-side reflow. It currently
reparses the bounded retained source using the same engine. The previous document
and its capture stay admitted until both the replacement layout and complete
first response are prepared. Failure or cancellation preserves the old preview.
Successful replacement invalidates old rendered offsets and row positions; save
an original-source anchor and resolve it under the new generation to restore place.

Document generations increase independently of content-query and outline
generations. Failed admitted prepare/clear attempts still consume their ID.
`fcb_reader_info` exposes `accepted_document_generation`. A cancellation that
arrives after state acceptance can suppress delivery, not roll back acceptance;
info reconciles that boundary. The shared registry's nonblocking operation locks,
owner-qualified handles, cancellation epochs and retiring-capacity permits apply.
The final release of a layout is worker work, not a redraw/input callback.

Clearing the document releases derived preview state only. Source, content search,
outline and previously returned responses retain their own lifetimes. No change to
atlas geometry, camera, acknowledged presentation, or a different reader occurs.

## Limits and proof boundary

The existing document adapter accepts valid UTF-8, with an optional UTF-8 BOM.
UTF-16 or malformed text remains readable/copyable through the ordinary reader,
but document preparation explicitly refuses it. Maximum document input is 256 KiB
(default 64 KiB), width 4..512 logical columns, flow lines/items 1..32768 and blocks
1..8192. Host pages are 1..128 rows/headings, context is at most 16 KiB, and copy
selections/payloads are at most 256 KiB. Source/layout overlap and response buffers
remain separately admitted; these ceilings are not total process RSS guarantees.
Flow/source limits produce errors, never a silently truncated successful document.

Preparation brackets the synchronous upstream parser/flow call with cancellation
checks; that call is not preemptible. Viewport, heading, selection and copy calls
reuse the retained result without source I/O. This is logical FrankenMarkdown
flow, not native font shaping or Metal presentation. Source/preview/split widgets,
images/asset activation, physical-Mac interaction and accessibility qualification
remain separate work. Markdown-source bytes are not executed as instructions.

Regression coverage was added in `fcb-document/tests/owned_reader.rs`,
`fcb-app/tests/retained_reader_document.rs`, and the bridge's document registry and
FFI tests. It includes actual search-to-captured-reader-to-document navigation
after disk replacement, independent source/outline/query state, reflow, original
BOM coordinates, source selection, cancellation, contention and retiring capacity.
The bridge declares the already-transitive `markdown` feature explicitly; no new
package, parser, runtime or native framework enters the dependency closure.

These changes were authored code-first. Rust compilation, test execution, strict
RCH and native hardware qualification have not run in the authoring environment.
The configured verification batch should include the new document tests, existing
retained document/reader/outline tests and `fcb-bridge`. No bead or product gate is
claimed complete by these host/bridge changes.
