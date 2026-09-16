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
fcb search payload.bin --raw-hex 00ff --json
fcb read huge.rs --offset 4294967333 --bytes 32768 --encoding utf8 --json
fcb search huge.rs --offset 4294967333 --bytes 32768 --encoding utf8 --text needle --json
printf 'banana' | fcb search --stdin --text ana --json
fcb read --json -- --filename-starting-with-a-dash

fcb inspect /path/to/repository --workspace --json
fcb search /path/to/repository --workspace --text 'pub fn' --json
fcb search /path/to/repository --workspace --path 'src/lib' --json
fcb search /path/to/repository --workspace --text needle --max-files 8192 --max-total-bytes 67108864 --json

fcb search huge.rs --whole-file --text needle --json
fcb search huge.rs --whole-file --text needle --max-scan-bytes 4294967296 --json
fcb search payload.bin --whole-file --raw-hex 00ff --json
fcb search /path/to/repository --workspace --whole-file --text 'pub fn' --json
fcb search /path/to/repository --workspace --whole-file --raw-hex 00ff --json
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

Text matching is exact and case-sensitive. A literal is prepared in UTF-8 and
UTF-16LE/BE; decoder validation, code-unit alignment and actual leading-BOM
handling make encoded matching equivalent to these exact text semantics.
Literal matches may cross any number of short reads or buffer boundaries, even
when the literal is longer than the 16 KiB input buffer. Internal U+FEFF scalars
remain content. Normalization, case folding and regex are not silently applied.
`--encoding` can declare `utf8`, `utf16le`, or `utf16be`; automatic detection uses
the beginning of the SAME continuous observation. `--raw-hex` searches arbitrary
original bytes and cannot be combined with an encoding override.

The operation's allocations depend on fixed input/decoder scratch, literal
length and result capacity, not the source file's length. The input buffer is
16 KiB; decoder scratch and KMP state are separately admitted, so **16 KiB is not
a claim about total process memory**. Each step performs at most one successful
read, 32 read attempts including interruptions, and one batch of at most 64
candidate matches. Both source processing and retained output are bounded.
Blocking OS calls do not acquire a fabricated wall-clock deadline.

`--max-scan-bytes` controls original-source I/O independently from capture
capacity. Its default is 256 MiB and its maximum selectable allowance is 1 TiB.
The command also has a fixed limit of 16,777,216 read attempts. These are
independent limits: choosing the maximum byte allowance does not bypass the
read-call limit. In a workspace, byte, read-call and stored-hit budgets are
**global**, not reset per file; failed reads still spend their work allowance.
A limit stops with explicit partial coverage and an unexamined-file count.

`--whole-file` requires named files and `search`; it does not accept stdin,
window offsets/byte counts, path queries, `--max-file-bytes` or
`--max-total-bytes`. The latter are full-capture reservations for the default
indexed route. Workspace discovery limits and static exclusions still apply.
Use `--max-files` and `--include-excluded` with an explicit workspace as before.

### Retention and consistency

Streaming results retain **literal witnesses only**, not arbitrary discarded
source ranges. The matched original bytes are proven equal to the encoded
literal and share one immutable backing value rather than being copied for
every occurrence. The public `FileSearchReport::retain_hit` can materialize an
exact `ObservedExtent` for a selected occurrence without rereading a changed
live file. It never synthesizes neighboring lines, a file digest, or a full
snapshot. Reading neighboring source is a new observation with a new revision.

Before/after metadata is taken on the actual open file. Length and modification
time differences, short reads and missing metadata prevent a complete native
result. An unchanged-metadata result is still **not an atomic snapshot**. A
pathname replacement does not cause a mid-search reopen, and positioned reads
do not move a cursor shared with the host's other file handle.

`input_complete` means the declared continuous byte sequence was searched.
`whole_file_complete` additionally requires unchanged before/after metadata.
`state` distinguishes completion, match limit, byte limit, read-call limit,
short read, unsupported text, cancellation and failure. Encountered malformed
text invalidates that file's exact-text results; raw-byte search remains an
explicit alternative. Other completed files' results remain useful.

Whole-file JSON uses `strategy: "streaming-whole-file"`. Each file record carries
exact original-byte hit ranges, counters, consistency and terminal state.
`literal_original_hex` supplies the shared witness once when hits exist.
`decoded_range` is null: the scanner does not invent global decoded coordinates.
Workspace output has `files`, `files_examined`, `unexamined_files`,
`incomplete_files`, `workspace_complete`, global counts and `stop_reason`.
An exactly full result buffer is not truncation; one extra verified occurrence
establishes truncation. A complete zero-match scan exits 1; incomplete scans exit
3 even with zero hits. The 8 MiB response cap remains independent of scan size.

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
**case-sensitive literal**, including spaces and strings resembling query
syntax. Each file uses the existing UTF-8/BOM-marked UTF-16 decoder. Unsupported
exact decoding is reported separately, not as a successful no-match result.

The workspace default policy is explicitly named
`product-defaults/no-rule-files-v1`: common build/dependency/VCS directories and
selected binary extensions are excluded by the existing product matcher.
`--include-excluded` switches to `all-regular-files/no-rule-files-v1`. Symlinks
are not followed and special objects are not read. Exclusion counts are counts
of observed excluded entries; an excluded directory may contain an unknown
number of descendants.

**Nested `.gitignore` and `.fcbignore` files are not loaded as configuration in
this bounded lane.** Those files may still be ordinary source members searched
for literal text. The metadata walker never reads them while determining
membership, so repository-provided rule files cannot cause hidden source reads
or an unbounded growing rule set. This is not a claim of Git ignore compatibility
for the workspace CLI.

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
matches actually seen, not an exhaustive total. An actual leading BOM can be
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
