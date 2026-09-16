# Comparing saved source snapshots

Status: production implementation and regression suites are committed. Rust
compilation and independent strict-RCH execution are pending. This does not
qualify a native comparison panel or close the FCB-056 acceptance gate.

```sh
fcb snapshot diff before.fcbs after.fcbs --json
fcb snapshot diff before.fcbs after.fcbs --limit 200 --hunks 100 --json
fcb snapshot diff before.fcbs after.fcbs --max-work 16777216 --max-edits 512 --json
fcb snapshot diff --help
```

The command opens only the two explicitly selected archives. It does not query
Git, inspect an original working tree, execute source, extract files, or write to
either input. Inputs may be created by `fcb snapshot save`. Existing snapshot
inspect/read/search commands remain unchanged.

## Saved membership versus source identity

A sorted native-byte path merge classifies records as follows:

| Kind | Evidence |
| --- | --- |
| `unchanged` | Both archived captures have the same verified length and SHA-256 digest. |
| `changed` | Both archived captures are available but their length/digest differs. |
| `added` / `removed` | A captured member is absent from the opposite **closed** saved membership under the same recorded policy. |
| `only-before` / `only-after` | The opposite scope is incomplete or uses a different policy; absence cannot establish an addition/removal. |
| `unavailable` | At least one present member has no captured bytes. Empty captured files are a different state. |

These are statements about the saved scopes, not events in a Git history or an
atomic filesystem snapshot. Matching paths align records; they do not prove a
logical file survived an atomic save or path reuse. Equal contents at different
paths do not imply a rename. Case-distinct names, invalid UTF-8 bytes, and literal
Unix backslashes remain separate identities. JSON paths retain reversible hex.

The command reports policies and both artifact digests. Numeric ordinals, file
IDs, offsets, distances and counters are canonical decimal strings. Ephemeral
file IDs are response-local. Use archive digest, native member path and original
byte range to identify the saved evidence across invocations, never the ephemeral
ID alone.

## Exact byte correspondence

For displayed changed members, the public facade loads and digest-verifies both
source captures. The `fcb-analysis` engine then compares their **original bytes**
using bounded Myers insert/delete search and verified common prefix/suffix runs.
An exact result reports the shortest insert/delete byte distance and a sequence
of paired old/new ranges. Adjacent edits are grouped into changed regions.

`equal` spans contain byte-verified equal data. `changed` spans contain the exact
old and replacement ranges. If work or edit-distance admission is exhausted,
`unresolved` preserves the entire unrefined interior rather than inventing equal
content or silently dropping ranges. Prefix/suffix equality already established
remains usable. Engine ranges partition both captures completely, including
insertions and deletions with a zero-length range on one side.

This is not a Unicode text diff or line-based unified patch. A byte span can cut
inside a UTF-8 scalar or UTF-16 code unit. JSON calls its granularity
`original-bytes`; changed/unresolved snippets are bounded original-byte hex,
not fabricated decoded strings. Use the ordinary snapshot reader to decode a
selected side with valid context:

```sh
fcb snapshot read before.fcbs --member src/lib.rs --offset 120 --bytes 4096 --json
fcb snapshot read after.fcbs --member src/lib.rs --offset 128 --bytes 4096 --json
```

The facade's `SnapshotPair` also supports both retained readers directly. It
keeps the exact sources usable after both archive handles have been dropped.
Its comparison wrapper exposes borrowed bytes and ranges, not cloneable source
handles that could outlive their managed-byte reservation.

`corresponding_after` maps a nonempty old range only when it is wholly contained
in one byte-verified equal span. It refuses cross-edit and zero-width boundary
ambiguity. Repeated data may admit several equally good edit alignments; the
engine chooses one deterministically. This is not annotation identity evidence.
No automatic annotation reattachment, rename inference, or atlas repacking is
performed by this slice.

## Bounds and partial output

Defaults are 100 displayed non-unchanged paths, 64 spans per pair, 8,388,608 total
diff work units, and an edit-search distance of 256. Hard CLI maxima are 4,096
paths, 1,024 spans, 67,108,864 work units, and search distance 512. Every compared
byte and frontier cell spends work; bounded reconstruction is also admitted.
A trivial insertion/deletion can be resolved without exploring edit diagonals,
so its exact reported distance can exceed the search-distance setting.

The work allowance is shared across displayed changed members. A zero remaining
allowance returns coarse unresolved ranges without loading another source pair.
The listing limit does not stop the metadata merge: counters still describe all
saved native paths. Span and listing truncation are explicit. Hex previews stop
at 64 source bytes per side and carry their own truncation flag; bounded previews
do not invalidate otherwise complete range correspondence.

The three completeness axes are separate:

- `content_evidence_complete`: both saved memberships are closed, policies match,
  and there are no unavailable/uncertain members.
- `detail_complete`: all selected changed pairs were fully refined and no change
  records or paired ranges were omitted by display limits.
- `complete`: both of the above, with no listing truncation.

`--limit 0` can obtain summary counters without loading source pairs, while
truthfully reporting omitted change details. A work/edit cap yields exit 3 and
useful ranges, not an exhaustive false negative. Unchanged metadata comparison
loads no member payload after archive validation.

The paged archive implementation still validates both entire archives first.
Its format size limits are unchanged. JSON separates `archive_validation_bytes`,
`member_payload_bytes_loaded`, `diff_work_units`, and
`peak_retained_pair_bytes`. The last metric counts the retained pair's source
bytes, not validation scratch, member-copy overlap, metadata, or total process
footprint. Pair loading, retained source, bounded trace/output and archive
metadata all have separate managed reservations under the app's existing guard.

This is synchronous bounded worker work, not a fixed-duration UI callback or a
persisted diff index. Cancellation publishes one cancellation response; source
I/O, checksum, or admission errors publish one error response. A failed stdout
write cannot be repaired by appending another JSON document. Inputs remain
unchanged in every case.

## Exit codes and APIs

For this comparison command, exit 0 means comparable equal saved scopes, exit 1
means a complete comparison found differences, exit 3 means partial evidence,
refinement or displayed ranges, exit 2 means error, and exit 130 cancellation.
Other commands retain their own existing exit-1 meanings.

`fcb-analysis::comparison` exposes the capture engine and range correspondence.
The existing additive `snapshot` feature selects the first-party analysis crate
and exposes `fcb::search::snapshot_comparison`: `SnapshotComparison` performs the
fixed-storage metadata merge; `SnapshotPair` loads retained versions;
`SnapshotPairComparison` and `ComparisonReader` preserve their byte ownership.
Default/search-only library profiles still do not select analysis/store through
this route or perform implicit I/O. No new third-party dependency is introduced.

## Regression coverage

Engine tests compare small edit scripts against an independent dynamic-programming
LCS oracle, verify whole-source reconstruction, preserve coarse coverage under
budget exhaustion, and exercise malformed bytes, UTF-16, repeated lines,
cancellation and allocation refusal. Public facade tests cover missing members,
policy mismatch, raw paths, source retention, both readers and stale identities.
CLI tests exercise actual archives and the binary, reconstruct reported byte
ranges, verify read-only effects, and distinguish work/list/span limits.
Their presence is not a Rust execution or native qualification receipt.
