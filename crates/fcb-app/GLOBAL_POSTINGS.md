# Global posting indexes for saved source

Implementation and regression tests are committed. Independent Rust compilation
and strict-RCH execution remain pending. This is not a native performance or
release-gate qualification.

## Build, inspect, search, and refresh

```sh
fcb snapshot index build saved.fcbs --inverted --output saved.fcbo --json

fcb snapshot index inspect saved.fcbs \
  --index saved.fcbo --index-digest "$INDEX_DIGEST" --json

fcb snapshot index search saved.fcbs \
  --index saved.fcbo --index-digest "$INDEX_DIGEST" --text 'pub fn' --json

fcb snapshot index refresh after.fcbs \
  --base before.fcbs --index before.fcbo --index-digest "$OLD_INDEX_DIGEST" \
  --inverted --output after.fcbo --json
```

The digest is the `index_digest` from the original trusted build or refresh
receipt. Retain it separately from the artifact; never compute an expected pin
from the untrusted file merely to pass validation. File extensions are cosmetic;
the checked wire format selects the implementation. Builds without `--inverted`
keep the previous FCBI per-member layout. Inspection and search accept either
layout without a new flag. Refresh can consume either layout and can publish
either: request `--inverted` explicitly for global output.

Both text and `--raw-hex` search reuse the existing exact matcher. Source reading,
result identities, unavailable-member accounting, decoding, result limits and
one-match lookahead remain the same. No original live roots are accessed, and
archive member names never become filesystem paths to open.

## What this changes

The previous saved index probes every member's own gram set. Global postings
transpose those SAME trusted complete segments into a sorted table of
`(gram, member ordinal)` pairs. Construction adds no source parser or matcher.
Each query locates the first, middle and last trigram lists, chooses the smallest
list, and checks membership in the other lists. It does not walk file ordinals
that the posting lists already exclude.

Candidate preparation allocates no file-universe-sized vector or bitset. A
resumable step inspects at most one driver posting and one fallback-list member,
with at most three binary membership lookups. Fallbacks merge in the original
member order. Thus the first retained hits and limit lookahead do not change
because a different trigram list was selected. Co-occurring grams are only
candidates: every candidate still receives member-digest and exact-match checks.

UTF-16 and malformed text cannot borrow UTF-8 negative certificates. Their text
queries take the original scanner route; byte search can use compatible byte
postings. Uncovered files also remain in the fallback lists. Short needles scan
captured membership rather than inventing an empty candidate set. Unavailable
members and unfinished saved discovery continue to prevent false completeness.

For a rare query, work after opening the index depends on its rarest posting
list and fallback membership, rather than all saved files. Common grams and
large fallback sets can still require broad scans. This is an algorithmic change,
not a measured Rust wall-clock speedup.

## I/O, memory, and trust boundaries

The new FCBO/1 artifact is standalone: it does not refer back to a mutable FCBI
file. It contains per-member source digests, lengths, coverage/count metadata,
and globally sorted unique u24-gram/u32-ordinal pairs in checked u64 words. Its
full pinned digest, envelope checksum, schema, counts, pair ordering, member
bounds, per-member posting cardinality and source binding are validated before
any negative certificate is used. A self-consistent omission remains detectable
only against the separately retained trusted pin; structural checks cannot prove
semantic completeness from an arbitrary attacker-generated index.

Limits remain 65,536 members and 2,097,152 member/gram pairs. The global artifact
has a 24 MiB ceiling. Each pair occupies eight bytes on disk and in the main
resident table, versus four bytes for a gram in the legacy per-member payload.
Fallback tables and member metadata are additionally charged. The existing
snapshot source-size limits are unchanged.

**The posting artifact is read and validated in full on open.** It is not a
page-demand-loaded index or compressed posting codec. A cold CLI invocation can
read MORE index bytes than the legacy route, even though its query metadata work
is smaller. Embedded hosts can retain one immutable table for repeated queries.
The ordinary archive body scan also remains unless an independent trusted catalog
is supplied:

```sh
fcb snapshot index search saved.fcbs \
  --catalog saved.fcbc --catalog-digest "$CATALOG_DIGEST" \
  --index saved.fcbo --index-digest "$INDEX_DIGEST" --text 'pub fn' --json
```

Catalog opening checks 56 archive boundary bytes plus EOF; catalog and posting
files are still read in full. Unread archive body integrity remains unchecked.
Selected members are always verified. Completeness refers to the trusted saved
observation, not a fresh health check of every backing-file byte.

Build transposition is bounded worker work and sorts the admitted pair table.
Cancellation is checked between bounded loops/stages, not inside std's sort.
Fresh-build segments, inverse tables and encoded output have independent leases.
Refresh of an inverse artifact reserves forward/inverse overlap before converting
back to the existing content-reuse engine; the conversion reservation follows the
returned forward index conservatively until it drops. No uncharged source or
posting payload escapes. All limits share the app's existing 256 MiB guard, so
not every combination of individual maxima is guaranteed admission.

## Receipts and counters

`index_layout` names `global-postings-v1` or `per-member-grams-v1`.
`candidate_strategy` names `rarest-posting-intersection` or `per-member-probes`.
`metadata_members_visited` counts source-directory records actually visited by
the query, not metadata decoded while opening the index. `posting_entries_visited`
counts driver entries, `posting_list_lookups` counts logical range lookups, and
`posting_membership_lookups` counts binary membership operations, not comparisons
or elapsed time. Initial index I/O remains visible in `index_bytes`.

For global queries, `index_eliminated_files` is a conservative zero lower bound
until `posting_cursor_complete` is true, then the exact number of excluded
captured members. An early result-limit stop does not promote unvisited candidates
to excluded files. Cursor completion is not match completion: a final member may
still truncate results. `workspace_complete`, `truncated`, unsupported/missing
members and actual loaded/scanned bytes remain separate facts.

Publication reuses the existing exclusive no-overwrite writer and effect-aware
receipts. Canceled or interrupted new outputs do not modify old artifacts.
Index data remains source-derived sensitive material; global organization is
not encryption or permission to disclose it. No active-manifest transaction,
automatic trust registry, cache cleanup, or native presentation is introduced.

## Regression surfaces

`global_snapshot_postings` compares candidate sets with the legacy prefilter and
exact results with the unfiltered production scanner. It includes quotas,
UTF-16, arbitrary-byte search, false positives, cross-file limits, unavailable
members, cancellation, independent ownership and exact hit reopening. A guarded
source refuses every unselected payload read; the unfiltered negative control
must trip it. A 512-member fixture requires one driver visit for a rare literal
and zero source-record visits for an absent gram.

`global_postings_wire` checks a Python struct/hashlib canonical vector, every
single-byte mutation/truncation, malformed re-signed structures and a deliberate
checksum-only trust failure. `inverted_snapshot_index_cli` covers actual command
composition, both wire layouts, catalogs, refresh, failure effects and a binary
consumer. These are committed tests, not Rust execution receipts.
