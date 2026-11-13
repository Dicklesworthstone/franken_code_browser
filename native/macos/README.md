# Native macOS app

This directory contains the SwiftUI/Metal FrankenCodeBrowser app and the narrow `franken-macos` Rust facade. It is part of the **same `franken_code_browser` repository** as the engine. The app links the workspace's `fcb-bridge` static library; no second checkout is needed.

From the repository root on macOS 14+:

```sh
./scripts/install_macos_app.sh --build
```

To inspect a built app before installation, run `APP="$(./scripts/build_macos_app.sh)"`,
then `open "$APP"` and `./scripts/install_macos_app.sh "$APP"` if desired.

## City view

After preparing a text atlas, choose **City** in the toolbar. The City uses the
same file identities and footprints as 2D; switching projection does not repack
the repository or read source. It starts at the current 2D neighborhood. **Fit
City** fits all known buildings, including their elevated extents. Returning to
**2D** preserves its camera and history, while the selected file and open reader
remain available.

Building height represents **observed file bytes**, with a logarithmic transform
capped at **1 GiB**. Known files have a 2% minimum height, including known empty
files; the maximum height is 12% of the atlas's larger ground dimension. Unknown
sizes are gray flat parcels, and partial catalog membership remains explicit in
the legend. These metadata observations are not exact source line counts or a
claim that every project file has been discovered.

| Action | Control |
| --- | --- |
| Pan | Drag, two-finger scroll, arrow keys, or the four pan buttons |
| Orbit | **Option-drag**, **Option-left/right**, or the rotate buttons |
| Tilt | **Option-up/down** or the tilt buttons; tilt is bounded to 75° |
| Zoom | Pinch, **Command-scroll** or **Control-scroll**, or the zoom buttons |
| Read a building | Click its visible surface; the existing frontal source reader opens |
| Read files with the keyboard | **Read previous/next file**, or **Command-Option-[ / ]** |
| Return | **Return to 2D** or the toolbar projection picker |

Camera changes and projection switches are direct, including reduced-motion
use. City keeps reading frontal instead of putting text on oblique faces.
Filenames unsupported by the ordinary UTF-8 opener remain available through
**Project Files** or **Quick Open**. Workspace, directory, file and saved-capture
search remain available; choosing a new visible-file search scope requires 2D.

The Rust scene owns the checked camera matrix and CPU hierarchical picking.
Native code uploads canonical instances once, draws with depth32Float and
strict less-than depth, and acknowledges a frame only after Metal's actual
presentation callback. Camera publication, resize, layout replacement and
closing fence activation of older pixels. A picked building is a file identity,
not an exact text location; source activation follows the existing capture path.

Native preparation, response decoding, and final retirement run off the UI
thread. There are two globally admitted City scene lifetimes, one coalesced
waiting gesture per scene, at most two submitted draw commands per surface,
and bounded depth/drawable allocations. Graphics unavailability or admission
refusal leaves **Return to 2D** and **Retry City** available. The first native
qualification batch, including actual GPU shared-edge coverage and physical-Mac
interaction, is pending; authored fixtures are not a rendered-product pass.

The focused native regression entry point is
[`swiftui/tests/test-city.sh`](swiftui/tests/test-city.sh). It compiles the
production City sources and checks worker scheduling, lost-reply recovery,
presentation/resize fences, identity, and admission using injected wire replies.
Run it in the independent native verification lane; it does not exercise GPU
pixels. Rust geometry, host, registry, and C ABI regressions live alongside their
production implementations.

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

### Repository ignore rules

The native file browser, Quick Open, project-wide source search, 2D atlas and
City use the engine's bounded repository-rule policy. Discovery reads nested
`.gitignore` and `.fcbignore` files; `.fcbignore` follows `.gitignore` within each
directory. The existing product exclusions and protected internal directories
still apply. Rule files are configuration inputs, never commands to execute.

The browser and filename finder report how many rule files were applied. Rule
reads have their own counters and limits: 16 KiB per file, 64 KiB in total,
256 admitted files and 1,024 patterns, with separate compile and matching-work limits.
Metadata discovery still reads no source payload. An unsupported, unreadable,
oversized or unsafe rule file leaves the affected scope unknown and reports
partial coverage and a diagnostic; it cannot turn hidden files into proven
no-match results or deletion evidence. Excluded-directory counts do not invent
a count for their unvisited descendants.

Use **Refresh project**, or the search surface's **Refresh file catalog**, after
changing ignore rules. Captures and searches already retained by an earlier
catalog keep their original identity and coverage.

### Reading large files from Project Files

Every **Project Files** row offers **Read ranges**, so a file can be read directly
from a discovery page even when it is larger than the complete reader's **32 MiB**
limit. Files whose observed size exceeds that limit recommend this action.
Clicking the filename still requests an independent complete capture; an earlier
size observation does not decide whether the current file fits that request.
If capture reaches its input limit, its message points to **Read ranges**.

**Read ranges** opens the existing **Observed Range Reader** with a bounded first
range. It supports byte and physical-line navigation, literal and occurrence
search, and exact copying from retained observations. Each newly read range is
an observation of the current file, with its own identity and coverage. It does
not claim a whole-file immutable revision or replace an earlier exact capture.

The clicked file must still belong to the displayed retained discovery page and
the same project root and access grant. Raw native filenames, including names
that are not UTF-8, follow their retained byte identity. The app closes this
current-source surface when the project is replaced or refreshed, or its access
grant changes. Entered range work drains before its descriptor and access lease
retire; independently retained complete readers remain usable.

Focused native cases in
[`swiftui/tests/test-project-catalog.sh`](swiftui/tests/test-project-catalog.sh)
cover oversized metadata, explicit range admission after capture refusal, raw
filenames, stale rows and grants, cancellation/drain, and surviving exact readers.
Executable verification and physical-Mac interaction checks remain pending.

### Refreshing an open project

**Refresh project** prepares a new bounded metadata page while the accepted
project remains usable. You can keep browsing its files, searching, moving the
atlas, and reading captured source during that work. **Cancel refresh** or a
failed scan leaves that project in place. Only one replacement may be pending;
after cancellation, another refresh waits for entered I/O to drain.

A validated replacement is installed only while the same window, project,
byte-exact root, and access grant still own the request. Successful refresh
restarts current-project search against the new catalog while keeping the open
retained reader's source, page, Find selection, and navigation. Quick Open,
pinned readers, and Reading Desk captures also keep their original source bytes.
The earlier atlas keeps its membership, previews, and navigation until you
choose **Rebuild atlas**; **Project Files** shows the refreshed discovery
separately. File and directory search scopes remain selected, while an old
visible-atlas scope must be selected again after the new map is built.

Filesystem-change notices are hints to refresh. A partial first page remains
partial, and the notice clears only after complete discovery without omissions
or a newer observed change. Choosing a different folder or granting access
again replaces the project immediately through the normal opening flow. This
refresh operation does not automatically reconcile deleted files or migrate
annotations between captures.

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

## Finding filenames in a saved repository

Open an FCBS archive in **Saved Repository** and use **Find filename or path**
in the **Saved members** sidebar. **Exact**, **Prefix** and **Fuzzy** use the
shared ranked path engine over every recorded member, including files whose
source was unavailable when the archive was created. **Match case** is optional;
the default uses Unicode scalar lowercase matching without normalization.
Finding names and paging results use archive metadata only.

The finder retains up to 256 ranked names. **Next names** and **Previous names**
browse that accepted ranking; **Read** verifies and opens the selected captured
member in an independent reader. Selecting a name also makes that member
available to the documentation and import direction filters, even when it is
outside the ordinary member page. Unavailable entries remain visible with their
recorded reason and cannot open a source reader. Original native filename bytes
remain the identity behind the escaped display label.

Partial filename results show how much of the saved manifest was examined.
**Search more** repeats the same accepted query with a larger bounded allowance,
keeping the current results available through failed or canceled replacements.
A complete ranking with more than 256 matches reports its separate retention
limit. Narrow the query to reach other matches. Historical incomplete project
discovery remains distinct from searching all names actually in the archive.

Changing query options disables the previous rows until **Find** runs again;
**Use accepted query** restores their options without searching. **Reload names**
recovers retained results after an unavailable reply. **Clear** removes filename
results independently of source searches and relationship inventories. Activated
readers remain usable after clearing results or closing the archive. Host, C ABI
and native regression cases cover these flows; independent executable
verification and physical-Mac qualification remain pending.

## Searching saved repository text

Open **Saved Repository → Search → Literal** to search the captured FCBS archive.
**Match case** starts enabled. Turn it off for the same Unicode scalar lowercase
matching used by live text search; neither mode applies full case folding,
canonical normalization, or expression syntax. **Count all matches** remains
independent of the 4,096 retained result limit and does not turn partial archive
coverage into a complete count.

Lowercase queries scan verified saved members directly, because exact FCBD
postings cannot establish absence under a different matching policy. An
attached index stays available for subsequent exact searches. The accepted
query's case policy follows its counts, result pages and source activation.
Changing the draft's policy disables actions on the previous results;
**Use accepted query** restores those inputs without rescanning.

**Read source** opens the exact original-byte occurrence, preserving the actual
UTF-8 or UTF-16 spelling even when the query matches a lowercase expansion.
The source reader remains independently usable after the archive closes.
Case/mode, byte-witness, index-bypass, cancellation and native ABI regressions
are included; executable verification remains pending.

## Searching saved repository structure

Open **Saved Repository**, choose an FCBS archive, and use **Search** to select
**Symbols**, **Headings**, or **Occurrences**. The scope is the entire opened
saved archive. Search runs on the worker over verified captured bytes; saved
member names are labels and never open working-tree paths.

| Mode | Matching and evidence | Per-member source limit |
| --- | --- | --- |
| Symbols | Case-sensitive exact, prefix, or contains matching against shared outline names, including qualified names. Rust, Python, JavaScript, TypeScript, Go and C/C++ declarations are heuristic candidates. | 64 KiB |
| Headings | Case-sensitive exact, prefix, or contains matching against FrankenMarkdown heading titles. Results retain duplicate-aware slugs and qualified original source ranges. | 256 KiB |
| Occurrences | One exact identifier token, including spelling matches in declarations, comments and string literals. Supported UTF-8 and BOM-marked UTF-16 keep their original byte coordinates. | 512 KiB |

These evidence levels remain visible in each result. Symbols and occurrences do
not claim compiler binding or a call graph. Search preserves Unicode spelling
without normalization. Each request accepts up to 1,024 UTF-8 query bytes,
retains up to 4,096 results, and reads at most 64 MiB of aggregate member source.

The coverage summary distinguishes unsupported language or decoding,
unavailable captures, per-member source limits, extractor limits, omitted
metadata, a result cap and an exhausted aggregate source budget. It reports
how many manifest members were visited. **Complete extraction processing**
describes the admitted extraction pass, not semantic completeness. Incomplete
archive discovery also keeps coverage partial; empty partial results cannot
establish that a name is absent.

Choose **Read source** to reverify the selected saved member and transfer it to
an independent retained reader. Symbol query text may be a qualified name while
the selected source bytes contain only its local name. Heading ranges can
include Markdown syntax. Ranges up to 2,048 bytes expose an exact original-byte
selection witness; larger targets open a bounded 16 KiB context window and
retain their full target range for navigation. Opened readers keep their own
capture after the archive closes, and can be pinned or kept in the Reading Desk.

Editing the query, mode or name-matching option disables actions on earlier
rows. **Use accepted query** restores their exact accepted inputs. Pagination
and **Reload results** read retained metadata without repeating extraction.
Failed or canceled replacements preserve the previous accepted structural
query when its state can be verified. Structural results have their own
generation namespace; clearing them leaves accepted literal and expression
results, documentation links and independent readers available.

The shared engine, host and C boundary have structural-search regression cases.
The native decoder/model/retained-reader cases are included in
`swiftui/tests/test-saved-search.sh`, using the shared saved-repository fixture.
They cover qualified names whose source spelling differs, headings, UTF-16
occurrences, large target ranges, partial coverage, independent generations,
lost replies, cancellation and stale or forged activation receipts. Executable
Rust/Swift verification and physical-Mac qualification remain pending.

## Following imports in a saved repository

Open an FCBS archive in **Saved Repository**, choose **Imports**, and select
**Build imports**. This uses the same Rust, JavaScript/TypeScript/JSX/TSX and
Python lexical import engine and finite catalog policies as Project relationships.
The archive supplies both source evidence and target membership; opening an
import does not require a working-tree checkout or grant access to live paths.

**All imports** lists the retained statements. Select a member on the left and
choose **Outgoing** or **Imported by** to inspect either direction. Each row
keeps the original statement and specifier byte ranges and reports whether its
target is captured, unavailable, missing, ambiguous, external or unresolved.
**Read statement** opens the original source near that exact statement range.
**Open target candidate** is available only for a unique captured catalog target.
Both actions reverify the member digest from the already opened archive before
installing an independent reader; in-place corruption never substitutes live
source. Opened readers remain usable after the import inventory or archive closes.

The build processes one source member at a time, admits at most 128 KiB per
source and 32 MiB in aggregate, and retains up to 4,096 import occurrences.
Accepted inventory data contains metadata, with no retained source payloads.
Pagination and direction changes use that inventory without repeating source
reads. Missing captures, unsupported sources, parser diagnostics, source limits,
row truncation and incomplete archive discovery remain visible coverage limits.
An empty partial result does not establish the absence of incoming imports.

Import builds and clears have their own generation, independent of text,
structural and documentation-link searches. Failed or canceled replacements
preserve the previous accepted inventory; **Reload** reconciles a lost response
with the same open archive. Original UTF-8 and UTF-16 byte evidence, native path
bytes, cancellation, stale identities, corruption and independent reader
lifetimes have host, C-ABI and native regression cases. Executable verification
and physical-Mac qualification remain pending.

## Saved repository search indexes

In **Saved Repository → Search → Literal**, expand **Saved search index**
and choose **Build and save index…** to create an FCBD index from the archive
already open in the app. Choose a new destination in the save panel. The build
uses the retained archive descriptor and verifies captured members; it does not
reopen the archive's original pathname or read the live project. An existing
destination is never overwritten. The output contains unencrypted source-derived
search data, so choose its destination with the same care as the saved sources.

The native build admits up to **256 KiB per member**, **64 MiB of source** and
**2,097,152 posting pairs**, with a **24 MiB** encoded output ceiling. It uses the
existing bounded resident builder, inversion and demand-paged encoder. Members
outside the build quotas remain explicitly uncovered and can still be searched
by exact source scanning. The receipt shows indexed, uncovered and unavailable
counts and retains the archive's historical discovery completeness.

When complete bytes have been written, choose **Attach built index**. This uses
the trusted page-manifest pin returned by the build and revalidates the file and
its exact archive binding. Building alone preserves the current attachment,
search results and captured readers. Keep the displayed **Build receipt SHA-256**
with the FCBD file if it will be attached in another session. To attach an
existing index, paste that trusted pin and choose the file in the same panel.
No index is inferred from a neighboring filename or loaded automatically.

Cancellation waits for entered work to drain. A retained write receipt records
whether nothing was created, an incomplete destination remains, or complete
bytes were written with file/directory synchronization requested. A failed sync
or late cancellation can leave a complete file available for explicit verified
attachment. **Reload write receipt** recovers the same immutable result without
building, writing or rereading source. An uncertain outcome never retries a
write automatically; inspect the destination before choosing another filename.
The app retains destination access through entered work and subsequent explicit
attachment. These receipts do not certify durability across power loss.

The matching CLI can also build an index explicitly; retain its receipt:

```sh
fcb snapshot index build /absolute/path/source.fcbs \
  --output /absolute/path/search.fcbd --paged --json
```

Use the receipt's `index_digest` when `index_pin_scope` is
`page-manifest-envelope-sha256`. This pins the metadata envelope and its page
hashes; a whole-file checksum is a different value. Native attachment reads the
bounded manifest and keeps a 64 KiB page cache. Posting pages are verified when
read, and candidate matches still verify the exact saved source. The UI shows
indexed, uncovered and unavailable member counts. Short literals, uncovered
members and text encodings without compatible postings retain direct scanning.
Unicode lowercase literal, expression and structural searches use direct source scans;
the accepted query reports which policy actually ran.

Each accepted literal result reports its own index generation, eliminated
members, direct-scan fallbacks and postings visited. **Reload index status**
refreshes cumulative page I/O without reopening either selected path. **Detach
index** changes subsequent literal searches; existing result pages and captured
readers remain available. A rejected replacement preserves the prior index.
Lost or canceled replies reconcile the accepted generation on the same handle.
If status remains unknown, new index changes and literal searches wait for a
successful reload or archive close. Both possible index grants stay retained
until that outcome is known, and entered work drains before closure releases
the archive and index access.

The native model/receipt and public-header C marshaling regressions are in
`swiftui/tests/test-saved-search.sh` and `test-saved-index-build.sh`; actual
FCBS/FCBD construction, create-only publication, cancellation, receipt recovery
and ABI cases are in `crates/fcb-app/tests/saved_repository_index_build.rs`,
`crates/fcb-bridge/src/saved_sessions_tests.rs`, `saved_index_build_ffi_tests.rs`
and `saved_ffi_tests.rs`.
These additions still require the configured independent strict-RCH verifier.
Physical-Mac picker, sandbox access and native UI qualification remain pending.

## Comparing saved project captures

Choose **Compare Captures** in the toolbar, or **Compare captures…** inside
**Saved Repository**. Select a **Before** and **After** FCBS archive, then choose
**Compare captures**. Use Saved Repository's save action to capture a project
at different observations. The comparison opens both archives together and
retains the accepted pair while a replacement opens or is canceled.

The file list aligns exact project-relative path bytes from the two saved
manifests. **Differences** includes changed, added, removed, one-sided and
unavailable entries. Choose **All** or a specific classification to inspect
the rest, including unchanged files. The header reports progress and coverage;
previous/next navigation follows the returned page boundaries.

| Classification | Evidence |
| --- | --- |
| Changed | Both sides captured the same exact path with different source bytes. |
| Unchanged | Both sides captured the same exact path with equal source bytes. |
| Added / Removed | A path occurs on one side, the capture policies agree, and the opposite manifest has complete membership. |
| Only before / Only after | A path occurs on one side, but policy or discovery coverage cannot establish its absence from the other observation. |
| Unavailable | A recorded path has unavailable source on one or both sides, including one-sided entries. |

**Read before** and **Read after** open the selected saved member.
**Compare file** opens both retained versions in the existing byte-comparison
reader, where bounded change pages navigate each side's original byte ranges.
An unavailable member stays visible with its recorded reason. A captured empty
file remains readable. **Keep in Reading Desk** retains a source for notes,
bookmarks and checkpoints after the comparison closes.

Opening a pair verifies both archives. Classification and paging then use
retained metadata without loading member payloads. Opening a member reads and
reverifies its bytes through the original archive descriptor before transferring
them to an independent reader. Replacing the archive pathname or editing the
live project cannot silently substitute another source. A changed archive
payload fails verification; an already opened reader retains its own capture.

Each native archive is limited to 65,536 members, 80 MiB of archive bytes,
64 MiB of captured source, 1 MiB per captured member and 2,048 bytes per member
path. Comparison progresses in steps of up to 256 merged paths; list pages
request 64 entries and may be shorter to respect the 4 MiB response bound.
Two comparison owners allow an accepted pair to remain available while a new
pair is validated. Cancellation, invalid archives and failed replacement leave
the prior accepted comparison intact.

The before/after roles are chosen explicitly. This workflow compares saved
observations by path; it does not infer renames, Git history, logical file
continuity or automatic annotation relocation. An incomplete capture remains
explicitly incomplete.

Rust host, registry, C-boundary and native regression fixtures accompany this
workflow. They are code-first additions and have not been executed in this
session. Independent Rust/Swift execution and physical-Mac qualification remain
pending.

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

## Workspace expression search

Select **Expression** in the search-mode picker to combine literal text with
required terms, exclusions and metadata filters. For example:

```text
"pub fn" Result -unsafe path:src/ lang:rust
```

This finds occurrences of `pub fn` in admitted Rust files whose path contains
`src/`, whose captured contents contain `Result`, and whose captured contents do
not contain `unsafe`. Whitespace between positive terms means AND; `-term`
means NOT. Required and excluded terms apply to the **same complete captured
file**, so they may occur on different lines. Results select occurrences of the
first positive term or quoted phrase. Other required terms qualify the file;
they do not create additional result rows.

| Syntax | Meaning |
| --- | --- |
| `needle required -excluded` | Find `needle` in files containing `required` and no `excluded`. |
| `"exact phrase"` | Keep spaces within a literal phrase; `\"` and `\\` escape a quote or backslash. |
| `path:src/ -path:generated/` | Include and exclude paths by substring. |
| `path:**/*.rs` | Apply a path glob; `*` and `?` stay within a path component, while a whole `**` component can cross directories. |
| `path:"directory with spaces/"` | Keep spaces inside a path-filter value. |
| `lang:rust`, `type:rs`, `-lang:python` | Include or exclude a language/file extension using the shared engine's aliases. |

An expression must contain a positive text term, fit within 1,024 UTF-8 bytes,
and contain at most 64 terms and filters. A filters-only or exclusions-only
request is refused. Regex directives are refused; OR and parentheses are not
Boolean operators in this language. Choose **Text** when the entire input
should be one literal. **Match case** applies to all content terms; when off,
comparison uses Unicode lowercase with the same exact-source-spelling rules as
ordinary text search. Path and language filters retain their own metadata
semantics.

Expression filters intersect the selected file types and **Workspace**,
**Folder**, **File**, or frozen **Visible atlas files** location. They cannot
widen that selection or grant access to another root. The summary separates
files outside the selected scope from files excluded by expression metadata.
Those metadata exclusions are decided before source reads and do not consume
the live query's file or capture budget.

Leave **Captured index** off to capture matching-scope sources during each
query. Turn it on to prepare the current file/location scope once and run both
Text and Expression queries against those retained bytes. Editing the
expression or changing **Match case** reuses the prepared captures. **Refresh**
captures current sources again. Expression filters narrow a query over the
prepared index; they do not change which sources the index originally captured.
Widening a query beyond that captured scope can therefore leave files
uncaptured until Refresh. Indexed queries and result activation do no live
source reads.

Both routes retain at most 1,000 primary matches. **Count all matches** starts
enabled for Text and Expression searches, with either case choice, across live
captures and the retained workspace index. Counting continues through the
admitted scope after navigation rows fill; expressions count primary occurrences
only after their same-capture predicates succeed. A complete scan can report an
exhaustive total while retaining only the first 1,000 selectable matches.
Eligible files outside the captured scope, unavailable captures and verification
limits still leave the count explicitly partial. Turn counting off to stop when
an additional match proves the navigation-row limit was exceeded. Changing the
choice starts a fresh query; structural modes keep their separate bounded-result
contracts. Live capture and index preparation admit at most 4,096 files, 1 MiB per file and 32 MiB of
aggregate source-read work. A prepared index additionally admits at most 2,097,152 grams;
files without usable gram coverage use retained-source verification. Each
expression has a separate **256 MiB verification-work budget**, shared across
all required, excluded and primary probes. Rechecking the same capture for
several terms spends that budget again. A limit can leave partial results even
when every source file fits the capture limits. An unsearched suffix never
proves that an excluded term is absent, and partial/no-proof outcomes never
become a complete empty result.

Opening a result imports its exact retained capture and verifies the primary
match at its original byte range, including UTF-16 or a file changed on disk
since capture. The reader's Find uses the original matched spelling rather
than the full expression. Source excerpts, atlas highlights, cancellation and
stale-result rejection use the existing search and reader routes.

Host, C-boundary and native decoder/transport regressions accompany this
integration. They have not been executed in this code-first session; Rust and
Swift compilation, executable qualification and physical-Mac interaction
verification remain pending.

## Project documentation links and backlinks

Choose **Documentation** in the toolbar, or use **Backlinks to this file** and
**Links from this file** in the Inspector. **Capture and build links** prepares
an independent captured project index, then extracts Markdown destinations
through the shared parser. This covers links from documentation to source files
as well as links between documents. File-type display filters and the visible
atlas region do not narrow this project capture.

Use **All**, **Outgoing**, or **Backlinks** to browse the inventory. **Use path**
selects an exact project-relative filename; **Whole project** removes that file
filter. A path supplied from the Inspector keeps its original native bytes,
including a non-UTF-8 filename. A path from another atlas is resolved against
this inventory's own catalog instead of reusing a foreign file ID. An absent
catalog path is reported as unavailable, not as an empty complete backlink set.

Each row distinguishes a captured target from an unavailable, uncaptured,
missing, external or unresolved destination. **Links from source** and
**Backlinks to target** navigate between those file relationships.
**Read source document** opens the exact Markdown capture that supplied the
evidence. **Read captured target** opens a resolved target from the same index;
it is unavailable when no target capture exists. These actions neither reread
the working tree nor fetch external URLs. Pin a reader to keep code and its
documentation visible while following another link.

Evidence identifies the **whole captured Markdown document**. The parser's
destination establishes the relationship, but this surface does not claim an
exact source span for the link token. Opening `guide.md#section` now checks the
selected target's canonical heading identity through the shared Markdown
adapter, then starts the reader at the verified original byte range in that
**retained capture**. Percent escapes are decoded once; a query suffix does not
change the captured file. List pages do not parse target headings eagerly.
Missing, ambiguous, invalid, unsupported or over-budget fragments open the
captured file at its beginning with an explicit explanation. Non-Markdown line
fragments such as `code.rs#L20` do not become guessed line jumps. Reading the
source evidence still starts at the beginning of the whole Markdown document.
These are documentation relationships; compiler imports, calls and dependency
completeness are not inferred. The heading-navigation addition requires
executable and physical-Mac qualification.

Source preparation admits at most 4,096 files, 1 MiB per file and 32 MiB of
source-read work. Link extraction visits at most one captured Markdown
document per worker step, parses at most 256 KiB per document and 32 MiB in
aggregate, and retains at most 4,096 links. Progress reports missing captures,
unsupported or oversized documents, parse limits, diagnostics and truncated
link output. A partial inventory cannot establish that a file has no backlinks.
Markdown extraction accepts UTF-8; UTF-16 Markdown is reported as unsupported
and leaves coverage partial. Captured UTF-16 target files remain readable.

**Rebuild links** reuses the prepared source captures. **Refresh sources**
captures the current project again so file edits can affect the inventory.
Already opened readers and pins retain their original bytes. Cancellation and
replacement reject stale link responses; an old link identity cannot authorize
an open against a replacement index.

Host, registry, C-boundary and native regression fixtures accompany this
workflow. They remain unexecuted in this code-first session; compilation and
physical-Mac interaction verification are pending.

## Project import navigation

In **Project relationships**, choose **Imports**, then **Capture and build
import candidates**. The inventory reuses the same captured sources as the
documentation view. **Outgoing** shows import statements in one file;
**Imported by** lists statements whose catalog candidate is that file.
**Read import evidence** opens the original statement's exact capture and byte
range. **Read target candidate** opens an independent reader over the retained
target, even if the live files have since changed.

The shared import engine supports Rust external module/use declarations,
JavaScript/TypeScript static imports and re-exports (including JSX/TSX), and
Python `import` and `from` statements in `.py` and `.pyi` files. Python aliases,
comma-separated imports, parenthesized from-import lists and explicit line
continuations preserve their original statement evidence. Comments and strings
are classified by the shared FrankenMarkdown lexer.

Python navigation uses an explicit catalog policy. `from .worker import Tool`
looks for the unique `worker.py` or `worker/__init__.py` candidate beside the
source; additional leading dots ascend catalog directories without escaping
the root. `from . import helper` offers the corresponding `.helper` candidate.
Its exact specifier evidence is the imported name `helper`; the displayed
destination combines that name with the statement's relative prefix.
Multiple possible paths remain ambiguous, with no target chosen. Absolute
imports such as `import requests` and `from package import value` retain
readable evidence but do not search `sys.path`, execute package code, or infer
installed dependencies. A catalog candidate does not establish a Python runtime
binding. Bare-relative wildcard imports have no guessed target.

The inventory admits 128 KiB per source, 32 MiB of aggregate parsing and 4,096
retained statements. Unsupported syntax, malformed decoding, parse limits and
omitted rows remain visible coverage conditions. These limits apply to the
captured project import browser. The **Saved Repository → Imports** workflow
applies the same lexical policies to archived members. Rust/Swift regression fixtures cover Python
source spans, UTF-16 coordinates, relative targets, ambiguity, stale captures and
native evidence validation. Executable and physical-Mac verification remain
pending.


## Workspace identifier occurrences

Choose **Occurrences** in the search-mode picker and enter one identifier-like
token to find its exact spelling across the selected project scope. For example,
`run` matches complete `run` tokens, including those in comments and strings,
while excluding `runner`, `_run`, `run_` and `$run`. Matching is case-sensitive;
Prefix, Contains and Match case controls do not change this mode.

The shared reference engine defines the token policy: ASCII letters, digits,
underscore and dollar belong to tokens. Non-ASCII non-whitespace, non-control
runs stay intact conservatively, including combining marks. A query must fit
within 1,024 UTF-8 bytes, contain one token, and not start with an ASCII digit.
There is no Unicode normalization. These are **whole-token text occurrences**,
including declarations, comments and literals; they do not establish compiler
name binding or a complete semantic-reference inventory.

Occurrences honors the selected file types and **Workspace**, **Folder**,
**File**, or frozen **Visible atlas files** location. It captures sources during
the query and publishes bounded progressive results through the normal results
list and atlas overlays. The captured-index switch is inactive for this mode.
All file extensions can participate when their source encoding is supported;
this mode does not rely on the declaration extractor's language list.

The native query retains at most 1,000 matches, examines at most 4,096 files and
spends at most 32 MiB of source-read work. The shared occurrence extractor admits
**512 KiB per complete file**, with UTF-8 and BOM-marked UTF-16 in either byte
order. Oversized sources, malformed text, read failures and exhausted budgets
remain explicit incomplete coverage. Reaching the result allowance alone does
not prove omitted matches: the query can continue checking later files for a
further whole-token occurrence.

Selecting a result opens its exact retained source and revalidates the selected
original-byte range through the reader's existing Occurrences inventory. It
does not reopen the working file or substitute a substring hit. The resulting
highlight and copy actions retain their own occurrence identity, including when
the file changes after the workspace search. Results from superseded modes,
scopes or query generations cannot authorize a new selection.

The host, C boundary, native worker/decoder and retained-reader regression
suites cover the complete route. These code-first additions still await
executable Rust/Swift verification and physical-Mac qualification.

## Searching large files across a project

Choose **Search large project files…** in the search sidebar to search text or
exact whole identifiers in the sidebar's selected location and file types,
using a separately retained project catalog. Workspace, folder, exact file and frozen
visible-atlas selections use the same predicates as ordinary search. Rust scopes
include `Cargo.toml`; custom extension scopes keep their exact selected keys,
and an empty custom scope searches no files.
This route streams file contents through the shared Rust matcher, including
files beyond the ordinary search's 1 MiB per-file and 32 MiB total capture limits.
It recognizes UTF-8 and BOM-marked UTF-16 in either byte order. Queries contain
1–1024 UTF-8 bytes; overlapping text matches keep their exact original byte ranges.

Choose **Text** for literal matching. **Match case** selects exact spelling;
turning it off uses Unicode scalar lowercase matching without canonical
normalization or full case folding. Lowercase results retain each match's
original spelling and encoded bytes.

Choose **Occurrences** for one complete, case-sensitive identifier. This route
continues beyond the captured occurrence extractor's 512 KiB source allowance,
subject to the same total scan limits as Text. It uses the same token policy as
captured occurrences: ASCII letters, digits, underscore and dollar are token
characters, and an identifier cannot start with an ASCII digit. Non-ASCII
non-whitespace/non-control runs use conservative boundaries. Comments and
string literals are included. These are text candidates, without compiler
resolution or a claim of semantic completeness.

The matcher carries token state across decoder windows and work quanta.
Only an observed delimiter or actual source end can finish a matching token;
a scan-budget cutoff does not turn a prefix of a longer token into a hit.
Occurrences supports the same scopes, progress, Stop, cancellation and
**Count all matches** options. Match case is fixed on in this mode; returning
to Text restores the saved case preference. Opening from the sidebar's
Occurrences mode selects it automatically.

The sheet freezes the location and file-type selection when it opens. Moving the
camera, changing the sidebar or refreshing the sheet's catalog cannot widen that
selection. Reopen the sheet to choose another scope. For a visible-atlas scope,
choose the atlas view in the sidebar first. Files absent from the search catalog
remain separately reported as missing requested files, preventing a false
complete result. Location/type exclusions are reported separately from discovery
policy exclusions and do not consume file-read, byte or result budgets.

Open the sheet, enter the literal or identifier, choose **256 MiB**, **1 GiB**, or **4 GiB** as
the total source I/O budget, and press **Search**. There is no per-file capture
ceiling. The scan admits at most 1,048,576 read calls and retains at most 256
matches. It uses a 16 KiB input buffer. Exact Text and Occurrences share one
encoded witness per file with retained matches; lowercase Text retains each
hit's original spelling and bytes. Complete sources are not retained. While the scan runs,
the sheet shows cumulative byte/read progress and previews up to the first 32
matches from finalized file observations. The unfinished file contributes I/O
progress, but its tentative matches are withheld until its observation finishes.

Enable **Count all matches** to continue through the selected files after the
256 retained rows fill. The total includes overlapping occurrences and matching
files with no retained rows. The sheet distinguishes the total counted matches
from the rows available to open or copy. A finished, unchanged observation can
establish a complete count even when additional rows were omitted. If a byte or
read-call budget, unsupported text, unavailable file, changed source or Stop
prevents complete coverage, the displayed count remains partial. Counting uses
the same memory, I/O and cancellation limits; it does not retain more source or
occurrence rows. Without this option, the first additional match beyond the row
allowance still stops the scan. Changing the option revokes the previous query's
results and requires a new search.

Choose **Stop and keep matches** to accept the finalized-file results immediately
after the current scanner quantum drains. The unfinished file is abandoned and
remains unexamined; bytes and read calls already spent remain counted. The
accepted partial result makes all retained matches available for selection,
copying and verified current-context reads. Preview rows become actionable only
after that terminal result is accepted. If the search finishes before Stop is
observed, its completed result is preserved.

**Cancel and discard**, query edits and replacement searches revoke the current
attempt's delivery while entered work drains on the worker. They do not convert
a canceled attempt into a stopped result. Previously accepted evidence remains
readable when retained by the surface, with obsolete actions disabled.

The catalog admits at most 20,000 regular files, subject to separate path and
discovery-page limits and the product's exclusions. Search covers the eligible
members of this catalog under the sheet's frozen scope. **Refresh file catalog**
discovers current membership again within the same location and type selection, including
new files. Existing queries never turn newly created or uncatalogued files into
proven no-match results. The summary separately reports incomplete discovery,
unexamined files, unsupported text, unavailable files, failed reads, and byte,
call or match-limit stops, and user-requested stops. Excluded entries include pruned directories; their
count is not an invented count of all files beneath them.

Select a result to inspect its retained literal. **Copy match** copies that
decoded spelling; **Copy original hex** copies the exact original encoded bytes.
These actions do no source I/O and remain meaningful if the working-tree file
changes. **Read current context** explicitly opens the current canonical pathname
under the current project access grant, creates an independent observed reader,
and checks the literal's original bytes before highlighting. Changed bytes are
reported as changed; the earlier result never claims the new file's identity.
For an occurrence result, this comparison verifies the retained token bytes;
it does not re-establish surrounding token boundaries or compiler binding.
Changing the query, mode or case policy, refreshing the catalog, replacing the
project or closing the sheet closes this child reader and revokes its actions
synchronously.

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
including UTF-16 surrogate pairs. Enable **Count all matches** to continue
counting after the retained-hit table fills without retaining more rows or source
bytes. A completed unchanged observation can then report an exhaustive count;
byte/read-call limits, decoding failures, source changes and cancellation keep
that count explicitly partial. Without the option, Find preserves its existing
stop-at-first-overflow behavior. The result reports actual bytes scanned and
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

## Appearance

The **Appearance** menu offers **System**, **Light**, and **Dark**, plus an
**Increase Contrast** option. System follows the host window appearance; the
system's Increase Contrast setting is respected even when the app's additional
contrast option is off. These preferences persist across launches.

The atlas, City view, source readers, selection, and search colors use the same
resolved palette. Changing appearance retains captured source, lexical roles,
source selections, glyph positions, and camera geometry. Background-dependent
colored raster tiles are refreshed; old in-flight tiles cannot publish into the
new palette. Existing prepared-source archives keep their canonical color format.

This source addition includes regression cases for contrast, retained text,
selection, and native drawing. Executable and physical-Mac qualification remain
pending; it is not a claim about the already released binary.

## Styled Markdown preview

Open a retained source file and choose **Markdown**, or **Split preview** to keep
the source alongside the document. The native reader now prepares a **Styled**
view by default when the host supplies the rich-display capability. **Text**
keeps the existing selectable logical preview available. The **Columns** field
and **Reflow** control change the width without reopening the file.

Styled pages draw FrankenMarkdown's shaped glyph IDs and positions using the
exact bundled font bytes supplied by the retained reader. Headings, prose,
lists, quotes, tables, code fences and supported vector decorations retain the
shared engine's layout and semantic color roles. Appearance changes recolor
the accepted display without reparsing or changing its source identity.

**Previous page** and **Next page** move through bounded display pages. Switching
between Styled and Text, or reflowing a mapped page, restores its enclosing
captured Markdown block. Click a mapped element, use **Show source**, or use
its accessibility action to inspect that block; split mode also navigates the
source pane. This correspondence is at block granularity. It does not claim
glyph-exact or inline-exact source spans. Text selection and verified copy
remain available in Text and in the source reader.

In the preview's **Links** list, **Open in browser** explicitly sends a verified
HTTP or HTTPS destination to your configured default browser. The row shows the
URL passed to the OS. This route accepts ASCII or punycode hosts, preserves
percent escapes, and refuses credentials, hidden controls, bidi formatting and
custom schemes. Loading a document or its links never launches a browser.
Canceling, reflowing, replacing or closing the preview withdraws a pending
handoff. URL-policy and stale-action regressions are included; executable and
physical-Mac browser-handoff verification remain pending.

Preparation admits UTF-8 captures up to 64 KiB, at most 8,192 display elements,
131,072 glyphs, and 8 MiB of bundled font resources. A page admits at most 128
elements and 16,384 glyphs. Source, generation, layout and page identities are
checked before accepting asynchronous results or enabling source actions.
Cancellation, capture replacement and failed reflow cannot reactivate stale
pages. Each logical generation retains one rich width; reflow creates a fresh
generation.

The bundled upstream shaping profile does not cover every writing system or
combining sequence. Unsupported text leaves Text and the exact source readable.
Local PNG/JPEG images can now fill the upstream image slots in Styled mode.
Opening a current project file supplies its explicit folder access alongside the
reader's native lease. Before the first image read, Rust verifies that the
current Markdown still matches the retained capture exactly. Saved/offline
readers have no implicit local-image authority. Images are current local
observations, not historical assets reconstructed from the source capture.

The reader admits at most 4 MiB of encoded data per image, 8 MiB and 16 images
per document, and returns identity-checked chunks with an exact content digest.
The native worker verifies that digest, accepts a complete single-frame PNG or
JPEG with at most 8,192 pixels per side and 16,777,216 source pixels, and makes
a display-sized thumbnail of at most 1,024 pixels per side. A page retains at
most 16 MiB of decoded RGBA pixels; codec working memory is a separate bound.
Decode and final pixel retirement run off the interaction thread. Drawing only
aspect-fits the prepared pixels in FrankenMarkdown's existing rectangle.

Remote URLs, missing/currently changed sources, unavailable folder access,
unsupported formats and exceeded limits leave alt-text placeholders with
coverage information. No image request fetches the network. These additions
include real PNG/JPEG pixel, framing, resource, source-authority and cancellation
regressions; executable and physical-Mac qualification remain pending.
Math and diagrams may use the upstream textual fallback. If source provenance
cannot be qualified, the styled content can remain readable with source actions
unavailable. These additions have wire, lifecycle, adapter, host and C-ABI
regression cases, but executable and physical-Mac rendering qualification remain
pending. They do not update the already released application binary.

### Following links between captured files

A local destination in the preview's **Links** list can open another file under
the active project grant. The existing link operation validates the originating
capture before returning an independently retained target reader. The linked
source sheet now offers **Back** and **Forward** across the last three accepted
destinations. Moving through this trail reuses captured bytes and keeps each
reader's page, selection, search and preview state without reopening its path.
The same history is available when opening a file through **Project files**:
the originating source stays on the left, with linked destinations on the right.

The originating source stays available behind the sheet. **Return to source**
closes the trail and restores that reader. An unavailable destination or
canceled onward link leaves the accepted source and its Forward history intact.
Only a successfully accepted destination discards the old Forward branch.
At the three-destination limit, accepting another target releases the oldest
linked reader; the originating source remains available separately. Reader
capacity can still refuse an additional target without evicting the current one.

Refreshing the catalog under the same root and native grant keeps accepted
captures and cancels pending link work. New onward links revalidate their source
against the refreshed project. Replacing the root or its grant closes the trail.
Cancellation retains the single pending-operation slot until entered work
finishes, so repeated clicks cannot accumulate background link opens.

The history lifecycle suite covers retained positions and bytes, successful
branching, failed/canceled opens, bounded eviction, refresh, grant replacement,
dismissal and late replies. Executable verification and physical-Mac scrolling,
selection, focus and accessibility qualification remain pending.

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

## Finding text in a retained file

The paged source reader's **Find** scans its complete retained capture, including
text outside the visible page. **Match case** starts enabled. Turn it off for the
same Unicode lowercase comparison as workspace search, including `k` matching
`K` and a lowercase expansion mapping back to the complete original character.
UTF-8, BOM-marked UTF-16, and original CR/LF/CRLF spellings keep their source-byte
coordinates. **Previous hit**, **Next hit**, and **Copy hit hex** use each hit's
original spelling and encoded bytes, even after the disk file changes.

Enable **Count all matches** to continue scanning after navigation rows fill.
The summary reports the full observed count separately from the retained rows;
byte limits or unsupported text remain explicitly incomplete. Find retains at
most 4,096 rows, and long Unicode witnesses can lower that limit to bound the
response. Queries admit at most 4,096 UTF-8 bytes. Editing the query or either
option immediately revokes the old hit actions and cancels pending publication
while preserving the readable source page.

The additive `fcb_reader_find_with_options` ABI preserves the original exact
Find entry point. Host, C-boundary, and native model/decoder cases cover count
limits, UTF-16, expansion witnesses and stale/canceled requests. The native suite
is available through `swiftui/tests/test-reader-search.sh`; independent
strict-RCH execution and physical-Mac qualification remain pending.

## Selected passages, copying, and notes

Drag or use the keyboard to select a passage in a retained source reader. The
reader resolves that page's native text selection through the Rust source map
and shows its exact original-byte range. Ordinary atlas file opening now uses
these reader tools for an already captured file without rereading its path.

Use **Copy selection** to choose **Copy selected text**, **Copy text with
location**, or **Copy original selection as hex**. Logical text preserves its
Unicode spelling, source order and CR/LF/CRLF. Location copy adds the captured
source identity, reversible path, original-byte range, encoding and replacement
policy; it includes the retained capture digest when available. Hex preserves
the original encoded bytes, including malformed sequences. A decoded replacement
character is identified explicitly rather than being described as an original
source byte.

**Keep selected text** retains that exact passage in the Reading Desk, where
existing bookmark and personal-note actions use the saved range. Save a desk
checkpoint to preserve those notes and locations after closing the app. A
reader opened from the desk can also update its saved range from selected text.
Without a manual text selection, Keep uses the current exact Find, Outline or
Occurrences result when available, otherwise the current page.

Selection work is coalesced on the existing bounded worker. A pending or refused
selection disables exact Keep and Copy actions; it never silently becomes a
whole-page selection. Changing pages, replacing the capture, or closing the
reader revokes old selection replies. Failed navigation preserves the displayed
selection intent and resolves it again; **Retry selection** is available when
needed. A delayed desk transfer also checks that the original selection is still
current before importing it.

Selections are confined to one displayed page and whole composed characters;
disjoint native ranges cannot be saved as one enclosing passage. The resolver
admits at most 256 KiB of selected original bytes, with the existing 16-byte
allowance for adjusted page boundaries. Reading Desk transfer retains the
complete source and remains limited to 4 MiB per capture. Each clipboard action
admits at most 1 MiB of UTF-8 payload under a 2 MiB aggregate staging charge.
Oversized payloads are refused intact. Payloads are staged before clipboard
publication, with an intervening ownership-change check; a native publication
failure is reported without claiming rollback.

The production Rust/C regression sources, portable native decoder/model tests,
AppKit selection tests, private-pasteboard tests, and desk transfer tests are
included. The native lanes are `swiftui/tests/test-reader-text-selection.sh`
and `swiftui/tests/test-desk-reader.sh`; the supplied-reader lane covers
transport capability preservation. These are code-first additions awaiting
independent strict-RCH execution and physical-Mac qualification.

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
