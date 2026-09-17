# Exact search expressions

Implementation and regression tests are committed; Rust compilation and test
execution are pending independent batch verification. This document is not an
execution receipt, language-resolution claim, or product-gate qualification.

## Commands

```sh
# Find occurrences of the exact phrase in Rust files that also contain Result
# and do not contain unsafe anywhere in the same captured file.
fcb search ROOT --workspace \
  --query '"pub fn" Result -unsafe path:src/ lang:rust' --json

# Repository policies are a separate explicit choice.
fcb search ROOT --workspace --respect-ignores \
  --query 'needle required -forbidden' --max-scan-bytes 16777216 --json

# Evaluate the same syntax over saved source without accessing the original root.
fcb snapshot search saved.fcbs \
  --query 'ana required -forbidden lang:rs' --json

# Reuse an independently pinned catalog instead of validating the archive body
# on open. Selected members are still verified before predicate evaluation.
fcb snapshot search saved.fcbs --query '"pub fn" Result lang:rust' \
  --catalog saved.fcbc --catalog-digest "$TRUSTED_CATALOG_DIGEST" --json
```

`--query` selects expression syntax. `--text` remains a literal, including text
such as `needle -forbidden` or `path:src/`. It is not silently reparsed. Expressions,
raw-byte queries, and literal queries are mutually exclusive within one command.

The live command requires `--workspace` and uses bounded complete captures. It
does not accept `--stdin`, single-file windows, `--whole-file`, or encoding
overrides: those routes have different source-retention/scope contracts and keep
their existing literal APIs. An uncaptured suffix cannot prove a document-wide
negative predicate. Snapshot expressions use the saved complete member captures.

## Existing query language, not a second parser

The commands use `fcb-search::ParsedQuery` and its existing limits: 1,024 raw UTF-8
bytes and 64 tokens. The first positive term or quoted phrase supplies the
returned occurrence ranges. Additional positive terms are document-wide AND
predicates; `+term` is also accepted. `-term` and `-"quoted phrase"` exclude a file
when that text occurs anywhere in that same capture. Quoted phrases support
escaped quotes and backslashes. Every text comparison is case-sensitive exact
decoded matching, not a compiler symbol or whole-word match.

Positive predicates may occur on different lines, but never in different files.
Predicate occurrences do not multiply the primary hits. Overlapping primary
matches remain separate occurrences. Negative-only or filter-only expressions
are rejected because no positive primary needle exists.

`path:` / `-path:` and `lang:` / `-lang:` constrain the metadata scope; `type:` is
an alias for `lang:`. All listed constraints apply together. Language aliases
are the existing extension-based mappings, not parser-inferred languages.
Multiple language inclusions are conjunctions, not an implicit union.

Path filters preserve the existing verifier's restricted semantics: a leading
`*` requests a suffix test (for example `path:*.rs`); other values are substring
keys (for example `path:src/`). They are **not a complete glob grammar**, and
`path:src/` is not a root-confinement boundary. Valid UTF-8 paths are filter keys
unchanged; non-UTF-8 paths use the established reversible escaped search label.
Native path bytes are separately preserved for output and source identity.

There is no new regex, OR, nesting, case-folding, or Unicode-normalization mode.
Words such as `OR` are ordinary literal tokens, not boolean operators. Explicit
regex prefixes are rejected with the existing query error. Filenames and option
values that resemble flags remain data when passed as values or after `--`.

## Live capture and saved-source execution

Live expressions reuse the current workspace discovery, optional repository-rule
policy, host file reader, capture admission, `EphemeralIndex`, and exact query
verifier. Path/type exclusions are decided before a source read, so excluded files
do not consume retained-source bytes or the filesystem payload-read allowance.
The selected manifest removes those entries instead of describing them as missing
selected captures. An oversized or unreadable selected file remains unavailable.

The live route retains its admitted source collection; it is not a one-member
streaming implementation. Its existing defaults remain 4,096 files, 1 MiB per
capture, and 32 MiB retained source. Discovery limits still apply to the original
workspace traversal, so a later query filter cannot undo unfinished discovery.

Saved-source expressions use `fcb::search::expression::ExpressionPlan` and
`ExpressionQuery`. A preliminary metadata pass counts the selected scope one
record per step, including missing selected members. The query then loads one
member with digest verification and moves that SAME owned buffer between the
existing `ReaderSearch` predicate and primary scans. No term reopens the source,
no complete decoded source copy is needed, and files are never concatenated.

Each step evaluates at most one metadata record, loads one admitted member, or
runs one bounded matcher quantum. Loading and hashing a whole admitted member
remain worker I/O, not an interaction callback or an OS wall-clock deadline.
Patterns, metadata scratch, hit tables, member buffers, and matcher state have
separate managed-resource reservations. Caller-supplied owners, source intervals,
query generations, and allocation IDs are checked before publication.

`ExpressionQuery::new_indexed` additionally accepts the existing pinned per-file
`SnapshotIndex`. Only a mandatory positive primary term can certify exclusion;
NOT terms never act as unsafe negative prefilters. UTF-16, malformed text, short
needles, and uncovered segments take their normal exact scan path. That indexed
constructor is currently a public-library option: the new snapshot CLI expression
route does not consume an index artifact. Existing `snapshot index search` retains
its literal/raw query routes, including resident and demand-paged postings.

## Work, completeness, and identity

`--max-scan-bytes` admits aggregate matching work across ALL predicate and primary
scans: 256 MiB by default, with an explicit maximum of 1 TiB. Reading the same
retained source for another term still spends matching work. This does not replace
source-capture, source-load, archive-validation, or configuration-I/O accounting.
Live source admission and index construction may precede a matching-work stop.

A partial predicate scan cannot establish that a required term is absent or an
excluded term never occurs. Primary hits are withheld until their predicates
qualify. A primary scan that later reaches its work cap can return useful exact
hits with partial coverage. Unsupported text is counted separately, rather than
silently searched through replacement characters as though it were exact text.

`workspace_complete` requires completed selected-source search, closed discovery,
no unavailable selected captures, and no unsupported selected text. Missing files
outside the explicit metadata scope do not poison that scope's answer. Incomplete
discovery remains incomplete, including unknown repository-policy subtrees.

`--limit` counts only qualified primary hits. Rejected files do not manufacture
truncation. An exactly full buffer is not proof of another hit; cross-file
lookahead continues. Limit zero performs lookahead too: an exhaustive negative
can succeed with no hits, while an actual occurrence returns a truncated response.
`matches_seen` is observed qualified matches, not an invented exhaustive total
when work or result limits stop the query.

Saved hits retain archive/source digests, owner-qualified file/revision IDs,
query generation, occurrence identity, and original-byte ranges. UTF-16 positions
include the correct original encoding/BOM offsets. `ExpressionHit::open` reloads
and verifies the selected saved member, then exposes the ordinary bounded reader.
A stale query or changed backing bytes cannot open beneath an old hit's identity.
Already verified member bytes remain coherent if the backing artifact changes
between predicate scans; reopening changed backing still fails verification.

JSON is one bounded document with canonical decimal-string counters and reversible
native paths. Human query text and filenames escape terminal controls. Exit codes
are 0 for a complete result with hits, 1 for a complete negative, 2 for error,
3 for partial/truncated results, and 130 for cancellation. Search creates no
artifacts, runs no repository commands, and never modifies either source route.

## Regression surfaces and remaining boundary

The public consumer suites `snapshot_expressions` and `expression_capture_io`
compare results with the existing production capture scanner, cover UTF-16 and
predicate/work/result boundaries, and independently guard actual archive reads.
They exercise retained-buffer mutation between terms, changed-source rejection,
filtered unavailable members, cancellation, stale identities, and reader reopening.
`expression_cli` covers live and saved commands, repository rules, catalog opening,
raw names, read-only effects, and the actual executable after a source-root move.
These tests are committed, not claimed executed in this implementation session.

This adds usable expression search, not a native search panel, general boolean or
regex engine, complete glob matching, or new index format. Existing snapshot size
limits, trusted-catalog integrity boundaries, and capture-consistency limitations
remain unchanged. No performance benchmark or native qualification is inferred.
