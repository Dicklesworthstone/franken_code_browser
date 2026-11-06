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

## Local test image

For a local drag-to-Applications test image:

```sh
./scripts/package_macos_dmg.sh --app "$APP" \
  --output "$PWD/dist/FrankenCodeBrowser-local-test.dmg" --local-test
```

The script refuses to replace an existing output. `--local-test` is not notarized and is not a public release. See the root [README](../../README.md) and [distribution status](../../DISTRIBUTION.md) for the supported build and release paths.

The `swiftui/` sources own the project picker, atlas, search, reader, prepared-text cache, and retained Metal presentation. `src/` owns typed Apple object and callback lifetimes for native consumers; it does not own source discovery, parsing, or search policy. The native sources were imported from the former local `franken_macos` checkout at commit `8ffd16f795e9064753d95d28988b7433c0132f8e`; that checkout is preserved as a history archive, not needed for this build.
