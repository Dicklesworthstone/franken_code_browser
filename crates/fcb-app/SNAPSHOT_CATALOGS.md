# Trusted catalogs for selective snapshot opening

Status: production routes and regression tests committed; Rust compilation and
independent strict-RCH execution are pending. This is a cold-opening improvement
for the existing FCBS snapshot format, not the metadata database or native GUI.

## Create the catalog once

```sh
fcb snapshot catalog saved.fcbs --output saved.fcbc --json
```

This operation opens and hashes the entire source archive, then saves its native
paths, member offsets, lengths, per-member source digests, archive identity,
policy, and discovery completeness. It does not copy source payloads into the
catalog. A missing capture stays distinct from a captured empty file.

Retain the returned `catalog_digest` in trusted state **separately from the
catalog file**. It is SHA-256 over the complete catalog artifact, not the checksum
stored inside it. Never compute an expected pin from an untrusted catalog merely
to make it pass. A checksum or a matching filename does not establish authorship,
complete membership, correct source offsets, or authorization.

The destination must be new. Existing files and symlinks are not overwritten,
parent directories are not created, and interrupted files are not automatically
removed. The existing effect-aware writer requests Unix mode `0600` and file plus
parent-directory synchronization. Its receipts distinguish no creation, partial
creation, fully written but sync-unconfirmed, and sync-requested completion.
Cancellation or a lost stdout receipt cannot undo a completed catalog export.
These are not qualified power-loss or atomic-database-publication guarantees.

## Reopen without scanning source payloads

With the independently retained digest in `CATALOG_DIGEST`:

```sh
fcb snapshot inspect saved.fcbs \
  --catalog saved.fcbc --catalog-digest "$CATALOG_DIGEST" --json

fcb snapshot read saved.fcbs --member src/lib.rs --line 20 \
  --catalog saved.fcbc --catalog-digest "$CATALOG_DIGEST" --json

fcb snapshot search saved.fcbs --text 'pub fn' \
  --catalog saved.fcbc --catalog-digest "$CATALOG_DIGEST" --json
```

The catalog path and pin must be supplied together. There is no automatic sidecar
lookup, trust inference, extraction, root grant restoration, repair, or fallback.
The member name remains an archive lookup key, never a live filesystem path.
Ordinary commands without catalog options retain full archive validation.

A pinned open reads **24 header bytes and 32 footer bytes from the source
archive**, performs an EOF probe, and uses three seeks. It checks the archive's
length/schema and retained checksum value; it does **not recompute the checksum
of the body**. Short reads and interrupted calls are bounded and counted.

The entire catalog file is still read and checked before opening the source.
Thus 56 is the archive-byte cost of this opening route, **not total process I/O**.
Metadata memory and parsing scale with admitted catalog membership, not source
payload size. Native calls are not guaranteed to return within a wall-clock
interaction deadline. These are worker operations, not paint callbacks.

Each selected member still goes through the existing digest-verifying loader
before it is returned, decoded, searched, or used to reopen a hit. A changed
member is rejected. Previously retained captures remain independently readable.

## Combine with a saved substring index

Catalogs and substring indexes serve different purposes and require independent
pins. A catalog avoids the initial body scan. An index can eliminate subsequent
member loads whose saved substring sets cannot contain the requested needle.

```sh
fcb snapshot index search saved.fcbs \
  --catalog saved.fcbc --catalog-digest "$CATALOG_DIGEST" \
  --index saved.fcbi --index-digest "$INDEX_DIGEST" \
  --text 'pub fn' --json
```

Indexed build and inspection also accept the paired catalog options. Index build
still verifies every source member it actually indexes; quota-uncovered files
remain eligible for exact scan fallback. Indexed search retains the same matcher,
short-query and UTF-16 fallback routes, result limits, and exact hit activation.
Both catalog and index files are read and checked on reopen. An index negative
can require no source payload reads after the 56-byte archive boundary check.
This does not claim a measured wall-clock speedup or eliminate metadata work.

## Saved-scope completeness is not archive health

Output distinguishes:

| Field | Meaning |
| --- | --- |
| `archive_open_validation` | `full-archive` or `trusted-catalog-and-boundaries` |
| `archive_body_verified_on_open` | Whether this opening actually hashed the full archive body |
| `catalog_digest` | Trusted catalog pin used, or null for an ordinary full open |
| `member_verification` | `digest-before-publication` |
| `archive_validation_bytes` | Actual source-archive bytes read during opening, not catalog/index-file I/O |
| `member_payload_bytes_loaded` | Subsequent source payload loads, separately accounted |

An unread region can be corrupt without a selective operation discovering it.
For example, an index can correctly establish that an original saved file has
no matching needle while its current backing bytes have been damaged. The
negative concerns the **trusted saved observation**, not today's health of the
backing file. Reading that damaged member will fail its digest check. A complete
query therefore does not imply every archive byte was checked in this invocation.
Human output explicitly warns when body validation was skipped.

Original incomplete discovery and unavailable members remain incomplete; no
catalog can turn their absence into complete search evidence. A pinned directory
cannot export a new catalog as though it had performed a full source validation.
Creating a new catalog always uses the ordinary full validation route.

## Public APIs and boundaries

The additive `snapshot` feature exposes
`fcb::search::snapshot_catalog::{CatalogArtifact, PinnedCatalog, CatalogError}`.
`SnapshotDirectory::encode_catalog` exports fully validated metadata.
`PinnedCatalog::decode_pinned` validates a separately trusted full-artifact pin
and all fields into a fresh host ownership domain.
`PagedSnapshot::open_pinned` consumes the catalog and an explicitly supplied
seekable reader. The result uses the existing paged query, indexed query,
selected-capture, and bounded-reader APIs. No new matcher or source decoder is
introduced, and library defaults do not acquire ambient resources.

FCBC/1 uses the existing canonical envelope and SHA-256 implementation. Decoding
independently reconstructs FCBS/1 framing: captured offsets must exactly match
member payload positions, with no overlap, hidden gap, or footer alias. Paths
must be ordered, unique native relative bytes. Counts, aggregate path/reason/source
sizes, versions, flags and trailing data are checked before publication.

Existing source archive limits are unchanged, including default 1 MiB captures,
64 MiB source and 80 MiB archives. Catalog files are capped at 16 MiB; the CLI's
shared 256 MiB resource limit may refuse combinations of otherwise legal maxima.
A catalog contains names and source digests and is sensitive metadata, not an
anonymized or encrypted artifact. It is not safe for indiscriminate disclosure.

Committed tests include a source-reader guard that fails on any unselected body
read, full-scan equivalence for mixed encodings and limits, member mutation,
forged metadata with recomputed checksums, a deliberately incorrect checksum-only
host, exact source reopening, cancellation, effects, and actual CLI process routes.
The canonical vector is independently generated with Python struct/hashlib.
None of these committed Rust tests is represented as an execution receipt.

Automatic trusted-manifest management, authenticated publication, global postings,
transactional replacements and garbage collection, larger source formats, and
native integration remain separate work. Diff, trail, and symbols CLI commands
continue to use their existing full-validation opening paths.
