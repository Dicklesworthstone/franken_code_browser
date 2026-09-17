# Source symbol candidates

Status: implementation and regression tests committed. Rust compilation and
independent strict-RCH execution are pending. This is not native UI or compiler
name-resolution qualification.

## Commands

```sh
# List the recognized outline of one source file.
fcb symbols src/lib.rs --json

# Keep every same-name candidate, including candidates in different files.
fcb symbols /path/to/repository --workspace --name run --json

# Prefix or substring lookup over candidate names, not source-text regex.
fcb symbols /path/to/repository --workspace --name parse --match prefix --json
fcb symbols src/lib.rs --name Reader --match contains --json

# Search only saved source observations; the live root is never consulted.
fcb symbols saved.fcbs --snapshot --name run --json
fcb symbols saved.fcbs --snapshot --member src/lib.rs --json
fcb symbols saved.fcbs --snapshot --member-hex 7372632fff2e7273 --json

# Explicit source interpretation for unknown suffixes or BOM-less UTF-16.
fcb symbols generated.source --language rust --encoding utf16le --json
fcb symbols --help
```

The command never implicitly consumes stdin. Directory enumeration requires
`--workspace`; ordinary file mode does not widen a file into a repository. Archive
mode validates the selected snapshot and loads only one selected member at a time.
Nothing is extracted, executed, written, sent to a language server, or fetched
from the network. The original saved root may be absent or changed.

`--name` is an exact, case-sensitive name filter by default. `--match prefix` and
`--match contains` select those literal operations. Omit `--name` to list the
recognized outline. Filtering is performed before the global display limit;
multiple candidates are not silently reduced to one chosen definition. A parent
candidate may be absent from a filtered listing; its outline-local ID remains
reported, and an unfiltered file listing shows the surrounding outline.

The current routes reuse `fcb-analysis::OutlineExtractor`:

| Route | Recognized subset |
| --- | --- |
| Rust | Selected items, functions, impl methods, and module declarations. Inline module and trait contents are not exhaustively traversed. |
| Python | Line/indentation-based class and function candidates. |
| JavaScript/TypeScript | Selected line-based function, arrow-function, class, interface and type declarations. |
| Go | Selected function, receiver-method and type declarations. |
| C/C++ | Class, struct and namespace declarations; ordinary C/C++ functions are not currently extracted. |

These are **heuristic outline candidates**, not compiler-resolved definitions.
The existing extractors have incomplete grammar/lexical coverage. Multiline
constructs, identifiers and nesting can be missed or overrecognized. A byte-valid
range is evidence of the displayed source, not proof that the grammar or semantics
were resolved. JSON therefore always reports `compiler_resolved: false` and
`symbol_inventory_complete: false`. A no-match result is not proof that no
compiler definition exists. Markdown is deliberately not routed through this
adapter; reusable document parsing remains with the document engine's owner.

## Exact positions and activation

Candidate records contain response-local file, revision and candidate IDs, an
optional parent candidate ID, name, kind, language, logical line and original-byte
ranges. Lines use LF-delimited decoded-source coordinates; visual columns and
UTF-16 code-unit positions are not invented. BOM-marked UTF-16 is decoded before
extraction, then validated evidence is mapped back to its original bytes.
Malformed source decoding is refused for candidate extraction; ordinary raw and
text reading remain available. Source extents cannot be passed off as complete
files beginning at byte zero.

Where the extractor provides a name range, its decoded bytes must exactly match
the candidate name and lie inside the declaration evidence. Other routes retain
the declaration span and report a null name range rather than guessing one.
Native filename bytes are preserved in machine output. Human candidate names
use the existing terminal/control-safe display path.

`fcb-analysis::symbols::CapturedSymbols` borrows the exact source. The public
facade adds `BrowserView::symbols`, `PreparedSearchCapture::symbols`, and
`SourceReader::seek_symbol` under the combination of `search` and `analysis`.
The existing `snapshot` feature selects these dependencies and also supports
`PagedCapture::symbols` and `PagedCapture::symbol_target`.

Reader activation validates owner, file, source revision, active query, actual
source bytes and decoding compatibility. It selects the exact identifier span
when available, otherwise the declaration evidence, then uses the existing
resumable reader. Refreshing another view does not relabel or reopen the retained
source. Incorrectly reusing a source ID for changed bytes is rejected too.

The current source reader auto-detects its encoding. Declared BOM-less UTF-16 can
produce valid candidates, but activation in an incompatible auto-detected UTF-8
reader returns `SYMBOL_READER_ENCODING_MISMATCH`; it does not display fabricated
text or line coordinates. Original-byte evidence remains available. The default
BOM-marked UTF-16 reading path is supported by the integration tests.

CLI results do not retain source after the process exits. A later live-file read
is a new observation; it is not automatically the old candidate's capture.
Embedded callers retain the capture, or use the saved archive and its digest
when they need durable evidence. Do not join separate CLI invocations by their
response-local numeric IDs alone.

## Bounded work and truthful partial results

Each extraction admits at most 64 KiB of original bytes, 64 KiB of decoded UTF-8,
4,096 LF-delimited lines, and 4,096 retained candidates. Rust additionally admits
at most 128 opening braces in the decoded input, including braces in strings or
comments. Python indentation is capped at 128 bytes. These are conservative
whole-input guards around the legacy extractor, whose own depth/item limits are
not consistently applied. They bound recursion and teardown before entering the
extractor; they do not reinterpret a cut-off prefix as a complete source file.

The CLI defaults to 4,096 discovered/visited files, 8 MiB total member source
reads, and 100 displayed candidates. `--max-files` may rise to 65,536, `--max-bytes`
to 64 MiB and `--limit` to 4,096. The per-file extraction caps remain fixed.
`--limit 0` still computes bounded candidate counts and reports omitted output.
A full buffer is marked truncated only after more matching candidates are found.

Only one source member and outline are retained at a time. The outline adapter
reserves a conservative envelope for decoding maps, extraction scratch, nested
items and output before construction. Source, archive metadata, discovery,
diagnostic paths and response storage have separate reservations. Reported
`peak_retained_source_bytes` counts source payloads, not total process memory.

Snapshot opening still reads and validates the entire bounded archive. Output
separates `archive_validation_bytes` from later `member_payload_bytes_read` and
`analysis_source_bytes`. This is bounded-residency symbol processing, not a
persistent symbol index or a claim of constant-time archive opening.

Live workspaces reuse the existing static exclusion policy. `--include-excluded`
is explicit and workspace-only. Symlinks are not followed by this route, but
separate path checks are not race-safe confinement against hostile ancestor
replacement. Sources are observed per file, not as one atomic repository instant.

Unsupported languages, unavailable sources, size/work refusals, invalid evidence
and per-file item limits contribute separate counters. Up to 64 diagnostic paths
are retained; omitted diagnostics have an explicit count. Discovery completeness,
processing completeness and listing truncation are separate. None of them changes
the always-incomplete compiler-symbol inventory claim.

Processing is synchronous worker work, not an interaction callback. Cancellation
is checked around each admitted extractor invocation, during source reads and
between results; the legacy extractor itself is not preemptible. Response
encoding is private and bounded before delivery. Errors discard private partial
JSON; a broken stdout write does not append a second document or alter source.

Exit 0 means candidate processing and display finished with matches; exit 1 means
that processing finished without a matching candidate in the recognized subset.
Exit 3 means useful partial scope, analysis or output; exit 2 is an operation
error; exit 130 is cancellation. None means compiler name resolution succeeded.

## Regression coverage

Tests exercise the actual extractor, source maps and reader for all exposed
language routes; duplicate names and parent IDs; UTF-8 BOM and UTF-16 positions;
source replacement and wrongly reused identities; paged captures retained after
archive closure; incompatible reader encodings; malformed/recursive input; file,
work and result limits; raw filenames; cancellation; broken output; and actual
binary workspace/offline calls. Tests are committed, not an execution receipt.
