# `fcb` headless source tools

`fcb-app` produces the standalone binary **`fcb`**. It composes the public `fcb`
source, reader, path-index and exact-search APIs. There is no second decoder,
matcher, directory walker or search index in the app. The public library keeps
its inert default profile. The app selects only the `search` profile and
std/core dependencies.

**Implementation status: code-first, independent RCH verification pending.**
There is no native AppKit/Metal GUI in this binary. `fcb`, `fcb FILE`, and human
`fcb open FILE` reserve that launcher route and return an explicit unavailable
error. `fcb --json` reports capabilities and never launches a window. `doctor`
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

Persistent index/cache commands, trail export, replay, Markdown preview, regex,
and the native launcher remain unavailable. This CLI lane does not claim
completion of all FCB-057 acceptance criteria.

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

### Retention and consistency

Streaming results retain **literal witnesses only**, not arbitrary discarded
source ranges. For expressions, these are primary-occurrence witnesses;
predicate evaluation does not retain all required/excluded-term source spans. Exact text and raw-byte matches share one immutable encoded
literal. Unicode lowercase matches retain each occurrence's original spelling
and encoded bytes, because the source may differ from the query in both
spelling and byte length. The public `FileSearchReport::retain_hit` can
materialize an exact `ObservedExtent` for a selected occurrence without
rereading a changed live file. It never synthesizes neighboring lines, a file digest, or a full
snapshot. Reading neighboring source is a new observation with a new revision.

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
membership. `--respect-ignores` and `--include-excluded` are mutually exclusive.
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
the other scans continuous source while retaining only literal witnesses.

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

All managed allocations share a 256 MiB admission ledger, including conservative
workspace metadata/capture reservations and old/new overlap. This is not a total
process/RSS limit; OS internals, argv and allocator overhead require headroom.

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

The projection reuses the shared Rust, JavaScript/TypeScript/JSX/TSX and Python
extractor and closed-manifest resolver. Each occurrence retains its original
statement and specifier byte ranges, source digest, language, extraction policy
and explicit resolution outcome. Python relative imports consider the finite
`.py` and `/__init__.py` candidates; absolute Python imports remain external.
These are lexical relationships with `compiler_resolved: false` and
`semantic_complete: false`. They do not inspect a runtime import path, execute
project code, install dependencies, or fetch external packages.

Preparation verifies at most one eligible source at a time. `MAX_LINKS` is
1..4096, aggregate source allowance is 0..33554432 bytes, and a source larger
than 131072 bytes is explicitly omitted. No source payload remains in the
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

All name matching is case-sensitive; the default is exact. Symbols are bounded
heuristic declaration candidates. Headings come from FrankenMarkdown with
qualified original-byte ranges. Occurrences are exact whole-token text
matches, including comments and string literals; prefix/contains and
compiler-reference claims are unavailable in that mode. UTF-8 and supported
BOM-marked UTF-16 source retain original-byte coordinates.

The scope is the whole saved archive. Member names remain reversible labels;
the command grants no live-root access and never reads current workspace
source. Results include the archive and source digests, member identity, exact
selected and evidence ranges, candidate kind/language, and heading slugs where
applicable. Use the existing `snapshot read` command with the reported member
and byte offset to inspect source.

`--limit` accepts 0–4096 (default 100); zero probes existence without retaining
a row. `--max-source-bytes` accepts 0–67,108,864 bytes (default 64 MiB) across
all loaded members. Per-member extraction limits remain 64 KiB for symbols,
256 KiB for headings, and 512 KiB for occurrences. Source, extraction and
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
