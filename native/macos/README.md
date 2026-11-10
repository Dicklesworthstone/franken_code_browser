# Native macOS app

This directory contains the SwiftUI/Metal FrankenCodeBrowser app and the narrow `franken-macos` Rust facade. It is part of the **same `franken_code_browser` repository** as the engine. The app links the workspace's `fcb-bridge` static library; no second checkout is needed.

From the repository root on macOS 14+:

```sh
./scripts/install_macos_app.sh --build
```

To inspect a built app before installation, run `APP="$(./scripts/build_macos_app.sh)"`,
then `open "$APP"` and `./scripts/install_macos_app.sh "$APP"` if desired.

## CLI to native project workflow

Direct-distribution builds now include the matching `fcb` executable at
`Contents/MacOS/fcb`. The build script builds the bridge and CLI together, signs
the nested executable before sealing the bundle, and the installer preserves it.
No shell profile, PATH directory, or existing command is changed automatically.

```sh
APP="$(./scripts/build_macos_app.sh)"
"$APP/Contents/MacOS/fcb" open /absolute/path/to/project
"$APP/Contents/MacOS/fcb" .
"$APP/Contents/MacOS/fcb" read README.md --json
```

On macOS, bare `fcb` launches/activates a compatible app. `fcb ROOT` and
`fcb open ROOT` start a new native instance with that explicit project directory.
Relative roots are resolved against the invoking directory. The existing native
`ContentView` consumes the per-launch `fcb.root` argument-domain override; the
launcher does not rewrite the preferences database or execute anything from the
project. Search, catalog discovery and reader admission remain native engine work.

A separately installed CLI locates the app using `FCB_APP_BUNDLE` (an absolute
`.app` path), its own enclosing bundle, `~/Applications/FrankenCodeBrowser.app`,
then `/Applications/FrankenCodeBrowser.app`. An invalid explicit override or
invalid enclosing bundle is an error, never permission to launch another copy.
For a local build in a nonstandard location:

```sh
FCB_APP_BUNDLE="$APP" fcb open /absolute/path/to/project
```

The launcher requires a versioned marker created by the current bundler. Older
bundles must be rebuilt. App Store builds advertise native-picker access, not
command-line root access, and omit the unsandboxed CLI from the app. Supplying a
root to that mode is refused: launch the app without a root and use its folder
picker/security-scoped access flow.

`--json`, `read`, `search`, `desk` and other machine commands retain their headless
routes. The reusable `fcb_app::run` service does not start native applications;
this handoff belongs only to the macOS executable. Other platforms remain
headless. Non-UTF-8 project and filename arguments are refused by the UTF-8
native startup path rather than being lossily rewritten.

### Opening a source file

```sh
"$APP/Contents/MacOS/fcb" open /absolute/path/to/src/main.rs
"$APP/Contents/MacOS/fcb" open -- -notes.rs
```

A regular-file request opens its **immediate parent directory** as the project
and selects that filename once the metadata catalog is ready. It does not walk
ancestors looking for a repository root, prepare all source previews, or execute
anything from the project. Parent-directory metadata discovery is part of this
native operation; use `read FILE` or `open FILE --json` for a file-only headless
read without that discovery.

The filename is passed as a bounded hex encoding of its original UTF-8 bytes.
The native endpoint accepts it only against the requesting worker's matching
root and the exact raw filename in that catalog, then invokes the same checked
reader action as a row click. Quoting, spaces, tabs, newlines and native
backslashes do not turn into command syntax. Unicode-equivalent but byte-distinct
filenames are not interchanged. A filename is a request for a **new capture**,
not an exact anchor into bytes observed by the launcher.

Discovery exclusions, catalog limits and normal reader admission still apply.
An excluded, filtered or uncatalogued file produces a visible limitation rather
than opening another row or declaring an unseen file absent. A known file in a
partial catalog remains openable. The first catalog presentation consumes the
startup request, including failure; project replacement or refresh cannot replay
it. It is never persisted as a filename preference.

File requests require the additional `fcb-launch-file-v1` bundle marker. Rebuild
older bundles; the launcher refuses a file request instead of silently dropping
its selection. Project-only older bundles retain their directory-launch behavior.
App Store builds do not advertise this marker and independently refuse argv file
activation: use the native folder picker and catalog there.

The Foundation-only endpoint regressions are in
`tests/AtlasStartupFileRegressionTests.swift`, compiled together with
`swiftui/AtlasProjectCatalog.swift` in the configured qualification lane. Rust
launcher tests remain in `crates/fcb-app/src/native_launch.rs`. Neither these
unexecuted suites nor portable packaging checks qualify native file activation
on a physical Mac.

`CLI_NATIVE_LAUNCH_SUBMITTED` means Launch Services accepted the handoff, **not**
that project discovery finished or a frame was presented. A canceled/timed-out
handoff can already have launched a window; inspect it before retrying. The
launcher reaps only its own `open` helper, never that window. Native compilation,
physical-Mac launch tests and product-gate qualification remain pending.

The portable packaging contract checks can be run with:

```sh
python3 scripts/test_native_launch_packaging.py
```

They execute the actual packaging scripts with fake compiler/signing tools and
retain their fixture logs. They establish packaging control flow, not native
compilation, code-signing validity, Launch Services behavior or rendering.

## Workspace text search

Text search starts with **Match case** enabled. Turn it off to compare Unicode
lowercase forms across both live captures and a retained workspace index. For
example, `needle` can find `NEEDLE`, and `k` can find the Kelvin sign `K`. This is
literal lowercase comparison: it does not add regular expressions, accent
folding, Unicode normalization, or full case folding (`ss` does not match `ß`).
Symbol and Markdown-heading name search keep their existing matching controls.

Changing the case choice starts a new query and rejects responses from the
superseded choice. A retained index can serve either choice without recapturing
its sources. When its exact-case index cannot prove a negative for the selected
mode, the shared engine scans the retained source within the verification
budget. Limits and unavailable files still produce explicitly partial results.

Each text result retains the original matched spelling and byte range. Opening
a result uses that spelling to verify the exact location in the same captured
source, including UTF-16 and files changed on disk since capture. Copying a hit
preserves its original bytes. Workspace queries remain limited to 1024 UTF-8
bytes; the retained reader accepts up to 4096 bytes so that a short lowercase
query can activate a longer original Unicode spelling.

Search rows also show bounded source context from the same retained capture,
with the exact matched span emphasized. This applies to live and captured-index
text search and to symbol and Markdown-heading results. Compact rows mark line
breaks with `↵`, tabs with `⇥`, and omitted context with `…`; they retain the
original source spelling and byte ranges for navigation. Excerpts use at most
8192 original bytes and 8192 decoded UTF-8 bytes. An unavailable or unrepresentable
excerpt leaves the exact result usable, and no excerpt rereads the live path.
Native accumulation preserves the existing 8 MiB allowance for mandatory hit
witnesses and admits up to 2 MiB of optional excerpt text and prepared fragments.
Excerpts that do not fit are omitted while the exact hits remain available;
these retained-text limits do not describe total or peak process memory.

The host, actual C-boundary, and native transport regression suites cover both
case choices, retained-index reuse, stale responses, spelling validation,
capture replacement, and Unicode source navigation. These are code-first
additions; executable Rust/Swift and physical-Mac qualification remain pending.

## Searching large files across a project

Choose **Search large project files…** in the search sidebar to search exact,
case-sensitive text across all files in a separately retained project catalog.
This route streams file contents through the shared Rust matcher, including
files beyond the ordinary search's 1 MiB per-file and 32 MiB total capture limits.
It recognizes UTF-8 and BOM-marked UTF-16 in either byte order. Queries contain
1–1024 UTF-8 bytes; overlapping matches keep their exact original byte ranges.

Open the sheet, enter the literal, choose **256 MiB**, **1 GiB**, or **4 GiB** as
the total source I/O budget, and press **Search**. There is no per-file capture
ceiling. The scan admits at most 1,048,576 read calls and retains at most 256
matches. It uses a 16 KiB input buffer and stores one encoded literal witness
per matching file instead of retaining complete sources. Results appear when
the bounded scan reaches a terminal outcome; **Cancel** and replacement searches
revoke obsolete deliveries while entered work drains on the worker.

The catalog admits at most 20,000 regular files, subject to separate path and
discovery-page limits and the product's exclusions. Search covers this catalog
independently of map zoom, display filters and the ordinary search's location
scope. **Refresh file catalog** discovers current membership again, including
new files. Existing queries never turn newly created or uncatalogued files into
proven no-match results. The summary separately reports incomplete discovery,
unexamined files, unsupported text, unavailable files, failed reads, and byte,
call or match-limit stops. Excluded entries include pruned directories; their
count is not an invented count of all files beneath them.

Select a result to inspect its retained literal. **Copy match** copies that
decoded spelling; **Copy original hex** copies the exact original encoded bytes.
These actions do no source I/O and remain meaningful if the working-tree file
changes. **Read current context** explicitly opens the current canonical pathname
under the current project access grant, creates an independent observed reader,
and checks the literal's original bytes before highlighting. Changed bytes are
reported as changed; the earlier result never claims the new file's identity.
Changing the query, refreshing the catalog, replacing the project or closing the
sheet closes this child reader and revokes its actions synchronously.

The safe host service is `fcb_app::host::atlas_stream_search`; native hosts use
[`fcb_atlas_stream_search.h`](../../crates/fcb-bridge/include/fcb_atlas_stream_search.h)
with existing atlas handles. The public `AtlasStreamSearch` accepts both borrowed
and retained atlas metadata without rebuilding geometry. Added core, host, C ABI,
native receipt, cancellation/lifetime and context-navigation cases cover this
integration. They are code-first additions: executable Rust/Swift verification
and physical-Mac qualification remain pending.

## Finding text in one large file

The **Observed Range Reader** can search a file without capturing its complete
contents. Open it from the current source or Quick Open, choose UTF-8, UTF-16 LE,
or UTF-16 BE, and enter an exact, case-sensitive literal. Search uses the same
opened file object as byte navigation, including after that pathname is replaced.
Reopen explicitly to observe the replacement file.

Each search starts at byte zero and reads through a 16 KiB streaming buffer.
The selected scan limit bounds source I/O; the default is 256 MiB and the maximum
is 4 GiB. At most 256 hits and a 4096-byte UTF-8 query are admitted. Overlapping
matches and matches split across read buffers retain exact original byte ranges,
including UTF-16 surrogate pairs. The result reports actual bytes scanned and
read, and distinguishes reaching the observed end from stopping at a byte,
read-call, result, or encoding limit. A completed scan is an observation of a
mutable file, never an atomic snapshot.

Selecting a result retrieves only its retained matched text and original bytes;
it does not reread the file. Copy preserves that matched spelling. **Read current
context** performs a separate bounded read with a fresh observation identity and
highlights the match only after its retained bytes agree with the new source
bytes. If the file changed, the original match remains identifiable and the new
context is labeled separately. Search does not invent surrounding text for an
old match or treat unchecked source as a proven negative.

**Go to verified match** reads fresh context and asks the shared source adapter
to verify the retained match before accepting the page. It checks original bytes,
the decoded literal and the exact source-to-text range together. A changed or
unavailable match leaves the accepted reader page intact. Verified selections
remain attached to retained pages, so Back/Forward can restore the same highlight
without source I/O even after another query replaces the active search. This
action also handles valid matches beside malformed neighboring text through the
shared decoder's source map. **Read current context** remains available when the
intent is to inspect what changed.

Press **Command-F** to focus Find. **Previous match** and **Next match** select
retained results without rereading source; choose the context action explicitly.
Restarting a valid Find revokes the previous result actions until a replacement
reply is accepted. If cancellation races with worker acceptance, the old report
stays readable and its controls remain disabled until another Find succeeds.

Editing the query or encoding revokes old targets, cancellation leaves the reader
usable, and stale worker replies cannot activate a different query. Searching
does not evict the reader's eight retained byte observations. The backend, C ABI,
and native model regression suites cover these contracts; executable Rust/Swift
and physical-Mac qualification remain pending.

## Line navigation in large files

The **Observed Range Reader** also accepts a one-based **Line** and a **Lines**
count. Choose a scan allowance and use **Go to lines** to read a physical line
window even when the file exceeds the complete reader's 32 MiB capture limit.
The range allowance bounds the selected source bytes; the scan allowance
separately bounds the prefix that must be examined to find those lines.

The operation starts at byte zero of the same already-opened file descriptor,
using the selected UTF-8, UTF-16 LE, or UTF-16 BE decoding. It retains the target
bytes during that same forward scan. It does not locate a line and then reread
possibly changed bytes, or reuse an old line index against a mutable file.
CR, LF, and CRLF are physical line endings; a final terminator does not create
an extra line. Original terminators remain available through exact hex copy.

Successful pages carry their observed physical line numbers and retain that
provenance through Back/Forward without file I/O. If EOF occurs partway through
the requested line count, the available final lines are returned. An absent
first line, an exhausted prefix-scan allowance, an oversized selected window,
a read-call limit, cancellation, and a detected source change have distinct
outcomes. They preserve the accepted page; a limit never becomes a claim that
the requested line is missing. Reduce **Lines** or increase the relevant
allowance when a selected window does not fit. Byte navigation remains useful
for a single line larger than the maximum retained window.

Each request admits at most 4,096 lines, 256 KiB of selected source plus bounded
decoding context, 4 GiB of scan input, and 1,048,576 read calls. The scan keeps a
16 KiB input buffer and bounded selected bytes, without a whole-file buffer or
one stored offset per line. Before/after metadata checks apply to the opened
descriptor, including after a pathname replacement; they do not establish an
atomic filesystem snapshot. **Reopen current file** explicitly follows the
current pathname.

Rust host, C-boundary, native transport, and reader-model regression cases
cover same-scan retention, UTF-16 and CRLF boundaries, large prefixes, retained
history, limits, stale replies, and cancellation. These are code-first changes;
executable Rust/Swift and physical-Mac qualification remain pending.

## Source reader display

The source reader's **Display** menu provides independent **Line numbers**,
**Visible whitespace**, and **Wrap lines** controls. Line numbers are on by default.
Wrapping changes visual rows; the gutter continues to identify physical source
lines, treating CR, LF, and CRLF as source line endings. Whitespace markers use
AppKit's display support without inserting characters into the captured text.

Paged reading uses the engine's qualified starting line when available. For a
byte or search-hit page whose absolute line is unknown, the gutter uses explicit
page-relative labels (`+1`, `+2`, …) and the reader identifies them as **Page
lines (+)**. It does not present those labels as file coordinates. Display
changes preserve source identity, manual selection, and the viewport anchor;
ordinary text copy and the separate original-byte hex actions keep their
existing semantics.

These controls apply within the source reader's existing admission bounds,
including retained and pinned pages. Native layout, scrolling, and copy
regressions are included for the configured macOS verification lane;
physical-Mac execution remains pending.

## Exact source locations

The paged reader's **Line**, **Column**, and column-unit controls resolve a
one-based location in the retained source. Choose **Unicode scalar**,
**UTF-16 code unit**, or **Decoded UTF-8 byte**, then use **Go location**. These
are logical columns: tabs count as one unit, and columns do not describe visual
width or bidirectional display order. A leading encoding BOM and CR/LF/CRLF
terminators are excluded. The caret after the last content character is valid;
a trailing line terminator does not create another physical line.

The shared Rust decoder resolves the position in bounded chunks and returns a
small page around it, including columns far beyond the first 64 KiB of a giant
line. Invalid columns and offsets inside a UTF-8 scalar or UTF-16 surrogate pair
are rejected. Failed or canceled navigation preserves the accepted page. Back,
Forward, and pinning retain the exact location and capture identity; editing the
location during a pending request revokes that request.

The position label reports the requested coordinate unit and exact original
byte offset. When AppKit cannot represent an exact logical position, such as a
boundary inside a composed character, the reader retains the logical location
and reports that the native caret is unavailable. It does not round the target.
Source, bridge, and native regression cases are included; executable Rust/Swift
and physical-Mac qualification remain pending.

## Identifier occurrences in retained source

Open **Occurrences** in the paged source reader, enter an identifier and press
**Find**. You can also choose **Occurrences** beside an outline entry to use its
exact name. The list searches the entire retained file and lets you page through
its results. Select a row to highlight that exact original-byte range in the
source, then use ordinary native Copy for decoded text or **Copy occurrence hex**
for the original encoded bytes. Navigation uses the same capture even if the
working-tree file has changed or been renamed.

These are case-sensitive whole-token text matches, including declarations,
comments and string literals. They do not establish compiler bindings or
distinguish references to separate declarations with the same spelling. The
shared Rust token policy groups ASCII letters and digits, `_`, `$`, and
conservative non-ASCII runs; matching does not normalize Unicode spelling.
Occurrence line labels count LF boundaries, including CRLF. Original byte
ranges remain exact for CR-only source as well.

This service admits up to **512 KiB** of valid UTF-8 or BOM-marked UTF-16 source
and identifiers of at most **1,024 UTF-8 bytes**. It retains at most **4,096
occurrences**, returns the first 64 rows and pages through subsequent retained
rows without rescanning. A limit reports an incomplete inventory and a lower
bound on matches instead of presenting a truncated count as exhaustive. Files
outside these bounds remain available through the ordinary source reader.
Editing the identifier or starting a replacement search revokes old row actions;
failed or canceled work preserves the accepted source page. Find, outline and
occurrence queries have independent generations and selection identities.

The Rust host and C-boundary suites cover exact-byte navigation, changed files,
UTF-16, limits and cancellation. The portable decoder and model suites are wired
through `swiftui/tests/test-reader-references.sh`; the supplied-reader suite also
covers native reference-call marshaling. These are code-first additions awaiting
the configured strict-RCH verification lane and physical-Mac qualification.

## Local test image

For a local drag-to-Applications test image:

```sh
./scripts/package_macos_dmg.sh --app "$APP" \
  --output "$PWD/dist/FrankenCodeBrowser-local-test.dmg" --local-test
```

The script refuses to replace an existing output. `--local-test` is not notarized and is not a public release. See the root [README](../../README.md) and [distribution status](../../DISTRIBUTION.md) for the supported build and release paths.

The `swiftui/` sources own the project picker, atlas, search, reader, prepared-text cache, and retained Metal presentation. `src/` owns typed Apple object and callback lifetimes for native consumers; it does not own source discovery, parsing, or search policy. The native sources were imported from the former local `franken_macos` checkout at commit `8ffd16f795e9064753d95d28988b7433c0132f8e`; that checkout is preserved as a history archive, not needed for this build.
