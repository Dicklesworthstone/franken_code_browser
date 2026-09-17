# Exact captured-source previews from atlas text results

The atlas text route can include a bounded source reader window around one
retained occurrence. The preview uses the SAME immutable capture as the search,
not a second read of the live file at an old byte offset.

```sh
fcb atlas /path/to/repository --text 'needle' --preview-hit 0 --json
fcb atlas /path/to/repository --text 'pub fn' --preview-hit 3 \
  --context-bytes 512 --match-limit 50 --json
fcb atlas /path/to/repository --text 'needle' --focus crates/fcb \
  --preview-hit 0 --context-bytes 32 --json
```

`--preview-hit` is a zero-based index into the retained `text_search.hits` of
THIS command's observation. It is not a durable cross-process source anchor.
Do not carry a numeric index from an earlier invocation and assume it still
names the same bytes. Library consumers retain the actual overlay/captures
instead of rescanning. The selected preview does not automatically move the
camera or change the existing metadata-only `--select` file selection.

## Bounds and exact selection

`--context-bytes` requests original source bytes on each side of the entire
match. It defaults to 256 and admits 0 through 16384. Small bounded padding
preserves scalar and CRLF boundaries; an additional four-byte halo per edge
supplies the existing decoder's context. Source length clamps the range at
both ends. A complete match is never shortened to fit its surrounding context.

The extent reader preserves UTF-8 scalar boundaries, UTF-16 surrogate pairs,
BOM handling, and CRLF context. Even a literal match ending between CR and LF
retains its exact selected range. The decoder's selected text is checked
against the searched text, original range, and original match bytes before
publication. Unsupported or inconsistent selection fails explicitly.

No new decoder, highlighter, document engine, filesystem walk, or source read
is introduced. Copying/decoding the bounded retained extent has its own two
managed allocations. It does not copy the entire capture or scan a large prefix
to invent a line number. These operations belong on the host's worker, not its
camera/input/redraw callback.

`--preview-hit` requires `--text` and must be below `--match-limit`.
`--context-bytes` requires `--preview-hit`. Invalid combinations fail before
source I/O. When the requested index is within the admitted row allowance but
no such hit was retained, the command returns `ATLAS_TEXT_MISSING_HIT`; it never
substitutes another occurrence or silently opens the current live source.

## Response and coverage

When requested, `text_preview` carries the file/atlas/capture identities and:

- `visible_range` and `original_hex` in original source bytes;
- `selected_original_range` and a separately named `selected_window_utf8_range`;
- decoded `text`, `has_replacements`, and `first_line` only when known;
- `prefix_bytes_omitted` and `suffix_bytes_omitted`, describing original bytes;
- `additional_source_bytes_read: "0"`.

A stripped initial BOM counts as omitted original bytes; it is not lost from
the authoritative capture. UTF-16 original offsets are not UTF-8 offsets.
Human output escapes terminal and directional controls, while JSON preserves
logical source text. This is explicitly `logical-captured-text-not-shaped`:
byte context does not establish paragraph bidi, ligature, grapheme, or native
font layout equivalence.

A valid preview remains useful when result rows were truncated. It does not
promote the surrounding search to complete: discovery, capture, decoding,
verification, and result limits retain their independent status and exit 3.
Errors discard the private candidate response; interrupted delivery never
appends a second JSON document. Producing this data does not acknowledge a
native presentation or close a native product gate.

## Library route

Enable `map` and `search`. After preparing a `WorkspaceTextSource` and obtaining
an `AtlasTextOverlay`, call its `preview_hit` with the actual source, occurrence
index, active query generation, `AtlasTextPreviewOptions`, resource budget,
two distinct allocation IDs, and cancellation callback. The returned
`AtlasTextPreview` owns a bounded decoded extent and borrows the original
captured selection and grant.

Before accepting a delayed result, call `validate_delivery` against the current
overlay/source/generation. A revoked grant, changed query, wrong occurrence, or
independently replaced capture is rejected. Equal numeric file/revision values
are not enough to rebind a prepared preview. Dropping an unaccepted replacement
does not alter the old overlay. Bytes already delivered to a consumer cannot
be retracted by later revocation.

## Verification boundary

Consumer tests: `crates/fcb/tests/workspace_atlas_preview.rs`.
CLI tests: `crates/fcb-app/tests/atlas_preview_cli.rs` and atlas parser cases.
Coverage includes UTF-8/BOM, both UTF-16 byte orders, supplementary characters,
in-content FEFF, CRLF cuts, bounded distant context, exact old source after a
live replacement, stale delivery, revocation, cancellation, denied admission,
partial search, no additional source I/O, and unchanged map parcels.

Code-first, batch verification pending. The authoring session has not compiled
or executed these Rust tests or run strict RCH/native hardware qualification.
No Rust test pass, native frame, or product-gate completion is claimed.
