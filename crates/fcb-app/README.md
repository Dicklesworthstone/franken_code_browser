# `fcb` headless file tools

`fcb-app` produces the standalone binary **`fcb`**. It composes the public `fcb`
source, reader, and exact-search APIs; it does not contain another decoder or
matcher. The original public library keeps its inert default profile. The app
selects only its `search` profile and std/core dependencies.

**Implementation status: code-first, independent RCH verification pending.**
There is no native AppKit/Metal GUI in this binary. `fcb`, `fcb FILE`, and human
`fcb open FILE` reserve that launcher route and return an explicit unavailable
error. `fcb --json` reports capabilities and never tries to launch a window.
`doctor` is a **static capability report**, not a benchmark, repair operation,
source scan, or claim that product gates passed.

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
```

The source scope is **one explicitly selected file**, not its siblings or an
implicitly discovered repository. Its parent path is reported for context but
is not treated as a recursively authorized workspace. A directory is refused.
Workspace CLI search, persistent index/cache commands, trail export, replay,
Markdown preview, regex, and the native launcher remain unavailable. This first
CLI lane does not claim completion of all FCB-057 acceptance criteria.

`read` is an explicitly headless human command. Human text preserves line feeds
and tabs but escapes terminal controls and directional controls. This is a safe
logical display, not an original-byte export. JSON carries the actual logical
text and a separate hex representation of the visible original source bytes.
Search output includes exact original byte ranges, not duplicated per-hit source
snippets or a reconstructed file.

## Bounds and semantics

Default visible size is 65,536 original source bytes; the maximum is 262,144.
Capturing a file range can include up to eight context bytes on either side.
Those context bytes are counted in `payload_bytes_read` but are not silently
added to the searched scope. Each positioned read step uses at most 64 KiB and
32 calls; the application additionally caps the operation at 4,096 read calls.
All managed allocations share a 256 MiB admission ledger. This is not a total
process/RSS limit: OS-owned argv, allocator metadata and standard-library/OS
internals are not measured as application buffers.

The default result limit is 100, with a maximum of 4,096. Reaching exactly that
many hits is not sufficient proof of truncation. The existing matcher scans for
one extra hit before returning `truncated: true`. `matches_seen` is a lower bound
when truncated, not an exhaustive count of the rest of the source.

For stdin, the byte limit bounds the **whole supplied observation**. The reader
consumes at most one extra byte to distinguish EOF from oversized input. Input
that does not reach EOF within the limit returns an error; a prefix is not
mislabelled as the whole stream. Stdin can be a pipe and can block inside a read.
The command must be explicitly selected; capabilities/help never consume stdin.

Encoding is auto-detected from the beginning of the SAME captured observation.
Supported declarations are `utf8`, `utf16le`, and `utf16be`. A nonzero text offset
requires a declaration; the app does not perform a second live header read and
pretend it belongs to the selected capture. Scalar/CRLF boundaries may shorten
or align a visible window. Original offsets remain absolute; decoded offsets
are **window-local UTF-8 bytes**, not UTF-16 units, graphemes or visual columns.
Missing global line context is `null`, not an invented line number. Interior BOM
characters remain content; an actual leading BOM is omitted from logical text.

Malformed source can be read with labelled replacements and exact original hex.
Exact text search refuses replacement windows rather than returning a false
negative. `--raw-hex` remains an exact original-byte search. Separate invocations
observe separate source sequences; adjacent windows do not establish an atomic
whole-file snapshot or cross-window matches.

## Machine schema and delivery

`--json` emits exactly one document with `schema: "fcb.cli/1"`. It is not NDJSON.
All integer-valued fields, including IDs, lengths, offsets, counts and line
numbers, are canonical unsigned **decimal strings**. Booleans are JSON booleans;
unknown values are JSON null. There are no lossy floating-point ID conversions.
IDs are explicitly `response-local`: do not join observations across invocations
using their integer values. Paths are tagged `unix-bytes` and include reversible
lowercase `hex` plus a secondary escaped `display`. Neither display strings nor
printed paths confer native access permission.

Successful command execution has `status: "ok"`, including useful partial
results. Consult the explicit coverage fields and the exit code:

| Exit | Meaning |
| --- | --- |
| 0 | The admitted whole-file representation completed, or an inert/inspection command succeeded. |
| 1 | A complete whole-file search found no matches. |
| 2 | Argument, source, decoder, admission, unavailable-capability or output error. |
| 3 | Useful partial source coverage or a truncated result set. Never an exhaustive negative. |
| 130 | Cooperative cancellation through the application API. |

Search distinguishes `scope_complete`, `whole_file_complete`, and `truncated`.
The text representation can be whole-file-complete while excluding an actual
leading BOM; it is still paired with original-byte ranges. A fully searched
window inside a larger file is not whole-file-complete, even when it has no
matches. Changed/short/unknown metadata observations cannot acquire a stronger
completeness claim merely because the byte matcher finished.

Errors are a single document containing `status: "error"`, `complete: false`,
and `{code, subsystem, message, retryable, next_action}`. Diagnostics do not echo
source payloads, failed raw paths, or query strings. Selected source/path data in
successful responses is intentional user-requested output, not a debug log.

The response is fully encoded in a leased buffer before any stdout write, with
an 8 MiB cap. Encoding failure discards the private candidate and emits an error
instead. Writes are at most 16 KiB each and at most 4,096 calls, including retries.
A blocked OS read/write has **no fabricated wall-clock deadline**. Backpressure
and shutdown are handled on the standalone command's thread, never in a shared
publisher or native interaction callback. A broken pipe or canceled delivery
can leave incomplete bytes on stdout: the command exits nonzero and emits only
a redacted stderr diagnostic, never a second JSON object appended to the prefix.
There is no streaming subscription protocol or custom process signal handler.

## Named-file admission boundary

Named-file commands currently support macOS and Linux x86-64/AArch64. Other
ABIs return `CLI_NATIVE_FILE_UNSUPPORTED`; bounded stdin remains portable.
The app uses safe `std::fs::OpenOptions` with read-only, no-follow-final-component,
nonblocking and no-controlling-terminal flags, then checks the actual opened
object and device/inode against the admission observation. Existing directories,
symlinks and special objects are refused before payload reads. No source write,
implicit directory scan, interpreter, shell command or executable from the
source tree is launched.

The numeric flags are the target ABI's O_NOFOLLOW, O_NONBLOCK and O_NOCTTY values.
They were checked against these system header definitions (16 September 2026):

```text
https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/sys/fcntl.h
https://raw.githubusercontent.com/torvalds/linux/master/include/uapi/asm-generic/fcntl.h
https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html
```

These safe-std flags and metadata comparisons are **not** qualified
race-safe ancestor confinement or an atomic filesystem snapshot. The parent
context does not turn into a root grant. A later native sandbox/root service must
supply its descriptor-relative policy and physical-Mac evidence separately.
Reading an already-open file after namespace replacement does not re-resolve its
path to a different source.

## Verification surfaces

The implementation includes parser/output unit tests, `headless_services`
in-process public-application tests, and `standalone_cli` process tests. The latter
invoke Cargo's actual `fcb` binary on real UTF-16 files, raw/control-character
filenames, regular-file admission failures, sparse offsets above 4 GiB, piped
stdin, and a copied executable outside the checkout. The independent test-only
JSON subset parser rejects duplicate fields, multiple documents, truncation and
unquoted numeric IDs. Tests do not count native presentation or macOS confinement
as verified by portable logic.

The independent verifier should execute the following Cargo selections through
the repository's required strict-RCH wrapper, not as local worker builds:

```text
cargo test --locked -p fcb-app --lib
cargo test --locked -p fcb-app --test headless_services
cargo test --locked -p fcb-app --test standalone_cli
```

No execution receipt, native qualification, signed artifact, or product gate
closure is asserted by the presence of this source and these tests.
