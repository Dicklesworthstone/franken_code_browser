# `fcb` headless source tools

`fcb-app` produces the standalone binary **`fcb`**. It composes the public `fcb`
source, reader, path-index and exact-search APIs. There is no second decoder,
matcher, directory walker or search index in the app. The public library keeps
its inert default profile. The app selects only the `search` profile and
std/core dependencies.

**Implementation status: code-first, independent RCH verification pending.**
There is no native AppKit/Metal GUI in this binary. On macOS, `fcb`, `fcb FILE`,
and human `fcb open FILE` can hand off to a compatible installed native app;
other platforms return an explicit unavailable error for GUI requests.
`fcb --json` reports capabilities and never launches a window. `doctor`
is a **static capability report**, not a benchmark, repair operation, source
scan, or claim that product gates passed.

## Commands

Once built by the authorized build lane, the executable supports:

```sh
fcb --help
fcb capabilities --json
fcb doctor --json
fcb inspect 'src/file with spaces.rs' --json
fcb read src/lib.rs
fcb open src/lib.rs --json
fcb search src/lib.rs --text 'pub fn' --json --limit 100
fcb search src/lib.rs --text 'cancel' --ignore-case --count-all --limit 50 --json
fcb search payload.bin --raw-hex 00ff --json
fcb read huge.rs --offset 4294967333 --bytes 32768 --encoding utf8 --json
fcb search huge.rs --offset 4294967333 --bytes 32768 --encoding utf8 --text needle --json
printf 'banana' | fcb search --stdin --text ana --json
printf 'Banana BANANA' | fcb search --stdin --text ana --ignore-case --count-all --limit 0 --json
fcb read --json -- --filename-starting-with-a-dash

fcb inspect /path/to/repository --workspace --json
fcb search /path/to/repository --workspace --text 'pub fn' --json
fcb search /path/to/repository --workspace --path 'src/lib' --json
fcb search /path/to/repository --workspace --text needle --max-files 8192 --max-total-bytes 67108864 --json

fcb search huge.rs --whole-file --text needle --json
fcb search huge.rs --whole-file --text needle --ignore-case --json
fcb search huge.rs --whole-file --text needle --max-scan-bytes 4294967296 --json
fcb search payload.bin --whole-file --raw-hex 00ff --json
fcb search /path/to/repository --workspace --whole-file --text 'pub fn' --json
fcb search /path/to/repository --workspace --whole-file --raw-hex 00ff --json
fcb search /path/to/repository --workspace --whole-file --text 'pub fn' --count-all --limit 50 --json
fcb search /path/to/repository --workspace --whole-file --text 'pub fn' --ignore-case --count-all --limit 0 --json
fcb search huge.rs --whole-file --identifier cancel --json
fcb search /path/to/repository --workspace --whole-file --identifier cancel --count-all --limit 50 --json
fcb search huge.rs --whole-file --query '"pub fn" Result -unsafe' --json
fcb search /path/to/repository --workspace --whole-file --query 'ana required -forbidden path:src/ lang:rust' --json
fcb search /path/to/repository --workspace --whole-file --query 'ana required -forbidden lang:rust' --ignore-case --count-all --limit 0 --json
```

By default, the scope is **one explicitly selected file**, not its siblings or
an implicitly discovered repository. Its parent path is reported for context
but is not a recursively authorized workspace. A directory requires explicit
`--workspace` on `inspect` or `search`. Selecting stdin, a single file, or a
metadata diagnostic never silently widens that authority.

## Find Markdown headings from the command line

```sh
fcb symbols . --workspace --headings --name Install --match prefix --json
fcb symbols guide.fcbs --snapshot --headings --name Deploy --json
fcb symbols NOTES --headings
```

Use `--headings` to list FrankenMarkdown headings, or add `--name` and an
exact, prefix or contains match on the case-sensitive title. Workspace and
archive searches select `.md`, `.markdown`, `.mdown` and `.mkd` suffixes
case-insensitively before spending payload bytes. Selecting an individual file
or archive member explicitly treats it as Markdown regardless of its filename.

Rows include the display title, canonical slug, level, path and original heading
block byte range. UTF-8 and BOM-marked UTF-16 preserve original coordinates.
Each document has a 256 KiB source and decoded-input ceiling. Unavailable input,
unqualified heading provenance and budget exhaustion leave coverage partial.
The `--limit` option controls displayed rows. A lower row limit still permits
a complete count when every eligible document was qualified, including when
`--limit 0` suppresses all rows. Check `heading_count_complete`,
`stats.matching_candidates_seen` and `listing_truncated` for those outcomes.

## Streaming saved snapshots

`snapshot save` discovers a bounded catalog, captures one member at a time and
writes the existing FCBS/1 format directly to a new private destination. It
releases each capture before opening the next source. Finalization hashes the
output in 64 KiB chunks and checks it against the accepted payload before writing
the checksum. Neither the complete source collection nor the encoded archive is
allocated as one buffer.

The compact CLI defaults stay at 4,096 files, 1 MiB per file and 32 MiB total
source. Larger exports require explicit budgets:

```sh
fcb snapshot save /path/to/repository --output repository.fcbs --respect-ignores \
  --max-files 65536 --max-file-bytes 33554432 --max-total-bytes 1073741824 --json
fcb snapshot inspect repository.fcbs --json
fcb snapshot read repository.fcbs --member src/large.rs --offset 4194304 --bytes 32768 --json
fcb snapshot catalog repository.fcbs --output repository.fcbc --json
```

The file-backed profile admits at most **32 MiB per captured member**, **1 GiB
total source** and **1 GiB + 16 MiB of archive bytes**. The metadata walk retains
its own path, queue, depth and page bounds; a larger requested file count does
not guarantee complete discovery. Failed source reads consume the shared I/O
allowance too. An oversized, changing, unreadable or over-budget member remains
an explicit unavailable record; a captured empty file stays distinct. Rule-file
reads retain their separately reported limits and evidence.

Ordinary and pinned-catalog snapshot opening, offline member reading, comparison,
Reading Desk saved repositories and direct trail reading use this larger paged
profile. They never reopen archive member names as live source paths. The Mac
Saved Repository window offers **Standard** and **Large workspace** capture
presets; its standard 20,000-file / 1 MiB / 64 MiB preset remains the default.
Search, structural analysis, index building, diff work and supplemental trail
pack validation keep their independently bounded operation budgets.

The save receipt retains the existing digest, coverage, rule and filesystem
effect fields. `payload_residency` is `one-captured-member`;
`largest_captured_member_bytes` reports the largest successfully captured member,
while `payload_bytes_read` includes failed reads. `archive_hash_bytes_read`
separately reports final archive readback. These are work and payload counters,
not a process-memory measurement.

Destinations are created exclusively with owner-only permissions and are never
overwritten or automatically removed. Cancellation can leave an empty or partial
destination. A completed checksum advances the effect to
`complete-file-sync-unconfirmed` before another cancellation check; successful
file and parent sync requests advance it to `complete-file-sync-requested`.
Broken response delivery preserves the known effect. Saved observations are not
an atomic cross-file filesystem snapshot, and no power-loss qualification is
claimed.

Library callers select `SnapshotLimits::paged()` with `SnapshotStreamWriter` and
`PagedSnapshot`. The contiguous `SnapshotBytes` encoder and `SnapshotView`
retain their compact limits. Regression source covers format equivalence,
bounded writer admission, a 96 MiB CLI archive with pinned reopening, larger
members, changed live sources, quotas and cancellation effects. Compilation,
execution and native qualification remain pending the independent strict-RCH
lane.

## Reading trail source packs

The CLI also provides bounded saved-archive and reading-trail commands, including
`trail add`, `inspect`, `read` and `export`. The native Saved Repository window can
follow the resulting FCBT files; see [reading-trail usage](../../native/macos/README.md#following-saved-reading-trails).

An exported trail can include exact selections from several captured repository
versions. Supply each archive explicitly; the verified archive digest chooses
the source for each saved visit, even when the archives contain identical paths:

```sh
fcb trail export investigation.fcbt --archive before.fcbs --archive after.fcbs --output investigation.json --json
fcb trail export investigation.fcbt --archive before.fcbs --archive after.fcbs --output smaller-pack.json --max-source-bytes 1048576 --max-validation-bytes 33554432 --json
```

Up to eight `--archive` arguments are accepted for `export`; `read` still accepts
one. Repeated or copied archives are fully validated before duplicate digests
share a retained handle. Explicit trail visits and notes remain ordered and are
never deduplicated. A reference to an archive that was not supplied stays visible
as unavailable (`TRAIL_OTHER_ARCHIVE`), and the export reports partial coverage.
An unreadable, corrupt, or over-budget supplied archive refuses the export before
creating the destination.

Archive opening has a separate cumulative allowance from selected-member
verification. `--max-validation-bytes` defaults to and cannot exceed 256 MiB;
admission counts each observed archive length plus one byte for its EOF probe.
Each archive retains the existing 80 MiB envelope limit. All retained archive
directories share a 64 MiB managed admission limit. The pack and receipt report
actual validation reads, admitted bytes, distinct/duplicate archives, and retained
and peak directory charges. Member verification still uses one shared
`--max-source-bytes` budget, including repeated visits. Output byte and Unicode
scalar limits include headers, provenance, and every retained note; source
selections are included whole or omitted with a reason.

The `fcb.trail-source-pack/1` pack includes an `archives` inventory. Its existing
`archive` object remains available when one distinct archive was supplied and is
`null` when several were supplied. Selected original bytes use lossless hex,
including UTF-16 and malformed text. Exports contain selected source and full
user notes without redaction, access no live source roots, and exclusively create
the requested new destination. Cancellation and response-delivery failure retain
the reported file effect; they cannot undo a completed write. Executable
regression verification of the multi-archive route remains pending.

The native Saved Repository workflow also supports mixed-archive source packs:
**Export source pack… → Include other saved archives…** combines the retained
archive with up to seven explicitly selected supplemental archives. CLI and
native export share the bounded archive-admission implementation and digest
routing policy. The native receipt binds the selected path inventory and
validation counters to the export operation; recovering that receipt performs
no source reads or publication retry. Supplemental files are selected for export
only and do not change the archive attached to trail browsing. See
[native source-pack export](../../native/macos/README.md#exporting-a-saved-trail-with-its-source)
for presets, coverage and pending verification.

Individual mixed-archive visits can also open from one explicitly selected FCBS
file through `SavedRepositorySession::open_trail_reader_with_archive`. The
request pins the accepted trail generation, digest and item before archive I/O,
then uses the shared bounded envelope validator and exact trail reader. The
prepared reader owns verified bytes independently of the primary session and
supplemental file grant. The receipt preserves the original other-archive entry,
identifies the actual source archive and resolved member separately, and uses
fresh staging source identities outside the primary member interval. See
[following saved trails](../../native/macos/README.md#following-saved-reading-trails)
for the native picker and pending independent verification.

On macOS, human `fcb open` and bare `fcb` can hand off to an installed native app.
This CLI lane does not claim completion of all FCB-057 acceptance criteria.

## Native file and line startup on macOS

```sh
fcb open /absolute/path/to/main.rs --line 120
fcb open --line 120 -- -notes.rs
fcb open /absolute/path/to/main.rs --line 120 --json
```

The first two commands open the native retained reader at physical line 120,
column one. Lines are one-based canonical decimal integers. The third command
uses the existing headless line reader and does not launch a window. Native
`--line` applies to a regular file; other reader options require `read` or
`open --json`.

The native project is the file's immediate parent. Once that root is admitted,
the explicit filename opens independently of catalog page limits, ignores and
display filters. The reader captures at most 32 MiB, resolves the line in those
same immutable bytes, and presents bounded pages. An absent/unmappable line
keeps the captured first page with a visible explanation; an unreadable or
over-limit source is refused. Source contents are never executed.

Rebuild the app with `scripts/build_macos_app.sh` to include the versioned
`fcb-launch-file-v1` and `fcb-launch-line-v1` markers. A bundle without line
support is refused before launch instead of opening at the wrong position.
App Store builds require their native picker and do not advertise command-line
file/line access. The launch receipt confirms submission only; native builds,
physical-Mac activation and independent RCH verification remain pending.

## Typed line and column navigation

```sh
fcb read /absolute/path/to/main.rs --line 120 --column 9 --json
fcb read /absolute/path/to/main.rs --line 120 --column 9 --column-kind utf16 --json
fcb open /absolute/path/to/main.rs --line 120 --column 9 --column-kind utf8 --json
```

Column navigation uses the same bounded forward scanner as the native
observed reader. It finds a one-based physical line and logical column while
retaining only bounded context, so a long line or file need not fit in a
whole-file capture. `--column-kind scalar` is the default; `utf16` counts
UTF-16 code units and `utf8` counts decoded UTF-8 bytes. BOMs do not count
toward columns, tabs count as one unit, and a content-end caret is valid.
An interior Unicode scalar or surrogate position is refused rather than rounded.
These coordinates do not claim grapheme, visual-column or shaped-caret semantics.

The default context limit is 64 KiB; `--bytes` accepts 4 through 262144 in
column mode. The default prefix scan allowance is 256 MiB, configurable with
`--max-scan-bytes` up to 1 TiB, with a separate 131072-read-call bound.
`--encoding auto|utf8|utf16le|utf16be` controls decoding. Auto detects
UTF-8/BOM UTF-16 from the same observation. `--column` cannot combine with
`--lines`; human `open` retains its existing native file/line launch grammar.

The `read-position` JSON receipt includes the original byte offset, typed
column, decoded offset within the displayed window, captured and visible ranges,
scan/read counters and exact original bytes as hex. The selected bytes come
from the same forward pass that resolved the position, without rereading the
live file. Metadata consistency is reported as an observation, not an atomic
whole-file snapshot. Original ranges and the decoded caret are checked through
the shared extent reader before output.

Exit 0 means the point and its context were resolved. Exit 1 reports a missing
physical line or logical column; exit 3 means an allowance ended before exact
resolution/context, and makes no missing-position claim. Invalid coordinates
return exit 2 and cancellation returns 130. A final line terminator adds no
phantom line. Regression sources cover encodings, every scalar boundary,
oversized lines, malformed arguments, cancellation and interrupted output;
independent strict-RCH execution remains pending.

## Whole-file streaming search

`search FILE --whole-file` scans one open regular file continuously from byte
zero, without first retaining a full capture. With `--workspace`, it searches
one admitted catalog member at a time. This route handles files larger than the
default workspace capture limit and does not accumulate the repository's source
payloads in memory. It uses the existing overlapping KMP matcher and source
UTF-8/UTF-16 decoder, not an external search executable or a second engine.

Text matching is exact and case-sensitive by default. A literal is prepared in
UTF-8 and UTF-16LE/BE; decoder validation, code-unit alignment and actual
leading-BOM handling make encoded matching equivalent to these exact text
semantics. Add **`--ignore-case`** to use the existing Unicode lowercase scalar
matcher for either a single file or a workspace. For example, `k` matches
`K`, `K`, and `k`; a match for `i` inside the lowercase expansion of `İ`
retains the entire original source scalar. Each occurrence keeps its own
original spelling and exact encoded bytes. This does not add full case folding
(`ss` does not match `ß`) or canonical accent normalization.

Literal matches may cross any number of short reads or buffer boundaries, even
when the literal is longer than the 16 KiB input buffer. Internal U+FEFF scalars
remain content. Regex is not silently applied. `--encoding` can declare
`utf8`, `utf16le`, or `utf16be`; automatic detection uses the beginning of
the SAME continuous observation. `--raw-hex` searches arbitrary original bytes
and cannot be combined with an encoding override or `--ignore-case`.
File-window and stdin search offer the same explicit lowercase and counting
options within their bounded captured scope; see below.

Use **`--identifier NAME`** instead of `--text` for exact whole-token occurrences
in either a named file or an explicit workspace. This uses the same shared
identifier matcher as native project search. A search for `cancel` matches
`cancel()` and `"cancel"`, while excluding `cancelled`, `_cancel`, `$cancel`,
`1cancel`, and `Cancel`. Comments and strings participate; results are text
candidates and do not claim compiler-resolved references. Tokens contain ASCII
letters/digits, `_`, `$`, and conservative non-ASCII non-whitespace/non-control
runs. That policy preserves combining marks and non-ASCII punctuation as part
of a token; it is not a language-specific or Unicode-identifier parser.

The identifier must be one nonempty token of at most 1024 UTF-8 bytes and cannot
begin with an ASCII digit. Matching is case-sensitive and uses exact encoding
semantics; `--ignore-case` and other query modes cannot be combined with it.
Both UTF-8 and UTF-16 retain exact original-byte witnesses, including matches
split across read buffers. Only a delimiter or the actual end of the observation
establishes the token's end: stopping at a byte budget never accepts a partial
token. `--identifier` requires `--whole-file`; source windows and stdin do not
silently acquire different boundary semantics. `--count-all`, including
`--limit 0`, and the existing global byte/read-call limits also apply.

Use **`--query EXPRESSION`** for the existing phrase, required-term, exclusion,
path and language syntax over a named file or workspace. For example,
`--query '"pub fn" Result -unsafe path:src/ lang:rust'` returns `pub fn`
occurrences only in Rust files whose same continuous observation contains
`Result` and contains no `unsafe`. All text terms share one continuous source
observation and one decoding pass; `--ignore-case` applies to all of them together. Path/language
filters run before source opening. `--text` remains a literal and is never
silently reinterpreted as an expression.

Streaming expressions qualify only after supported decoding reaches the end of
the admitted observation. Primary candidates from an invalid or incomplete file
are withheld, even when all positive terms were seen before the I/O limit.
Previously completed files remain useful in a partial workspace result. Terms
may cross input boundaries or occur far apart without requiring full captures.
A changed source is still an observation, not an atomic snapshot.

Expressions continue scanning and count every qualified primary occurrence
regardless of `--count-all`, because final predicates require the full stream.
`--limit` bounds only retained rows; a completed expression can therefore report
an exhaustive count and omitted rows together. `--limit 0` retains no witnesses
while distinguishing complete absence from qualified occurrences. Rejected
files contribute no count, rows or truncation. See
[search expression semantics](SEARCH_EXPRESSIONS.md) for the grammar, metadata
domains and saved-source route.

The operation's allocations depend on fixed input/decoder scratch, prepared
pattern lengths, term count and result capacity, not the source file's length.
The input buffer is 16 KiB; decoder scratch and KMP state are separately admitted, so **16 KiB is not
a claim about total process memory**. Each step performs at most one successful
read, 32 read attempts including interruptions, and one primary-matcher batch
of at most 64 candidate matches. Expressions also advance their bounded predicate
state over that same consumed input. Both source processing and retained output
are bounded.
Blocking OS calls do not acquire a fabricated wall-clock deadline.

`--max-scan-bytes` controls original-source I/O independently from capture
capacity. Its default is 256 MiB and its maximum selectable allowance is 1 TiB.
The command also has a fixed limit of 16,777,216 read attempts. These are
independent limits: choosing the maximum byte allowance does not bypass the
read-call limit. In a workspace, byte, read-call and stored-hit budgets are
**global**, not reset per file; failed reads still spend their work allowance.
A limit stops with explicit partial coverage and an unexamined-file count.

Add **`--count-all`** to continue scanning after the retained-hit allowance is
full. For example, `--count-all --limit 50` retains the first fifty occurrences
while counting later matches in the same file and subsequent workspace files.
`--count-all --limit 0` retains no occurrence rows and still counts matches.
The same overlapping literal semantics apply, including UTF-16 and cross-buffer
matches. The source buffer, retained-hit bound and global byte/read-call limits
are unchanged; counting never grants unlimited I/O or suppresses cancellation.
Without this opt-in, literal, raw-byte and identifier searches stop at the first
extra match. Expressions always finish predicate evaluation and count their
qualified primary occurrences as described above.

`--whole-file` requires named files and `search`; it does not accept stdin,
window offsets/byte counts, `--path` queries, `--max-file-bytes` or
`--max-total-bytes`. The latter are full-capture reservations for the default
indexed route. Workspace discovery limits and static exclusions still apply.
Use `--max-files` and `--include-excluded` with an explicit workspace as before.

### Progressive discovery and content search

Use **`--progressive`** to interleave repository discovery and whole-file search
without first retaining a complete workspace catalog:

```sh
fcb search /path/to/repository --workspace --whole-file --progressive --respect-ignores \
  --text 'pub fn' --count-all --limit 100 --json
fcb search /path/to/repository --workspace --whole-file --progressive --respect-ignores \
  --query 'anchor required -forbidden path:src/ lang:rust' --stream
fcb search /path/to/repository --workspace --whole-file --progressive --respect-ignores \
  --identifier cancel --skip-matches 256 --limit 100 --max-scan-bytes 4294967296
fcb search /path/to/repository --workspace --whole-file --progressive --include-excluded \
  --query 'generated_token path:target/ lang:rust' --stream
```

This route uses the same progressive catalog service and continuous file matcher
as native large-project search. It has **no aggregate file-count or cumulative
path-storage limit**: old catalog pages are evicted after examination while exact
match witnesses keep their own path and original bytes. A long traversal can
reach files beyond both the frozen catalog's 65,536-file maximum and its path
storage quota. Results follow observed discovery order; they are not a sorted
or atomic workspace snapshot.

Choose exactly one explicit discovery policy: **`--respect-ignores`** applies
product exclusions and bounded repository ignore rules; **`--include-excluded`**
searches normally ignored and generated files without loading repository rules
as configuration. The override reports `all-regular-files/no-rule-files-v1`,
`rule_files_enabled: false` and `rule_policy: null`. It still confines traversal
to the selected root, skips symlinks and special objects, and excludes FCB's
own cache, store, scratch and recovery artifacts. Omitting the policy returns
`CLI_PROGRESSIVE_REQUIRES_DISCOVERY_POLICY`; combining both returns
`CLI_INCOMPATIBLE_OPTIONS`, before discovery starts.

UTF-8 and BOM-marked UTF-16 text are detected automatically. Text and expressions
support `--ignore-case`; identifiers retain
their exact whole-token policy. Expressions establish required/excluded
predicates on each observed file before publishing primary matches. Counting
continues for expressions, or for literal/identifier searches with `--count-all`,
after retained rows fill. `--skip-matches` scans preceding occurrences within
the same I/O bounds; a later invocation is a fresh walk and can observe a
different order after filesystem changes.

Progressive invocation accepts 1–256 retained matches, query text up to 1,024
UTF-8 bytes, and a total scan allowance of 1 byte–4 GiB (256 MiB by default).
The read-call ceiling is 1,048,576. Discovery retains at most four 256-entry
pages, with 2,048-byte relative paths, depth 32 and a 4,096-entry pending
frontier. These are residency and traversal bounds, not a total discovered-file
ceiling. Omitted paths, refused frontier entries, rule diagnostics, unavailable
sources and incomplete scans remain explicit coverage metadata. Raw-byte
queries, explicit encodings, `--max-files`, zero-row
requests and captured-window options are rejected for this mode.

Ordinary output is human-readable; **`--json`** emits one bounded document with
schema **`fcb.progressive-search/1`**. **`--stream`** instead emits
**`fcb.progressive-search-stream/1`** records with `start`, `progress`, `match`
and exactly one required `terminal`. A match contains a finalized exact byte
range, matched text, original-byte hex and bounded same-observation context.
Progress exposes actual discovery,
scan, retention and omission counters. Numeric fields use canonical decimal
strings. The existing bounded sink limits each record to 8 MiB and the complete
stream to 256 MiB; output exhaustion stops further work and reports a partial
terminal at a clean record boundary. A failed or partially written record is
interrupted output and is not repaired by appending more JSON. Ordinary JSON
that exceeds its 8 MiB document cap returns a bounded error document.

Cancellation between bounded service steps preserves finalized matches and
reports canceled coverage. Complete counting with omitted rows remains partial
output, separately from count completeness. The existing frozen-catalog search
routes and their schemas keep their current contracts. Focused source, argument,
large-discovery and output-boundary regressions accompany this addition;
independent executable verification is pending.

### Later match windows

Use **`--skip-matches N`** with `--whole-file` to reach occurrences beyond the
first retained result table without increasing its 4,096-row maximum:

```sh
fcb search huge.rs --whole-file --text needle --skip-matches 4096 --limit 100 --json
fcb search /path/to/repository --workspace --whole-file --identifier cancel --skip-matches 4096 --limit 100 --stream
fcb search /path/to/repository --workspace --whole-file --query 'anchor required -forbidden' --skip-matches 100 --limit 50 --count-all --json
```

The offset counts matching occurrences, including overlaps, under the selected
literal, raw-byte, identifier or expression policy. In a workspace it spans
the admitted files in catalog order. Metadata exclusions, failed opens and
unqualified expression candidates do not consume it. Expressions still evaluate
their whole-file predicates before any candidate contributes to the offset.

Skipping scans from byte zero using the same decoding/token context and global
byte/read-call budgets. It does not seek into the middle of source or reserve a
larger hit table. Each invocation is a new live-file observation: edits or changed
catalog membership can change the result ordering. Offsets across invocations
are not pages of one immutable capture.

JSON reports and NDJSON progress/terminal records distinguish `page_start_after`
(the requested offset), `skipped_matches` (preceding matches actually observed),
`has_more_matches` (evidence of an omitted later occurrence), and
`next_match_offset` (the continuation after retained rows, or null). Per-file
records use file-local offsets; workspace summaries use the global offset.
A false `has_more_matches` does not prove absence when scan coverage is partial.
Zero-row requests never advertise a nonprogressing continuation. A terminal
record also withholds continuation if an output limit or cancellation prevented
a retained file record from being delivered; resuming must not skip unseen rows.

Counts include deliberately skipped matches. The existing truncated-row and
partial-result exit policy still reports these omitted earlier rows, even if
the last requested window reaches EOF. Use the window fields separately from
coverage and count completeness. `--count-all --limit 0` remains a bounded
count-only scan. `--skip-matches` is unavailable on the captured-window and
indexed-workspace CLI routes.

### Progressive NDJSON output

Add **`--stream`** to a whole-file search to receive discovery progress, scan
progress, and each completed file as soon as they are available:

```sh
fcb search huge.rs --whole-file --text needle --stream
fcb search /path/to/repository --workspace --whole-file --query 'anchor required -forbidden' --stream
fcb search /path/to/repository --workspace --whole-file --identifier cancel --count-all --limit 50 --stream
```

This flag is explicit, requires a named file or an explicit workspace, and
cannot be combined with `--json`. Ordinary `--json` still produces exactly one
bounded document. Window searches, stdin, and capability reports retain their
existing output modes.

Each newline-delimited JSON record uses schema **`fcb.search-stream/1`.
All integers, including the monotonically increasing `sequence`, are canonical
decimal strings. Native paths retain reversible byte encodings; embedded
newlines are JSON escapes, never record delimiters. The `start` record declares
the query, scope, limits, and `identity_scope: "invocation-local"`. Owner,
file, revision, and query IDs identify only this invocation; they must not
merge observations from separate processes.

| Record type | Meaning |
| --- | --- |
| `start` | Accepted query and explicit source/output budgets, published before source discovery or reads. |
| `progress` | Discovery, catalog readiness, source opening, or bounded scan progress. Match counts cover finalized files only. The catalog-ready record includes discovery limits, rule diagnostics, and the existing confinement/consistency contract. |
| `file` | One finalized source observation, with the same exact ranges, literal witnesses, bounded retained context, encoding, coverage, truncation, and expression qualification as ordinary whole-file JSON. Unavailable sources have explicit errors. |
| `terminal` | Final status and exit code, aggregate actual reads, coverage, finalized counts, delivered-file/hit counts, truncation, and any stopping reason. |

No expression candidate hits or candidate counts are published before all
predicates have been evaluated on that file's observed stream. Matching,
decoding, metadata filters, ignore policy, and global byte/read/hit budgets
are shared with ordinary whole-file search. `--count-all` can produce complete
counts with omitted result rows; that remains a partial result (exit 3), with
`match_count_complete: true` and `results_complete: false`.

The encoder reuses one **8 MiB** record buffer instead of accumulating a
repository response. A stream can deliver up to **256 MiB** total, with space
reserved for its terminal record. Reaching either output limit stops further
scanning and reports a partial terminal with `stop_reason: "output-limit"`.
Actual read counters include work already done for a record that could not fit;
`file_records` and `delivered_hits` distinguish delivered results from
finalized scan counters. Progress is emitted before opening the next source,
so a closed consumer can stop the operation before another file is read.

A consumer must require exactly one final terminal record; **EOF without a
terminal is interrupted output**, even if earlier file records were complete.
An argument/admission error may produce a terminal alone. Cancellation at a
clean record boundary emits a canceled terminal (exit 130). A failed write,
failed flush, or cancellation after a partial record poisons that output:
scanning stops, a redacted diagnostic goes to stderr, and no recovery JSON is
appended. Records are flushed individually; delivery limits each write to
16 KiB and allows at most 4096 write calls per record, including interrupted
retries. A foreign writer can block inside a single write or flush; these
bounds are not a wall-clock timeout.

The progressive route is implemented but awaits the same independent strict-RCH
verification as the existing CLI search routes.

### Retention and consistency

Streaming results retain exact match witnesses and a bounded excerpt around
each retained hit. The CLI requests **160 original bytes before and 160 after**
the match, clipped to the declared observed file bounds. These bytes come from
the same forward read that found the match. Returning results, formatting an
excerpt or materializing its retained extent does not reopen the file or issue
additional source reads. Reading source outside these retained ranges requires
a new observation.

An excerpt can have less following context than requested when the scan stops
at a byte, read-call or match allowance, fails, or is canceled. Its own
`complete` flag describes only the bounded requested excerpt; it does not imply
complete file or query coverage. No extra read is made to complete that suffix.
Count-only requests retain no match or context payloads.

The same context is included in standalone and frozen-workspace whole-file
JSON, `fcb.search-stream/1` NDJSON `file` records, and progressive workspace
JSON and NDJSON `match` records. Human output prints **Same-scan context**.
The added per-hit `context` object separates these coordinates and values:

| Field | Meaning |
| --- | --- |
| `requested_range` | The requested before + match + after interval, clipped to the observed file bounds. |
| `original_range`, `original_hex` | The exact retained original-byte interval and its lossless bytes. |
| `complete` | Whether the retained interval fills this bounded request. |
| `text`, `text_original_range` | Text produced by the existing source decoder and the original bytes it covers; scalar/CRLF guards can trim its edges and the leading BOM is excluded from text. |
| `match_range`, `match_decoded_range` | The exact original match range and its verified UTF-8 offsets within this excerpt's decoded text. These are not global decoded coordinates. |
| `text_has_replacements`, `text_unavailable_reason` | Explicit decoding coverage; text coordinates are null when guarded decoding or an exact match mapping is unavailable, while original hex remains available. |

CRLF is preserved in decoded excerpts. Text rendering never rounds a split
source scalar into a different match, and raw-byte search can retain binary
context without claiming a text interpretation. Context byte totals and the
160-byte allowances are reported alongside the existing match counters.
Schema identifiers remain unchanged: context fields are additive, while the
standalone `retention` and native host `capture_kind` markers now report
`"literal-witnesses-with-context"`. Existing match ranges and witness fields
retain their meanings.

For expressions, results still identify primary-occurrence witnesses; an
excerpt may contain neighboring terms but does not retain separate evidence
for every required or excluded predicate. Exact text and raw-byte matches share
one immutable encoded literal. Unicode lowercase matches retain each occurrence's
original spelling and encoded bytes, because the source may differ from the
query in spelling and byte length. The public `FileSearchReport::retain_hit`
materializes the exact matching bytes; `retain_context` materializes the
separately admitted surrounding bytes. Core callers opt into context before
scanning with `with_context`. Neither route constructs a file digest or a full
immutable source snapshot.

Before/after metadata is taken on the actual open file. Length and modification
time differences, short reads and missing metadata prevent a complete native
result. An unchanged-metadata result is still **not an atomic snapshot**. A
pathname replacement does not cause a mid-search reopen, and positioned reads
do not move a cursor shared with the host's other file handle.

`input_complete` means the declared continuous byte sequence was searched.
`whole_file_complete` additionally requires unchanged before/after metadata.
`match_count_complete` reports whether `matches_seen` is an exhaustive count for
that observed file (or the admitted catalog at workspace level). It can be true
while `truncated` is true: the scan finished, but some occurrence rows were not
retained. `count_all_matches` records the selected policy. A partial scan's count
is only the matches established so far, even when the row buffer is full.
`state` distinguishes completion, match limit, byte limit, read-call limit,
short read, unsupported text, cancellation and failure. Encountered malformed
text invalidates that file's exact-text results; raw-byte search remains an
explicit alternative. Other completed files' results remain useful.

Whole-file JSON uses `strategy: "streaming-whole-file"`. Matching metadata is
present on each file and at workspace level: `mode` is
`exact-decoded-text`, `unicode-lowercase-text`, `decoded-text-expression`,
`whole-token-occurrences`, or `original-bytes`;
`case_sensitive` records the text choice and `normalization` is `exact`.
The latter two fields are null for raw-byte search. Each file record carries
exact original-byte hit ranges, counters, consistency and terminal state.
`literal_text` remains the requested text. Exact text and raw-byte searches
supply their shared `literal_original_hex` once when hits exist. Lowercase
search sets that field to null and adds `matched_text` and `original_hex`
to each retained hit. `retained_witness_bytes` reports retained original-byte
payload, including zero for count-only scans. `decoded_range` is null: the
scanner does not invent global decoded coordinates.
Expression results also declare the original `expression`,
`predicate_scope: "same-observed-stream"`, and `path_filter_domain`. Each observed
file reports nullable `expression_qualified`: true or false after a supported
complete stream, null if unfinished or invalid. This reports predicates over
the observed sequence; changed file metadata can still make
`whole_file_complete` and `query_complete` false. Workspace `scope_files` and
`metadata_excluded_files` count known selected and filtered entries separately.
An excluded named file reports `state: "metadata-excluded"`, complete query/count,
zero source I/O and null source identities/lengths; `input_complete` and
`whole_file_complete` remain false because the source was never observed.
Identifier results additionally record `evidence_level: "whole-token-text-candidate"`,
`includes_comments_and_literals: true`, `compiler_resolved: false`, and
`semantic_complete: false`. Match-count completeness still describes the
observed token search, independently of those intentionally limited semantics.
Workspace output has `files`, `files_examined`, `unexamined_files`,
`incomplete_files`, `workspace_complete`, `matching_files`, global counts and
`stop_reason`. `matching_files` includes files with counted occurrences after
the global row allowance fills; these files have empty retained-hit arrays.
An exactly full result buffer is not truncation; one extra verified occurrence
establishes truncation. A complete zero-match scan exits 1; incomplete scans exit
3 even with zero hits. Omitted occurrence rows also exit 3, including a complete
`--count-all` scan: use `match_count_complete` to determine whether the reported
count is exhaustive. A completed counted scan has no `stop_reason` solely
because rows were omitted. The 8 MiB response cap remains independent of scan
size. This addition is code-first; executable qualification remains pending.

## Bounded verification in retained atlas search

Retained live and indexed atlas searches can yield while verifying one captured
file. Each step supplies at most 64 KiB of original source to the shared verifier.
Exact text, Unicode lowercase matching and explicit expressions retain decoder
and matcher context between calls, including UTF-8 scalars and UTF-16 surrogate
pairs split across slices.

A file's hits, occurrence count and matching-file count become visible together
after that file's verification finishes. Expression candidates remain private
until every required and excluded term has been checked in the same capture.
This preserves append-only results and prevents a late exclusion from invalidating
an already activated hit. Count-only queries retain no hit rows.

The current member stays captured across steps. Replacing its live path does not
change continued verification or later result activation. Source-read quotas
limit admission of another member; they do not discard unfinished verification
of bytes already read. Cancellation drops the pending replacement while preserving
the last accepted query and independently opened readers.

This allowance bounds verification input, not the complete worker call's duration.
Initial capture still reads an admitted member, and capture witnesses, result
copying and bounded excerpt encoding have separate work limits. Structural
analysis retains its existing per-file scheduling. No UI latency, native
smoothness or physical-Mac qualification is claimed from these source changes.
Portable Rust compilation and focused search regressions pass on nightly-2026-09-21.
The pinned-nightly, independent strict-RCH and native macOS qualification lanes
remain outstanding.

## Workspace inspection and indexed-capture search

`inspect ROOT --workspace` publishes a sorted catalog of regular files and
metadata counts. `search ROOT --workspace --path QUERY` uses the existing
native-path component/fuzzy index, with Unicode-lowercase matching and distinct
raw identities. Both operations read **zero source payload bytes**. Non-UTF8
names, case-distinct names, and literal Unix backslashes/colons retain their
native bytes; escaped display labels are not source identities.

Without `--whole-file`, `search ROOT --workspace --text TEXT` captures admitted
complete files, forms a search manifest, builds the existing ephemeral trigram
index, and verifies candidate hits against retained exact source. Text is a
**case-sensitive literal** by default, including spaces and strings resembling
query syntax. `--ignore-case` selects the existing Unicode lowercase comparison
for live captured-workspace `--text` and `--query` searches. Original source text,
UTF-8/UTF-16 byte ranges and capture digests remain attached to each hit; exact
trigram negatives are bypassed when they cannot cover the selected case mode.
This does not enable canonical normalization or the separate case-fold expansion
repertoire. Each file uses the existing UTF-8/BOM-marked UTF-16 decoder. Unsupported
exact decoding is reported separately, not as a successful no-match result.

`--max-scan-bytes` independently bounds literal or expression verification work;
the default is 256 MiB and the maximum is 1 TiB. It does not replace source-capture
or filesystem-read limits. A limited scan returns partial coverage, with
`work_limited` and `scanned_input_bytes` in JSON. `--limit 0` still probes for one
qualified hit: absence can be complete, while an existing hit yields a truncated
response with no hit rows. JSON declares `case_sensitive` for both text routes.

Add **`--count-all`** to either captured-workspace text route to keep verifying
after the retained-row allowance fills. For example:

```sh
fcb search /path/to/repository --workspace --text 'pub fn' --count-all --limit 50 --json
fcb search /path/to/repository --workspace --query 'needle required -forbidden lang:rust' --count-all --limit 0 --json
```

The first command retains at most fifty exact hits; the second counts qualified
primary occurrences without retaining rows. Conjunctions and exclusions filter
whole captures and do not multiply the occurrence count. Case-insensitive and
UTF-16 searches keep their existing decoder, original-byte maps and fallback
verification route. No source or work allowance increases with `--count-all`.

JSON reports `count_all_matches`, `matches_seen`, `stored_hits` and
`match_count_complete` independently of `truncated`. Count completeness requires
closed discovery and successful verification of every selected captured source;
unavailable/unsupported sources and verification-budget exhaustion keep it false.
`workspace_complete` continues to describe a complete response including its
retained rows, so omitted rows still return exit 3 even with an exact total.
Without `--count-all`, the existing bounded lookahead behavior is unchanged.
These routes remain pending independent executable qualification.

The workspace default policy is explicitly named
`product-defaults/no-rule-files-v1`: common build/dependency/VCS directories and
selected binary extensions are excluded by the existing product matcher.
`--include-excluded` switches to `all-regular-files/no-rule-files-v1`. Symlinks
are not followed and special objects are not read. Exclusion counts are counts
of observed excluded entries; an excluded directory may contain an unknown
number of descendants.

Use `--respect-ignores` with a workspace command to select the bounded
`repository-rules-v1` engine also used by native browsing, filename finding,
project search and atlas membership. It loads nested `.gitignore` and
`.fcbignore` files with separate configuration byte/read/work budgets; the
source-payload counters do not hide those reads. JSON includes `rule_policy`
limits, counters and diagnostics. Unsupported, unreadable or over-limit rules
leave affected paths unknown and coverage partial, rather than guessing their
membership. `--respect-ignores` and `--include-excluded` are mutually exclusive;
progressive whole-file search requires one of them explicitly.
`fcb snapshot save` accepts the same policy option and retains the rule evidence
with the saved catalog. Without the option, these CLI routes retain their
named policy that does not load repository rules as configuration.

Workspace admission defaults and CLI maximums for the indexed-capture strategy:

| Limit | Default | CLI maximum |
| --- | --- | --- |
| Retained catalog files | 4,096 | 65,536 |
| Complete bytes in one captured file | 1 MiB | 1 MiB |
| Total retained source and payload I/O allowance | 32 MiB | 64 MiB |
| Stored result/listing rows | 100 | 4,096 |
| Total retained raw path bytes | 512 KiB | Fixed for this CLI lane |
| Discovery pages | 4,096 | Fixed for this CLI lane |

Use `--max-files`, `--max-file-bytes`, `--max-total-bytes`, and `--limit` to lower
or select admitted limits. A zero file/total byte allowance permits only empty
captures; a zero result allowance retains no rows. There are additional fixed
walker bounds: one directory descriptor, a 64-directory queue, depth 32,
2,048-byte relative paths, and at most 256 traversal transitions per page.
Rejected names and empty directories consume work even when a page emits no
entries. Reaching a discovery limit leaves incomplete membership, not a closed
universe. Stable file IDs are assigned after raw-path sorting; a partial
catalog's admitted subset may depend on enumeration order.

Every source read is bounded, uses the already-opened file, and has cancellation
checks between steps. Interrupted/failed reads consume the shared I/O allowance;
indexed capture preparation is capped at 131,072 read calls. A file that is too
large, changed during reading, unavailable, or refused by a capture quota stays
an **unavailable member**. It is not truncated into a complete capture or omitted
from the meaning of workspace search. Captured old bytes remain authoritative
for index verification even if the live file changes later.

Workspace output distinguishes `discovery_complete`, `workspace_complete`, and
`truncated`/`listing_truncated`. Content output includes captured-byte/read-call
totals, index-eliminated files, fallback scans, exact source ranges, and
`unavailable_files` / `unsupported_text_files` with reversible paths, file IDs
and reason codes. Declared admission limits and discovery counters are included.
An empty workspace may be complete; an unfinished workspace with zero hits may
not. There is no atomic repository snapshot or persistent identifier inferred
from live directory/file observations.

The default indexed strategy does not accept stdin, byte-window flags, raw-hex
queries or an encoding override. The explicit whole-file strategy above accepts
raw bytes and encoding declarations. Neither strategy is a persistent or
out-of-core posting index: one retains bounded captures and an ephemeral index;
the other scans continuous source while retaining match witnesses and bounded
same-observation context.

## Later matches in saved archives

Saved searches expose the same bounded match windows as the native saved
repository browser. Add **`--skip-matches N`** to literal/raw/expression
search, structural search, or any saved-index search:

```sh
fcb snapshot search project.fcbs --text cancel --skip-matches 4096 --limit 100 --json
fcb snapshot search project.fcbs --query 'cancel Result -deprecated lang:rust' --skip-matches 100 --json
fcb snapshot structure project.fcbs --mode occurrences --name cancel --skip-matches 4096 --json
fcb snapshot structure project.fcbs --mode symbols --name Cancel --name-mode prefix --skip-matches 100 --json
fcb snapshot index search project.fcbs --index project.fcbd --index-digest TRUSTED_SHA256 \
    --paged --text cancel --skip-matches 4096 --limit 100 --json
```

The offset skips verified qualifying occurrences in the engine's deterministic
member/source order, including overlapping literal matches. It does not skip
files, index candidates, bytes, or rendered rows. Expressions skip only primary
occurrences in members whose complete captures satisfy every required and
excluded predicate. Symbols, headings, and identifier occurrences retain their
existing evidence classes and per-file extraction limits.

Every result reports `result_offset` (requested), `skipped_matches`
(actually observed), `has_more_matches`, and nullable
`next_match_offset`. Repeat the same query, options, and archive digest
with the returned next offset to reach later matches without raising the
4,096-row retention cap. Per-member indexes, resident global postings and
demand-paged postings use the same window order. Case-insensitive index
fallbacks keep the offset while directly verifying saved source.

A continuation is emitted only when later matches are known and this response
retained at least one row, with a strictly increasing offset. Zero-row
existence/count-only queries can report more matches while returning a null
continuation. A noncounting literal search with `--limit 0` retains its existing
no-scan behavior; add `--count-all` for an actual count. Null does not prove absence when source coverage or work is
incomplete. Structural metadata omissions consume qualified match positions;
use the engine's next offset rather than adding the visible row count.

Counts include skipped matches. Saved-scope coverage, total-count exactness,
omitted rows and more matches after this window remain separate. Ordinary
queries return partial exit 3 when any preceding matches were omitted, even
on a fully scanned final window. Explicit `--count-all` retains its
total-based exit contract: 0 for an exhaustive positive total and 1 for an
exhaustive zero total, including empty windows beyond the end. Incomplete
coverage or exhausted work still returns 3.

Each invocation reopens and verifies the selected archive and any explicit
index/catalog pins; compare `snapshot_digest` between requests. Replacing
the archive or changing the query does not continue the old result sequence.
Offsets do not remove source, extraction, memory, response, or matching-work
limits, and no live repository source is accessed.

## Single-file windows and stdin

Default visible size is 65,536 original bytes; the maximum is 262,144. A range
capture may include up to eight decoding-context bytes on either side. Those
bytes count toward `payload_bytes_read` but do not silently enlarge the search
scope. Each positioned read step has 64 KiB/32-call bounds; windowed single-file
operations also cap the operation at 4,096 read calls.

For stdin, the byte limit bounds the **whole supplied observation**. The reader
consumes at most one extra byte to distinguish EOF from oversized input. Input
that does not reach EOF within the limit returns an error; a prefix is not
mislabelled as the whole stream. Stdin can block inside a read. Its command must
be explicitly selected; capabilities/help never consume stdin.

Encoding is detected from the beginning of the same captured observation.
Declarations are `utf8`, `utf16le`, and `utf16be`. A nonzero text offset requires
a declaration: there is no second live header read relabelled as the selected
capture. Scalar/CRLF boundaries may shorten or align a visible window. Original
offsets remain absolute; decoded offsets are **window-local UTF-8 bytes**, not
UTF-16 units, graphemes or visual columns. Missing global line context is `null`.
Interior BOM characters remain content; an actual leading BOM is omitted from
logical text. Separate range observations do not establish cross-window matches.

Text search in a file window or stdin accepts **`--ignore-case`** and
**`--count-all`**, independently or together. Matching remains exact and
case-sensitive unless `--ignore-case` is selected. Lowercase matching reuses
the shared Unicode scalar matcher: `k` matches `K` and `K`, and matching
part of the lowercase expansion of `İ` retains the entire contributing
original scalar. This is Unicode lowercase comparison, not full case folding
or accent normalization. Original-byte ranges still refer to the captured
UTF-8 or UTF-16 source, while decoded offsets remain window-local.

`--count-all` continues through the admitted window after its retained-hit
limit, including `--limit 0` for counts without rows. It does not read the
rest of a large file, enlarge the window, join independent observations,
or ignore cancellation. Overlapping occurrences count separately. Original-byte
queries also support counting with `--raw-hex ... --count-all`; they reject
`--ignore-case` because byte matching has no text case semantics.
For example:

```sh
fcb search huge.rs --offset 4294967333 --bytes 32768 --encoding utf8 \
  --text cancel --ignore-case --count-all --limit 20 --json
printf 'Banana BANANA' | fcb search --stdin --text ana \
  --ignore-case --count-all --limit 0 --json
```

A finished window scan reports `scope_match_count_complete: true`.
`match_count_complete` certifies the whole observed file or stdin only when
the source-coverage and consistency checks also pass. `truncated` reports
omitted result rows independently: an exhaustive count can coexist with a
truncated list and exit status 3. Without `--count-all`, one extra match
establishes truncation and stops the scan; that lookahead count is not an
exhaustive total.

Window/stdin JSON keeps the existing encoding-valued `mode`. Text queries add
`case_sensitive`, `normalization: "exact"`, and
`case_insensitive_semantics: "unicode-lowercase"` when requested (otherwise null).
Raw queries use null for all three text-comparison fields. `count_all_matches`
records the selected counting policy.

`read` is explicitly headless. Human text preserves line feeds and tabs but
escapes terminal/directional controls. This is a safe logical display, not an
original-byte export. JSON contains actual logical text and separate original
source-byte hex. Malformed input is labelled with replacements; exact text
search refuses replacement windows while `--raw-hex` remains available.

## Machine schema and delivery

`--json` emits exactly one document with `schema: "fcb.cli/1"`, not NDJSON. All
integer-valued fields, including IDs, offsets, counts and limits, are canonical
unsigned **decimal strings**. Unknown values are JSON null. IDs are
`response-local`; do not merge observations across invocations by those values.
Paths are tagged `unix-bytes` with reversible lowercase `hex` and secondary
escaped `display`. Neither paths nor display labels confer native permission.

Successful execution uses `status: "ok"`, including useful partial output.
Consult coverage fields and the exit status:

| Exit | Meaning |
| --- | --- |
| 0 | Complete admitted result/representation, or a successful inert command. |
| 1 | A complete declared-scope search found no matches. |
| 2 | Argument, source, decoder, admission, unavailable-capability or output error. |
| 3 | Useful partial coverage, a per-file streaming failure, or a truncated result/listing. Never an exhaustive negative. |
| 130 | Cooperative cancellation through the application API. |

Windowed search keeps `scope_complete`, `whole_file_complete`, and `truncated`
separate. An exactly full result buffer is not proof of truncation: the existing
matcher performs one-match lookahead. Counts in a truncated content search are
matches actually seen. The window, stdin, whole-file and captured-workspace
`--count-all` routes can continue to an exhaustive count within their declared
scope with omitted rows. Windowed output additionally distinguishes
`scope_match_count_complete` from whole-source `match_count_complete`. An actual leading BOM can be
excluded from text while still covering the whole declared text representation.
Changed/short/unknown observations never acquire stronger consistency merely
because matching finished.

Errors are one document with `status: "error"`, `complete: false`, and
`{code, subsystem, message, retryable, next_action}`. Diagnostics do not echo
failed paths, source payloads or query strings. Successful source/path output is
explicit user-requested content, not a debug log.

Responses are encoded in a leased buffer before stdout, capped at 8 MiB. If the
complete response does not fit, the private candidate is discarded and an error
is returned; omissions are not hidden to manufacture a smaller successful
response. Writes are at most 16 KiB and 4,096 calls, including retries. A blocked
OS read/write has no invented wall-clock deadline. Broken/canceled delivery may
leave incomplete stdout; the exit is nonzero and only a redacted stderr error
follows, never a second JSON document appended to the prefix. There is no custom
signal handler or streaming subscription protocol.

Ordinary host managed allocations use a 256 MiB admission ledger, including
conservative workspace metadata/capture reservations and old/new overlap.
Retained `ReaderSession` document preparation, rich display and source
qualification share a separate 768 MiB document ledger. Accepted logical and
Styled layouts and their replacement candidates all charge that same document
ledger; admission failure preserves the accepted layout. The reader's two
ledgers have a combined 1 GiB ceiling. These are managed admission limits, not a
total process/RSS limit; other sessions, OS internals, argv and allocator
overhead require headroom.

Retained readers can opt into 512 KiB of original and decoded document source.
The general document defaults, link-collection bounds, atlas parcel profile and
Desk session admission remain independent. Native retained readers request
16,384 logical rows/items and 8,192 blocks, while page sizes stay bounded.

## Filesystem authority boundary

Named-file/workspace commands support macOS and Linux x86-64/AArch64. Other ABIs
return `CLI_NATIVE_FILE_UNSUPPORTED`; bounded stdin remains portable. Final file
opening uses safe `OpenOptions` with read-only, no-follow, nonblocking and
no-controlling-terminal flags, then validates the actual opened object and
its device/inode. No source write or repository command is executed.

The numeric flags are the selected ABI's O_NOFOLLOW, O_NONBLOCK and O_NOCTTY
values. Their previously inspected system references are:

```text
https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/sys/fcntl.h
https://raw.githubusercontent.com/torvalds/linux/master/include/uapi/asm-generic/fcntl.h
https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html
```

These checks do **not** establish race-safe ancestor confinement. Workspace
capture and streaming routes recheck each named component and do not
intentionally follow symlinks, but concurrent ancestor replacement remains a
native-service limitation. The public library's root grant tracks
scope/revocation; it is not a kernel sandbox. A descriptor-relative native
service and physical-Mac evidence are still required for stronger confinement
claims. Reading an already-opened file does not resolve its pathname again
after replacement.

## Verification surfaces

Parser/output tests, single-file service/process tests, public workspace
capture/index tests and real workspace CLI process tests are provided. Fixtures
cover mixed UTF-8/UTF-16, original ranges, raw/control/backslash names, sparse
files beyond 4 GiB, scope expansion, omissions, result limits, cancellation,
revocation and old-capture retention. A test-only independent JSON subset parser
rejects duplicate fields, multiple documents, truncated output and numeric IDs.
These test sources do not establish execution or native qualification.

Streaming tests additionally compare continuous scans with the retained-capture
oracle across short reads, long literals, BOMs, UTF-16 alignment, malformed
suffixes and result limits. File tests exercise large-file tails, shared-cursor
isolation, truncation/growth, namespace replacement and retained witnesses.
`whole_file_cli` exercises actual command dispatch and the standalone binary.
Its lowercase regressions cover UTF-8 and both UTF-16 byte orders, expansion
and original-byte witnesses, scalar splits, exact-mode negative controls,
global counting and limits, malformed input, cancellation and option rejection.

The independent verifier should run these selections using the repository's
strict-RCH lane, not local worker builds:

```text
cargo test --locked -p fcb-source --lib
cargo test --locked -p fcb-source --test metadata_discovery
cargo test --locked -p fcb-search --test ephemeral_index
cargo test --locked -p fcb-search --test path_navigation
cargo test --locked -p fcb --features search --test workspace_search
cargo test --locked -p fcb --features search --test workspace_capture_commit
cargo test --locked -p fcb --features search --test streaming_search
cargo test --locked -p fcb --features search --test file_stream_search
cargo test --locked -p fcb --features search --lib
cargo test --locked -p fcb-app --lib
cargo test --locked -p fcb-app --test headless_services
cargo test --locked -p fcb-app --test standalone_cli
cargo test --locked -p fcb-app --test workspace_cli
cargo test --locked -p fcb-app --test workspace_native_names
cargo test --locked -p fcb-app --test whole_file_cli
```

No compiled binary, execution receipt, signed artifact, native qualification or
product gate closure is asserted by the presence of this implementation.

## Attach a folder catalog to a Reading Desk

`repo-open-scoped ROOT POLICY FILE_LIMIT SCOPE SCOPE_VALUE` attaches an
explicitly bounded catalog before any source query or index build. Use
`repo-open-scoped-hex` when `ROOT` is a raw native path encoded as hex.

| Argument | Accepted values |
| --- | --- |
| `POLICY` | `repository-rules` or `include-excluded`. |
| `FILE_LIMIT` | 1–20,000 catalog entries. |
| `SCOPE` / `SCOPE_VALUE` | `workspace -`, or `directory` / `file` followed by the raw project-relative path bytes encoded as hex. |

For example, `directory 737263` discovers `src` directly. Unrelated siblings
do not spend that catalog's file allowance, so a late folder can be searched
even when the whole-project catalog stops before reaching it. Directory paths
admit 1–2,048 raw bytes and 1–31 nonempty components; non-UTF-8 names and literal
Unix backslashes retain their identity. Absolute, dot and dot-dot components
are refused.

Use `file 7372632f6c69622e7273` to attach only `src/lib.rs`. Direct file
admission does not enumerate sibling entries, so the selected file remains
reachable even when both the root and its parent directory exceed their
catalog limits. The file route admits 1–2,048 raw bytes and 1–32 components,
keeps the full project-relative name, and uses the same text, expression,
structural, filename and captured-index commands as other attachments.
A missing file, directory, symlink or special object is refused at attachment.

The original project root remains the grant and the basis for returned paths.
Repository rules inherited through ancestor directories remain effective under
`repository-rules`. `include-excluded` bypasses those rules and generated-file
exclusions while retaining confinement, regular-file admission and the existing
independent exclusions for FCB's own artifacts. Discovery receipts identify
the actual scope, policy and coverage: `discovery_scope: "file"` carries
`discovery_file` and a null `discovery_directory`; folder and workspace
receipts carry a null `discovery_file`. A complete folder or file catalog
covers only that selected universe. A deliberately excluded file can have
complete, empty eligible membership with an exclusion count; unresolved rule
coverage or an excluded ancestor remains partial. Explicit `include-excluded`
is required to search a file excluded by repository rules.

Use the returned repository token for subsequent text, expression, structural,
filename and index commands. A successfully admitted replacement gets a new
token; failed or canceled admission keeps the previous repository and token.
A delivery failure after acceptance does not roll back that new attachment.
Readers already imported into the Desk keep their captured bytes across
replacement and detachment. Attaching a catalog does not change the Desk
revision or add root authority to a checkpoint.

The existing `repo-open` and `repo-open-hex` retain their 4,096-entry workspace
catalog and repository-rules defaults. Query scopes narrow whichever catalog
is attached. Capturing and verification retain their own independent budgets.
Rust hosts can select the same behavior through
`DeskRepository::open_with_policy`, `open_directory_with_policy` and
`open_file_with_policy`.

## Find within a retained Reading Desk pane

`DeskSession::search_with_options` adds literal, expression and whole-token
identifier Find over a pane's immutable UTF-8/BOM UTF-16 capture, including
captures restored from checkpoints. The stdio interface exposes:

```text
find-page PANE QUERY_GEN MODE CASE COUNT START_AFTER LIMIT SCAN_BYTES TEXT
find-page-text-hex PANE QUERY_GEN MODE CASE COUNT START_AFTER LIMIT SCAN_BYTES UTF8_TEXT_HEX
```

`MODE` is `literal`, `expression` or `identifier`; `CASE` is `sensitive` or
`unicode-lowercase`; `COUNT` is `bounded` or `count-all`. Unicode lowercase
does not claim full case folding or normalization. Identifier mode requires
sensitive case and returns textual candidates, including comments and strings,
without claiming compiler references. Expressions support phrases, required
terms and exclusions. They publish qualified primary occurrences only after
the full captured input has been scanned; a byte-limited expression exposes
no tentative rows or count. Pane labels do not authorize path/type/language
filters, so those expressions are refused.

`START_AFTER` skips matching occurrences; each fresh query generation replays
the retained bytes from zero under `SCAN_BYTES`. `LIMIT` retains at most 4096
rows. Zero rows with `count-all` counts without navigable results. Each hit
includes its page-local `hit` index, one-based whole-capture `occurrence_id`
and exact original byte range. For a nonempty page with known later matches,
advance `START_AFTER` by its `retained_hits`, retaining the same query and
options and choosing a fresh generation.

Responses distinguish `search_complete`, `match_count_complete`,
`rows_truncated` and `has_more_matches`. A complete count can still return
exit 3 because rows were omitted. No known later match proves the end only
when scanning is complete. Existing `find`, `find-text-hex` and `search`
retain literal defaults. Failed or canceled replacements preserve the
accepted query; searching does not change selection, notes or reading history.
No page reopens a source path. Regression sources cover exact encoded ranges,
later windows, count-only results, late expression predicates, cancellation,
offline restoration and protocol framing; independent execution is pending.

## Search and count text in a live Reading Desk

A repository attached with `repo-open` supports scoped literal and expression
queries through the same incremental Desk workflow as structural search.
Queries can capture current sources or reuse an explicitly prepared in-memory
index. Hit activation imports the exact searched capture into the Desk.

| Command | Source and query |
| --- | --- |
| `repo-begin-matching TOKEN QUERY_GEN CASE COUNT LIMIT SOURCE_BYTES SCOPE SCOPE_VALUE TEXT` | Live literal search with explicit case, scope and counting options. |
| `repo-begin-expression TOKEN QUERY_GEN CASE COUNT LIMIT SOURCE_BYTES VERIFY_BYTES SCOPE SCOPE_VALUE EXPRESSION` | Live bounded expression with separate capture and verification budgets. |
| `repo-begin-indexed-matching TOKEN INDEX_GEN QUERY_GEN CASE COUNT LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE TEXT` | Literal verification over the named index's retained captures. |
| `repo-begin-indexed-expression TOKEN INDEX_GEN QUERY_GEN CASE COUNT LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE EXPRESSION` | Expression verification over those same immutable captures. |

Every command accepts a `-text-hex` suffix for UTF-8 hex query framing.
`CASE` is `sensitive` or `unicode-lowercase`. Lowercase matching applies
to primary, required and excluded content terms, preserves original source
byte ranges, and is not full Unicode case folding. It does not alter path
identity or make directory names case-insensitive.

`COUNT` is `bounded` or `count-all`. Both retain at most `LIMIT`
navigable hits; `count-all` continues scanning after that limit to determine
the total. `LIMIT=0` is valid only with `count-all` and requests counts
without retaining navigable rows. Overlapping occurrences are counted.
Expression totals count the primary term's occurrences after every required
and excluded term has been checked against the same complete capture; repeated
predicate matches do not multiply the total.

Expressions use the shared bounded parser: quoted phrases, required terms,
excluded terms prefixed with `-`, and `path:`/`lang:` filters. For example,
`"pub fn" Result -unsafe lang:rust path:src/` finds occurrences of
`pub fn` in selected Rust captures containing `Result` and no `unsafe`.
Only expression commands interpret these operators. Literal commands preserve
the whole input as literal text. Regex remains unqualified and is rejected.

Scopes use the same encoding as structural queries:

| `SCOPE` | `SCOPE_VALUE` |
| --- | --- |
| `workspace` | `-` for the attached authorized catalog. |
| `directory` | Hex UTF-8 relative directory path, with exact component boundaries. |
| `file` | Hex UTF-8 relative file path. |
| `visible` | Sorted unique lowercase raw-path hex CSV; `-` explicitly selects no files. |

Scope and expression metadata predicates narrow the attached catalog before
source/file quotas. They do not discover additional files beyond that catalog
or enlarge its root grant. Visible membership is declared by the caller;
missing members keep coverage incomplete. Invalid scopes never become a
workspace fallback.

`LIMIT` is at most 4,096. Live `SOURCE_BYTES` is 1–33,554,432.
`VERIFY_BYTES` is 0–33,554,432 for indexed literal queries and
0–268,435,456 for expressions. Captures remain limited to 1 MiB per file and
32 MiB per live query or prepared index; a larger expression verification
allowance does not increase source capture limits. Query text is at most
1,024 UTF-8 bytes. Unavailable sources, unsupported decoding and exhausted
work budgets cannot produce a complete count.

Use `repo-step` with the returned `step_count` to advance at most one
selected file, `repo-page`/`repo-progress` to inspect without advancing,
and `repo-cancel` to cancel the exact pending generation. These queries
share the existing increasing generation sequence with structural search,
index builds and clear operations. Failed or canceled replacements preserve
the previous accepted results.

`matches_seen` and `match_count_complete` describe counting;
`retained_hits` and `truncated` describe navigable rows. A counted query
may finish with `search_complete=true`, `match_count_complete=true`,
and `truncated=true`. That terminal response still has exit code 3 because
rows were omitted. A running response's exit code 3 is progress; quitting
with unfinished work also makes the session partial.

`repo-hit` selects the original byte range, including UTF-16 and Unicode
lowercase expansions. Indexed queries perform no live-source reads, and an
activated source survives index clearing and repository detachment. Search
itself does not change the Desk revision; successful hit activation does.

Rust hosts have the corresponding `DeskRepository::begin_query_scoped`,
`begin_expression_scoped`, indexed variants and counted variants. The
`desk_repository_text_queries` and `desk_repository_text_queries_cli` suites
cover the public and stdio paths. These are code-first regression sources;
independent strict-RCH execution remains pending.

## Later captured-index windows in a Reading Desk

Prepared Desk repository indexes now expose later occurrence windows through
`DeskRepository::begin_indexed_window_scoped` and four stdio commands:

```text
repo-begin-indexed-matching-window TOKEN INDEX_GEN QUERY_GEN CASE COUNT OFFSET LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE TEXT
repo-begin-indexed-expression-window TOKEN INDEX_GEN QUERY_GEN CASE COUNT OFFSET LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE EXPRESSION
```

Both commands also accept a `-text-hex` suffix. They reuse the index and
the normal `repo-step`, `repo-page` and `repo-hit` lifecycle. `OFFSET` is a
zero-based qualified occurrence offset across the indexed universe, distinct
from `repo-page`'s row offset within the already retained window. This makes
matches beyond the first 4096 reachable while retaining a bounded result page.

Use `next_match_offset` with the same index, query, case policy and scope,
choosing a fresh query generation. Skipped occurrences consume verification
work but no retained-hit capacity; counts include those occurrences.
Expressions qualify a complete captured file before its primary occurrences
count toward the offset. Count-only windows have no navigable continuation.
A null next offset after incomplete coverage does not prove that no later
matches exist; verification limits remain separate from complete counts.

Every window uses the indexed captures without reopening live source files.
Activating a row preserves its exact source revision and bytes even if the
live file changes. Imported readers and their bookmarks survive index clearing,
detachment and checkpoint restoration. Pending, failed or canceled replacements
preserve accepted rows. New regression sources cover late/overlapping UTF-8 and
UTF-16 matches, full-width offsets, filters, cancellation, partial coverage,
all four protocol variants and captured activation; strict-RCH execution remains
pending.

## Search live repository structure in a Reading Desk

A live repository attached with `repo-open` can search declaration names,
Markdown headings, and identifier occurrences without leaving `fcb desk --stdio`.
The public `DeskRepository::begin_structure` and
`begin_indexed_structure` methods expose the same workflow to Rust hosts.
These methods reuse the retained atlas search engine and its source captures;
they do not introduce another parser or matcher.

| Command | Behavior |
| --- | --- |
| `repo-begin-structure TOKEN QUERY_GEN MODE NAME_MODE LIMIT SOURCE_BYTES SCOPE SCOPE_VALUE TEXT` | Admit a bounded query over the attached catalog; subsequent steps capture live sources. |
| `repo-begin-indexed-structure TOKEN INDEX_GEN QUERY_GEN MODE NAME_MODE LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE TEXT` | Extract from a previously prepared in-memory index's captures, with zero live-source reads. |
| `repo-step TOKEN QUERY_GEN EXPECTED_STEP` | Advance at most one eligible file; use the response's `step_count` for the next step. |
| `repo-page TOKEN QUERY_GEN OFFSET LIMIT` | Page retained matches without advancing the query. |
| `repo-hit TOKEN QUERY_GEN HIT_ID` | Import the exact captured source and select the original byte range in the Desk. |

Both begin commands accept a `-text-hex` suffix when the needle needs
lossless UTF-8 hex framing. Modes are `symbols`, `headings`, and
`occurrences`. Name matching is case-sensitive with `exact`, `prefix`,
or `contains`; occurrences require `exact` and include tokens in comments
and string literals. Symbol results remain heuristic outline candidates.
Heading extraction is owned by FrankenMarkdown. Neither mode claims compiler
resolution or semantic completeness.

The scope is frozen at admission, before file, byte and match quotas:

| Scope | `SCOPE_VALUE` |
| --- | --- |
| `workspace` | `-`, meaning the existing authorized catalog. |
| `directory` | Hex-encoded UTF-8 relative directory path; component boundaries are respected. |
| `file` | Hex-encoded UTF-8 relative file path. |
| `visible` | Sorted unique lowercase raw-path hex values separated by commas; `-` means an explicitly empty set. |

Visible membership is supplied by the host and does not prove that a native
frame was displayed. Missing members remain explicit partial coverage.
Scopes cannot grant access outside the attached repository. Invalid scope
payloads do not fall back to workspace search.

`LIMIT` is 1–4,096. Live `SOURCE_BYTES` is 1–33,554,432; indexed
`VERIFY_BYTES` is 0–33,554,432. Existing per-file, extraction, metadata and
response limits also apply. Unsupported files, unavailable captures and
quota stops are reported rather than treated as complete absence. Indexed
structural search considers captured source even if it has no search grams.

For example, these are literal TAB-separated input frames (displayed below
using shell `printf` escapes). The directory scope `737263` is `src`:

```sh
printf '0\trepo-open\t/path/to/project\n0\trepo-begin-structure\t1\t1\tsymbols\texact\t100\t33554432\tdirectory\t737263\tload_config\n0\trepo-step\t1\t1\t0\n0\trepo-page\t1\t1\t0\t64\n0\tquit\n' | fcb desk --stdio
```

This fixed example inspects one step, then quits; if more files remain, its
session exit is partial. An interactive host should keep stepping while
`search_in_progress` is true, using each returned `step_count`, or explicitly
cancel the running generation. It should not send cancel after completion. A running reply's `exit_code=3` is progress, while a
terminal coverage failure or uncompleted work at EOF makes the session partial.
Query, index and clear attempts share an increasing generation counter.

`repo-progress`, `repo-cancel`, `repo-hit` and all ordinary Desk reading,
copying, pinning, history and bookmark operations work with these hits.
`repo-hit` uses the searched capture even when its live file has changed;
after activation, that source remains readable after clearing the index or
detaching the repository. Repository search itself does not change the Desk
revision; successful hit activation does.

## Follow saved imports in a persistent reading desk

`fcb desk --stdio` can browse lexical source imports inside an opened FCBS
repository, follow an exact import statement or its captured target, and retain
that source in the ordinary reading desk. The source remains available to
`copy`, history, bookmarks, annotations and FCBK checkpoint saves after
`saved-close`. The archive does not need a live working tree.

Input frames are tab-separated: expected Desk revision, command, then arguments.
Replies are one `fcb.desk-stdio/1` JSON object per frame. Start at revision zero
and use each reply's `accepted_revision` for the next request. Opening a saved
archive returns its attachment token in `saved_token`; this token is distinct
from member IDs and query generations. For example, this single process opens
an archive, prepares its imports, pages the accepted result, and exits:

```sh
printf '0\tsaved-open\trepository.fcbs\n0\tsaved-imports-build\t1\t1\t4096\t33554432\n0\tsaved-imports-page\t1\t1\t0\t64\n0\tquit\n' | fcb desk --stdio
```

Use these commands with the returned token and import generation:

| Command and arguments after the expected revision | Result |
| --- | --- |
| `saved-imports-build TOKEN IMPORT_GEN MAX_LINKS MAX_SOURCE_BYTES` | Prepare a bounded projection; return its first page and coverage. |
| `saved-imports-page TOKEN IMPORT_GEN OFFSET COUNT` | Page all retained occurrences. |
| `saved-imports-from TOKEN IMPORT_GEN MEMBER OFFSET COUNT` | Page imports originating in one member. |
| `saved-importers TOKEN IMPORT_GEN MEMBER OFFSET COUNT` | Page occurrences whose unique catalog target is that member. |
| `saved-import-source TOKEN IMPORT_GEN LINK_ID` | Import and select the exact original statement in the Desk. |
| `saved-import-target TOKEN IMPORT_GEN LINK_ID` | Import a captured target at byte zero. |
| `saved-imports-clear TOKEN NEW_IMPORT_GEN` | Discard the projection while retaining activated Desk sources. |

The projection reuses the shared Rust, JavaScript/TypeScript/JSX/TSX, Python,
C/C++, Go and Java extractors and the closed-catalog resolver. Each occurrence retains its original
statement and specifier byte ranges, source digest, language, extraction policy
and explicit resolution outcome. Python relative imports consider the finite
`.py` and `/__init__.py` candidates. Absolute imports check those filenames under
the catalog root, `src/`, and one enclosing regular-package root inferred only
from consecutive catalogued `__init__.py` parents. Unique paths report
`python-absolute-module-candidate`; conflicting paths remain ambiguous without
root priority, and absent candidates remain external. This is a filename policy,
not Python runtime binding or imported-member resolution.
These are lexical relationships with `compiler_resolved: false` and
`semantic_complete: false`. They do not inspect a runtime import path, execute
project code, install dependencies, or fetch external packages.

Rust path dependencies participate in these same commands when the archive
contains the importing package's and target package's `Cargo.toml` captures.
Ordinary explicit `[dependencies]` path entries and `[dependencies.alias]`
tables can map dependency aliases to a library's `src/lib.rs` or declared
`[lib] path`. The resolver checks package/library names and only admits a
unique catalogued module file or containing-module candidate. A statement such
as `use worker_alias::task::Task;` can therefore open the captured dependency's
`task.rs`, and the activated reader survives closing the archive.

Workspace-inherited path dependencies also participate when the captured
workspace root declares the member and its `[workspace.dependencies]` entry.
Member declarations support `alias.workspace = true`,
`alias = { workspace = true }` and `[dependencies.alias]` subtables. Paths
and package renames come from the workspace declaration; the path is relative
to that captured workspace root. Root packages and virtual workspaces are
admitted. Membership and exclusions support literal paths and patterns such as
`crates/*`, where `*` matches one complete directory component. Other glob patterns
and implicit workspace membership remain unresolved.

Each Cargo manifest is bounded to 64 KiB of UTF-8, with at most 256 dependency
aliases considered. Source and metadata work share the aggregate parse allowance;
saved source and simultaneously retained importer, workspace and target metadata
share the 1 MiB payload allowance. At most one nearest ancestor manifest is
consulted, and an uncaptured, invalid, non-workspace or over-budget boundary
leaves explicit unresolved evidence. Conditional, dev/build, registry, git and
path-base dependencies are not inferred. These candidates do not establish
compiler binding or an imported item's definition. Import pages continue to
read only retained result metadata. Resolver receipts use
`fcb-closed-catalog-imports-v15`; native consumers preserve the corresponding
v13/v14 policies and limits when reading older receipts. Workspace-inheritance execution remains pending in
the independent batch verification lane.

Preparation verifies at most one eligible source at a time. `MAX_LINKS` is
1..4096, aggregate source allowance is 0..33554432 bytes, and a source larger
than 1048576 bytes (1 MiB) is explicitly omitted. No source payload remains in the
projection. Pages read metadata only, retain stable one-based occurrence IDs,
and accept `COUNT` 1..128; member ordinals and page offsets are zero-based.
Always follow `next_offset`, which reflects the rows actually delivered.
`imports_complete`, source omission counters, parser diagnostics and
`truncated` describe coverage. An empty partial page does not establish that
the member has no importers.

Import generations are independent of Markdown relationships, literal queries,
expressions and disk indexes. Build and clear attempts strictly increase within
one attachment; an admitted failed or canceled replacement preserves the prior
accepted projection. Replies expose both `saved_import_generation` and
`last_saved_import_attempt`. A new archive attachment invalidates the old token
and starts a separate generation domain.

Source and target activation reverify the selected archive member before the
existing Desk import transaction. They alone change the Desk revision. Evidence
selects the complete original statement, including exact UTF-16LE/BE bytes;
`copy` returns those original bytes. Missing, ambiguous, external or unresolved
targets cannot open, and a recorded target without captured bytes remains
explicitly unavailable. A failed or canceled activation preserves the previous
place. Repeated member, literal-hit and import selections reuse the same exact
retained source identity.

Activation receipts use `selection_namespace: "saved-import"` and include
`import_generation`, `import_link_id` and `import_side`, along with the verified
source digest and the original byte selection. A checkpoint preserves activated
source bytes, places and notes; the archive handle and import projection are not
persisted or reopened. This consumer is code-first; independent executable
verification remains pending.

## Limit saved text and expression searches to a folder or file

Saved literal, original-byte and expression searches accept the same raw path
scope as structural searches. Literal and original-byte searches also accept
these scopes with all three saved substring-index layouts (FCBI, FCBO and FCBD).
The CLI expression route continues to scan verified members directly:

```sh
fcb snapshot search saved.fcbs --text needle --directory src --count-all --json
fcb snapshot search saved.fcbs --query 'needle required -excluded lang:rust' --directory src --json
fcb snapshot search saved.fcbs --raw-hex 6e6565646c65 --member src/main.rs --json
fcb snapshot index search saved.fcbs --index saved.fcbd --index-digest TRUSTED_SHA256 --paged \
  --text needle --member src/main.rs --skip-matches 100 --limit 50 --count-all --json
```

Use one of `--directory PATH`, `--directory-hex HEX`, `--member NAME`, or
`--member-hex HEX`. Omitting these options searches the whole saved archive.
Folder selection is recursive and compares exact raw path components: `src`
includes `src/nested/a.rs` and excludes `src-other/a.rs`. Hex forms preserve
non-UTF-8 labels. Absolute paths, empty components, dot components, parent
traversal, NUL bytes and conflicting scopes are rejected before opening input
files. A saved path remains an archive label and grants no live filesystem access.

Membership is frozen before metadata filters, posting candidates, source work,
match counts and result offsets. An expression's `path:` and language filters
further narrow that selection; conjunction and exclusion terms still apply to
the complete captured member. Counts include only qualified occurrences inside
the selected scope. Repeat the same scope alongside the same archive digest,
query and matching options when following `next_match_offset`.

JSON includes `scope`, `scope_path`, `scope_path_hex`, `scope_known_members`,
`scope_captured_members`, `scope_unavailable_members`,
`scope_membership_complete` and `scope_complete`. Whole-archive
`discovery_complete` stays separate, and `workspace_complete` is reserved for
a complete whole-archive search. A captured exact member can be searched
completely even if discovery elsewhere was incomplete. Missing requested
members remain partial. An empty folder is an exact zero only when archive
discovery was complete. Unavailable or unsupported selected members and query
work limits retain their existing partial-result behavior. Expression metadata
filters may establish that an unavailable selected member cannot qualify.

Rust hosts can use `SavedRepositorySession::search_scoped_page` and
`expression_search_scoped_page` with the shared `SnapshotScope`. Accepted
generations retain their own admitted copy of the raw scope; result pages and
hit-reader receipts echo it after index attachment, detachment or a failed
replacement query. Existing host methods keep their whole-archive behavior.
The public engine exposes scoped direct, per-member-index, postings and paged
postings constructors, with identical original-byte witnesses across routes.
Scoped host and CLI regression fixtures have been added; independent executable
verification remains pending.

## Search structure in a saved repository

The saved structural query uses the same symbol, whole-token and qualified
Markdown heading engines as the retained readers. It searches one explicitly
opened FCBS archive, retaining bounded result metadata while releasing each
member's source before loading the next one.

```sh
fcb snapshot structure saved.fcbs --mode headings --name "Getting started" --json
fcb snapshot structure saved.fcbs --mode headings --name "Install" --name-mode prefix --json
fcb snapshot structure saved.fcbs --mode symbols --name "load" --name-mode contains --json
fcb snapshot structure saved.fcbs --mode occurrences --name "load_config" --json
```

Name matching defaults to exact and case-sensitive. Add `--ignore-case` for Unicode scalar lowercase matching. Symbols are bounded
heuristic declaration candidates. Headings come from FrankenMarkdown with
qualified original-byte ranges. Occurrences are exact whole-token text
matches, including comments and string literals; prefix/contains and
compiler-reference claims are unavailable in that mode. UTF-8 and supported
BOM-marked UTF-16 source retain original-byte coordinates.

The default scope is the whole saved archive. Select a directory subtree or one
exact member to search a smaller scope, including with count-only or later windows.

```sh
fcb snapshot structure saved.fcbs --mode symbols --name open --directory src --count-all --json
fcb snapshot structure saved.fcbs --mode headings --name Install --member docs/setup.md --json
```

`--directory-hex` and `--member-hex` accept raw path bytes. The directory
is component-exact and recursive. For example, `src` includes `src/nested/a`
and excludes `src-other/a`. Only selected members consume query source bytes
or match offsets. Full archive validation on ordinary open still reads the
complete envelope. JSON reports `scope_path`, `scope_known_members`,
`scope_membership_complete`, and `scope_complete`. A file present in a
partially discovered archive can yield an exact count for that one file.
A missing requested file, an unavailable member, or incomplete directory
discovery produces partial coverage. A directory query over a complete
catalog with no matching members yields zero. `workspace_complete` is
reserved for whole-archive queries. Member names remain reversible labels;
the command grants no live-root access and never reads current workspace
source. Results include the archive and source digests, member identity, exact
selected and evidence ranges, candidate kind/language, and heading slugs where
applicable. Use the existing `snapshot read` command with the reported member
and byte offset to inspect source.

`--limit` accepts 0–4096 (default 100); zero probes existence without retaining
a row. `--max-source-bytes` accepts 0–67,108,864 bytes (default 64 MiB) across
all loaded members. Per-member extraction limits are 1 MiB for symbols,
256 KiB for headings, and 32 MiB for occurrences. Occurrence extraction uses
bounded decoder windows across each complete captured member, including bytes
after the displayed result window. Source, extraction and
metadata limits, unavailable members, unsupported inputs, incomplete archive
discovery, and observed additional matches all prevent complete coverage.
JSON reports these counts and `state`, `workspace_complete` and
`matches_seen_exact` separately. Complete refers to processing admitted
extraction, never complete compiler semantics. A partial zero-hit result has
exit code 3; only a complete negative result has exit code 1.

The existing opt-in `--catalog FILE --catalog-digest TRUSTED_SHA256` pair also
works here. It skips whole-archive validation at open, validates archive
boundaries, and verifies every loaded member before using its bytes. Unread
body integrity remains unchecked and is reported explicitly. Omitting the
catalog performs the ordinary full-archive validation. Structural search uses
no substring posting index.

The existing `fcb symbols SAVED.fcbs --snapshot` interface remains available
for saved outlines and reference candidates. This unified command additionally
provides repository-wide heading search and trusted-catalog opening.

## Count saved expression matches

Saved expression search can count every primary occurrence after applying its
whole-member required, excluded, and path predicates:

```sh
fcb snapshot search saved.fcbs --query 'needle required -excluded path:src/' --count-all --max-matches 0 --json

# Match every positive and excluded text term using Unicode lowercase comparison.
fcb snapshot search saved.fcbs --query 'needle required -excluded path:src/' --ignore-case --count-all --max-matches 0 --json
```

Here `needle` supplies the occurrences being counted; `required`, `-excluded`
and `path:src/` decide which members qualify. The retained-hit limit is
independent of counting: zero retains no rows, while a positive
`--max-matches` keeps that many exact witnesses and continues counting.
Source and work allowances, unavailable members, archive discovery, and the
reported completeness fields remain authoritative. `--count-all` does not
turn a partial scan into a complete count or widen its budgets.

`--ignore-case` applies one matching policy to the primary, required, and excluded
text terms. Metadata path and language filters retain their existing semantics.
Matching preserves the complete contributing source byte ranges, including UTF-16
and lowercase expansions such as `İ`. It does not add full case folding or accent
normalization. Snapshot expression JSON records the realized mode and primary
needle; lowercase rows leave `matched_text` null until source bytes are opened
and verified. See [search expressions](SEARCH_EXPRESSIONS.md) for the source,
counting, and retained-reader contracts.

## Build a saved repository search index in the host

`SavedRepositorySession::build_index(operation, destination, limits, canceled)`
creates a new FCBD index directly from the archive handle that the session
already owns. Renaming or replacing the archive pathname cannot redirect this
build to different source. Each loaded member is verified against the retained
archive directory before the existing `SnapshotIndex::build`, `invert` and
`encode_paged` engines use its bytes. No live workspace source is opened.

`SavedIndexBuildLimits` makes the work explicit: `max_file_bytes` is at most
256 KiB, `max_source_bytes` is at most 64 MiB across all attempted member loads,
and `max_grams` is at most 2,097,152 retained file/gram entries. Those ceilings
are also the defaults. Per-file grams remain capped at 65,536, scratch at
1 MiB, and the final FCBD artifact at 24 MiB. Zero quotas are accepted;
refused files remain present as uncovered records. One member is loaded at a
time, but construction, inversion and encoding use the bounded resident
production builder. This is not an out-of-core sort or an incremental store.
Old attached indexes, accepted queries, and previous build receipts stay
charged while a replacement build is admitted.

The destination must be a new file. The shared create-only writer requests
mode 0600 on Unix, never overwrites or deletes an existing file, and reports
the actual bytes written plus file and parent-directory sync progress. A
partial or canceled output remains at the chosen destination for inspection.
The artifact is source-derived sensitive data and is not encrypted. Sync
requests do not establish power-loss qualification.

A terminal build returns `fcb.saved-repository/1`, `command: "index-build"`,
the session/archive identity, `operation`, exact destination bytes, requested
limits, member-read accounting, coverage counts, `index_pin`, and the write
effect. The trusted pin comes from the production encoder and has
`index_pin_scope: "page-manifest-envelope-sha256"`; it is not a hash supplied
by the file being opened. `coverage_complete` requires complete archive
discovery and no unavailable or uncovered members. Limited coverage is a
valid partial build: ordinary searches still verify candidates and directly
scan members that the index cannot safely exclude.

Build operations have their own strictly increasing identity, independent of
index-attachment and query generations. `info` exposes
`last_index_build_attempt` and nullable `last_index_build_operation`. One
terminal receipt is retained by the session, including write failures and
cancellation. `index_build_receipt(operation, canceled)` returns the same
immutable receipt without rerunning construction, reading the destination,
or loading any source. Invalid, stale, or pre-admission failed requests leave
the prior receipt available; a newer admitted terminal operation replaces it.
The receipt belongs to this live saved session and is not a durable journal.

The host must reconcile a canceled or lost response through receipt lookup,
never by automatically repeating the write. Receipt storage and the initial
reply are admitted before publication. Once the writer returns, the host
retains the terminal effect before encoding or transport can fail, and no
late cancellation gate may replace that outcome. The effects distinguish
no create, an incomplete created file, complete bytes with sync unconfirmed,
completed file sync, and completed file plus parent sync.

Building leaves the attached index, accepted query, and retained readers
unchanged. Attachment is a separate explicit
`attach_index(generation, destination, trusted_pin, canceled)` action.
`attach_eligible` is true only after the complete artifact bytes were written;
it can remain true when a later sync or cancellation caused `status: "error"`.
Attachment still validates the current destination against the retained pin
and archive identity before accepting it. A failed or incomplete build never
becomes an attached index by implication.

`tests/saved_repository_index_build.rs` adds host regressions for retained
archive identity, raw destination names, partial coverage and direct-scan
fallback, exclusive writes, operation/receipt ownership, lost replies,
short and complete canceled writes, source damage, and every observed
construction/publication cancellation boundary. This implementation is
code-first; independent executable verification remains pending.

## Indexed case-insensitive search in saved repositories

Saved literal search now uses compatible persisted indexes with `--ignore-case`,
including forward FCBI segments, resident FCBO postings and demand-paged FCBD
postings. `SavedRepositorySession::search_matching_page` exposes the same route
to native hosts. Fresh index builds certify ASCII source rows and intersect
bounded unions of upper/lowercase trigram variants before loading candidates.
Every candidate still goes through the existing exact source-digest check and
Unicode scalar-lowercase matcher. Match order, original byte ranges, total
counts and `next_match_offset` keep their existing meaning.

Unicode sources, UTF-16, malformed text, short or non-ASCII needles, and uncovered
members keep the verified fallback route. Older semantics 1 indexes remain
readable and usable for exact search; their original coverage records are not
silently promoted into ASCII certificates. Build a fresh index to gain the new
pruning. A refresh that reuses old records preserves their conservative fallback.
Receipts identify `ascii-case-variants-with-source-fallback` and retain separate
candidate, fallback, source-read and completeness accounting.

## Retain and recover observed passages

`DeskSession::import_observation` accepts an existing `ObservedReaderSession`, an
accepted observation ID and a nonempty original-byte selection. The source
reader validates the selection against its retained logical window; the Desk
retains that observation's bounded bytes and decoder context. Neither this
transfer nor a later read reopens the source pathname. The public C equivalents
are in [`fcb_desk.h`](../fcb-bridge/include/fcb_desk.h).

For an immutable `ReaderSession`, `retained_passage` prepares at most 256 KiB of
selected original bytes plus eight decoder-context bytes per side, under the
reader's managed admission. `DeskSession::import_reader_passage` keeps that
excerpt without copying the whole capture; the C adapter is
`fcb_desk_import_reader_passage`. This supports selected passages from the
reader's full 32 MiB capture allowance while complete Desk panes still retain
their independent 4 MiB limit. UTF-8/UTF-16 selections must preserve exact
scalar and CRLF boundaries. The saved evidence keeps the original capture's
owner/file/revision, raw path, encoding, byte offsets and captured length.
It is labeled `observed-extent` / `host-supplied-sequence`: an immutable excerpt
from retained bytes, with no new live observation or whole-file Desk claim.
Source closure and later filesystem changes do not affect notes, recovery, or
source-pack export. A lost mutation reply requires a state read, never an
automatic second import.

State exposes separate `observations` and `observed_passages` arrays. These
records preserve absolute 64-bit offsets, reversible path bytes, original
observation provenance, explicit encoding, before/after observed lengths,
consistency evidence and an extent SHA-256. A passage stores its selected range,
label, exact UTF-8 body and update sequence. It is distinct from a complete-file
pane, and the unseen file never gains an immutability claim. Repeated current
selections share retained observation storage; restored provenance cannot alias
new live handles after a process restart.

The default 32 source slots and 32 personal-record slots are shared with complete
captures and bookmarks. Original bytes share the 32 MiB retained allowance.
Selected passages are at most 256 KiB; imported observation backing is bounded
by the core's 1 MiB extent limit and the Desk's source-byte limit. Reads return
logical text plus exact selected `original_hex`, and disclose replacement text
for malformed input. Editing or forgetting requires the current model revision,
a fresh attempt and the accepted passage update sequence.

After restoring an observed-passage checkpoint, `fcb desk --stdio` supports:

```text
EXPECTED_REV<TAB>observed-passage<TAB>PASSAGE
EXPECTED_REV<TAB>edit-observed-passage<TAB>PASSAGE<TAB>UPDATE_SEQUENCE<TAB>LABEL<TAB>BODY
EXPECTED_REV<TAB>edit-observed-passage-hex<TAB>PASSAGE<TAB>UPDATE_SEQUENCE<TAB>LABEL_UTF8_HEX<TAB>BODY_UTF8_HEX
EXPECTED_REV<TAB>forget-observed-passage<TAB>PASSAGE<TAB>UPDATE_SEQUENCE
```

Checkpoint format FCBK 1.2 preserves observed bytes and all accepted notes, with
fresh local identities on restore. Existing 1.0 and 1.1 checkpoints remain
readable; desks with no observed passages still emit 1.1. A failed or canceled
restore leaves the accepted Desk intact.

`DeskSession::export_source_pack` exports explicit bookmark selections and saved
observed passages through the host/C/native and stdio destination workflows.
Complete-capture-only desks retain `fcb.desk-source-pack/1`; desks with observed
passages emit `fcb.desk-source-pack/2`. Its typed observed entries carry absolute
selected ranges, exact selected original bytes, full notes and the retained
observation's raw path, encoding, extent digest and consistency evidence. The
surrounding extent is provenance, not additional exported source payload. No
live source path is opened, including after checkpoint restoration.

All records share source-byte, serialized-byte and serialized-scalar budgets.
Metadata for every record is reserved before any destination is created;
whole selections that cannot fit retain their notes and explicit omission
reasons. Coverage counts both record kinds. `observed_passages_included:true`,
`total_observed_passages`, and `all-bookmarked-selections-and-observed-passages`
identify the expanded scope. Complete-only receipts remain compatible, and the
native client accepts older observed-exclusion receipts only when that narrower
scope is explicit. Checkpoints remain the format for recovering the entire Desk.

Regression sources cover large offsets, UTF-8/BOM and both UTF-16 byte orders,
malformed bytes, canceled transfers, shared retention, stale note versions,
offline checkpoint recovery and real C/stdio boundaries. Executable verification
remains pending in the independent strict-RCH lane.

## Export Reading Desk source packs from the CLI

`fcb desk --stdio` can write the same bounded source packs as the native Desk.
Each command needs an explicit new destination and a maximum number of selected
original source bytes to disclose. These are TAB-separated fields, terminated
by LF:

```text
EXPECTED_REV<TAB>export<TAB>NEW_PACK<TAB>MAX_SOURCE_BYTES
EXPECTED_REV<TAB>export<TAB>NEW_PACK<TAB>MAX_SOURCE_BYTES<TAB>MAX_OUTPUT_BYTES<TAB>MAX_OUTPUT_SCALARS
EXPECTED_REV<TAB>export-hex<TAB>NEW_PATH_HEX<TAB>MAX_SOURCE_BYTES
EXPECTED_REV<TAB>export-hex<TAB>NEW_PATH_HEX<TAB>MAX_SOURCE_BYTES<TAB>MAX_OUTPUT_BYTES<TAB>MAX_OUTPUT_SCALARS
```

`export-hex` accepts native path bytes as hexadecimal, including paths with tabs,
newlines or invalid UTF-8. Numeric arguments use canonical unsigned decimal
strings. Output limits default to 16 MiB each; overrides must supply both limits,
each in `1..=16777216`. The scalar limit counts Unicode scalars in the complete
serialized JSON, including escaping, metadata, notes and provenance. It is not a
token estimate. A source cap of zero exports notes and provenance without source
payloads; repeated selections count separately against the source cap.

Export includes every saved bookmark and observed passage, preserving complete
personal labels and note bodies. It discloses only explicit saved selections,
never an entire open pane or surrounding observed extent. Each admitted selection
has exact original-byte hex and, when decodable within the remaining output
budget, exact readable text. An omitted selection retains its full note and an
explicit reason. If the notes and required metadata cannot fit, the command
refuses before creating a file. Packs are unredacted and unencrypted.

For example, recover a checkpoint and export up to 1 MiB of selected original
bytes within a 4 MiB serialized output budget:

```sh
printf '0\trestore\treading.fcbk\n1\texport\treading-pack.json\t1048576\t4194304\t4194304\n1\tquit\n' | fcb desk --stdio
```

Export preserves the accepted Desk revision and never reopens the source paths,
including after checkpoint recovery. Existing destinations and symlinks are
never overwritten. The `fcb.desk-stdio/1` response contains an export receipt in
`result`, with the pack schema, actual bytes and scalars, SHA-256 digest, source
coverage and filesystem effect. Exit status `3` reports a completed pack with
omitted selections; its notes are still complete. Readable-text availability is
reported separately from original-byte coverage.

The outer response retains any file effect even when cancellation races receipt
delivery. Cancellation during a write can leave an explicitly incomplete
destination; a broken output stream stops the session and reports its known
effect on stderr. Inspect an existing destination before retrying with a new
name. Transport failure does not roll back the write or automatically replay a
command. A source pack is for sharing selected evidence; `save`/`restore`
checkpoints remain the format for recovering the entire Desk.

`tests/desk_source_pack_cli.rs` covers retained bytes after live replacement,
mixed complete/observed checkpoint recovery, native paths, exact quotas,
malformed requests, no-overwrite behavior, cancellation and broken output.
Executable verification remains pending the independent strict-RCH lane.

## Retained-reader expression Find

`ReaderSession::search_expression_page` applies the shared bounded expression
syntax to one retained immutable source capture. The primary term supplies the
returned occurrences; additional terms and exclusions determine whether that
capture qualifies. Quoted phrases, overlapping primary matches, exact decoded
UTF-8/UTF-16 ranges and Unicode lowercase comparison use the same public search
engine as workspace expression search.

The arguments are the same as `search_page`: a fresh generation, raw expression,
occurrence ordinal to skip, retained row limit, original-byte scan allowance,
`ReaderFindOptions` and cancellation callback. For example, `"ana" permit -blocked`
selects occurrences of `ana` only when `permit` occurs and `blocked` does not occur
in that same capture. Raw expressions are at most 1024 UTF-8 bytes and 64 parser
tokens. Path, language and type filters return
`READER_SESSION_EXPRESSION_SCOPE_UNSUPPORTED`; a retained reader does not invent
a workspace metadata scope from its display path. The existing literal APIs
and the two fields of `ReaderFindOptions` retain their original meaning.

Qualification scans the complete capture within the byte allowance, even when
`count_all_matches` is false. The response echoes that requested policy and
reports actual completeness separately. `expression_qualified` is true after
complete eligibility, false after complete rejection, and null if the byte
budget or decoding prevents a complete answer. False and null carry no
tentative hits or match counts. Retained row omission remains independent from
scan completeness. Zero-row counting requires `count_all_matches`, as for
literal pages.

Every page replays the retained bytes from zero and counts skipped occurrences
without retaining them. Selection and exact copying remain attached to the
accepted generation and immutable capture; parsing/scanning/cancellation failures
preserve previously accepted hits. The additive C entry point is
`fcb_reader_find_expression_page` in `fcb_reader_sessions.h`, with the existing
ignore-case/count-all flag bits. It performs no live-path read.

Authored production-path regressions are in
`tests/retained_reader_expressions.rs` and the bridge's
`reader_expression_ffi_tests.rs`. They cover late exclusions, budget exhaustion,
malformed text, UTF-16, lowercase expansion witnesses, later occurrence pages,
stale/canceled requests and independent captured sources. Execution remains
pending the repository's independent batch verification lane.

## Matching brackets in retained source

`ReaderSession::matching_delimiter(start, end, canceled)` finds a lexical mate
for one selected `(`, `)`, `[`, `]`, `{` or `}` in the session's complete
immutable capture. `start` and `end` are half-open **original byte offsets**;
UTF-16 brackets occupy two original bytes. The shared capture map rejects
interior scalar or surrogate boundaries. The operation performs no source I/O
and preserves accepted Find, Outline and Occurrences state.

The `fcb.reader-session/1` response uses `command: "matching-delimiter"` and
echoes `selected_range`. `delimiter_state` is `matched`, `unmatched`,
`mismatched`, `not-code` or `not-delimiter`; only `matched` carries a non-null
`matching_range`. Receipts retain the ordinary source identities and declare
`evidence_level: "lexical-delimiter-candidate"`,
`source_context: "complete-capture"` and `semantic_resolution: false`.
FrankenMarkdown supplies token boundaries; comments, strings and template
contents remain opaque. Angle brackets are outside this operation.

Language comes from the existing Rust, Python, JavaScript/TypeScript/JSX/TSX,
Go, C/C++, Swift, Java and C# suffix policy. Original and decoded source each
have a **512 KiB** limit, with at most **1,024** nested delimiters. Malformed
encoding, incomplete literals and unqualified lexical contexts fail explicitly.
These include C++ preprocessing/digraph boundaries, Java Unicode escapes,
known JavaScript regex ambiguities and Swift code slashes where the pinned
lexer cannot distinguish division from regex text. An analysis refusal does
not shorten or invalidate the retained source.

The additive C entry point is `fcb_reader_matching_delimiter(handle, start,
end)` in `fcb_reader_sessions.h`. It uses the existing worker, handle epoch,
cancel and owned-string rules. Regression sources cover core matching, public
reader receipts, C transport ownership and native navigation. Executable
verification remains pending the independent strict-RCH lane.

## Larger source imports and relationship evidence

Captured-project and saved-repository import inventories accept source members
up to **1 MiB of original bytes**. This applies to the existing Rust, JavaScript,
TypeScript, JSX, TSX, Python, C/C++, Go and Java import recognizers. UTF-8 BOMs
and both UTF-16 byte orders preserve exact statement and specifier byte ranges,
including statements beyond the former 128 KiB boundary. Existing decoded
expansion policy remains in place for UTF-16; imports have no physical-line cap.

The shared analysis mapping cache decodes complete source through bounded
windows. Temporary decoded text, token storage and range maps are released
after extraction, and the retained reservation reflects the actual fact and
destination storage. Incoming/outgoing pages use that retained evidence.
Workspace activation copies the captured source without reopening its live
path; saved activation re-verifies the opened archive member and never opens
the corresponding live path.

The 4,096-fact cap, 1,024-byte destination cap, 32 MiB aggregate parse allowance
and caller resource budgets still apply. Go and Cargo metadata remain limited
to 64 KiB per manifest; a saved source plus its simultaneously retained metadata
must fit the 1 MiB combined payload allowance. Limits, unavailable captures, diagnostics
and unresolved targets remain explicit in coverage receipts. These lexical
relationships do not establish compiler resolution.

Regression sources cover late UTF-8/UTF-16 evidence, maximum original and
expanded decoded sources, reverse navigation, canceled replacements and
reader ownership after project/archive detachment. Execution and physical
macOS qualification remain pending in the independent verification lane.

## Follow C/C++ project headers

Live captured-project and saved-repository Imports can follow conventional
angle-header candidates such as `#include <project/api.hpp>`. The closed-catalog
resolver checks exactly the written project-relative path and that path beneath
`include/`. One matching path produces a navigable
`cpp-project-include-candidate`; two matching paths remain ambiguous, with no
priority between them. Incoming pages expose the corresponding captured include
statements as backlinks.

Paths preserve exact bytes and case. The spelling must contain a directory
component or an explicit header suffix (`h`, `hh`, `hpp`, `hxx`, `h++`, `inl`,
`inc`, `ipp`, or `tpp`); no extension is added. Absolute paths, escaping or empty
components, unsupported spellings and capacity overflow are refused before
catalog lookup. Bare standard-library names such as `<vector>` remain external.
Quoted includes retain their source-relative policy, and computed includes
remain unresolved.

Resolver v15 receipts declare
`cpp_angle_include_policy: "project-root-and-include-exact-candidates-v1"`.
The evidence remains lexical, with `compiler_resolved: false` and
`semantic_complete: false`. Compiler include roots, preprocessor conditions,
macros and build flags are outside this finite navigation policy. Native
consumers validate the version, policy, spelling and target-path relationship
before enabling navigation.

Evidence activation keeps the exact original statement range, including UTF-16
source. Target activation uses the selected retained capture or verified archive
member; replacing the live file cannot rebind an accepted relationship to new
bytes. Unavailable and ambiguous targets retain their explicit outcomes.

## Larger source outlines and symbol search

`symbols`, live and captured workspace Symbols, saved-repository Symbols, and
retained-reader Outline share the same declaration recognizers. Each complete
source can now contain up to **1 MiB of original bytes and 1 MiB of decoded
UTF-8**, with up to **65,536 physical lines**. UTF-8 BOMs and both UTF-16 byte
orders retain exact original name and declaration ranges. The 4,096-candidate,
128-level nesting, query, output and aggregate work budgets still apply; these
are heuristic candidates, not exhaustive compiler definitions.

The extractor decodes through bounded shared-source windows and retains sparse
mapping checkpoints. Temporary decoding and lexer allocations are released
before a completed inventory is retained. Larger admission does not raise the
host's managed-memory budget. Sources refused by any analysis limit remain
available to ordinary reading and exact text search.

Regression source covers late declarations beyond the former byte and line
limits, original-byte copying after live replacement, index and query closure,
UTF-16 and BOM coordinates, cancellation and the new source ceiling. Executable
verification remains pending the independent strict-RCH lane.

## C and C++ declaration candidates

File, workspace, saved-repository and retained-reader symbol navigation share
the C/C++ recognizer. Case-insensitive `.c`, `.h`, `.cpp`, `.cc`, `.cxx`
and `.hpp` suffixes select it. Explicit `--language c`, `h`, `cpp`, `cc`,
`cxx` and `hpp` aliases all produce the canonical `cpp` language tag:

```sh
fcb symbols Widget.hpp --name open --json
fcb symbols src --workspace --name open --json
fcb symbols project.fcbs --snapshot --name open --json
fcb symbols captured.txt --language cpp --name open --json
```

The pinned FrankenMarkdown C++ lexer supplies the tokens. The supported subset
includes namespace/type hierarchy, named classes, structs and enums, ordinary
function definitions and prototypes, methods, constructors, destructors and
supported type aliases. Comments, strings, raw strings, function bodies and
variable initializers remain opaque to declaration discovery. An exact
`name_range` selects the identifier separately from the containing declaration;
both ranges map back to the original captured bytes, including UTF-16LE/BE.
Saved lookup uses captured members without reopening the original workspace.

These are `heuristic-outline-candidate` results. Preprocessor directives are not
expanded, conditional branches are not evaluated and names are not compiler
resolved. Complex function pointer declarators, overloaded operators, advanced
constraint forms and complete C/C++ inventories are not claimed. The existing
1 MiB original/decoded-source, 65,536-physical-line, 4,096-item and bounded-nesting
limits remain in force. Line splices, trigraph/digraph spellings, universal-character escapes
outside literals and raw/directive forms the pinned lexer cannot safely delimit
are refused. These cases and malformed delimiters return an explicit symbol
error; workspace and archive coverage stays partial when a source cannot be
analyzed.

Production-path regressions cover file and explicit-language routes, live and
offline workspace lookup, lexical decoys, nested scopes, exact UTF-16 selection,
retained declaration copying and activation after a live file changes. These
tests are authored; independent executable verification remains pending.

## Java and C# declaration candidates

The shared symbol route accepts Java and C# for file, workspace, saved-repository
and retained-desk navigation. Infer them from case-insensitive `.java`/`.cs`
suffixes, or select `--language java` / `--language csharp` (`cs` and `c#` are
aliases). For example:

```sh
fcb symbols Browser.java --name open --json
fcb symbols src --workspace --name open --json
fcb symbols project.fcbs --snapshot --name open --json
fcb symbols captured.txt --language csharp --json
```

Java candidates include type/record/annotation declarations, nested types,
constructors and methods. C# adds namespace declarations and accessor or
expression-bodied properties. Both reuse pinned FrankenMarkdown lexer terminals
and shared original-byte maps; names in BOM-marked UTF-16 remain exact.
Bodies and initializers are opaque, and every candidate keeps the existing
`heuristic-outline-candidate` evidence level. Compiler name binding, local
declarations, conditional compilation evaluation and complete language
inventories are not claimed.

The shared 1 MiB source/decoded-text, 65,536-physical-line, 4,096-item and
128 nesting limits apply. Unsupported pre-token Java Unicode escapes and unsupported C#
escaped-identifier contexts return `SYMBOL_UNSUPPORTED_LEXICAL_CONTEXT`.
Workspace/offline responses retain their partial coverage when a source cannot
be analyzed; no unsupported source becomes a complete empty inventory.

Public-path regressions cover CLI scopes, live/indexed structural search,
UTF-16 evidence, retained C-bridge copies after live source replacement, lexical
contexts, limits, cancellation and stale activation. These tests are authored;
independent executable verification remains pending.

## Retained relationship parcel geometry

Live documentation/import page receipts now include the additive
`geometry_projection: "retained-atlas-parcels-v1"` capability. The header
contains `world` and nullable `filter_parcel`; each retained row contains
`source_parcel` and nullable `target_parcel`. Rectangles use decimal-string
`x`, `y`, `w` and `h` fields alongside the existing canonical identity fields.

Every rectangle comes from that relationship owner's retained workspace atlas,
under the existing source-manifest, layout, index, capture-manifest and
relationship-generation receipt. It is independent of the current display
`view()`. This serialization performs no source I/O, graph traversal or new
layout. A target parcel identifies catalog placement, not captured-source
availability. The selected file's parcel is retained even when its filtered
page has no links.

The native consumer validates the entire geometry capability, consistent
rectangles for repeated file IDs and world bounds before constructing a bounded
direct-neighbor view on the worker. Older list-only receipts remain supported;
a response with incomplete geometry fields is rejected. Existing exact-reader
admission, lexical/document evidence levels and page limits remain unchanged.

## Typed positions in observed large files

`ObservedReaderSession::read_position(line, column, kind, max_bytes,
max_scan_bytes, encoding, canceled)` reaches a one-based physical line and
one-based logical column without retaining the complete file or line. The
public C counterpart is `fcb_observed_reader_position`. Column kinds use the
same shared source mapping as the retained reader: Unicode scalars, UTF-16 code
units, or decoded UTF-8 bytes. Source BOMs and CR/LF/CRLF terminators do not count
as content. A content-end caret is valid; interior scalar or surrogate positions
are refused.

The worker scans the existing opened descriptor from byte zero, keeping bounded
rolling context during that same pass. It never locates against one observation
and rereads current bytes to construct another. At most 256 KiB of page content,
bounded decoder context, 4 GiB of total input and 1,048,576 read calls are admitted.
The shared scanner processes at most 16 KiB per step; scratch is independent of
line length. Limits and cancellation preserve accepted observations while
charging work already spent.

Successful `fcb.observed-reader/1` pages use command `position` and carry
`position_navigation`: requested line/column/kind, exact original offset,
page-relative decoded UTF-8 offset, scan and selected-byte limits, actual scanned
input, payload bytes read and read calls. Scanning can stop counting columns
before the following page context finishes reading, so payload bytes may exceed
scanned input. Before/after descriptor metadata must agree; this remains
non-atomic observed evidence, with `whole_file_immutable:false`.

Retained-window, copy and selection receipts preserve this navigation object
unchanged, including its offset relative to the original accepted page. History
and copy perform no source reads. A page beginning within the scanned target line
can report that line as `first_physical_line`; a prefix containing earlier lines
uses only independently known file-prefix evidence. Missing lines, missing
columns, scalar splits, scan exhaustion and read-call exhaustion remain distinct.
Source, host and C-boundary regression cases are authored; independent executable
verification remains pending.
