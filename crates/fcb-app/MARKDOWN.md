# Captured Markdown reading

`fcb markdown` connects the existing FrankenMarkdown-backed `fcb-document`
engine to explicit file capture, heading navigation, logical flow windows,
and separately named rendered-text and original-Markdown copy operations.

```sh
fcb markdown README.md --json
fcb markdown README.md --heading installation --lines 30 --json
fcb markdown README.md --line 41 --lines 40 --width 100 --json
fcb markdown README.md --copy-start 10 --copy-end 20 --json
fcb markdown README.md --max-source-bytes 262144 --json
fcb markdown --help
```

## Source and layout contracts

Only the explicitly named regular file is opened. No directory enumeration,
implicit stdin, link following, remote fetch, image loading, transclusion,
script execution, clipboard publication, cache creation, or source write occurs.
The existing leaf no-follow/nonblocking and opened-object checks are reused.
Ancestor replacement is not qualified as race-safe confinement.

A full observed source capture is required within a default 64 KiB allowance,
configurable through `--max-source-bytes` up to 256 KiB. Oversized or changed
input is an error, never a truncated file represented as a complete document.
Metadata consistency is not an atomic filesystem snapshot guarantee. The input
read-call limit is 4096. Invalid UTF-8 and UTF-16 are refused explicitly; use the
ordinary source reader for those formats. UTF-8 BOMs are removed only from the
parser view and added back to every original-byte coordinate.

The existing upstream parser, logical flow, canonical heading slugs and source
map are reused. No Markdown parsing, wrapping or generic display engine is
implemented in FCB. Width is logical character-cell width, not measured native
font width. Output is not native shaping, native rendered pixels, or a promise
of visual bidi/grapheme equivalence. Line text can omit trailing whitespace;
`rendered_utf8_range` identifies its full underlying logical-text range.

Preparation runs on the caller's worker under explicit source, block, item,
line and managed-memory admissions. Defaults are 4096 blocks, 8192 flow lines
and 8192 elements. An upstream budget failure is an error, not a complete
partial parse. Cancellation is checked before/after the upstream synchronous
call and while validating/encoding output; it does not preempt a parser call
or make an OS read interruptible. The managed reservation is not an OS process
footprint guarantee. No runtime or worker pool is created by the library.

## Navigation and wire format

`--line` is one-based in rendered logical flow, NOT the original source file.
`--lines` defaults to 40 and accepts 1 through 1024; `--width` defaults to 100
and accepts 4 through 512. `--heading` uses the upstream canonical slug without
a leading `#` and is incompatible with an explicit `--line`. Missing headings
are errors; the reader never substitutes another section.

The `fcb.cli/1` response contains a `document` with `fcb.document/1` fields.
All integer IDs, offsets, line numbers and counts use decimal strings. Native
paths retain reversible Unix bytes; escaped display labels are secondary.
Heading ranges and line `enclosing_original_range` fields address the retained
original source, including BOM translation. Enclosing ranges can contain syntax
or unselected text and are not literal per-glyph source mappings. Synthetic
empty source spans are null rather than fabricated source selections.

`document_complete` means bounded preparation finished. It is distinct from
`whole_document_visible`: a page or heading jump can omit earlier/later text.
`next_rendered_line` identifies a following page, or null at the end. Exit 0
means the whole logical document is visible; exit 3 means a bounded window;
exit 2 is an error; exit 130 is cancellation. JSON is privately encoded under
the existing 8 MiB cap, and failures discard the candidate. Broken delivery
can leave incomplete output but does not append a second JSON document.

`--copy-start` and `--copy-end` select a nonempty, half-open rendered UTF-8 byte
range. The response returns the logical text and separately labeled enclosing
original Markdown bytes/range. It never concatenates disjoint spans and calls
them literal original source. Human output escapes terminal/bidi controls.
These are returned data, not an OS clipboard write.

## Retained embedding

Enable `fcb`'s `markdown` feature and use `fcb::document::reader::DocumentReader`.
Supply a `CompleteCapture`, owner-qualified document identity/generation,
options, explicit resource budget/allocation, and cancellation callback.
Windows borrow the completed layout without reparsing or copying a prefix.
Heading navigation resolves upstream source identity rather than saved pixels.
Validate delayed deliveries against the same capture object and generation;
equal numeric IDs or fingerprints do not authorize replacement bytes. The host
must additionally revalidate its own grant before delivering retained output.

Tests: `crates/fcb-document/tests/retained_reader.rs`,
`crates/fcb-app/tests/markdown_cli.rs`, and parser tests in `src/markdown.rs`.
Upstream flow/source-map APIs were inspected at FrankenMarkdown revision
`d0cf12ba86d9510c69c59a71a883b2b25c47606c`; the repository's existing sibling-path
dependency arrangement remains unchanged and is not a resolved release pin.
Code-first, batch verification pending: this authoring session has not compiled
or executed Rust tests, strict RCH, or native hardware qualification. No native
or release gate is closed by this command.
