# Demand-paged saved-source posting indexes

Implementation and tests are committed, code-first/batch-verification pending.
No Rust build, native performance, or product-gate qualification is asserted by
this document. This extends the existing exact snapshot search engine; it is not
a second matcher, decoder, source provider, or live-workspace service.

## Commands

```sh
fcb snapshot index build saved.fcbs --paged --output saved.fcbd --json

fcb snapshot index inspect saved.fcbs --paged \
  --index saved.fcbd --index-digest "$INDEX_DIGEST" --json

fcb snapshot index search saved.fcbs --paged \
  --index saved.fcbd --index-digest "$INDEX_DIGEST" --text 'pub fn' --json

fcb snapshot index search saved.fcbs --paged \
  --index saved.fcbd --index-digest "$INDEX_DIGEST" --raw-hex 00ff01 --json
```

Retain `INDEX_DIGEST` from the trusted build receipt separately from the file.
For FCBD it is the SHA-256 of the complete **page-manifest envelope**, which
includes every page digest. It is NOT the SHA-256 of the complete index file.
Never compute a replacement expected pin from an untrusted input to satisfy the
check. Existing FCBI/FCBO formats continue using their full-artifact digests.
The explicit `--paged` flag selects this distinct pin/open contract; filenames
and extensions convey no trust or format authority.

`--paged` is available on build, inspect, and search. It cannot be combined with
`--inverted`. The existing incremental refresh route still consumes/publishes
FCBI and FCBO, not FCBD. A paged index can be rebuilt from its snapshot; this
change does not imply paged incremental merge or an active-generation database.
Commands without `--paged` retain their existing behavior.

Build options remain `--max-grams`, `--max-file-bytes`, and `--max-source-bytes`.
Search accepts `--limit`; text is an exact case-sensitive literal, not advanced
query syntax. Short literals, UTF-16 text, malformed text and uncovered members
follow the existing compatibility/fallback semantics. Insufficient index quotas
leave entire members uncovered; they do not create partial negative certificates.
Missing sources and incomplete saved membership still prevent false completeness.

## What is now demand loaded

Opening reads a 32-byte format header and one pinned metadata envelope, validates
all source-row metadata against the chosen snapshot directory, and seeks to the
expected file end. **It does not read any posting page.** The metadata contains
source digests, coverage and counts, plus each page's first/last pair, length and
SHA-256. Header lengths, page boundaries and total sizes are independently derived
from that pinned metadata; index-provided offsets are not blindly followed.

Globally sorted `(trigram, member ordinal)` pairs use the same eight-byte packing
as the resident global index. Pages contain at most 2,048 pairs / 16 KiB. Page
fences locate the three sampled trigram ranges; the rarest list drives the same
intersection and original-member-order fallback merge as the resident route.
A metadata fence can sometimes prove a boundary without reading any page.

A resumable cursor step loads at most **one index page**, and retains binary-search
bounds across page misses. Its work does not allocate a repository-sized candidate
set. The CLI reserves a four-page **64 KiB posting-payload cache**; embedded hosts
choose 1 through 16 pages. Metadata, fallback lists, source loading, matcher/results
and other buffers are separate from that cache and remain resource-accounted.
This is not a claim that the entire process consumes only 64 KiB.

Page misses use a bounded round-robin cache. Each loaded page must match its pinned
digest, ordering, ordinal bounds and fence values before it becomes usable. A
failed load invalidates its candidate slot and latches the index error. A canceled
load cannot publish partial bytes; a new query can reuse the still-valid index.
Verified cached pages retain their original evidence if the backing file changes.
After eviction, reloading changed bytes fails instead of silently replacing that
evidence under the old identity.

The existing `PagedQuery::new_paged` routes candidates into digest-verified source
member loading and the production exact matcher. A query step may additionally
load one separately admitted source member and run one bounded matcher step; the
one-page bound is specifically the posting I/O budget, not a total step-byte or
wall-clock guarantee. Read calls, short reads and interruptions are bounded;
generic blocking readers still do not provide an OS deadline.

## Combine index paging and selective snapshot opening

```sh
fcb snapshot index search saved.fcbs --paged \
  --catalog saved.fcbc --catalog-digest "$CATALOG_DIGEST" \
  --index saved.fcbd --index-digest "$INDEX_DIGEST" \
  --text 'pub fn' --json
```

The independent trusted catalog avoids the ordinary full source-archive validation
pass. The new paged index avoids the full posting-body read. Both metadata artifacts
are still read in full. Selected source members are always digest-verified; no
original live root is consulted. Without a catalog, the source archive still gets
its existing full validation, even though the index is demand paged.

A completed query refers to the trusted saved observation. It does NOT certify
that every unread region of either backing file is currently undamaged. Inspection
can validate the manifest of a file containing a damaged unrequested posting page;
requesting that page later refuses it. An unrequested corrupted page cannot change
the already trusted metadata's exclusion evidence, but it remains unverified disk
content. The CLI explicitly reports `index_body_verified_on_open: false` and
`index_page_verification: "sha256-before-use"`.

## Wire format and bounds

FCBD/1 consists of the checked 32-byte header, an FCPM/1 canonical envelope, and
contiguous posting pages. The envelope reuses `fcb-store` framing and SHA-256.
It contains the snapshot digest, fixed trigram semantics and page size, member/
posting/page counts, 49-byte member rows and 56-byte page descriptions. All integers
are little endian. Page descriptors include digest and first/last pair, not an
unchecked arbitrary file offset. Global sorting is fixed by trusted page fences;
local sorting is checked when the corresponding page is loaded.

Limits remain 65,536 members and 2,097,152 gram/member pairs, with a 24 MiB index-file
ceiling and 4 MiB metadata ceiling. The existing snapshot archive/source limits
are unchanged. Memory reservations precede vectors and include construction overlap,
metadata and fixed cache capacity. A tiny budget refuses opening or construction;
it never silently excludes files. Borrowed query cursors retain the index lease.

**Construction is still resident.** Building uses the existing production segment
builder, transposes its complete keys, sorts the admitted pair table, then encodes
page digests and bytes. This work does not implement external sorting, compressed
postings, unlimited archives, background merging, or transactional replacement.
Page caching can trade additional reads for bounded residency; a frequent query
or tiny cache can touch/reload many pages. No measured Rust speedup is claimed.

## Receipts, publication, and public APIs

`index_layout` is `demand-paged-postings-v1`; `index_pin_scope` is
`page-manifest-envelope-sha256`. `index_bytes` is total file size, not bytes read.
`index_open_bytes_read` includes the outer header and complete manifest.
`index_page_bytes_read`, `index_page_loads`, `index_page_cache_hits`,
`index_page_evictions` and `index_cache_capacity_bytes` describe actual page access.
Failed reads can add byte/read-call counts without adding a successfully verified
page load. Archive validation bytes, selected-member bytes and matcher scan bytes
remain separate. Metadata rows parsed during open are not query candidate visits.

Publication shares the existing exclusive no-overwrite writer and effect-aware
terminal receipt. It creates no parent directories and does not delete partial
outputs, replace old indexes, or imply power-loss qualification. Lost stdout cannot
undo a completed write. Index pairs and native-source metadata remain sensitive,
source-derived data; page organization is neither encryption nor export permission.

The additive public surface is `fcb::search::snapshot_index::paged`:
`PagedPostings`, `PagedIndexArtifact`, `PagedPostingCandidates`, `PagedPostingError`
and `PostingPageIo`, together with `SnapshotPostings::encode_paged` and
`PagedQuery::new_paged`. It accepts host-supplied seekable readers and explicit
budgets/pins. There is no implicit path lookup, runtime, thread, or new dependency.
Existing hit identities and `PagedCapture::open_hit`/reader activation are unchanged.

## Regression coverage

`demand_paged_postings` compares candidates to resident postings and complete hit/
coverage/limit behavior to unfiltered production queries. It includes one-page
cache eviction, zero index quotas, raw bytes, UTF-16, missing sources, stale queries,
cancellation and exact retained-source navigation. A guarded reader independently
refuses unselected index-body access; each cursor step's actual byte delta must
stay within one page. The negative control attempts a forbidden body read.

`demand_paged_posting_wire` pins Python struct/hashlib reference bytes, distinguishes
manifest pins from whole-file hashes, mutates every metadata/payload byte, truncates
every position, checks malformed metadata even with recomputed checksums, and
exercises interrupted/short/dishonest readers and owner boundaries.
`demand_paged_index_cli` covers filesystem publication, both independent pins,
selective search, corruption, read-only behavior, lost receipts and the actual
binary. These are committed tests, not Rust execution receipts.
