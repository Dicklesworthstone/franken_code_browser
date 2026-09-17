# Whole-file text search on the atlas

The atlas can search files beyond the complete-capture size limit without
retaining those entire files. It uses the existing streaming matcher and source
decoder, with the SAME frozen catalog and file-to-parcel bindings as the map.

```sh
fcb atlas . --whole-file --text 'needle' --json
fcb atlas . --whole-file --text 'pub fn' --focus crates --match-limit 50 --json
fcb atlas . --whole-file --text 'needle' --max-scan-bytes 1073741824 --json
fcb atlas . --whole-file --text 'needle' --max-read-calls 4096 --json
fcb atlas . --whole-file --text 'needle' --respect-ignores --json
```

## Source and retention

`--whole-file` requires `--text`. It streams each admitted regular file from
byte zero using a 16 KiB input buffer. UTF-8 and BOM-marked UTF-16LE/BE use the
existing exact, case-sensitive literal semantics; malformed/unsupported text
remains an explicit incomplete file, never a successful negative answer.
Matches can cross input-buffer boundaries. Native paths stay byte-authoritative.

No CompleteCapture, persistent index, spool file, or source-sized retained
allocation is created. The library retains compact file status records, bounded
hit ranges, and one encoded literal witness per matched file. Completed worker
reports are discarded before opening the next file, releasing their decoder
scratch reservations. It never keeps a decoder-sized lease for every file.

Each hit has the original byte range and exact original hex witness, including
UTF-16 byte order. The witness comes from equality established by the existing
streaming engine. Surrounding bytes were not retained and cannot be fabricated.
The report's `retain_hit` method can put ONLY that literal witness into the
existing ObservedExtent reader, without reopening the live file.

For that reason, `--preview-hit`, `--context-bytes`, `--markdown-hit`, and their
associated document options cannot be combined with whole-file search. Neither
can `--max-file-bytes` or `--max-total-bytes`: these control complete captures,
not streaming I/O. Invalid combinations fail before source I/O. Leave off
`--whole-file` to retain the existing captured-source/context/Markdown workflow.
Ordinary atlas and path search remain metadata-only.

## Global limits and coverage

`--max-scan-bytes` means actual GLOBAL source bytes read in this mode, including
work spent before failed/unsupported files. It defaults to 256 MiB and admits
0 through 1 TiB. It is not a per-file allowance. The nonstreaming text route
retains its existing separate exact-verification budget/default.

`--max-read-calls` defaults to and cannot exceed 16,777,216. Interrupted reads
spend this allowance. A zero byte/call allowance opens no source files; directory
metadata and explicitly authorized repository-rule reads remain separate.
Discovery limits still apply and cannot establish absence in an unknown suffix.

`--match-limit` bounds retained occurrences globally, independently of atlas
parcel limits. Filling the buffer does not itself prove truncation: the engine
continues with one bounded lookahead, including into later files. Zero retains
no hits but may prove no match or stop after the first positive witness; it is
NOT an exhaustive count-only operation. Partial counts are labeled accordingly.

Search covers the catalog, not the camera focus. Offscreen/out-of-focus matches
retain identity with a null projected rectangle. File/directory overlays count
retained occurrences and distinct matching files separately. Sibling-group
boxes do not borrow counts for their larger parent directory. Base layout and
camera geometry are unchanged by search.

The `text_search.strategy` field is `streaming-whole-file` and retention is
`literal-witnesses-only`. Per-file records distinguish completion, unavailability,
unsupported encoding, short reads, metadata changes and I/O failures. Totals
include unexamined files, incomplete files, bytes/read calls, and the stopping
limit. `counts_complete` requires closed discovery and complete observations
for every admitted file. Matching before/after metadata is not an atomic or
cross-file snapshot guarantee. A changed file stays visibly incomplete.

Exit 0 means complete requested atlas/search (including complete no-match).
Exit 3 preserves incomplete search/discovery/detail; errors are 2 and cancellation
130. One JSON document is staged under the existing 8 MiB response cap; delivery
failure never appends another document. Human literal text escapes controls.
No source files, network resources, caches, or clipboard destinations are written.
The existing path-checked provider is not qualified against hostile ancestor races.

## Retained host integration

Use `fcb::map::workspace::stream_search::AtlasStreamSearch`. Supply the actual
WorkspaceAtlas, a separately compiled StreamingNeedle, fresh source revisions
and query generation, limits, budget, and distinct output/worker allocations.
Drive `step` with StreamReadStep and an authorized file-opening callback. One
step opens at most one file and executes one existing FileSearch quantum; zero
byte/call/hit quanta do no work. Foreign file/metadata calls have no hard deadline.

Progress counts all spent source I/O immediately; hit rows describe finalized
file observations. Cancellation/revocation closes the active file and prevents
new publication. Finished reports outlive the compiled pattern and worker and
validate delayed delivery against the same atlas object and query generation.
Retained older results are not mutated by a replacement query. A host still
owns scheduling and revalidates its grant before a new action; this module does
not create a runtime or a native presented frame.

Tests: `crates/fcb/tests/workspace_atlas_stream.rs`,
`crates/fcb-app/tests/atlas_stream_cli.rs`, and atlas argument tests. Coverage
includes multi-megabyte source, buffer-boundary Unicode/UTF-16, global quotas,
lookahead, unavailable/changed/unsupported files, exact retained old witnesses,
raw paths, unchanged geometry, canceled work and interrupted output.
Code-first, batch verification pending: this authoring session did not run
Rust compilation, Rust tests, strict RCH, or native hardware qualification.
