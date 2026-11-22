# Native macOS app

This directory contains the SwiftUI/Metal FrankenCodeBrowser app and the narrow `franken-macos` Rust facade. It is part of the **same `franken_code_browser` repository** as the engine. The app links the workspace's `fcb-bridge` static library; no second checkout is needed.

From the repository root on macOS 14+:

```sh
./scripts/install_macos_app.sh --build
```

To inspect a built app before installation, run `APP="$(./scripts/build_macos_app.sh)"`,
then `open "$APP"` and `./scripts/install_macos_app.sh "$APP"` if desired.

## Open files and project folders

Use **File → Open…** (**Command-O**), the toolbar **Open** button, Finder's
**Open With**, or drop one local file or folder onto the main window. A folder
uses the existing project access and progressive catalog workflow. A file opens
in an independent **Selected File** reader. Files up to **32 MiB** have retained
source pages, Find, Outline, Markdown and original-byte selection/copy. Opening a file leaves the
current project available. The app registers as an alternate viewer for text,
source-code files and folders; it does not take over default file associations.

Larger files open directly in the **Observed Range Reader** using the selected
file's own access. Opening reads metadata and the first **64 KiB** range with
bounded decoding context. Byte navigation, physical-line navigation, bounded
Find, retained observation history and original-byte selection/copy work without
capturing the whole file or choosing its containing project. The first range
uses explicitly labeled UTF-8 decoding; choose UTF-16LE or UTF-16BE when needed.
Each range is a separate observation; unread regions remain unknown.
Symbolic links, special files, remote URLs, multiple items and
paths that cannot be represented losslessly as UTF-8 are refused. Use the
project catalog's native path support for filenames outside this entry point's
UTF-8 contract.

In sandboxed builds, the selected file has its own in-memory, read-only native
grant. Its containing folder is only a path component: it is not authorized,
remembered as a project, scanned, or used to load neighboring Markdown images
or links. **Open Project Folder…** makes that broader choice explicit.
**Refresh from Disk** and the range reader's **Reopen current file** retain the
original file-only scope.

Only one opening operation and one newest pending choice are admitted per
window. Typed file-URL drop providers have their own one-active/one-pending
admission and stay retained through canceled callbacks. Canceling, closing or making a newer choice fences late panel/drop and
worker results. A replacement file appears only after its first page validates;
failure preserves the current reader and its accepted bytes. Finder events route
to one existing window, or wait for a window on launch/reopen.

The focused regression entry point is
[swiftui/tests/test-native-open.sh](swiftui/tests/test-native-open.sh).
It exercises production intake/routing and reader models with controlled
transports. SwiftUI typechecking, actual Finder/drop delivery, security-scoped
grants and physical-Mac interaction still require the independent native
qualification lane; these fixtures do not constitute a rendered-product pass.

## Open the current file in an editor

The retained source reader and Observed Range Reader provide **Open Current
File in Editor…**. On first use, choose an installed application in the native
panel. The app remembers that editor for this session. The adjacent menu lets
you choose another editor or forget the selection. Saved-source readers without
current file access keep the action disabled; choose the file with **Open** to
provide that access explicitly. A pinned live reader keeps its own file access
after its original reader closes.

This opens the **current disk file**, which can differ from the retained capture
or range observation. Captured bytes remain unchanged, and captured line/column
coordinates are not passed as if they described the current disk version. The
native host passes separate application and document URLs to `NSWorkspace`,
without a shell, guessed editor command-line flags, or path decoding. It honors
the selected application installation instead of substituting a different
running copy. Paths must round-trip losslessly as UTF-8 and contain no control
or directional-spoofing characters.

An editor choice that finishes after its reader closes or is replaced cannot
send a file or change the configured editor. Cancellation withdraws an action
before OS submission, and another action waits until the old choice drains.
After submission, the OS request is allowed to finish with both the file and
application access leases retained. Forgetting the editor or closing the reader
does not release a grant still used by that request. Application metadata checks
run on the bounded native worker.

The focused injected-boundary fixtures are in
[swiftui/tests/test-editor-handoff.sh](swiftui/tests/test-editor-handoff.sh),
with live/fork/retained-only capability checks in the existing reader fixtures.
They are code-first additions pending strict-RCH execution. Actual application
launching and sandboxed macOS handoff still require native qualification.

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
City use the engine's bounded repository-rule policy by default. Discovery reads nested
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

**Project Files → Include ignored and generated files** prepares replacement
Project Files and atlas metadata under the selected policy. The worker verifies
that the metadata receipt matches the requested policy before publishing it;
unknown policy names, malformed rule fingerprints and mismatched receipts are
rejected. The previous project and readers remain available until preparation
and first-page validation succeed. Failed or canceled switches preserve the
previous scope, and **Refresh project** keeps the accepted policy.

A policy change starts a fresh atlas membership set, including when the new
scan is partial: files excluded by the new scope cannot survive from the old
map as unconfirmed parcels. A partial refresh under the same policy still
preserves prior unconfirmed observations. Camera and stable layout are retained
where applicable; the receipt's partial coverage is not deletion evidence.

**Quick Open → Include ignored files** restarts filename discovery for the current query. **Search large
project files… → Include ignored files** applies to the next **Search** and
survives **Reset search**. Superseded rows cannot authorize new actions; retained
source evidence remains readable.

Project Files' accepted discovery policy also governs ordinary sidebar search
and captured-index preparation. Switching policy invalidates stale search
actions and prevents reuse of an index prepared under the other policy.
Visible-atlas search keeps its frozen path membership within the selected
discovery policy. Quick Open and Search large project files retain their own
independent controls. Search and index replies must match their opened catalog's
policy, including the exact rule-observation fingerprint when present. The override reports
`all-regular-files/no-rule-files-v1`, with no repository-rule configuration reads.
Root confinement, symlink and special-object refusal, resource limits and
independent exclusions for FCB's own artifacts still apply.

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
project remains usable. If an atlas is already open, the same transaction also
stages a fresh bounded catalog, incremental parcel placement, and its spatial
index on the project worker. You can keep browsing, searching, moving the atlas,
and reading captured source during that work. **Cancel refresh**, a failed scan,
or a rejected layout leaves the accepted project in place. Only one replacement
may be pending; after cancellation, another refresh waits for entered I/O to drain.

A replacement is installed only while the same window, project, byte-exact root,
access grant, atlas revision, and file scope still own the request. Successful
refresh restarts current-project search against the new catalog. Existing raw-path
parcels keep their position and dimensions even when observed file sizes change.
New files are packed with the existing Rust layout engine in a new neighborhood
beyond the previous coordinate frame. **Show added files** takes you there; your
current camera position, Back/Forward history, and selection otherwise remain in
place. When City is visible, its last displayed center, zoom, orbit, and tilt are
carried into the refreshed scene. Explicit file-scope repacking still compacts
the selected membership and uses the ordinary camera initialization.

Viewport source previews are observed again after publication under the new
layout revision, so a late old preview cannot populate the refreshed atlas. The
open captured reader keeps its source, page, Find selection, and navigation.
Quick Open, pinned readers, and Reading Desk captures also keep their original
source bytes. Current-range observations are closed because they belong to the
retired current-project authority. File and directory search scopes remain
selected; an old visible-atlas search scope must be selected again.

A partial metadata scan under the same discovery policy never proves that a missing path was deleted. Earlier
parcels not re-observed in that scan stay on the map, are counted as unconfirmed
in the refresh notice, and show an explicit qualification in the inspector.
Only a complete scan removes absent paths from map membership; it does not delete
captured sources, annotations, or reader history. The retained union is bounded
at 20,000 files. If that union or layout admission exceeds its limits, the entire
refresh is refused and the previous project remains available; choose a narrower
file scope before retrying. Refresh before first preparing an atlas still does
only progressive discovery and does not force atlas construction.

Filesystem-change notices remain hints to refresh. A partial first page remains
partial, and the notice clears only after complete discovery without omissions
or a newer observed change. Choosing a different folder or granting access again
replaces the project through the normal opening flow. Refresh does not migrate
annotations between different source captures.

### Automatic live refresh

Enable **Live refresh** in the toolbar's **Refresh** menu to keep this window's
project catalog, atlas and project search current after observed filesystem
changes. It is off initially and scoped to this window. Changes are coalesced
for 750 milliseconds and use the same bounded, cancelable refresh transaction
described above. Repeated saves update the pending change without creating
parallel scans or postponing the coalescing interval indefinitely.

Automatic work waits while you interact with search or move the camera, while
project loading/export is active, and while current-range, streaming-search,
relationship, file-finder, Reading Desk, saved-repository or comparison workflows
are open. These conditions are checked again before installing the replacement.
Open captured and pinned readers keep their original bytes, selection and
navigation. A successful replacement restarts project search under the new
catalog authority.

A newer filesystem event, queued watcher hint, changed grant/root or newly
opened protected workflow prevents an older automatic candidate from replacing
the project. The latest observed change stays pending for a later available
turn. A partial, failed or canceled attempt is not retried indefinitely without
a new change: the notice offers **Refresh project** for an explicit retry or
continued discovery. Partial discovery still cannot establish deletion.

**Pause live refresh** turns the option off and cancels an automatic attempt
while its entered work drains. Turning the toggle off leaves a manual refresh
under its existing explicit cancellation control. Unavailable monitoring or a
moved root suspends automatic work; choosing the folder again restores the
normal access and monitoring flow. Native lifecycle tests cover coalescing,
protected workflows, new-event publication races and retained-reader continuity;
physical-Mac qualification remains pending.

### Opening a source file

```sh
"$APP/Contents/MacOS/fcb" open /absolute/path/to/src/main.rs
"$APP/Contents/MacOS/fcb" open /absolute/path/to/src/main.rs --line 120
"$APP/Contents/MacOS/fcb" open --line 120 -- -notes.rs
```

A regular-file request opens its **immediate parent directory** as the project
and selects that filename once the project root is ready. It does not walk
ancestors looking for a repository root, prepare all source previews, or execute
anything from the project. Parent-directory metadata discovery is part of this
native operation; use `read FILE` or `open FILE --json` for a file-only headless
read without that discovery.

The filename is passed as a bounded hex encoding of its original UTF-8 bytes.
The native endpoint accepts it only against the matching established root,
then opens that exact single-component filename through the retained reader.
Quoting, spaces, tabs, newlines and native
backslashes do not turn into command syntax. Unicode-equivalent but byte-distinct
filenames are not interchanged. A filename is a request for a **new capture**,
not an exact anchor into bytes observed by the launcher.

An explicitly named file does not wait for the optional 20,000-entry atlas
catalog and remains openable when discovery ignores or display filters hide it.
Normal source admission still applies: the retained reader accepts a complete
regular-file observation up to 32 MiB, with no prefix substituted for an
over-limit or failed capture. The startup request is consumed once under the
matching root; project replacement or refresh cannot replay it. It is never
persisted as a filename preference.

`--line N` selects one-based physical line N at Unicode scalar column one using
the reader's checked position service over that same immutable capture. The
native caret uses the validated source-to-UTF-16 mapping. Zero, signed,
noncanonical, repeated and overflowing line arguments are rejected. If the
line cannot be resolved, the native reader keeps its captured first page and
reports the requested line failure. Canceling or opening another source revokes
pending line publication. Refresh from disk starts new capture history without
replaying the startup location. For headless line output, use
`fcb open FILE --line N --json` or `fcb read FILE --line N`.

File requests require the additional `fcb-launch-file-v1` bundle marker. Rebuild
older bundles; the launcher refuses a file request instead of silently dropping
its selection. Line requests additionally require `fcb-launch-line-v1`, so an
older file-capable bundle cannot silently open at line one. Project-only older
bundles retain their directory-launch behavior.
App Store builds do not advertise these markers and independently refuse argv file
activation: use the native folder picker and catalog there.

The Foundation-only endpoint regressions are in
`tests/AtlasStartupFileRegressionTests.swift`, compiled together with
`swiftui/AtlasProjectCatalog.swift` in the configured qualification lane. Rust
launcher tests remain in `crates/fcb-app/src/native_launch.rs`. The production
reader startup/cancellation tests are in `tests/AtlasReaderStartupRegressionTests.swift`,
using the existing `AtlasReaderRefreshFixture.swift` with the production reader
model, coordinator and queue. The existing `swiftui/tests/test-project-catalog.sh`
and `swiftui/tests/test-reader-position.sh` runners include these fixtures for
the strict-RCH qualification lane. Neither these
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

## Saving the current project

Open **Saved Repository → Save current project…** to choose a new FCBS archive
destination. The action captures current on-disk observations using repository
ignore rules, with limits of 20,000 known files, 1 MiB per member and 64 MiB of
total captured source. Source bytes and filenames are stored as plaintext. The
receipt reports captured and unavailable members, discovery coverage and the
actual destination effect, including when cancellation arrives after writing.
Existing destinations are never overwritten. **Open saved archive** opens the
completed saved observation independently of subsequent live edits.

The shared native/CLI writer now releases unused capture quota and temporary
handoff reservations after all capture callbacks finish. FCBS encoding writes
directly into one exactly reserved final envelope, retaining the existing
canonical format and checksum. This removes the former four-times-archive
reservation and duplicate output buffer without raising the 256 MiB managed
budget or the source limits. Capture work still reserves its full working
overlap before reading; unavailable members remain explicit.

`crates/fcb-app/tests/snapshot_export_capacity.rs` exercises the production host
at the 64 MiB source limit, opens the saved final member after a live edit, and
checks that a smaller requested quota still produces a partial archive. Codec
and capture-ledger regressions accompany it. These are unexecuted code-first
regressions; independent batch verification remains pending.

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

### Saved result windows

Literal, expression, symbol, Markdown heading and identifier-occurrence results
offer **First matches**, **Earlier matches** and **Later matches**, alongside
the ordinary **Previous page** and **Next page** controls. Each result window
retains at most 4,096 hits. Later follows the producer's verified match offset,
so results beyond the first retained table—including more than 4,096 identifier
occurrences in one member—remain reachable without increasing that cap.

Earlier repeats the accepted query up to 4,096 qualified positions earlier;
First returns to offset zero. The displayed skipped count belongs to the current
window. Each new window uses a fresh query generation and freezes the accepted
query, matching/count policy and source budget. It revalidates the opened
archive's captured members and rechecks whole-member expression predicates;
it never opens a current-project path.

Counts include skipped matches, while complete coverage and evidence of later
matches remain separate. After cancellation or a failed/lost reply, the browser
reconciles the host's accepted table without repeating the scan. Already opened
readers remain usable. Previous/Next page within one retained window still reads
only metadata.

## Searching saved repository structure

Open **Saved Repository**, choose an FCBS archive, and use **Search** to select
**Symbols**, **Headings**, or **Occurrences**. The scope is the entire opened
saved archive. Search runs on the worker over verified captured bytes; saved
member names are labels and never open working-tree paths.

| Mode | Matching and evidence | Per-member source limit |
| --- | --- | --- |
| Symbols | Case-sensitive exact, prefix, or contains matching against shared outline names, including qualified names. Rust, Python, JavaScript, TypeScript, Go, C/C++, Swift, Java and C# declarations are heuristic candidates. | 64 KiB |
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

## Recovering Reading Desk work

Open **Reading Desk → Recovery… → Enable automatic recovery** to keep local
copies of that desk's captured sources, bookmarks, reading locations and
accepted notes. This is an explicit choice for each open desk. Constructing the
model, opening a repository or merely listing recovery copies does not enable
writes. Copies live in the app's private Application Support folder and are
**unencrypted**. Text still in an open note editor is not included until its
change has been accepted by the desk.

Automatic saves coalesce nearby accepted changes and run on the existing desk
I/O worker. A verified save protects only the exact revision it captured. A
newer edit, an unverified mutation, or an unconfirmed write keeps the existing
close/quit protection active. **Pause automatic recovery** preserves existing
copies. **Save recovery copy now** and **Retry recovery save** let you request
an immediate attempt; **Save new checkpoint…** remains available for an explicit
destination outside the recovery store.

The store keeps the two latest confirmed copies for each desk, with at most
eight desk sessions and 256 MiB in total. Each checkpoint is limited to 64 MiB;
admission reserves space for a new candidate before writing, including overlap
with earlier copies. A successful replacement is published only after the
checkpoint and its separate commit record have been synchronized. It never
evicts another desk's notes to make space. Interrupted or unconfirmed generations
remain visible for inspection and explicit deletion instead of silently consuming
all available capacity. Their source-byte count is unknown until core validation.

A confirmed copy must pass a full checksum check against its original save
receipt, including every captured source and note byte. The core streams this
check through the already-open file descriptor with at most 64 KiB of scratch
space. A damaged body with an intact checksum trailer becomes unconfirmed;
it cannot displace an earlier intact backup during rotation. If verification
is unavailable, the store cannot confirm or automatically retire those copies.

Use **Refresh copies**, select a copy and choose **Open copy…** to restore. A
populated desk requires review before replacement, and an open note editor must
be finished or dismissed first. The core validates saved bytes before adoption;
source paths inside the checkpoint grant no working-tree access. Restoring an
unconfirmed copy still requires a new confirmed save before the desk is treated
as safely persisted. A missing or incomplete payload is delete-only. Deleting
the current persistence copy, or an uncertain deletion outcome, restores the
desk's unsaved-work protection when no other confirmed checkpoint covers it.

Private file permissions, descriptor-relative no-follow operations and a
cross-process lock protect store-owned I/O. Checksum verification uses the pinned
file descriptor without changing its cursor. Save and restore still pass paths
to the native bridge, bracketed by directory identity checks; this is not a
confinement claim against a hostile process running as the same user. Filesystem
and injected model regressions cover body corruption, rotation, quotas,
interrupted copies, accepted revisions, cancellation, lost replies and close
protection. Their execution through the
required verifier, app-bundle compilation and physical-Mac qualification remain
pending. The focused fixtures are in `swiftui/tests/test-desk-recovery-store.sh`
and `swiftui/tests/test-reading-desk.sh`; the existing desk close, note and reader
suites compile the same recovery-enabled model.

## Navigating C and C++ declarations

C and C++ **Symbols** search and retained-reader **Outline** use the pinned
FrankenMarkdown C++ lexer. Comments, quoted literals and raw strings stay
opaque. The shared recognizer follows namespace and type scopes to find
ordinary function definitions and header prototypes, methods, constructors,
destructors, named classes/structs/enums and supported type aliases. Function
bodies and variable initializers cannot contribute declaration-looking decoys.

Each candidate keeps a separate exact name range and declaration range in the
retained capture. Selecting a method highlights its name; copying its
declaration includes its recognized body. Both UTF-8 and BOM-marked UTF-16
preserve original bytes. The same route serves live project search, saved
repositories and retained readers, including captures whose live files have
changed or whose original directory no longer exists.

The language picker calls this **C / C++**. Case-insensitive `.c`, `.h`,
`.cpp`, `.cc`, `.cxx` and `.hpp` filenames use the canonical `cpp` route.
Candidates remain **heuristic outline candidates**: no preprocessor execution,
macro expansion, compiler name binding or complete declaration inventory is
claimed. Function pointer declarators, overloaded operators and advanced
constraint syntax are outside the recognized subset. Conditional branches are
not evaluated; a lexical declaration is not proof that a compiler enables it.

The existing 64 KiB original/decoded-source, 4,096-line/item and bounded-scope
limits apply. Unsupported lexical transformations and malformed delimiters fail
explicitly while ordinary source reading remains available. Authored regressions
cover comment/raw-string decoys, nested methods, header declarations, exact
UTF-16 names, saved archives and opening a retained search result after a live
edit. Independent executable and physical-Mac verification remain pending.

## Navigating Python declarations

Python **Symbols** search and retained-reader **Outline** recognize `class`,
`def` and `async def` candidates through the pinned FrankenMarkdown lexer.
Comments, docstrings, raw/bytes/formatted strings and multiline expression
contents stay opaque, so declaration-looking text inside them cannot create
false navigation entries.

The bounded recognizer follows logical statements and indentation iteratively.
It supports multiline signatures, explicit line continuations, decorators before
declarations, generic type-parameter headers, nested classes and functions, and
methods under classes. Names have exact ranges separate from the declaration
range; copying a declaration includes its recognized indented body. Both UTF-8
and BOM-marked UTF-16 copies retain the original captured bytes after live files
change. Live projects, saved repositories and retained readers share the route.

These are **heuristic outline candidates**. Decorators are not resolved,
assignments and dynamically created definitions are not indexed, and no Python
code is executed. LF/CRLF input, space/tab indentation, 64 KiB source, 4,096
physical lines, bounded scopes and bounded output define the supported subset.
Incomplete strings, mismatched delimiters, unsupported whitespace and incomplete
declaration headers fail explicitly while the ordinary source reader remains
available. Unit regressions and a C-ABI retained-reader journey cover opaque
strings, nested declarations, cancellation and UTF-16 name/body copying;
executable and physical-Mac qualification remain pending.

## Navigating Go declarations

**Symbols** search and retained-reader **Outline** now use the shared
FrankenMarkdown Go lexer to recognize declaration candidates. Comments, raw
strings, interpreted strings and rune literals cannot create outline entries
from declaration-looking text.

The supported navigation subset includes package-level functions and methods,
generic functions, individual and grouped type declarations, and grouped or
individual `const`/`var` bindings. Generic parameters stay outside the selected
name. Struct and interface declarations have distinct kinds; their fields and
methods, function-local declarations and initializer bodies remain opaque.
Receiver methods are listed as method candidates without claiming that a
receiver has been resolved to a particular type.

Selecting a result highlights its exact name in the same retained capture.
Copying the declaration uses its captured source range, including a recognized
function body. UTF-8 and BOM-marked UTF-16 preserve original-byte selection and
copying even after the working-tree file changes. Live workspace search, saved
repository search and retained outlines use the same adapter.

These remain **heuristic outline candidates**, not compiler definitions or a
complete package inventory. The existing 64 KiB source, 4,096 physical-line and
bounded output limits still apply. Unsupported or malformed lexical context is
reported explicitly, with the ordinary source reader still available. Focused
unit and live/saved-to-retained-reader regressions are included; executable and
physical-Mac qualification remain pending.

## Navigating Swift declarations

Swift source now participates in **Symbols** searches in live projects and saved
repositories, and in the retained reader and Reading Desk **Outline** tools.
The reader infers Swift from the filename (including an uppercase suffix); its
language selector also offers **swift** for captures with another extension.
Choose a candidate to select its original name bytes, or copy its declaration
evidence from the same retained capture. UTF-8 BOM and BOM-marked UTF-16 captures
preserve their original byte coordinates.

The Swift route recognizes structs, classes, actors, enums, protocols,
extensions, nested types, functions, methods, initializers, deinitializers,
subscripts, simple stored/computed properties and type aliases. Protocol
requirements without bodies remain navigable. Single escaped identifiers select
the spelling inside their backticks; qualified extension names retain their
complete original spelling. Access modifiers, attributes and generic signatures
do not change the candidate's identity.

The pinned FrankenMarkdown Swift lexer owns comment and string boundaries.
Nested comments, raw/multiline strings, imports, function bodies, property
bodies and closures do not contribute declarations. This is a conservative
declaration recognizer: local declarations, enum cases, tuple and secondary
comma-separated bindings, macro expansion and compiler name resolution are
outside its scope. It does not evaluate conditional compilation or claim a
complete Swift grammar or symbol inventory.

The shared 64 KiB original/decoded source, 4,096-line and 4,096-candidate limits
apply. Swift additionally caps delimiter nesting at 128 and a retained name at
4,096 UTF-8 bytes. Mismatched or unclosed delimiters produce an explicit
extraction refusal while the source remains readable; reaching the item limit
keeps the existing partial-result label. Cancellation and replaced query
generations retain the existing reader/session authority checks.

Regression cases cover actual extraction, CLI file/workspace/offline-archive
routes, retained-reader selection and copy, original UTF-16 offsets, protocol
requirements, opaque syntax, limits and cancellation. The native decoder admits
the Swift language without changing evidence or capture validation. These
cases are authored; executable Rust/Swift verification and physical-Mac
qualification remain pending.

## Navigating Java and C# declarations

Java and C# source now participates in **Symbols** searches in live projects,
retained indexes and saved repositories, and in retained-reader and Reading Desk
**Outline** tools. Filenames ending in `.java` or `.cs` select the language,
including uppercase suffixes. The reader also offers **Java** and **C#** for
captures with another extension. The headless language selectors are `java`
and `csharp` (`cs` and `c#` are aliases).

| Language | Navigable declaration candidates |
| --- | --- |
| Java | Classes, interfaces, enums, records, annotation types, nested types, ordinary and compact constructors, methods, generic methods, interface/abstract signatures and annotation member defaults. |
| C# | Block and file-scoped namespaces, classes, structs, interfaces, enums, records, nested types, constructors, generic methods, explicit-interface methods, tuple-returning methods and accessor/expression-bodied properties. |

Choosing a candidate selects its exact original name bytes; declaration copying
uses the same captured source. UTF-8 BOM and BOM-marked UTF-16 in both byte
orders preserve their original coordinates. A C# `@name` selects the name after
the escape prefix; a dotted namespace preserves its full source spelling,
including intervening separators and spacing.

The existing FrankenMarkdown lexers own comments, strings, text blocks,
interpolation and token boundaries. Method bodies, field initializers, local
declarations and enum constants do not contribute candidates. Fields, delegates,
events, indexers, destructors and operator declarations are outside this slice.
C# conditional compilation is not evaluated. Every result remains a
`heuristic-outline-candidate`, without compiler binding or an exhaustive
symbol-inventory claim.

The shared limits remain 64 KiB of original and decoded source, 4,096 lines and
candidates, 128 delimiter/scope levels and 4,096 UTF-8 bytes per retained name.
Unbalanced delimiters fail closed. Java pre-token Unicode escapes and
unsupported line terminators, and C# identifier escapes outside opaque
comments/strings, return `SYMBOL_UNSUPPORTED_LEXICAL_CONTEXT`. Refusal leaves
the source readable and keeps search coverage explicitly incomplete.

Authored regressions exercise the shared extractor, CLI file/workspace/offline
archive routes, live and indexed structural search, retained C-bridge selection
and copy, and strict native language admission. Independent executable and
physical-Mac verification remain pending.

## Following imports in a saved repository

Open an FCBS archive in **Saved Repository**, choose **Imports**, and select
**Build imports**. This uses the same Rust, JavaScript/TypeScript/JSX/TSX,
Python, C/C++, Go and Java lexical import engine and finite catalog policies as Project relationships.
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

## Following saved reading trails

Open an FCBS archive in **Saved Repository**, choose **Reading trail**, then
**Open reading trail…** to select an FCBT file. The list preserves the saved
order, repeated visits, exact original byte ranges and user rationale. Use
**Previous item**, **Next item** or the paged list to select an entry, then
**Open selected source** to read it.

A trail can reference several archives. Every entry remains visible, including
ones marked **Requires a different saved archive**, missing, uncaptured or
changed. For an entry in another archive, choose **Choose archive and open…**
and select its FCBS file. The selected file must match the visit's archive
digest, then prove its exact member path, source digest, length and original
range before the independent reader opens. The current archive, accepted trail
and search stay available; the selected file is used for this capture only.
A wrong archive or failed read leaves the current reader intact.

The archive picker is also available when **Previous trail item** or **Next
trail item** reaches an other-archive visit. **Back** and **Forward** restore
retained primary and supplemental readers, and the same trail controls keep
following the original sequence. The supplemental reader supports exact source
copying and **Keep trail range in Desk** after its temporary file grant ends.
Archive-local document links, images and trail authoring require their own
archive authority; a supplemental reader does not inherit the primary
archive's authority for those operations.

The reader's **Previous trail item** and **Next trail item** follow the saved
sequence across page boundaries. An unavailable visit leaves its rationale
selected and the current reader intact; navigation can continue from that
visit. **Back** restores an earlier retained reader. **Return to trail range**
returns to its context, and **Keep trail range in Desk** preserves the complete
original range for notes and bookmarks, including zero-length point bookmarks
and selections that span several reader pages.

Attachment reads at most 4 MiB and 1,024 entries. Pages contain at most 32
entries and load reference metadata only. Source comes from the retained primary
archive or the explicitly selected supplemental file, with a bounded initial
16 KiB context window. Supplemental opening reuses full-envelope validation
with a 256 MiB byte allowance and 64 MiB directory admission; a stale trail
selection is rejected before opening the supplied path. The file grant lasts
through verification and capture, including canceled worker drain. An independent
reader retains its verified bytes after the trail or archive closes. Failed or
early-canceled replacements preserve the preceding attachment. Late cancellation
can arrive after acceptance; **Reload state** recovers the accepted attachment
after an unavailable reply without reopening the trail pathname.

Supplemental visit opening has host, registry, C ABI and native fixture coverage
in source. Executable regression and physical-Mac picker/lifetime verification
remain pending the independent strict-RCH verification campaign.

### Creating and extending a trail in the native reader

Open a captured member, search hit or trail entry, then choose **Add to reading
trail…** in its saved reader. Choose **New trail** with a title, or **Append to
attached trail**. The editor starts from the verified selected text or the
source's exact match/trail range when available. **Use source range** also offers
the current window, the complete member and a point bookmark. Original byte
offsets remain editable within the verified member length.

Add an optional rationale and choose **Save new trail file…**. Titles admit up
to 256 UTF-8 bytes and rationale up to 4,096 bytes. An append preserves every
existing visit, repeated selection and note, including entries for other
archives, and records the original trail's digest as its parent. The source is
verified through the already opened archive descriptor, so replacing the
archive or trail pathname cannot silently substitute new content.

Every save uses an explicit new destination and refuses to overwrite an
existing file. The attached trail remains available until you deliberately
choose **Open reading trail…** for the new file. Cancellation or an unavailable
reply may follow a filesystem effect; the editor reports the retained write
outcome and **Reload write receipt** retrieves it without repeating the write.
Uncertain or refused outcomes preserve the draft. Source/range/parent changes
while the save panel is open require reviewing the draft again.

CLI commands also create and append immutable trails with an explicit new
destination. For example, after saving `project.fcbs`:

```sh
fcb trail add project.fcbs --member src/main.rs --start 0 --end 80 \
  --note 'Start at the entry point' --title 'Project tour' --output tour.fcbt
fcb trail add project.fcbs --member src/lib.rs --start 0 --end 120 \
  --note 'Follow the public interface' --from tour.fcbt --output tour-expanded.fcbt
```

Choose ranges within each captured member. Ranges are half-open original byte
offsets, not text columns. Trail files contain notes and references; the saved
archive supplies the source. The native attachment, authoring, reader navigation
and Desk handoff have host, C-boundary and Swift regression fixtures. Independent
execution and physical-Mac qualification remain pending.

### Exporting a saved trail with its source

In **Saved Repository → Reading trail**, choose **Export source pack…** to save
the attached trail's notes, exact source references and verified selections in
a new self-contained JSON file. Presets admit up to **64 KiB**, **1 MiB** or
**4 MiB** of selected original bytes. The save panel explains that source,
paths and notes may contain secrets; the export does not redact or encrypt
them. Existing destinations are never overwritten.

The pack uses the same `fcb.trail-source-pack/1` representation as CLI trail
export: exact original bytes are lowercase hex, preserving malformed text,
byte-order marks and selections inside a text scalar without inventing a decoded
representation. Saved order, repeated visits, zero-length bookmarks and rationale
remain intact. It is a source-evidence artifact, not an FCBT trail attachment or
an archive, and its notes are never executed.

The ordinary presets use the currently retained FCBS archive descriptor.
To include visits from other repository captures, choose **Export source pack…
→ Include other saved archives…**, pick a size preset and explicitly select
**one to seven additional archives** in the file panel. Then choose a new export
destination. The retained archive is always included, for at most eight inputs.
Supplemental selections belong to that export only; they do not attach archives
to the browsing session or change which trail entries can be opened there.

Every supplemental archive is fully validated before source selection. Routing
uses its verified archive digest, so identical paths in different captures
cannot substitute for one another. Duplicate digests share the first verified
handle after all supplied copies have been validated; explicit trail visits and
their notes are still preserved separately. Replacing the current archive's
pathname or the attached trail pathname cannot substitute new source.

Supplemental envelope validation has a separate **256 MiB** aggregate limit.
Admission counts each selected file's observed length plus one EOF probe; each
archive retains the **80 MiB** envelope ceiling, and supplemental directories
share **64 MiB** of managed admission. Invalid, unreadable or over-budget
supplemental files refuse the export before creating its destination. Receipts
include the ordered selected-path inventory, validation work and duplicate
counts. The existing member-verification and selected-source budgets apply
across all archives, including repeated visits.

Visits referencing an archive that was not supplied, missing members or changed
captures keep their notes and explicit unavailable reasons. Source limits omit
whole selections, with the omission recorded beside the reference. This can
produce a complete JSON file with partial source coverage; the receipt reports
those outcomes independently.

Each operation admits at most **1,024 visits**, **64 MiB** of whole-member
verification and **16 MiB** each of complete encoded bytes and Unicode scalars
in the JSON representation. All admitted notes and provenance are reserved
before source reads. An oversized metadata envelope refuses before creating
a destination. Only one selected member is retained during verification;
repeated visits count against the source-work allowance separately.

The accepted trail, queries and opened readers remain available after export.
Cancellation waits for the entered worker to drain, then reports the actual
destination effect. **Reload export receipt** recovers the retained terminal
result without reading or rewriting the destination. A missing reply never
automatically retries a write; inspect an uncertain destination and choose a
different new filename for another export. The destination and selected-archive
grants remain held until the entered worker finishes, including cancellation
or closing the view.

The host, registry, public-header marshaling and native model regression sources
cover exact bytes, unavailable visits, whole-selection limits, metadata refusal,
retained descriptors, mixed-archive digest routing, duplicate inputs, validation
limits, exclusive creation, cancellation, lost replies and grant retirement.
Independent strict-RCH execution and physical-Mac save-panel
qualification remain pending.

## Comparing retained source ranges

Use **Pin source** in two retained readers, then choose **Compare** in **Pinned
sources**. The first pin is Before and the second is After. Each source can
contain up to 4 MiB for this comparison; the comparison uses their retained
bytes without reopening either live file. Saved-capture **Compare file** uses
the same reader controls.

**Open pair** selects a reported range on both sides. **Previous change** and
**Next change** skip equal ranges and include explicitly unresolved ranges;
the selected row shows its position among the changes. **Show equal ranges**
also exposes unchanged spans. **Refine** retries a budget-limited comparison
with the larger bounded allowance. Both source windows are prepared before
either is installed, so a failed or canceled change navigation preserves the
previous accepted pair.

Each reader distinguishes the complete compared byte range from the part
displayed in its bounded page. For a long range, **Range → Show range start**
and **Show range end** reach its ends. **Copy range** offers the shown text,
the shown original bytes as hex, or the full range location with capture
identity. **Keep compared range** preserves the complete target in the Reading
Desk, including bytes beyond the displayed page. Ordinary page copying remains
separate from copying the compared range.

Added or removed spans can have an empty side: its point anchor remains empty
and uses an exact caret when representable. A difference can also split a
UTF-8 scalar, UTF-16 unit, composed character or CRLF pair. Those byte boundaries
are not rounded into an approximate text highlight; exact shown bytes and the
full location remain available. Pages with decoding replacements likewise keep
their byte evidence without claiming an exact native text selection. These
comparisons describe the chosen captures and do not establish Git history or
automatic note relocation.

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
reader, with paired range selection and Previous/Next change navigation as
described above.
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
expression or changing **Match case** reuses the prepared captures. Changing
the accepted discovery policy starts a new catalog and capture preparation;
a paused build or completed index under the other policy is never reused.
**Refresh** captures current sources again. Expression filters narrow a query over the
prepared index; they do not change which sources the index originally captured.
Widening a query beyond that captured scope can therefore leave files
uncaptured until Refresh. Indexed queries and result activation do no live
source reads.

Each live query or captured-index result window retains at most 1,000 primary matches. **Count all matches** starts
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

### Later result windows in a captured index

With **Captured index** enabled, **Previous results** and **Next results** reach
matches after the first 1,000 for Text and Expression searches. The sidebar
shows the current ordinal range separately from the total count and coverage.
Each window keeps at most 1,000 exact source witnesses, while a completed count
can include every match in the admitted captures.

Changing windows verifies the same immutable captures with the same query,
case choice, counting choice, file types and location scope. It performs no
additional source reads. Skipped matches still consume the verification-work
budget, so a far window can remain partial when that budget is exhausted.
Next is available only when the engine has actually observed a later match;
an absent Next button does not make partial coverage complete.

The accepted rows and current reader stay available while a later window is
prepared. A failed or canceled replacement preserves them. Refresh, a changed
project grant, or another query invalidates the old continuation rather than
opening current files under its capture identity. A successful window receives
fresh query and presentation identities, keeping delayed clicks from selecting
a different occurrence with the same local row number.

The shared engine, host, C ABI, native worker and transport have regression
sources for later windows, overlapping matches, UTF-16, expression predicates,
cancellation, scope changes and retained reader ownership. Executable Rust/Swift
and physical-Mac verification remain pending.

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

The relationship reader also follows local links from its Markdown preview.
It matches the exact preview occurrence against the retained documentation
inventory, including its original destination bytes and source capture, then
opens the captured target at its qualified heading or source-line range.
Resolving a link pages metadata without changing the Inspector's projection,
file filter or page. Missing targets and occurrences outside a partial inventory
leave the current reader in place.

**Back** and **Forward** restore the last three independently retained readers,
including their source positions and preview state. Failed or canceled opens
preserve Forward; a successful new visit releases the abandoned forward entries.
The oldest reader is released when the bound fills. **Keep in Reading Desk**
keeps the verified source selection or current byte window through the existing
capture-transfer path. Refreshing the project preserves reading history but
revokes onward links from sources belonging to the previous capture.

Evidence identifies the **whole captured Markdown document**. The parser's
destination establishes the relationship, but this surface does not claim an
exact source span for the link token. Opening `guide.md#section` now checks the
selected target's canonical heading identity through the shared Markdown
adapter, then starts the reader at the verified original byte range in that
**retained capture**. Percent escapes are decoded once; a query suffix does not
change the captured file. List pages do not parse target headings eagerly.
Missing, ambiguous, invalid, unsupported or over-budget fragments open the
captured file at its beginning with an explicit explanation. Non-Markdown line
fragments such as `code.rs#L20` use the shared retained-source line resolver;
unavailable line ranges fall back to the beginning with an explanation. Reading the
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
Markdown link extraction accepts UTF-8 and BOM-marked UTF-16 in either byte
order. The 256 KiB source bound applies separately to original bytes and decoded
parser input. Each occurrence retains the whole original Markdown capture as
evidence, including its BOM. Malformed or oversized documents leave coverage
partial while other links remain available; captured UTF-16 targets stay readable.

**Rebuild links** reuses the prepared source captures. **Refresh sources**
captures the current project again so file edits can affect the inventory.
Already opened readers and pins retain their original bytes. Cancellation and
replacement reject stale link responses; an old link identity cannot authorize
an open against a replacement index.

Host, registry, C-boundary and native regression fixtures accompany this
workflow. They remain unexecuted in this code-first session; compilation and
physical-Mac interaction verification are pending.

## Direct relationship neighborhoods

**Project relationships** now shows a **Direct neighborhood** map when an exact
file is selected. Choose **Documentation** or **Imports**, build the retained
inventory, and use an Inspector relationship action or **Use path** to select a
catalog file. The current **All**, **Outgoing**, **Backlinks** or **Imported by**
filter determines which direct evidence records appear.

Arrows connect the source and target parcels at positions supplied by that
relationship inventory's own retained atlas. Orange indicates outgoing
connections and blue indicates incoming connections. Select a parcel to inspect
that file's incoming and outgoing relationships; the existing evidence list and
exact source/target reader actions remain available. A selected file with no
retained relationships still has its own parcel.

The map groups repeated directed pairs and shows their per-page evidence count
as **×N**. At most **32 distinct connections** are drawn from the current
bounded result page. Its summary separately counts records without catalog
targets, records beyond the map limit, and records on other result pages.
Changing the page changes the displayed neighborhood. An uncaptured catalog
target can have a parcel without gaining a source-reader action.

Positions belong to this captured relationship inventory, independently of the
main atlas's current camera or display filter. Changing live files does not
change these retained positions or evidence; **Refresh sources** creates a new
inventory. Old-page clicks cannot activate a replacement page. Older hosts
without geometry continue to provide the evidence list.

This view presents direct documentation evidence and lexical import candidates.
It does not compute transitive reachability, strongly connected components or
compiler dependencies. Those graph analyses remain with the designated shared
graph engine. Authored host/native cases cover scope changes, live edits,
missing targets, zero-edge files, duplicate evidence, map limits, stale layouts
and malformed geometry; executable and physical-Mac verification are pending.

## Project import navigation

In **Project relationships**, choose **Imports**, then **Capture and build
import candidates**. The inventory reuses the same captured sources as the
documentation view. **Outgoing** shows import statements in one file;
**Imported by** lists statements whose catalog candidate is that file.
**Read import evidence** opens the original statement's exact capture and byte
range. **Read target candidate** opens an independent reader over the retained
target, even if the live files have since changed.

The shared import engine supports Rust external module/use declarations,
JavaScript/TypeScript static imports, re-exports and literal load expressions
(including JSX/TSX), Python `import` and `from` statements in `.py` and `.pyi`
files, C/C++ includes, Go package imports and Java type/static imports. Python aliases,
comma-separated imports, parenthesized from-import lists and explicit line
continuations preserve their original statement evidence. Comments and strings
are classified by the shared FrankenMarkdown lexer.

Bare `require('./worker')` and literal `import('./worker')` expressions now
participate in the same **Outgoing**, **Imported by**, evidence and target-reader
flows. TypeScript `import Worker = require('./worker')` retains the exact
`require(...)` expression. Comments, newlines and an optional trailing comma
are admitted around one quoted argument. Each row keeps the original call and
specifier byte ranges, including BOM-marked UTF-16 coordinates.

These rows are lexical candidates. A bare `require` can refer to a locally
shadowed function; an `import()` spelling can occur in a TypeScript type query.
The labels do not establish a runtime load or compiler binding. Relative
specifiers resolve only against the captured catalog: an exact path takes
precedence; an extensionless path can otherwise offer a unique supported
extension or directory-index candidate. Competing paths remain ambiguous. Bare package names stay
external; the resolver does not read package manifests, follow Node resolution,
or execute source.

Member calls such as `loader.require(...)`, constructors and `require.resolve`
are outside the load-expression grammar. Computed expressions, template
specifiers and multiple arguments (including import options) disclose partial
coverage. Escaped quoted spellings retain unresolved evidence. If the pinned
lexer cannot safely classify a regex-versus-code boundary, extraction refuses
that source with partial coverage rather than publishing apparent imports from
inside the regex. This conservative boundary also declines newline-leading
division. The same policy applies to saved repository imports and their CLI
output.

Rust `use` trees produce one retained occurrence per imported path, including
aliases, nested groups, grouped `self`, and glob imports. For example,
`use crate::worker::{self, Task as Job};` produces `crate::worker` and
`crate::worker::Task`. Each occurrence keeps the exact original statement and
the original bytes of its own leaf (`self` or `Task`); the displayed destination
is a normalized path, not a verbatim source excerpt. Malformed or over-budget
trees remain unresolved evidence with partial coverage. Nested scopes, macros,
and explicit path overrides do not acquire conventional file-resolution authority.

Qualified Rust `crate::`, `self::` and repeated `super::` paths can navigate
conventional module candidates in the captured catalog. The resolver checks
both `name.rs` and `name/mod.rs`; competing module files or inferred crate roots
remain ambiguous. A qualified path with a known module prefix, such as
`crate::worker::Task`, can open the captured worker module as a **containing-module
candidate**. The row and opened reader distinguish this from an exact module-file
candidate: neither establishes the imported item's definition or compiler binding.

Root inference uses the nearest conventional `lib.rs` or `main.rs`, with bounded
ancestor and path-component traversal. It stops at automatic-target boundaries
such as `src/bin`, `tests`, `examples` and `benches`. Bare/external imports,
unknown roots, unsupported module spellings and exhausted limits retain readable
evidence without a guessed target. Custom Cargo layouts, conditional compilation,
re-exports and `#[path]` resolution require stronger evidence than this policy
provides. The same navigation policy applies to saved archive members without
reopening a working tree.

Python navigation uses an explicit catalog policy. `from .worker import Tool`
looks for the unique `worker.py` or `worker/__init__.py` candidate beside the
source; additional leading dots ascend catalog directories without escaping
the root. `from . import helper` offers the corresponding `.helper` candidate.
Its exact specifier evidence is the imported name `helper`; the displayed
destination combines that name with the statement's relative prefix.
Absolute `import package.worker` and `from package.worker import Tool` consider
`.py` and `/__init__.py` paths under the catalog root, its conventional `src/`
directory, and at most one enclosing regular-package root established by
consecutive catalogued `__init__.py` parents of the importing source. A missing
marker stops that ancestry walk. Identical roots are deduplicated; multiple
possible paths remain ambiguous with no priority or target chosen. An absolute
name with no candidate remains external.

These lookups use catalog metadata only. They do not search `sys.path`, execute
package code, inspect build configuration, infer namespace-package roots or
installed dependencies, or resolve imported members to additional modules.
The native display labels absolute results as **Python source-file candidates**
and says that runtime import binding is not resolved. Absolute resolution admits
at most 64 source-directory and destination components, a 1 KiB destination,
16 KiB candidate paths and 70 catalog lookups, using fixed scratch storage.
Bare-relative wildcard imports have no guessed target. Live and saved regression
cases cover exact retained readers, UTF-16 ranges, raw parent names, conflicting
roots and unavailable captures; executable and native verification remain pending.

C and C++ literal includes participate in both Project and Saved Repository
Imports. Quoted `#include "header.h"` can open an exact source-relative catalog
candidate. Angle headers stay external; computed and macro includes stay
unresolved. Statement and specifier ranges preserve the original UTF-8/UTF-16
bytes, and evidence/target readers retain their accepted captures. The policy
does not infer compiler include directories, expand macros, or evaluate
conditional branches.

The pinned lexer's coverage limits are explicit: a capture containing any
backslash-newline (including multiline macros), trigraph, bare-CR line ending,
or raw-string delimiter outside the upstream-supported subset is counted
unsupported. Its import coverage stays partial and its source remains readable.

Go single and grouped imports in `.go` files keep aliases, blank imports and
dot imports in their exact source evidence. Quoted and raw import paths share
the original UTF-8/UTF-16 byte mapping. With a captured `go.mod`, a path inside
that module can navigate to the lexicographically first direct `.go` file in
the package directory, excluding `_test.go` and dot/underscore-prefixed files.
The row identifies this as a **representative Go package file**. **Imported by**
attaches to that representative file, not every source file in the package.
Build tags, operating-system/architecture constraints and compiler bindings are
not evaluated.

Resolution uses the nearest catalogued `go.mod` and its captured module
directive. Missing, uncaptured, unsupported or over-budget metadata leaves the
import explicitly unresolved; the resolver never falls through a nested module
to its parent. A local import that crosses another catalogued module boundary
is also unresolved. Imports outside the captured module remain external. The
policy does not interpret `go.work`, `replace`, vendor trees, module caches or
escaped path spellings, and does not run Go or fetch dependencies. Include
`go.mod` in a custom capture scope to enable module-local destinations.

Module metadata is capped at 64 KiB per lookup and shares the 32 MiB aggregate
work allowance. Project receipts report `parsed_module_bytes` separately from
`parsed_source_bytes`; both count against the parse limit. Saved-archive reads
verify the metadata digest, count it in `member_bytes_read`, and keep combined
source and metadata payloads within 128 KiB. Paging accepted imports performs
no new source or metadata reads. Evidence and target readers keep their exact
captured bytes after metadata edits, projection replacement or archive detachment.

Java imports in `.java` files now participate in the same live, saved-repository
and reading-desk navigation. Ordinary type imports, nested types, static member
imports and static on-demand imports retain the whole original statement and
its exact specifier bytes, including BOM-marked UTF-16 coordinates. Comments,
strings and text blocks do not contribute import statements. Java pre-token
Unicode escapes, unsupported line terminators and malformed import statements
leave source coverage explicitly incomplete.

For `import example.Tool;`, the finite catalog policy considers
`example/Tool.java` beneath the repository root, `src`, `src/main/java` and
`src/test/java`. For a source within a conventional nested module, the same
four roots beneath the nearest `src`-owning module are also eligible. A static
import first removes its final member or `*` component. Shorter dotted prefixes
can identify an **enclosing Java type candidate**, as with
`import example.Tool.Nested;` and `example/Tool.java`. A target must be unique
across every eligible source path; competing roots or enclosing types remain
ambiguous, with no reader target authorized.

These are conventional **Java source candidates**: the policy does not inspect
package/type declarations, resolve classpaths, interpret Maven or Gradle, or
establish a compiler binding. It opens the retained candidate at its beginning,
without claiming the imported declaration's location. An ordinary on-demand
import such as `import example.*;` keeps readable evidence with no individual
target chosen. Missing, uncaptured and unavailable candidates likewise retain
their evidence and explicit status. Saved navigation uses archive catalog
membership and verified captured bytes without reopening a working tree.

The inventory admits 128 KiB per source, 32 MiB of aggregate parsing and 4,096
retained import occurrences. Unsupported syntax, malformed decoding, parse limits and
omitted rows remain visible coverage conditions. These limits apply to the
captured project import browser. The **Saved Repository → Imports** workflow
applies the same lexical policies to archived members. Rust/Swift regression fixtures cover Rust
use trees, JavaScript literal loads, Python imports, C/C++ includes, Go module
candidates and Java type/static imports, UTF-16
coordinates, relative targets, ambiguity, stale captures and native evidence
validation. Executable and physical-Mac verification remain
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

Choose **Search large project files…** in the search sidebar to search text,
expressions or exact whole identifiers in the sidebar's selected location and file types,
using independent, progressive project discovery for workspace, folder and exact-file
scopes. Content search continues beyond the atlas's 20,000-file ceiling without
building a larger map or retaining every discovered pathname. Frozen visible-atlas
selections keep their explicit map membership. All four selections use the same
location and file-type predicates as ordinary search. Rust scopes
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

Choose **Expression** to combine literal terms, quoted phrases, exclusions such
as `-generated`, and `path:` or `lang:` filters. For example,
`"retry request" timeout -deprecated path:src/ lang:rust` finds occurrences of
`retry request` in admitted files whose observed text also contains `timeout`
and does not contain `deprecated`. The first positive literal supplies the
counts, retained bytes and highlights. The shared engine parser admits up to
64 terms and filters within the 1,024-byte query limit; regular expressions
remain unavailable. **Match case** applies to all text predicates. Opening the
sheet from the sidebar's Expression mode preserves that mode and the raw query.

Every required and excluded term is checked against the same continuous file
observation, including terms beyond the retained-hit allowance or earlier
capture limits. An observation cut short by an I/O limit, Stop or a decoding
failure cannot provide expression hits. Reaching the observed end establishes predicates over
those observed bytes; it does not establish an atomic file snapshot. Changed or
unavailable final metadata remains disclosed and keeps overall coverage partial.
Retained result ranges identify primary literal occurrences. Their bounded
excerpts may include nearby text, but do not retain independent evidence for
every other required or excluded term.

Expression path and language filters intersect the frozen sidebar location and
file types before source reads. UTF-8 root-relative paths are filtered directly;
non-UTF-8 paths use the engine's escaped search key. The summary separately
reports files excluded by these expression filters and by native location/type
selection. **Count all matches** stays on in Expression mode because a file must
be observed through its end before all text predicates can be established. The
selected byte and read-call limits still apply, at most 256 rows are retained,
and **Stop and keep matches** remains available. Returning to Text or Occurrences
restores the user's counting preference.

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
selection. Reopen the sheet to choose another scope. Workspace, folder and exact-file
searches observe current membership anew on each query; their discovered and eligible
file counts grow as the walk advances. The progress display keeps the total unknown
while more discovery remains and separately reports discovered files awaiting scope
admission. For a visible-atlas scope,
choose the atlas view in the sidebar first. Files absent from the search catalog
remain separately reported as missing requested files, preventing a false
complete result. Location/type exclusions are reported separately from discovery
policy exclusions and do not consume file-read, byte or result budgets.

Open the sheet, enter the literal, expression or identifier, choose **256 MiB**, **1 GiB**, or **4 GiB** as
the total source I/O budget, and press **Search**. There is no per-file capture
ceiling. The scan admits at most 1,048,576 read calls and retains at most 256
matches. It uses a 16 KiB input buffer. Exact Text, Expression and Occurrences share one
encoded primary witness per file with retained matches; lowercase Text and Expression retain each
hit's original spelling and bytes. Each retained hit also keeps up to 160 original
bytes before and 160 after the match, from those same reads. Context residency
stays bounded per match; the scan does not retain a separate whole-file capture.
While the scan runs,
the sheet shows cumulative byte/read progress and previews up to the first 32
matches from finalized file observations. The unfinished file contributes I/O
progress, but its tentative matches are withheld until its observation finishes.

For Text, Expression and Occurrences, **Scan later matches** and **Scan earlier matches**
open another bounded result window when available. Each action scans current
files again from the beginning, charging skipped matches against the same I/O
budgets. The new window replaces the previous observation and receives fresh
query and source identities; files may have changed between scans. Progressive
queries also rediscover membership, and discovery order can change between walks.
These ordinal windows therefore describe new observations, not stable pages of a
frozen workspace result set. A regular
**Search** starts again at the first window. Each Expression window counts all
qualifying primary matches within the I/O budgets and retains up to 256 results
after the requested number of skipped matches. Required and excluded predicates
are re-evaluated against each new file observation, including files whose
primary matches all precede the retained window.

Enable **Count all matches** to continue through the selected files after the
256 retained rows fill. The total includes overlapping occurrences and matching
files with no retained rows. The sheet distinguishes the total counted matches
from the rows available to open or copy. A finished, unchanged observation can
establish a complete count even when additional rows were omitted. If a byte or
read-call budget, unsupported text, unavailable file, changed source or Stop
prevents complete coverage, the displayed count remains partial. Counting uses
the same memory, I/O and cancellation limits; it does not retain more source or
occurrence rows. Without this option, the first additional match beyond the row
allowance still stops Text and Occurrences scans. Changing the option revokes the previous query's
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

Workspace, folder and exact-file search has no total discovered-file cutoff. It
retains at most four discovery pages of 256 entries each, one active streaming
reader, 256 result rows and a bounded diagnostic prefix. By default, the shared
`repository-rules-v1` policy observes supported `.gitignore` and `.fcbignore`
rules. Traversal still has separate path, depth, pending-directory and rule-reading
limits; a refusal remains visible as incomplete discovery. Matches keep their raw
path and original bytes independently of discovery-page eviction, so a result
from a later file can still be copied or opened in the current-context reader.

Visible-atlas search continues to use the separately retained atlas catalog, with
at most 20,000 regular files and the same explicit region membership. **Reset
search** releases a progressive query's results and prepares another query within
the frozen selection; **Refresh file catalog** rediscovers the visible-atlas
catalog. The next progressive query observes current membership again, including new files.
Neither path claims an atomic workspace snapshot or treats undiscovered files as
proven no-match results. The summary separately reports incomplete discovery,
unexamined files, unsupported text, unavailable files, failed reads, and byte,
call or match-limit stops, and user-requested stops. Excluded entries include pruned directories; their
count is not an invented count of all files beneath them.

Result rows show a retained excerpt when its decoded boundaries can be verified.
Select a result to inspect the excerpt around the highlighted match. The excerpt
requests 160 original bytes on each side, clipped to the observed file bounds.
It uses only bytes already read by that search; reaching a byte, read-call or
match limit never triggers additional reads to fill its suffix. A short suffix
is labeled **partial excerpt**, independently of file or query completeness.

The displayed text uses scalar and CRLF boundary guards and can cover less than
the retained original-byte range. Both ranges are shown when they differ. CRLF
is preserved; a leading BOM is represented in the original bytes and excluded
from decoded text. **Copy excerpt** copies the verified displayed text, while
**Copy excerpt hex** preserves every retained excerpt byte, including edge bytes
that cannot form a text highlight. When guarded decoding is unavailable, the
literal and original hex remain inspectable. **Copy match** and **Copy original
hex** continue to copy only the exact match's decoded spelling and encoded bytes.
Selecting or copying these retained values performs no source I/O and remains
meaningful after the working-tree file changes or disappears.

**Read current context** explicitly opens the current canonical pathname
under the current project access grant, creates an independent observed reader,
and checks the literal's original bytes before highlighting. Changed bytes are
reported as changed; the earlier result never claims the new file's identity.
For an occurrence result, this comparison verifies the retained token bytes;
it does not re-establish surrounding token boundaries or compiler binding.
For an expression result, it verifies only the primary literal bytes; it does
not re-evaluate other required or excluded terms against the current file.
Changing the query, mode or case policy, refreshing the catalog, replacing the
project or closing the sheet closes this child reader and revokes its actions
synchronously.

The progressive host service is
`fcb_app::host::catalog_session::CatalogContentSearchSession`; native hosts use
[`fcb_catalog_stream.h`](../../crates/fcb-bridge/include/fcb_catalog_stream.h).
Each query owns an independent handle, bounded advance operations and
revision-checked result pages. Its `fcb.catalog-stream/1` receipts explicitly
describe progressive membership and never invent atlas node or layout identities.
The visible-atlas route retains `fcb_app::host::atlas_stream_search` and
[`fcb_atlas_stream_search.h`](../../crates/fcb-bridge/include/fcb_atlas_stream_search.h).
Both reuse the same streaming matcher, expression semantics and exact witness
representation. New receipts add per-hit `context` metadata, retain their schema
identifiers, and report `capture_kind: "literal-witnesses-with-context"`.
Existing literal witnesses keep their meaning; older witness-only receipts
remain readable without an invented excerpt. Added core, host, C ABI and native cases target large discovery,
encoding, original-byte retention, query replacement, cancellation, bounded
results, counted progress and Stop. They are code-first additions: executable
Rust/Swift verification and physical-Mac qualification remain pending.

## Finding text in one large file

The **Observed Range Reader** can search a file without capturing its complete
contents. Open it from the current source or Quick Open, choose UTF-8, UTF-16 LE,
or UTF-16 BE, and select **Text**, **Expression**, or **Occurrences**. Text uses a
literal; **Match case** chooses exact spelling or Unicode scalar lowercase
matching. Occurrences uses one exact whole identifier. Search uses the same
opened file object as byte navigation, including after that pathname is replaced.
Reopen explicitly to observe the replacement file.

Each search starts at byte zero and reads through a 16 KiB streaming buffer.
The selected scan limit bounds source I/O; the default is 256 MiB and the maximum
is 4 GiB. At most 256 hits are retained. Text admits 4,096 UTF-8 query bytes;
Expression and Occurrences admit 1,024. Overlapping
matches and matches split across read buffers retain exact original byte ranges,
including UTF-16 surrogate pairs. Enable **Count all matches** to continue
counting after the retained-hit table fills without retaining more rows or source
bytes. A completed unchanged observation can then report an exhaustive count;
byte/read-call limits, decoding failures, source changes and cancellation keep
that count explicitly partial. Without the option, Text and Occurrences stop
after the first match beyond the retained result allowance. The result reports actual bytes scanned and
read, and distinguishes reaching the observed end from stopping at a byte,
read-call, result, or encoding limit. A completed scan is an observation of a
mutable file, never an atomic snapshot.

Choose **Expression** for required terms, quoted phrases and exclusions. For
example, `"retry request" timeout -deprecated` finds the primary phrase
`retry request` only when the same observed file also contains `timeout` and
does not contain `deprecated`. The first positive literal supplies the counts,
highlights and retained byte witnesses; **Match case** applies to every text
predicate. The shared parser admits at most 64 terms within the 1,024-byte query
limit. Regular expressions are unavailable. Path, language and type filters are
rejected because this reader searches one explicitly opened file.

Expression searches check all required and excluded terms through the end of
one continuous observation, including text beyond the retained-hit allowance.
**Count all matches** stays on in Expression mode; returning to another
mode restores the previous counting preference. A scan cut short by a byte or
read-call limit, cancellation or malformed text cannot establish the predicates
and cannot publish expression hits. A completed predicate rejection reports no
qualifying matches. Reaching the observed end can qualify an expression over
those bytes, while changed or unavailable final metadata still keeps the overall
coverage partial. This does not turn the observation into an atomic snapshot.
Each result-page rescan evaluates the complete expression again.

Selecting a result displays its retained match and a captured excerpt with the
match highlighted; it does not reread the file. Each hit retains up to 160 original
bytes before and after the match from the same scan. These bounded excerpts
survive live edits and remain separate from the reader's eight observation
pages. They have requested and actual ranges, an independent completeness flag,
original hex, and decoded text and match coordinates when scalar boundaries
permit. Byte, call and match limits never trigger extra reads to fill context.
Clipped excerpts expose original hex when decoded text is unavailable. **Copy
captured text** copies the lossless decoded excerpt; **Copy captured hex** copies
the original captured bytes, including when text is unavailable. Replacement
text is never offered as a lossless copy. Hosts with older witness-only receipts
continue to show their retained matches without claiming captured context.

**Open captured excerpt** opens those same search bytes in the main reader,
selects the exact match, and adds an independent page to the eight-page history.
It performs no source reads or metadata checks, even if the file has changed.
The page retains the original search observation, occurrence and context ranges;
its new page identity does not imply a fresh observation of the current file.
Select any complete character range in this page to copy its text, original hex,
or text with location and original-search provenance. Back/Forward and these
selection actions remain available after another Find replaces the original
query. History eviction still applies. Context without safe decoded boundaries
remains available through captured hex; older transports leave this action
unavailable. Canceled, stale or refused openings preserve the accepted page.
Independent history copies are accounted for separately from bytes read from
the source, within the existing bounded retention allowance.

Copies of the match preserve its original spelling. **Read current
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
without source I/O even after another query replaces the active search. For an
Expression hit, this fresh context check verifies only the primary matched
bytes. The page retains the expression and its qualification from the original
search observation; it does not claim to recheck required or excluded terms
elsewhere in the current file. This
action also handles valid matches beside malformed neighboring text through the
shared decoder's source map. **Read current context** remains available when the
intent is to inspect what changed.

Press **Command-F** to focus Find. **Previous match** and **Next match** select
retained results and their captured excerpts without rereading source; choose
the current-context action explicitly when a fresh observation is wanted.
They wrap only when the completed observation's entire match set fits in the
current page. An incomplete scan or a page that omits earlier or later matches
does not turn its retained subset into a complete cycle.

Use **Rescan next matches** to reach occurrences beyond the 256 retained rows,
and **Rescan previous matches** to return to an earlier result page. These
actions scan the same opened file object again from byte zero, skipping the
requested number of occurrences before retaining a new bounded page. Skipped
matches still spend the selected scan and read-call limits. A limit can stop
the scan before the requested page; the result then reports incomplete coverage,
rather than claiming those matches are absent.

Each rescan creates a fresh observation and query identity. Live edits may
change occurrence positions and counts between pages, so the reader replaces
the result page instead of combining different observations into one snapshot.
Labels show the retained occurrence range and distinguish a completed count
from a lower bound. Copy and verified context navigation use the accepted
page's exact original-byte witnesses, including later-page UTF-16, overlapping
and lowercase matches. Hosts without the additive page API retain first-page
Find and cannot perform result-page rescans.

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

Press **Command-R**, or choose **Markdown preview** in the workspace toolbar,
to switch visible Markdown atlas parcels between source and rich preview.
The preference applies to `.md`, `.markdown` and `.mdown` files in this window.
With Source Reader hidden, the command leaves the atlas in view. Zooming into
a parcel prepares its preview on the existing bounded source worker; an overview
does not parse every Markdown file in the project.

Atlas previews use the exact accepted capture and FrankenMarkdown's shaped glyph
IDs, supplied font faces and display geometry. Headings, prose, lists, tables,
code fences and supported vector decorations keep that upstream layout.
Preview placement stays within each existing parcel, preserving neighboring
geometry and camera position. A source/preview/source toggle reuses the retained
source; a warm return to preview reuses the prepared display. Changing the file
scope refits those same immutable windows without moving an older frame's tiles.

The initial atlas rich-display allowance is **64 KiB of UTF-8 source**, at a
fixed 640-point document width and at most 64 retained display windows. A limit,
unsupported layout, invalid source correspondence or unavailable display service
keeps the captured source readable and labels the fallback. Optional rich
content cannot displace a source preview that fits its budget. Image slots
show labeled placeholders in the atlas; opening the retained reader provides
the existing separately authorized image route.

Verified query matches use the original captured bytes and digest before mapping
to enclosing rendered blocks. Block highlights do not claim glyph-exact or
inline-exact source ranges; unmapped source stays available in the reader.
Appearance changes recolor accepted geometry without reparsing. Glyph-only
windows use the existing Metal glyph path; decorated windows retain their
complete native drawing and bounded raster/detail coverage.

While preview is enabled, an active visible retained reader can open its document
sheet, and newly opened Markdown captures prefer that preview. **Source**,
**Command-R** inside the preview, or **Done** returns to the same captured source
and turns off this window's preview preference. A verified Markdown region
restores its original-byte anchor; otherwise the source page and native selection
stay in place. Hidden history readers and pins do not open previews.

Reopening a prepared reader preview reuses its captured document and last preview
position. **Split preview** enables **Follow source** by default. Source paging,
line/column navigation, Find hits, outline selections and identifier occurrences
then move the preview to their verified original-byte anchor after the source
reader finishes. Turn off **Follow source** to navigate the panes independently;
**From source page** remains an explicit synchronization action in either mode.
Scrolling within a source or preview page remains independent. This is enclosing
Markdown-block navigation, not pixel scrolling or a glyph-exact correspondence.
Pending work is canceled when its presentation is withdrawn.
Canceled or stale atlas jobs cannot replace a newer project, layout or toggle
choice, and reader-handle retirement remains bounded even if a close is busy.

Native regression sources cover supplied glyph replay, complete continuation
pages, exact capture and source-map checks, decorated windows, query geometry,
warm toggles, visible-only demand, scope reuse, stale replies and retirement.
These code-first changes await independent strict-RCH executable verification
and physical-Mac visual, wheel and frame-cadence qualification.

Open a retained source file and choose **Markdown**, or **Split preview** to keep
the source alongside the document. The native reader prepares a **Styled**
view by default when the host supplies the rich-display capability and the
capture fits its 64 KiB source limit. **Text** keeps the selectable logical
preview available for UTF-8 captures up to **256 KiB**. Larger supported
documents automatically use Text with an explanation; choosing Styled keeps
that logical preview and its navigation available. The **Columns** field and
**Reflow** control change the width without reopening the file.

Styled pages draw FrankenMarkdown's shaped glyph IDs and positions using the
exact bundled font bytes supplied by the retained reader. Headings, prose,
lists, quotes, tables, code fences and supported vector decorations retain the
shared engine's layout and semantic color roles. Appearance changes recolor
the accepted display without reparsing or changing its source identity.

**Previous page** and **Next page** move through bounded display pages. Switching
between Styled and Text, or reflowing a mapped page, restores its enclosing
captured Markdown block. Headings, document links, flow-row jumps and source
following keep **Styled** selected and use the existing qualified display layout;
they do not reparse, reshape or reload bundled fonts. Only the latest source
anchor is queued while another operation is running. Source reads, selected-byte
mapping and pinning finish before automatic following can use the shared worker.
Preview-to-source actions consume their matching echo without a navigation loop.

The logical page and matching styled page publish together. If a remap fails or
is canceled after its submission, the previous pixels remain readable but their
old row actions are disabled. **From source page**, or the next source navigation
with Follow source enabled, can retry against the retained layout without reflow.
An unqualified styled source map keeps the accepted pages in place and explains
that Text is required for navigation. Closing or replacing the capture withdraws
pending anchors and old page authority. Click a mapped element, use **Show source**,
or use its accessibility action to inspect that block; split mode also navigates the
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

Logical preparation uses the existing public 256 KiB source bound with at most
8,192 flow rows, 8,192 flow items and 4,096 blocks; each page requests at most
64 rows. The larger source allowance preserves heading/link navigation,
original-byte anchors, rendered-text copy and enclosing-Markdown copy. Files
above 256 KiB remain available in the source reader.

Styled preparation retains its 64 KiB source bound, at most 8,192 display
elements, 131,072 glyphs, and 8 MiB of bundled font resources. It does not queue
typography, font or image work for a document above that source bound. A page admits at most 128
elements and 16,384 glyphs. Source, generation, layout and page identities are
checked before accepting asynchronous results or enabling source actions.
Cancellation, capture replacement and failed reflow cannot reactivate stale
pages. Each logical generation retains one rich width; reflow creates a fresh
generation.

`tests/test-large-document-preview.sh` covers the native size boundaries,
fallback, late source anchors, copy domains, reflow and cancellation. The Rust
host regression `crates/fcb-app/tests/retained_reader_large_document.rs`
exercises actual larger Markdown parsing and original BOM/CRLF offsets, including
the exact 256 KiB boundary. These code-first additions await independent
execution; they do not qualify native drawing or raise the styled-rendering limit.

`tests/test-document-preview.sh` and the selected-file reader cases in
`tests/test-native-open.sh` include authored source-follow coalescing, worker
arbitration, Styled navigation, failed/canceled remap recovery and stale-delivery
regressions. Independent strict-RCH execution and physical-Mac split-pane
interaction qualification remain pending.

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

## Finding text in a Markdown preview

Prepare **Markdown** or **Split preview**, enter text in the preview's **Find**
field and submit the query. Search uses FrankenMarkdown's retained logical
rendered text, so a phrase such as `bold phrase` can match `**bold** phrase` or
`[bold](guide.md) phrase`. Matching is literal and case-sensitive, preserves the
exact Unicode spelling, and includes overlapping occurrences. The response
counts every match in the accepted document; **Previous match** and **Next
match** navigate across bounded pages of results.

Opening a match selects **Text** and shows the logical flow context where it
starts. Separators between displayed rows lead to the next available row;
trailing separators lead to the last row. A document that renders only
separators can have matches with an empty display context. These locations are
rendered UTF-8 ranges. The existing **Show source** and copy actions continue
to use their qualified enclosing-source mapping.

The optional host capability accepts a query of 1–1,024 UTF-8 bytes and returns
at most 128 match records per page. Each page scans at most 16 MiB of retained
rendered text using the shared incremental matcher, with bounded cancellation
checks and no source reads, reparsing or full-document hit table. Native query,
job, document-generation and page identities fence asynchronous results.
Editing the query, reflowing, replacing the capture or closing the preview
withdraws stale Find actions. A failed or canceled search preserves the
accepted preview and source reader.

The public host method is `ReaderSession::find_document`; the C entrypoint is
`fcb_reader_document_find`. Rust host/registry/ABI and native decoder/model
regressions are included in source. Their execution and physical-Mac interaction
verification remain pending the independent strict-RCH campaign.

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

Choose **Text** or **Expression** in the retained reader's Find controls. Text
uses a literal spelling. Expression uses the shared query language: for example,
`"request failed" timeout -deprecated` finds the quoted primary phrase only
when the same captured file also contains `timeout` and contains no
`deprecated`. Expressions accept at most 1,024 UTF-8 bytes and 64 terms;
literal Text retains its 4,096-byte allowance. Path, language and type filters
belong in project search and receive an explicit refusal here.

Expression mode always checks the complete retained capture and counts primary
occurrences, while retaining only the bounded navigation page. Its count
control stays checked; returning to Text restores the previous counting
preference. An incomplete or undecodable scan exposes no tentative expression
matches, and the summary distinguishes incomplete qualification from a complete
no-match result. Exact hit navigation and copying use the primary term's
original spelling and encoded bytes, never the expression syntax. The same
controls work in pinned readers, Reading Desk and saved-source readers.

The paged source reader's **Find** scans its complete retained capture, including
text outside the visible page. **Match case** starts enabled. Turn it off for the
same Unicode lowercase comparison as workspace search, including `k` matching
`K` and a lowercase expansion mapping back to the complete original character.
UTF-8, BOM-marked UTF-16, and original CR/LF/CRLF spellings keep their source-byte
coordinates. **Previous hit**, **Next hit**, and **Copy hit hex** use each hit's
original spelling and encoded bytes, even after the disk file changes.

Focus the source text or its Find field to use **⌘F**, **⌘G**, and **⇧⌘G**.
The commands belong to that reader, including independently pinned readers.
⌘F selects its query; ⌘G and ⇧⌘G move through its accepted matches. Empty or
pending reader searches keep those commands local.

**Next hit** continues beyond the retained row limit by loading another bounded
page; **Previous hit** returns through earlier pages. The position shows global
occurrence numbers, not a page-local index. Each page retains at most 4,096
rows, and long Unicode witnesses can lower that limit to bound the response.
Pages replay the same immutable capture from byte zero, skipping earlier
occurrences before retaining witnesses. They do not reread the live file.
Replay spends the ordinary scan and cancellation budgets, so later pages may
take longer even though retained result memory stays bounded.

In Text mode, enable **Count all matches** to continue counting after a page
fills. The summary distinguishes the full observed count from the current page; byte
limits or unsupported text remain explicitly incomplete. Navigation wraps
between the first and last match only after a complete scan has established the
total. Before then, Previous at the first match stays there; Next continues
forward. A partial scan never presents its last retained row as the file's final
match. Literal queries admit at most 4,096 UTF-8 bytes; expressions admit 1,024.
Editing the query, mode or either option immediately revokes the old hit actions and cancels pending publication.
Canceling a page request preserves the readable source but revokes its old hit
actions; use Find to start again.

Opening a workspace text or expression result can also continue through these
pages to locate its exact original range when a long witness reduces the first
page's capacity. Automatic activation admits at most 64 pages and 64 MiB of
aggregate capture replay; reaching either limit reports that the selected hit
is unavailable. It never substitutes a nearby occurrence or opens the live file.

The additive `fcb_reader_find_page` and `fcb_reader_find_expression_page` ABIs use
global occurrence offsets and fresh query generations. Expression is a separate
entry point, preserving the original literal Find flags and older transports.
Hosts without expression support keep literal Find available. Source-selection
Find and automatic project-hit activation explicitly use literal spelling.
Host, C-boundary, and native model/decoder regressions cover result-page
boundaries, UTF-16, overlapping matches, lowercase expansion witnesses, and
stale/canceled requests. The native suites are available through
`swiftui/tests/test-reader-search.sh` and `swiftui/tests/test-reader-commands.sh`;
independent strict-RCH execution and physical-Mac qualification remain pending.

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

**Search selection** uses that resolved passage as an exact query. Choose **Find
text in this file**, **Find declaration candidates in this file**, or **Find
identifier occurrences in this file** to search the same retained capture. Text
Find accepts up to 4,096 UTF-8 bytes; exact declaration-name filtering accepts
256; occurrence queries accept 1,024 and are validated by the shared identifier
engine. Text Find switches to case-sensitive matching while preserving its
count-all option. Declaration candidates are heuristic; occurrences include
comments and string literals and do not establish compiler bindings.

Live-project readers, Project Files, Quick Open, and pinned readers also offer
these searches **in project scope**. They fill the search sidebar, preserve its
current location and file-type filters, and start a fresh query using at most
1,024 selected UTF-8 bytes. Declaration names use exact matching. Project Files
dismisses its reader sheet to reveal the results; an already mounted main or
pinned reader keeps its capture and position. The selected spelling comes from
the retained source, while project results belong to their own current or
indexed captures. Selection text is never interpreted as an expression.

Search actions preserve whitespace, CR/LF and Unicode spelling without trimming,
normalization or truncation. NUL-containing text and selections containing
decoding replacements cannot seed a text query. Changing selection, page,
capture, or reader operation revokes an older menu's receipt before dispatch.

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

## Going to a matching bracket

Select a single parenthesis, square bracket or brace in a retained source
reader, then choose **Go to matching bracket** beside the selection tools.
The reader searches the complete retained capture, opens the matching page
and selects the corresponding bracket. The destination becomes an exact
selection for copying, keeping in the Reading Desk or jumping back to its mate.
Ordinary **Back** and **Forward** retain the visited source pages.

Matching uses language tokens and skips comments, strings and template
contents. It reports lexical candidates rather than compiler-resolved
structure. Unmatched or inconsistent nesting has a distinct explanation.
The source is the same captured version even if the live file changes.
Canceled, refused and stale results preserve the current page, exact selection
and navigation history; the destination is checked against its retained bytes
before it replaces the page.

The current analysis admits complete captures up to **512 KiB** in both
original and decoded text, with at most **1,024** nested delimiters. UTF-8 and
BOM-marked UTF-16 preserve their original byte offsets. Supported filename
families are Rust, Python, JavaScript, TypeScript, JSX, TSX, Go, C/C++, Swift,
Java and C#. Angle brackets, replacement-decoded text, incomplete literals
and unqualified language contexts are refused explicitly. Swift code slashes
currently refuse analysis because the pinned highlighter cannot reliably
separate regex literals from division. Larger captures remain readable with
the existing reader tools; Observed Range Reader windows do not claim a
complete-source bracket match.

The production decoder/model regressions run through
`swiftui/tests/test-reader-text-selection.sh`; full-width C argument and
response-ownership cases extend `swiftui/tests/test-supplied-reader.sh`.
Rust, C-boundary and native tests are authored, with independent strict-RCH
execution and physical-Mac qualification still pending.

## Exporting original captured bytes

In a retained source reader, choose **Export bytes…** to save the entire capture,
the shown page, the selected original bytes, or the full compared range. The
export sheet also accepts an explicit zero-based start and exclusive end byte.
Ranges can cross reader pages and exceed the 2 MiB clipboard staging budget;
the complete retained reader capture can be up to **32 MiB**.

Export uses the accepted immutable capture, even if its original pathname has
changed. It preserves the exact encoding, BOM, line endings, NULs and malformed
bytes, without decoding, reopening source or constructing a second complete
payload. An empty range creates an empty file. This action belongs to the
complete retained reader; large-file observations remain bounded observations.

Choose a new destination in the native Save panel. Existing destinations are
never replaced. If the reader, navigation or selected range changes while the
panel is open, the export must be chosen again from the current source. Once
writing starts, its captured source and destination access stay retained until
the operation returns, including after cancellation.

The result distinguishes no destination effect, a created but incomplete file,
all bytes written, file synchronization, and parent-directory synchronization.
Canceling can leave a partial or complete file; the displayed result preserves
the actual written-byte count and completed synchronization steps. A missing
receipt is reported as unverified, and the app never automatically repeats an
uncertain write. Inspect the named destination before choosing a new export.

Regression cases cover original encodings and ranges, exports larger than the
clipboard budget, existing destinations, changed live source, stale selections,
cancellation and retained-session lifetimes. These code-first additions await
independent strict-RCH executable and physical-Mac verification.

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
