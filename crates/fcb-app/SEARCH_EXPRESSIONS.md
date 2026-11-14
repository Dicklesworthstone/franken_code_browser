# Search expressions with original-byte witnesses

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

# Search a file beyond the capture limit using one continuous observation.
fcb search huge.rs --whole-file \
  --query '"pub fn" Result -unsafe' --json

# Filter workspace metadata before opening source files, then count qualified
# occurrences while retaining no result rows.
fcb search ROOT --workspace --whole-file \
  --query 'ana required -forbidden path:src/ lang:rust' \
  --ignore-case --count-all --limit 0 --json

# Evaluate the same syntax over saved source without accessing the original root.
fcb snapshot search saved.fcbs \
  --query 'ana required -forbidden lang:rs' --json

# Apply Unicode lowercase comparison to every text term and count all qualified
# primary occurrences without retaining rows. This never reads the live root.
fcb snapshot search saved.fcbs \
  --query 'ana required -forbidden lang:rs' --ignore-case --count-all --limit 0 --json

# Reuse an independently pinned catalog instead of validating the archive body
# on open. Selected members are still verified before predicate evaluation.
fcb snapshot search saved.fcbs --query '"pub fn" Result lang:rust' \
  --catalog saved.fcbc --catalog-digest "$TRUSTED_CATALOG_DIGEST" --json
```

`--query` selects expression syntax. `--text` remains a literal, including text
such as `needle -forbidden` or `path:src/`. It is not silently reparsed. Expressions,
raw-byte queries, and literal queries are mutually exclusive within one command.

Live expressions have two explicit source routes. The default requires
`--workspace` and uses bounded complete captures. Add `--whole-file` to scan a
named file or workspace member continuously without retaining its complete
source; this route also accepts encoding declarations. Neither expression route
accepts stdin or single-file byte windows. An unobserved suffix cannot prove a
document-wide negative predicate. Snapshot expressions use the saved complete
member captures.

## Existing query language, not a second parser

The commands use `fcb-search::ParsedQuery` and its existing limits: 1,024 raw UTF-8
bytes and 64 tokens. The first positive term or quoted phrase supplies the
returned occurrence ranges. Additional positive terms are document-wide AND
predicates; `+term` is also accepted. `-term` and `-"quoted phrase"` exclude a file
when that text occurs anywhere in that same source observation or capture.
Quoted phrases support escaped quotes and backslashes. Text comparisons are case-sensitive decoded
matching by default. All expression routes accept `--ignore-case`,
which applies the existing Unicode lowercase comparison to primary, required and
excluded text together. Metadata path/type filters keep their existing semantics.
The result declares `case_sensitive`, and hits retain complete contributing source
byte ranges. Snapshot results also declare `mode` as `decoded-text-expression` or
`unicode-lowercase-expression`, with `normalization: "exact"`. Unicode scalar
lowercase comparison can expand a character, so a query `i` can select the complete
original `İ` scalar, and `k` can select `K`. It does not fold `ß` to `ss` or equate
composed and decomposed accents. These routes do not imply compiler symbols or
whole-word matching.

Snapshot expression rows retain metadata without source payloads. Their
`primary_needle` identifies the requested primary term; `matched_text` is `null`
for lowercase matches because query spelling cannot certify source spelling.
Open the verified member at `original_range` to obtain the original bytes. The
retained saved-repository reader API returns the actual decoded source spelling
as `selection.text`, its exact `original_hex`, and the frozen matching policy.

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
Words such as `AND` and `OR` are ordinary literal tokens, not boolean operators.
Explicit regex prefixes are rejected with the existing query error. Filenames and option
values that resemble flags remain data when passed as values or after `--`.

## Whole-file expressions in one observation

`--whole-file --query` uses the shared streaming expression matcher and source
decoder. Every primary, required and excluded term observes the same decoded
stream from one open file. The command does not reopen a changing live file for
each term, buffer a full source copy, or add a second query parser. Terms can be
far apart and can cross the 16 KiB input boundaries. UTF-8 and UTF-16 matches
retain their complete original-byte ranges, including BOM and surrogate offsets.
`--ignore-case` applies to every text term together and retains the primary
occurrence's actual original spelling.

A file qualifies only after the complete admitted observation has been decoded
successfully, a primary occurrence and every required term were seen, and no
excluded term was seen. Until then, primary occurrences are private candidates. A malformed suffix,
source-read failure or exhausted byte/read-call allowance publishes no hits or
match count from that unfinished file. An earlier completed file can still
contribute useful results to a partial workspace response. This rule also
applies to expressions without negative terms: an early positive match is not a
complete-file expression result.

Unlike literal whole-file search, an expression must continue to the end to
decide its predicates. It therefore counts every primary occurrence in each
qualified file even without `--count-all`. `--limit` only bounds retained rows,
and a rejected file never consumes that allowance or manufactures truncation.
With `--limit 0`, a complete negative exits 1; qualified occurrences produce
an exact count with no rows and exit 3 for omitted results. `--count-all` remains
accepted and is recorded in `count_all_matches`, but does not change expression
qualification. Ordinary `--text`, `--raw-hex` and `--identifier` retain their
existing first-extra-match stop unless counting is requested.

For this route, `--max-scan-bytes` counts original-source I/O once, independent
of the number of predicates. The existing byte and read-call limits are global
across a workspace. Source capture limits do not apply, and expression scope
does not allow combining `--whole-file` with `--max-file-bytes` or
`--max-total-bytes`. Matcher state and retained witnesses remain bounded by
query and result admission; the input-buffer size is not a total-memory claim.

Path and language filters run before source opening. Workspace path filters use
root-relative UTF-8 or reversible escaped search keys. A named-file filter uses
the absolute supplied native path's UTF-8 or escaped key, without canonicalizing
its components; `path_filter_domain` identifies the chosen domain. Workspace
`scope_files` and `metadata_excluded_files` separate selected known entries
from metadata exclusions. An excluded named file reports
`state: "metadata-excluded"`, zero source I/O, and null source identities and
lengths. Its query and count are complete, but `input_complete` and
`whole_file_complete` remain false because no source observation occurred.
Unfinished discovery still prevents complete workspace coverage.

Whole-file expression JSON declares `strategy: "streaming-whole-file"`,
`mode: "decoded-text-expression"`, the original `expression`, and
`predicate_scope: "same-observed-stream"`. `case_sensitive` records the
selected text comparison. On an observed file, `expression_qualified` is true
or false after supported completion and null while qualification remains
unproven. This concerns predicates within the observed sequence; changed
before/after metadata can still leave `whole_file_complete` and `query_complete`
false. `truncated` describes omitted rows independently of
`match_count_complete`; a completed expression can have both fields true.
Only primary witnesses are retained. Predicate qualification does not retain
every contributing source span, neighboring lines, or a full snapshot.
Unchanged before/after file metadata is still not an atomic snapshot.

## Retained native-host streaming expressions

`RetainedAtlasStreamSearch::search_scoped_expression_with_progress` exposes
the same single-observation engine through the native host's existing frozen
catalog. It accepts the expression, location/file-type scope, case policy and
ordinary streaming I/O limits. Path/language predicates intersect that native
scope before any source file is opened. Original catalog file IDs and source
observation ordinals remain stable through this projection.

This route always counts all qualified primary occurrences within the byte and
read-call budgets, retaining at most the requested number of rows (1–256).
It does not stop predicate evaluation when those rows fill. Progress previews
include only finalized files; **Stop and keep matches** discards an unfinished
file's candidates while preserving earlier results. Hard cancellation discards
the attempted replacement and leaves the previously accepted query available.
Invalid syntax likewise leaves accepted evidence intact and performs no source
I/O. Existing literal and occurrence methods retain their original semantics.

Progress, terminal pages and selected-witness replies carry the same immutable
expression policy:

| Field | Meaning |
|---|---|
| `mode` | `decoded-text-expression`; `case_sensitive` selects the text comparison. |
| `expression` | The original bounded query. |
| `needle`, `primary_needle` | The primary literal whose occurrences supply result ranges. |
| `query_syntax` | `fcb-bounded-expression/1`. |
| `predicate_scope` | `same-observed-stream`, not a retained complete capture. |
| `path_filter_domain` | `root-relative-utf8-or-escaped-search-key`. |
| `eligible_files`, `excluded_files` | Membership after the native location/file-type scope. |
| `expression_scope_files`, `expression_excluded_files` | The subdivision of eligible files after expression metadata filters. |

Examined and unexamined counts sum to `expression_scope_files`. Content-predicate
rejections are examined nonmatching files, not metadata exclusions. Missing
requested visible paths and incomplete discovery still prevent a complete
search claim. Completed observations with changed or unavailable metadata may
retain expression witnesses while making overall coverage incomplete.

Pages and hit lookup perform no additional source reads. A retained hit contains
only the primary occurrence's original bytes and decoded spelling. Reading its
current surrounding context is a separate observation: matching those bytes
again does **not** revalidate every required or excluded expression term against
the current file. The host never labels a changed live file as the original
expression observation.

## Live capture and saved-source execution

Captured-workspace expressions reuse the current discovery, optional repository
rules, host file reader, capture admission, `EphemeralIndex`, and exact query
verifier. Path/type exclusions are decided before a source read, so excluded files
do not consume retained-source bytes or the filesystem payload-read allowance.
The selected manifest removes those entries instead of describing them as missing
selected captures. An oversized or unreadable selected file remains unavailable.

The captured route retains its admitted source collection; it is not a one-member
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
NOT terms never act as unsafe negative prefilters. Lowercase plans bypass exact
index exclusions rather than treating an incompatible negative as proof. UTF-16, malformed text, short
needles, and uncovered segments take their normal exact scan path. That indexed
constructor is currently a public-library option: the new snapshot CLI expression
route does not consume an index artifact. Existing `snapshot index search` retains
its literal/raw query routes, including resident and demand-paged postings.

## Work, completeness, and identity

For captured-workspace and saved-source expressions, `--max-scan-bytes` admits
aggregate matching work across ALL predicate and primary scans: 256 MiB by
default, with an explicit maximum of 1 TiB. Reading the same retained source for
another term still spends matching work. This does not replace
source-capture, source-load, archive-validation, or configuration-I/O accounting.
Live source admission and index construction may precede a matching-work stop.

A partial predicate scan cannot establish that a required term is absent or an
excluded term never occurs. Primary hits are withheld until their predicates
qualify. On the captured and saved routes, a subsequent primary scan that reaches
its work cap can return useful exact hits with partial coverage. The single-pass
whole-file route instead withholds all candidates from an incomplete observation.
Unsupported text is counted separately, rather than silently searched through
replacement characters as though it were exact text.

`workspace_complete` requires completed selected-source search, closed discovery,
no unavailable selected sources, and no unsupported selected text. Missing files
outside the explicit metadata scope do not poison that scope's answer. Incomplete
discovery remains incomplete, including unknown repository-policy subtrees.

`--limit` counts only qualified primary hits. Rejected files do not manufacture
truncation. An exactly full buffer is not proof of another hit; cross-file
lookahead continues. Limit zero performs lookahead too: an exhaustive negative
can succeed with no hits, while an actual occurrence returns a truncated response.
`matches_seen` is observed qualified matches, not an invented exhaustive total
when work or result limits stop the captured or saved query. Whole-file
expressions count all qualified occurrences when their observation completes,
independently of retained-row limits.

Snapshot `--count-all` continues past the retained-row cap, including a zero cap,
while keeping the same source and matching-work allowances. A completed count
reports `matches_seen_exact: true` even when `truncated` indicates omitted rows.
An interrupted or otherwise incomplete scan keeps the total a lower bound.

The retained host API `SavedRepositorySession::expression_search_matching` and
the additive C endpoint `fcb_saved_expression_search_matching` accept matching
and counting policy together. A successful generation freezes both policies for
paging and reader activation. Invalid or canceled replacements preserve the
previous accepted results. The original expression APIs keep exact matching,
and literal queries, attached indexes, and already opened readers remain
independent of expression replacement or clearing.

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
`whole_file_expression_cli` covers large-file streaming, metadata filtering
before source access, UTF-16 offsets, cross-buffer predicates, lowercase
witnesses, invalid suffixes, global I/O limits, zero-row counting and executable
dispatch. `atlas_stream_expression` exercises the retained native-host route
with real files, late required/excluded terms, bounded result retention, UTF-16
and lowercase witnesses, native/expression scope intersection, incomplete
observations, Stop/cancellation, immutable pages and query-error recovery.
These tests are committed, not claimed executed in this implementation session.

The query language remains the existing bounded conjunction/exclusion syntax;
there is no general boolean or regex engine, complete glob grammar, or new
index format. Existing snapshot size limits, trusted-catalog integrity
boundaries, and capture-consistency limitations remain unchanged. No performance
benchmark or native qualification is inferred.
