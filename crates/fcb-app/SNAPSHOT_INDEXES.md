# Persistent saved-snapshot substring indexes

Status: production implementation and regression tests committed; Rust compilation
and independent strict-RCH execution are pending. This route does not complete
G4, the metadata database, the native UI, or out-of-core posting-list qualification.

## Build once and reuse

```sh
fcb snapshot index build saved.fcbs --output saved.fcbi --json
```

Retain the returned `index_digest` from this trusted build receipt separately
from the sidecar. It is the SHA-256 of the **entire index artifact**. With that
retained value in `INDEX_DIGEST`:

```sh
fcb snapshot index inspect saved.fcbs \
  --index saved.fcbi --index-digest "$INDEX_DIGEST" --json

fcb snapshot index search saved.fcbs \
  --index saved.fcbi --index-digest "$INDEX_DIGEST" --text 'pub fn' --json

fcb snapshot index search saved.fcbs \
  --index saved.fcbi --index-digest "$INDEX_DIGEST" --raw-hex 00ff01 --json
```

`--text` is an exact case-sensitive literal, not query syntax, normalization,
case folding, or regex. Raw search selects original bytes. `--limit` defaults
to 100 and allows 0 through 4,096; lookahead distinguishes an exactly full
result buffer from omitted matches, including across intervening eliminated
files. All hit ranges remain original-source byte offsets.

Nothing opens the original live repository, runs its programs, loads configuration
from it, or accesses the network. The only inputs are the explicitly selected
snapshot and index files. Native paths in results come from the validated
snapshot and remain reversible byte payloads. A moved original repository does
not change offline results.

Ordinary `fcb snapshot search saved.fcbs --text needle --json` remains available
without an index or digest. A missing, corrupt, mismatched, or untrusted sidecar
can be ignored by selecting that unfiltered route, or rebuilt explicitly into
a **new** destination. Nothing automatically deletes a sidecar or user state.

## A checksum is not proof of complete indexing

A malicious sidecar could omit a source trigram, retain the correct source digest,
and recompute its own checksum. It would remain syntactically valid but could
hide a real match from exact verification. The final matcher cannot recover a
file that a corrupt prefilter incorrectly excluded.

For this reason, reopening requires a **separately retained trusted build digest**.
The CLI never reads that pin from the index itself, guesses it from a filename,
or computes it from untrusted index bytes to make them pass. Use the digest from
an original trusted build/accepted publication, not an attacker-supplied receipt.

This is an explicit host trust contract, not self-authentication, a signature,
or access control. Embedded hosts must retain the digest in their trusted
publication state. Passing `hash(untrusted_index)` as the expected digest violates
that contract; structural validation cannot compensate. The tests include this
incorrect checksum-only host as a negative control that demonstrably loses hits.
Without a trusted pin, rebuild from the validated source snapshot instead.

After the full-artifact pin matches, decoding independently validates envelope
schema/version/flags, exact length, counts, ordinal membership, source lengths
and digests, coverage tags, ordered unique 24-bit grams, and trailing data. The
sidecar must name the same complete snapshot digest as the opened archive; equal
paths or member counts are insufficient. Fresh embedded owner IDs are not read
from disk. Prior index objects remain immutable while replacements are built.

## Reuse the production engine, preserve the source universe

The builder loads and digest-verifies one saved member at a time, uses the
existing `EphemeralIndex` to construct its actual sorted trigram segment, and
serializes that segment through the new borrowed `segment_image` API. There is
no second gram-construction or exact-matching implementation in the adapter.

The sidecar is a collection of **per-file substring sets**, not a global inverted
posting list. Queries visit bounded member metadata and probe the first, middle,
and last needle grams. A compatible missing gram permits skipping the source
load. A candidate still has to pass the existing member-digest verification and
production streaming matcher/decoder. A positive gram probe is never a hit.

Coverage is explicit for each member:

- Complete UTF-8 byte segments can filter exact UTF-8 text and raw bytes.
- Complete non-UTF-8 byte segments can filter raw bytes; text uses the decoder.
- Unindexed or quota-refused captures always take the direct-scan route.
- Unavailable members remain unavailable members, not empty files.

Needles shorter than three bytes always scan. UTF-16 and malformed source text
always reach the existing text decoder, even when the raw grams cannot match a
UTF-8 needle. This keeps unsupported-text coverage visible and avoids a false
complete negative. A raw-byte search can still use those files' byte segments.

`IndexedNeedle` binds the prefilter pattern to the exact immutable literal used
by the matcher. Hosts cannot provide one candidate-filter needle and a different
verification needle through `PagedQuery::new_indexed`.

Index quota exhaustion does not make an otherwise captured snapshot unsearchable.
A build may return exit 3 for uncovered members, while a subsequent query can
return complete results by scanning those members. Saved discovery completeness,
unavailable captures, text-decoding failures, and result truncation remain
independent. Completeness refers to the **saved observed scope**, never the current
filesystem or an atomic live-repository instant.

## Bounds and honest I/O accounting

The CLI uses the existing defaults: at most 256 KiB of source examined for one
index segment, 65,536 unique grams per segment, 1 MiB of per-segment scratch,
and 2,097,152 total stored grams. A captured file above the indexing threshold
remains eligible for direct search. Build controls are:

```sh
fcb snapshot index build saved.fcbs --output limited.fcbi \
  --max-grams 100000 --max-file-bytes 65536 --max-source-bytes 8388608 --json
```

`--max-grams` may range from 0 to 2,097,152, `--max-file-bytes` from 0 to
262,144, and `--max-source-bytes` from 0 to 67,108,864. The existing snapshot
format's limits are unchanged, including its default 1 MiB per capture and
80 MiB total archive cap. Sidecars admit at most 65,536 member records and
16 MiB of encoded data. All integer-valued JSON fields remain canonical decimal
strings.

**Opening the source snapshot still reads and hashes the entire archive.**
The sidecar is also read and validated in full on reopen. It avoids rebuilding
source grams and can eliminate subsequent member loads/scans; it does not remove
initial source validation, establish constant-time opening, or promise a cold
single-query speedup. Retaining an already-open archive/index in an embedded
host amortizes that opening cost across queries.

Output separates `archive_validation_bytes`, `index_build_source_bytes`,
`member_payload_bytes_loaded`, `loaded_members`, `scanned_bytes`,
`index_eliminated_files`, `index_candidates`, and `index_fallback_files`.
`index_build_source_bytes` is zero on reopen. `unique_grams` sums the distinct
sets per file; it is not deduplicated globally. `peak_retained_source_bytes`
counts the query's source payload, not total process memory.

The retained metadata/gram tables, source loads, capture-copy overlap, engine
scratch, encoded sidecar, loaded sidecar and response buffers use distinct
managed-byte leases. The CLI retains its 256 MiB managed admission guard.
Not all independent maxima must fit concurrently: shared admission may refuse
before publication. The sidecar tables are bounded in-memory values after
reopening, not demand-paged posting pages or an out-of-core merge system.

## Publication and privacy

Build is the only write route and requires an explicit new destination. It uses
the existing exclusive-creation writer: no overwrite, no parent-directory
creation, no automatic cleanup, and requested Unix mode `0600`. A sidecar does
not contain complete source payloads, but its grams and digests can reveal or
help reconstruct source fragments. Treat it as **sensitive source-derived data**,
not encrypted metadata suitable for indiscriminate sharing.

The archive is validated and the complete candidate index is built/encoded before
destination creation. Interrupted output files remain on disk and are reported
through the existing `effect` values: `none`, `destination-created-incomplete`,
`complete-file-sync-unconfirmed`, or `complete-file-sync-requested`.
Cancellation and lost stdout receipts cannot undo a completed write. Successful
file/directory synchronization requests are not a power-loss qualification or
an atomic database-manifest transaction. A lost trusted digest receipt requires
an explicit trusted recovery or new build, not automatic trust in the disk file.

Build/inspect exit 0 means the indexed saved scope has closed discovery and no
unavailable/uncovered members; exit 3 means a useful partial index. Search uses
0 for complete results with hits, 1 for a complete saved-scope negative, and 3
for partial source/results. Errors return 2, cancellation 130. Successful
indexing alone does not prove text decoding is supported for every captured file.

## Public APIs and regression coverage

`fcb::search::snapshot_index::{SnapshotIndex, IndexArtifact, IndexDecision}` and
`fcb::search::paged_snapshot::{IndexedNeedle, PagedQuery}` are available through
the additive `snapshot` profile. They create no runtime, filesystem grant or
database. Constructors are explicit worker operations; synchronous per-file
sorting and hashing are bounded but do not carry a hard wall-clock deadline.

The `persistent_snapshot_index` suite compares indexed/persisted results with
unfiltered production scans across text/raw modes, UTF-16, malformed source,
short needles, result limits, missing members and exhausted quotas. It checks
candidate loads, exact hit reopening, cancellation, owner identity and retained
reservations. `saved_index_wire` pins the exact FCBI/1 bytes to an independently
generated Python struct/hashlib vector and tests self-consistent forged omissions.
`snapshot_index_cli` covers actual files and subprocesses, source-root moves,
no-overwrite effects, lost receipts, trusted-pin failure and read-only fallback.
These committed tests are not a Rust execution receipt.

Remaining work includes automatic trusted manifest management, persistent global
posting lists, out-of-core query pages/merges, transactional replacement and GC,
independent Rust/native qualification, and the native search UI. This route does
not advertise the facade's broader `persistence` service as implemented.
