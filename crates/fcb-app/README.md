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
```

By default, the scope is **one explicitly selected file**, not its siblings or
an implicitly discovered repository. Its parent path is reported for context
but is not a recursively authorized workspace. A directory requires explicit
`--workspace` on `inspect` or `search`. Selecting stdin, a single file, or a
metadata diagnostic never silently widens that authority.

Persistent index/cache commands, trail export, replay, Markdown preview, regex,
and the native launcher remain unavailable. This CLI lane does not claim
completion of all FCB-057 acceptance criteria.

## Workspace inspection and search

`inspect ROOT --workspace` publishes a sorted catalog of regular files and
metadata counts. `search ROOT --workspace --path QUERY` uses the existing
native-path component/fuzzy index, with Unicode-lowercase matching and distinct
raw identities. Both operations read **zero source payload bytes**. Non-UTF8
names, case-distinct names, and literal Unix backslashes/colons retain their
native bytes; escaped display labels are not source identities.

`search ROOT --workspace --text TEXT` captures admitted complete files, forms a
search manifest, builds the existing ephemeral trigram index, and verifies
candidate hits against retained exact source. Text is a **case-sensitive literal**,
including spaces and strings resembling query syntax. Each file uses the
existing UTF-8/BOM-marked UTF-16 decoder. Unsupported exact decoding is reported
separately, not as a successful no-match result. No new regex/parser/matcher is
used by this route.

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

Workspace admission defaults and CLI maximums:

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
the operation is also capped at 131,072 read calls. A file that is too large,
changed during reading, unavailable, or refused by a capture quota stays an
**unavailable member**. It is not truncated into a complete capture or omitted
from the meaning of workspace search. Captured old bytes remain authoritative
for index verification even if the live file changes later.

Workspace output distinguishes:

- `discovery_complete`: the admitted regular-file membership was fully observed
  under the reported policy and without discovery limitations.
- `workspace_complete`: the query's declared scope was fully examined, without
  missing captures/unsupported text in content mode. For path mode this describes
  the completed scan/count; display truncation remains a separate fact.
- `truncated` or `listing_truncated`: a bounded result/listing omitted rows.

Content output includes captured-byte/read-call totals, index-eliminated files,
fallback scans, exact source ranges, and `unavailable_files` /
`unsupported_text_files` records with reversible paths, file IDs and reason
codes. Declared admission limits and discovery counters are included. An empty
workspace may be complete; an unfinished workspace with zero hits may not.
No whole-repository atomic snapshot or persistent identifier is inferred from
ordinary live directory/file observations.

Workspace operations do not accept stdin, byte-window flags, raw-hex queries or
an encoding override. Use the single-file route for bounded raw-byte access,
far offsets, and explicit encoding declarations. This ephemeral implementation
is bounded in-memory search, **not out-of-core whole-repository indexing**.

## File windows and stdin

Default visible size is 65,536 original source bytes; the maximum is 262,144.
Capturing a file range can include up to eight context bytes on either side.
Those bytes are counted in `payload_bytes_read` but not silently added to the
searched scope. Each positioned read step uses at most 64 KiB and 32 calls;
single-file operations also cap the operation at 4,096 read calls.

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
| 3 | Useful partial coverage or a truncated result/listing. Never an exhaustive negative. |
| 130 | Cooperative cancellation through the application API. |

File search keeps `scope_complete`, `whole_file_complete`, and `truncated`
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
capture rechecks each named component and does not intentionally follow
symlinks, but concurrent ancestor replacement remains a native-service
limitation. The public library's root grant tracks scope/revocation; it is not a
kernel sandbox. A descriptor-relative native service and physical-Mac evidence
are still required for stronger confinement claims. Reading an already-opened
file does not resolve its pathname again after replacement.

## Verification surfaces

Parser/output tests, single-file service/process tests, public workspace
capture/index tests and real workspace CLI process tests are provided. Fixtures
cover mixed UTF-8/UTF-16, original ranges, raw/control/backslash names, sparse
files beyond 4 GiB, scope expansion, omissions, result limits, cancellation,
revocation and old-capture retention. A test-only independent JSON subset parser
rejects duplicate fields, multiple documents, truncated output and numeric IDs.
These test sources do not establish execution or native qualification.

The independent verifier should run these selections using the repository's
strict-RCH lane, not local worker builds:

```text
cargo test --locked -p fcb-source --lib
cargo test --locked -p fcb-source --test metadata_discovery
cargo test --locked -p fcb-search --test ephemeral_index
cargo test --locked -p fcb-search --test path_navigation
cargo test --locked -p fcb --features search --test workspace_search
cargo test --locked -p fcb --features search --test workspace_capture_commit
cargo test --locked -p fcb-app --lib
cargo test --locked -p fcb-app --test headless_services
cargo test --locked -p fcb-app --test standalone_cli
cargo test --locked -p fcb-app --test workspace_cli
cargo test --locked -p fcb-app --test workspace_native_names
```

No compiled binary, execution receipt, signed artifact, native qualification or
product gate closure is asserted by the presence of this implementation.
