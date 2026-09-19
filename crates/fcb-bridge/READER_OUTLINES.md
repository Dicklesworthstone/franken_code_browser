# Structural navigation in retained readers

`include/fcb_reader_outline.h` exposes the existing bounded source-symbol engine
through open reader handles. A reader created from an atlas content-search hit
keeps the searched source; building its outline and opening a declaration do not
return to the live path. The implementation uses `CapturedSymbols` for extraction
and the ordinary `ReaderSession` decoder for exact source/window selection.

## Host workflow

Run these synchronous operations on the host's worker, not its input or drawing
callback. Open a reader by the normal reader, atlas-pick, file-finder, or captured
content-hit route. Then call `fcb_reader_outline(reader, generation, language,
max_items)`. Null language infers the retained label's suffix. Explicit supported
language names include Rust, Python, JavaScript, TypeScript, Go and C/C++ using
the lower-case spellings in the header. Markdown stays with its document owner.

Inspect `status` before using a response. The first outline response contains at
most 64 rows. `fcb_reader_symbols` pages and filters the retained inventory with
case-sensitive exact, prefix or contains matching. Null/empty needle lists the
inventory. Pass the returned `symbol_id`, not a row offset, to
`fcb_reader_symbol` for source context and selection. `fcb_reader_copy_symbol`
returns exact original identifier bytes or declaration-evidence bytes as hex;
it does not write the native clipboard or reconstruct a function.

Each row carries a stable outline-local ID, optional parent ID, depth, name,
kind, declaration line, original evidence range and optional exact name range.
Filtering may omit a parent row; its ID still belongs to the same full inventory.
A missing exact name range explicitly falls back to the declaration evidence.
The selection response carries both original bytes/ranges and window-local UTF-8
ranges validated by the ordinary reader's exact source-selection round trip.
Native text layout must perform its own subsequent UTF-16/visual conversions.

## Source and identity

All calls refer to the reader's immutable capture, including after live edits,
renames, removal, atlas closure, or replacement of a repository query. New source
requires a new reader. Building or clearing an outline never changes source,
content-search state, atlas geometry, or native presentation.

Outline generations are independent from content-query generations. They must
increase for each admitted build/clear attempt; failed admitted attempts still
consume their ID. Replacement prepares records and the response privately, then
publishes together. Failure or cancellation preserves an older accepted outline.
Old symbol IDs are refused after successful replacement or clear. Within one
outline, IDs stay unchanged across filters and pages.

Search selections carry `query_generation`. Outline selections instead carry
`outline_generation`, `symbol_id`, `selection_namespace: "outline"`, and the
candidate evidence label. Equal numeric counters do not merge the two domains.
Hosts must also keep the reader owner/file/source identity attached to responses.

`fcb_reader_info` reports `accepted_outline_generation` for reconciliation.
Cancellation that arrives after acceptance can suppress delivery, not roll back
accepted state. Existing cancellation/close/try-lock ownership rules apply;
closing a handle does not free a JSON string already returned to the caller.

## Capability and limits

These are **heuristic declaration candidates**, not compiler definition resolution
or a complete inventory of semantic entities. `semantic_complete` is always false.
No recognized declaration is not proof that a source contains none.

The existing extractor admits at most 64 KiB of original AND decoded text, 4096
lines, at most 128 Rust opening braces and at most 128 bytes of Python indentation.
Its non-resumable extraction remains worker work. Limits, malformed text and
unsupported languages are explicit errors; the ordinary source reader remains
available. There is no FCB-local Markdown parser or external semantic process.

Retained presentation records are separately admitted up to 1 MiB before copying
names. Source bytes keep the reader's existing reservation. The existing extraction
scratch, old/new outline overlap, and returned responses retain separate managed
leases. Inventory retention is at most 4096 rows; page size is 1..128; filter text
is at most 256 UTF-8 bytes; selection context is at most 16 KiB. Inspect
`output_limited`, `retained_symbols`, `matched_symbols`, `count_basis`, and
`next_offset` separately. A fully paged limited inventory is still limited.

## Verification boundary

`fcb-app/tests/retained_reader_outline.rs` adds production host coverage.
`src/reader_outline_sessions_tests.rs` covers registry lifetime/cancellation and
an actual repository-search-to-reader-to-outline journey. C entrypoint workflows
and signature checks are in `src/reader_outline_ffi_tests.rs`.

The bridge now directly names the already-transitive `fcb` dependency to consume
its public symbol enums; no new library package, parser, runtime or native
framework enters the dependency closure. The manifest and lock dependency edge
are updated together.

These changes are code-first. Compilation and test execution have not occurred
in the authoring environment. The configured independent verifier should include
the `retained_reader_outline` and existing `retained_host_reader` host tests plus
`fcb-bridge` tests in its strict-RCH batch. Native outline widgets, keyboard routing,
shaping, accessibility and physical-Mac presentation are not qualified by these
host/bridge changes. No bead or product gate is declared complete.
