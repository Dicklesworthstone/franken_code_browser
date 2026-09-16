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

`inspect` and `search` open only the explicitly selected archive. They never
consult, create, or grant access to its original source paths. A moved, removed,
or changed live repository does not change the saved bytes. Inspection lists
native paths, capture availability, original lengths, and missing-source reason
codes. Its `--limit` defaults to 100 and is capped at 4,096.

Search uses the existing ephemeral trigram index and exact capture verifier.
`--text` is a case-sensitive literal, not advanced query syntax or regex.
UTF-8 and BOM-marked UTF-16 retain exact original-byte match ranges. Unsupported
text is counted separately from absent matches. Search supports `--limit` with
the same bounds; postings are rebuilt in memory from saved captures, not loaded
from persistent posting pages. The embedded `SavedWorkspace::reader` API also
connects restored captures to the bounded source reader.

## Completeness, identities, and the format

The `FCBS` version-1 envelope reuses `fcb-store`'s canonical little-endian framing
and SHA-256 checksum. Paths are length-tagged native Unix bytes, not lossy text.
An unavailable file is distinct from a captured empty file. Sorted membership,
counts, source/path byte limits, schema version, flags, lengths, checksum, and
trailing bytes are validated before members are exposed. Checksums establish
integrity, not authorship, authenticity, filesystem permissions, or freshness.

The format stores no absolute root path, executable policy, native access grant,
or persistent in-process FileId. Restoration remaps files and source revisions
into checked, fresh host-provided identity intervals. CLI IDs remain
`response-local`; the envelope digest identifies the saved artifact across runs.
Do not join different responses merely because their integer IDs are equal.

`discovery_complete` preserves whether the saved member list was complete under
its recorded policy. Missing captures and unfinished discovery survive export
and restoration. `workspace_complete` in offline search refers only to that
saved scope, never the current filesystem. A zero-hit partial snapshot cannot
be reported as an exhaustive negative. Save/inspect report whether they accessed
a live root; offline search always reports `live_roots_accessed: false`.

The default codec accepts at most 65,536 files, 1 MiB per capture, 64 MiB source,
8 MiB raw path bytes, and an 80 MiB total archive. The CLI retains its 256 MiB
managed-memory guard. Old captures, writer scratch, archive bytes, restored
captures, and query buffers overlap and are separately charged. Not every
combination of the individual maxima fits that shared guard; admission can
refuse before creating the destination. This is bounded in-memory restoration,
not out-of-core persistence. There is no compression or automatic cache GC.

## Writes and terminal outcomes

A complete archive is encoded and checked before exclusive destination creation.
Writes are byte/call capped. On completion the command requests file and parent
directory synchronization. It does not claim native power-loss qualification or
atomic DB/rename publication. An interrupted write can leave a partial destination;
it is kept for explicit inspection and rejected by archive validation.

Error and success receipts carry an `effect` field:

| Effect | Meaning |
| --- | --- |
| `none` | This invocation did not create a destination. |
| `destination-created-incomplete` | A new destination exists; the full archive was not written. |
| `complete-file-sync-unconfirmed` | All archive bytes were written; synchronization was not confirmed. |
| `complete-file-sync-requested` | File and directory sync requests returned successfully; no power-loss qualification is inferred. |

Cancellation after a completed save does not turn the result into “nothing
happened.” A lost stdout receipt also cannot undo the export. Broken response
delivery returns an error and a redacted effect marker on stderr, not a second
JSON document appended to an incomplete response. Check the chosen destination
before an explicit retry; there is no automatic overwrite, cleanup, or retry.

Exit codes retain the app convention: 0 completed operation, 1 complete saved-scope
search with no hits, 2 error, 3 useful partial scope/listing/results, 130 cancellation.
Successful save does not imply the source is text-decodable or that every original
workspace member was captured; consult the recorded completeness and counts.

## Verification surfaces

The codec includes corruption/truncation and valid-checksum semantic negative
controls. `source_snapshot_vectors` pins exact bytes independently generated
with Python `struct` and `hashlib`. Facade tests cover fresh identity intervals,
UTF-16 reading/search, missing members and resource admission. CLI suites
`snapshot_cli` and `snapshot_boundaries` cover real files, source replacement,
raw names, empty/partial workspaces, no-overwrite behavior, interruption, lost
receipts, and the actual binary. Their presence is not an execution receipt.

The public library exposes this through the additive `snapshot` Cargo feature
and `fcb::search::snapshot`; default/search-only hosts do not acquire the store
dependency or perform filesystem work implicitly. The existing `persistence`
capability remains unavailable; this export route does not claim that service.
