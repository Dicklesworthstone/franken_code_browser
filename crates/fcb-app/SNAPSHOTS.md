# Saved source snapshots

Status: implementation and tests committed; independent strict-RCH execution
is pending. These commands do not implement the metadata database, persisted
posting lists, a native GUI, or the full persistent-index release gate.

## Commands

```sh
fcb snapshot help
fcb snapshot save /path/to/repository --output /path/to/new-source.fcbs --json
fcb snapshot inspect /path/to/new-source.fcbs --json
fcb snapshot search /path/to/new-source.fcbs --text 'pub fn' --json
fcb snapshot search /path/to/new-source.fcbs --raw-hex 00ff --json
fcb snapshot read /path/to/new-source.fcbs --member src/lib.rs --line 20 --lines 40 --json
fcb snapshot read /path/to/new-source.fcbs --member-hex 7372632fff2e7273 --raw --json
```

`save` explicitly enumerates the selected root and captures admitted regular
files. It uses the existing bounded workspace and file-reading services. The
output contains original source bytes: **it is a plaintext source export, not
an encrypted cache or metadata-only index**. No export occurs during ordinary
search, inspection, capability queries, or library construction.

The destination must not exist. Creation is exclusive; existing files,
symlinks, and previous interrupted destinations are never overwritten or
removed. Parent directories are not created. New Unix destinations request
mode `0600`; this is filesystem access control, not encryption or secure erasure.
Use a destination outside source trees to avoid discovering a prior archive as
an ordinary source file in a subsequent save.

Save supports `--include-excluded`, `--max-files`, `--max-file-bytes`, and
`--max-total-bytes`. Defaults are the current workspace policy: 4,096 files,
1 MiB per file, 32 MiB total captured source, static product exclusions and no
nested rule-file configuration loading. Symlinks are not followed. Existing
pathname checks are not a race-safe sandbox against hostile ancestor replacement.
A save is a set of per-file observations, not an atomic filesystem snapshot.

## Offline access and its costs

`inspect`, `search`, and `read` open only the explicitly selected archive. They
never consult, create, or grant access to its original source paths. A moved,
removed, or changed live repository cannot change the saved bytes.

Opening performs a complete sequential integrity/format pass through the archive
using at most 64 KiB per read. It retains metadata (native names, offsets,
lengths, unavailable reasons and per-member SHA-256 digests), not archive source
payloads. **Opening is still O(archive bytes) I/O and hashing**, not an instant
random-access operation. `archive_validation_bytes` and
`archive_validation_read_calls` report that cost separately from member loads.

Each subsequent member load reads only that member from the same open handle,
then checks its per-member digest before publishing bytes. Pathname replacement
does not redirect an already-open File. In-place changes that alter the requested
member's bytes are refused. Retained captures remain readable after the archive
handle closes. Checksums do not establish authorship or an atomic filesystem
snapshot. The host owns admission of the regular archive file and the lifetime
of the handle; no mmap, extraction, or root reopening is involved.

Inspection retains no member payload. It lists native paths, capture
availability, original lengths and missing-source reason codes. Its `--limit`
defaults to 100 and is capped at 4,096.

Search uses the existing resumable streaming matcher/decoder, one digest-verified
member at a time, without restoring all sources or building a whole-archive
trigram index. `--text` is a case-sensitive literal, not advanced query syntax or
regex. UTF-8 and BOM-marked UTF-16 retain exact original-byte ranges. Unsupported
text is counted separately from absent matches; `--raw-hex` searches arbitrary
original bytes. `--limit` defaults to 100 and is capped at 4,096. One-match
lookahead crosses member boundaries: filling the result buffer exactly does not
by itself establish truncation. Files are never concatenated for matching.

Search JSON names `engine: "bounded-stream-exact"` and
`payload_residency: "one-verified-member"`, and reports `peak_source_bytes`,
`member_payload_bytes_loaded`, `member_read_calls`, and `scanned_bytes`.
This is sequential bounded-memory search, **not a persisted postings index or a
claim that every query is faster**. Full archive validation precedes querying;
searched members are then reread and independently verified.

Reading selects a saved name with `--member NAME`, preserving native argument
bytes, or `--member-hex HEX` for an unambiguous byte identity. It never normalizes
that name into a filesystem access. `--line N` is one-based and exclusive with
`--offset N`, an original-byte position. `--bytes` defaults to 64 KiB and is
bounded to 4..262144; `--lines` defaults to 100 and is bounded to 1..4096.
The complete selected member is verified before a bounded window is decoded.

Text windows preserve valid scalar/CRLF boundaries and emit actual original
ranges, logical line numbers, long-line continuation flags, `next_offset`, and
`reaches_eof`. Malformed replacement text is labelled, and `original_hex` retains
the actual source bytes. `--raw` bypasses decoding and returns original-byte hex,
including BOMs; it cannot be combined with line-based options. Human text escapes
terminal and directional controls; JSON preserves logical text and raw hex.
Window limits can produce exit 3 even when the selected saved file is complete.

## Completeness, identities, and the format

The unchanged `FCBS` version-1 envelope reuses `fcb-store`'s canonical little-endian
framing and SHA-256 checksum. Paths are length-tagged native Unix bytes, not lossy
text. An unavailable file is distinct from a captured empty file. Sorted
membership, counts, source/path byte limits, schema version, flags, lengths,
checksum and trailing bytes are validated before members are exposed. Checksums
establish integrity, not authenticity, filesystem permissions, or freshness.

The format stores no absolute root path, executable policy, native access grant,
or persistent in-process FileId. Facade restoration/paged queries use checked,
fresh host-provided identity intervals. CLI IDs remain `response-local`; the
envelope digest identifies the saved artifact across runs. Do not join different
responses merely because their integer IDs are equal. Opening a paged hit checks
its archive, query, source digest and original range again.

`discovery_complete` preserves whether the saved member list was complete under
its recorded policy. Missing captures and unfinished discovery survive export
and reopening. `workspace_complete` in offline search refers only to that saved
scope, never the current filesystem. A zero-hit partial snapshot cannot become
an exhaustive negative. Save reports live-root access; offline routes report
`live_roots_accessed: false`.

The default codec accepts at most 65,536 files, 1 MiB per capture, 64 MiB source,
8 MiB raw path bytes, and an 80 MiB total archive. These format bounds have not
been raised. The CLI retains its 256 MiB managed-memory guard. Paginated source
access bounds resident payload to the current member, but path/digest metadata
still scales with admitted member count. Retained selected captures are charged
separately. Save still builds an admitted in-memory export with old/new writer
overlap. Not every combination of the individual maxima fits the shared guard.
There is no compression, automatic cache GC, or persistent search-segment reuse.

## Writes and terminal outcomes

A complete archive is encoded and checked before exclusive destination creation.
Writes are byte/call capped. On completion the command requests file and parent
directory synchronization. It does not claim native power-loss qualification or
atomic DB/rename publication. An interrupted write can leave a partial destination;
it is kept for explicit inspection and rejected by archive validation.

| Effect | Meaning |
| --- | --- |
| `none` | This invocation did not create a destination. |
| `destination-created-incomplete` | A new destination exists; the full archive was not written. |
| `complete-file-sync-unconfirmed` | All archive bytes were written; synchronization was not confirmed. |
| `complete-file-sync-requested` | File and directory sync requests returned successfully; no power-loss qualification is inferred. |

Cancellation after a completed save does not turn the result into “nothing
happened.” A lost stdout receipt cannot undo the export. Broken response delivery
returns an error and a redacted effect marker on stderr, not a second JSON
document appended to an incomplete response. Inspect the chosen destination
before an explicit retry; there is no automatic overwrite, cleanup, or retry.

Exit codes retain the app convention: 0 completed operation, 1 complete saved-scope
search with no hits, 2 error, 3 useful partial scope/listing/results/window, 130
cancellation. Successful save does not imply text-decodability or capture of every
original member; consult completeness and counts.

## Verification surfaces and public API

Codec tests include corruption/truncation and valid-checksum semantic negative
controls. `source_snapshot_vectors` pins independent canonical bytes. The new
`fcb-store::paged_snapshot` tests compare resident and streaming decoders, bounded
metadata charges, digest revalidation, short reads and cancellation. Public tests
`paged_snapshot_workflow` connect the existing matcher, exact hit opening and
reader, including actual file-handle replacement. CLI tests `snapshot_paged_cli`
cover read/line/byte/raw-name modes, limits, old source, controls, and the binary.
Existing `snapshot_cli` and `snapshot_boundaries` remain regression suites.
Their presence is not an execution receipt.

The additive `snapshot` Cargo feature exposes `fcb::search::snapshot` for export
and in-memory restoration, and `fcb::search::paged_snapshot` for file-backed
access, `PagedQuery`, `PagedCapture`, and the borrowed `PagedReader`. Default and
search-only hosts do not acquire store dependencies or filesystem activity.
The `persistence` capability remains unavailable: these explicit archive APIs do
not claim the metadata database or its ownership/publication service.
