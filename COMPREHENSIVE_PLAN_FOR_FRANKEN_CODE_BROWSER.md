# Comprehensive Plan for FrankenCodeBrowser

## A modular Rust library and native GPU-accelerated source browser for Apple Silicon

**Product name:** FrankenCodeBrowser  
**Executable:** `fcb`  
**Public Rust library:** `fcb`, with separately consumable `fcb-*` components  
**Plan revision:** R4 — headless sequencing repair, platform-bridge ownership, FrankenTUI work package, and gate ordering review  
**Document date:** September 12, 2026  
**Status:** revised architecture and implementation specification. The plan has been reviewed and revised; the proposed application, APIs, upstream extensions, and performance targets are not represented as implemented or benchmarked.  
**Primary target:** late-model Apple Silicon Macs, especially M4/M5 configurations with at least 24 GB of unified memory.  
**Implementation foundation:** memory-safe Rust, Asupersync, FrankenMarkdown, and narrowly selected or extracted first-party FrankenSuite components.  
**Reference experience:** the user-supplied 42.9-second UI recording.  
**Planning precedent:** the complete `docs/planning/COMPREHENSIVE_PLAN_FOR_FRANKEN_MARKDOWN.md`, with additional integration, native-GPU, scale, and acceptance detail. [R1]

> Open a repository. See its structure as a place. Fly continuously from the whole codebase into legible, selectable, syntax-highlighted source. Search without losing that place. Read its documentation beautifully. Nothing on the navigation critical path should scale with the entire repository.

This is a full-product plan, not a proposal to stop at a toy visualization. Phases sequence the dependency graph; they do not redefine the finished product as a screenshot, a simulated renderer, or a collection of stubs.

**Ownership directive:** FrankenCodeBrowser, Asupersync, FrankenMarkdown, and every other Franken project discussed here share the user's ownership. Reusable Markdown, syntax-highlighting, font/text, mathematics, diagram, source-provenance, and Markdown-flow improvements required by this project are implemented in **FrankenMarkdown**, including its own workspace crates, and consumed through committed public APIs. A browser-local Markdown engine is not an acceptable temporary shortcut. Other shared improvements likewise land in the appropriate existing owner repository. Proposed APIs and crate names in this plan remain proposals until their consumer tests pass.

**Two equal product surfaces:** `fcb` is both a real standalone executable and a modular Rust library. The application is a consumer of the same library that other projects use. The library does not take over its host's event loop, runtime, allocator, logging, signal handlers, filesystem authority, or GPU device. §§6 and 27 define the concrete boundaries.

---

## Contents

1. [Product thesis and success criteria](#1-product-thesis-and-success-criteria)
2. [Study of the supplied UI](#2-study-of-the-supplied-ui)
3. [Non-negotiable engineering contracts](#3-non-negotiable-engineering-contracts)
4. [Repository-by-repository findings and reuse decisions](#4-repository-by-repository-findings-and-reuse-decisions)
5. [User experience and interaction specification](#5-user-experience-and-interaction-specification)
6. [System architecture and crate boundaries](#6-system-architecture-and-crate-boundaries)
7. [Identity, snapshots, and invalidation](#7-identity-snapshots-and-invalidation)
8. [Filesystem ingestion and live updates](#8-filesystem-ingestion-and-live-updates)
9. [Spatial layout and semantic zoom](#9-spatial-layout-and-semantic-zoom)
10. [Source storage and large-file reading](#10-source-storage-and-large-file-reading)
11. [Syntax highlighting and language understanding](#11-syntax-highlighting-and-language-understanding)
12. [Native Markdown rendering](#12-native-markdown-rendering)
13. [Fonts, shaping, selection, and text rendering](#13-fonts-shaping-selection-and-text-rendering)
14. [Native Metal renderer](#14-native-metal-renderer)
15. [Frame pacing and interaction latency](#15-frame-pacing-and-interaction-latency)
16. [Asupersync orchestration and cancellation](#16-asupersync-orchestration-and-cancellation)
17. [Search and progressive results](#17-search-and-progressive-results)
18. [Dependency graphs and code intelligence](#18-dependency-graphs-and-code-intelligence)
19. [Persistence and FrankenSQLite integration](#19-persistence-and-frankensqlite-integration)
20. [Unified-memory budgets and pressure control](#20-unified-memory-budgets-and-pressure-control)
21. [Performance objectives and measurement](#21-performance-objectives-and-measurement)
22. [Safety, security, privacy, and trust boundaries](#22-safety-security-privacy-and-trust-boundaries)
23. [Accessibility and native macOS behavior](#23-accessibility-and-native-macos-behavior)
24. [CLI, agent interface, and observability](#24-cli-agent-interface-and-observability)
25. [Testing and qualification](#25-testing-and-qualification)
26. [Build, dependency closure, and distribution](#26-build-dependency-closure-and-distribution)
27. [Upstream extension contracts](#27-upstream-extension-contracts)
28. [Implementation phases and release gates](#28-implementation-phases-and-release-gates)
29. [Implementation work packages](#29-implementation-work-packages)
30. [Risks, rejected approaches, and decision rules](#30-risks-rejected-approaches-and-decision-rules)
31. [Requirement traceability and completion checklist](#31-requirement-traceability-and-completion-checklist)
32. [Research provenance and source ledger](#32-research-provenance-and-source-ledger)

---

## 1. Product thesis and success criteria

### 1.1 What we are building

FrankenCodeBrowser is a modular Rust source-browsing engine with a native macOS application for **understanding and navigating source code spatially**, not an editor with an unusually elaborate minimap. Its main surface is a stable, zoomable repository atlas. Directories form neighborhoods; files form parcels; recognizable source structure and code lines emerge as the user approaches. A synchronized reading surface makes arbitrarily awkward files readable without distorting the atlas.

The atlas and the reader are two views of the same versioned source model. Search results, document headings, graph edges, bookmarks, and history entries resolve to the same source identities. Selecting an item in one surface selects it everywhere appropriate. There is no second, inconsistent notion of a file inside the renderer.

The application must remain useful when indexing is incomplete, a syntax extension is unavailable, the repository is changing, or memory is constrained. In those cases it gives immediate access to the available source and states the relevant limitation. It never substitutes an empty screen for an unfinished index.

### 1.2 Finished-product capabilities

The finished product includes:

- A continuous 2D repository atlas with directory/file/source detail levels, stable positions, keyboard navigation, and smooth trackpad interaction.
- A genuinely useful optional 2.5D code-city view using the same map, with tilt, orbit, metric-based extrusion, and instant return to a frontal reading view.
- Selectable, copyable, syntax-highlighted source; source outlines; line navigation; multiple pinned reading panes; history; bookmarks; and scoped search.
- Native Markdown documents with excellent typography, code blocks, tables, mathematics, diagrams where supported, local images, source navigation, and synchronized source/preview views.
- Progressive, bounded indexing and search over repositories larger than RAM; live-file refresh; accurate incomplete/stale/error states; and durable personal navigation state.
- Evidence-qualified relationships between source entities, including imports and selected symbol relationships, without passing heuristic guesses off as compiler-proven facts.
- A small native macOS shell, a retained Metal rendering engine, and Asupersync-owned background work, with explicit memory and latency budgets.
- A public `fcb` Rust facade supporting headless source/search use, renderer-neutral embedding, and native view embedding without mandatory app or database ownership.
- Locally assembled reading trails and bounded source-evidence packs, with exact provenance and explicit export consent, using shared first-party primitives rather than a model dependency.

### 1.3 What success feels like

Opening a project gives a useful overview before full analysis finishes. A pinch or wheel gesture moves the camera immediately. Search starts returning useful matches while the rest of the query completes. Clicking a result lands on the right source bytes, not merely on an approximate rectangle. A Markdown file looks like a well-designed technical document, not a terminal rendering pasted onto a GPU texture.

The user can alternate between overview and exact reading without losing orientation. Fast motion never triggers a burst of expensive synchronous parsing. Long indexing jobs do not turn the window into a progress dialog. On a 24 GB machine, the application remains a considerate neighbor to coding agents, terminals, browsers, and compilers.

### 1.4 Non-goals

This project is not a full IDE, code execution environment, terminal emulator, general HTML/CSS browser, Three.js replacement, distributed agent manager, or neural embedding platform. Source editing, builds, debuggers, and compiler execution are not required for the source-browser release. An explicit “open in editor” integration may hand off a location to a configured external application, but it must not become a runtime prerequisite.

There is no requirement to ship Windows, Linux, iOS, or a browser UI in the first native release. The host-independent Rust library must nevertheless compile and operate without Apple frameworks on supported non-Mac test hosts. This is a real dependency boundary, not a promise of native GUI parity. A caller may use the source, search, layout, or document-adapter components without a Mac window. Do not make the Mac renderer pay for a speculative general graphics framework.

---

## 2. Study of the supplied UI

### 2.1 What was actually observed

The supplied recording was inspected through extracted frames across its duration. It is approximately 42.9 seconds long, 848 × 510 pixels, and encoded at 30 frames per second. Those are recording properties, not measurements of the application's rendering performance.

| Approximate interval | Visible behavior | Consequence for this design |
|---|---|---|
| 0–12 seconds | A dense, nested rectangular code map transitions between whole-project overview and detailed readable code. | Continuous semantic zoom is the defining interaction, not a decorative feature. |
| 13–22 seconds | A search/results sidebar accompanies highlighted regions and navigation into code. | Search must operate across the spatial and textual representations with shared selection and source anchors. |
| Middle-to-later portion of the recording | The map becomes a tilted, extruded code-city-like scene and the camera moves through it. | Preserve an optional perspective mode, but use the same spatial identity and avoid a separate visualization silo. |
| Around 40 seconds | The scene returns to a frontal overview. | Switching projections must preserve orientation and selection. |

Other visible details include a restrained dark canvas, colored region boundaries, dense labels, a narrow top control area, and a right-hand sidebar with Inspector, Results, and History surfaces.

The application name, exact search semantics, underlying implementation, performance, and the meaning of every tiny label cannot be established from the recording. This plan does not invent those details. Markdown rendering, robust source selection, accessibility, and several navigation refinements below are proposed additions rather than claims about the reference UI.

### 2.2 What to preserve

Preserve the powerful spatial metaphor: a repository becomes a place that can be inspected at multiple scales. Preserve the effortless alternation between global structure and exact code. Preserve the synchronization between results and geometry. Preserve the feeling that source is already present and the camera is revealing it, rather than launching a new editor every time a file is clicked.

### 2.3 What to improve

The proposed system must not make the user read tiny text simply because a file occupies a narrow parcel. It therefore introduces a **reading lens**: a selected file or source range can expand into a frontal, pixel-crisp surface while a tether and subtle map highlight preserve its location.

Information should not become a wall of competing labels. Labels are admitted by screen-space importance and collision tests. At distant scales, line density and color summarize source; at reading scales, the exact text replaces that summary. A transition band and hysteresis prevent labels from flickering in and out.

The 3D mode must serve a question. Examples: “Which modules have grown?”, “Where are the largest files?”, or “Where do these search matches cluster?” Height always has a visible metric legend. It is never an unexplained visual effect. Flat mode remains the best mode for extended reading.

### 2.4 Core interaction loop

```text
Open repository
    → progressively populated, stable atlas
    → search / navigate / inspect a neighborhood
    → select file or source entity
    → zoom or promote into reading lens
    → inspect source / Markdown / relationships
    → jump to another exact source anchor
    → return through spatial history without losing place
```

A complete implementation must demonstrate this loop using real repositories and real files, not prerecorded data or canned geometry.

---

## 3. Non-negotiable engineering contracts

### 3.1 Rust and memory safety

All authoritative source, parsing, indexing, layout, UI-state, search, graph, and render-planning crates must compile with `#![forbid(unsafe_code)]`. No `unsafe` fast path is admitted to these crates for indexing, SIMD, pointer tricks, byte casts, or unchecked bounds access.

Native AppKit and Metal access requires an explicit system-ABI boundary. Rust's 2024 edition requires unsafe external declarations because their signatures and contracts are the binding author's responsibility. A safe Rust application cannot conjure a native Metal implementation from `std` alone. [A4]

**Architecture decision (updated 2026-09-22):** keep a narrowly scoped, first-party `franken-macos` platform crate that provides safe, owned, thread-affine APIs and contains the audited system-ABI implementation. It lives at `native/macos/` in this repository so the complete app builds from one checkout. FrankenMarkdown's optional Mac font adapter may consume this crate without FCB or FrankenMarkdown hosting the other's implementation (§6.2). This is the only new application-side exception to the `unsafe` prohibition. It may contain the minimal unsafe Rust required to call Apple frameworks and implement their callbacks, but no source parsing, arbitrary plugin loading, indexing algorithm, or general application logic.

The supported claim is therefore **a memory-safe Rust application above an explicit, audited native boundary**, not “there is no unsafe code anywhere in the operating system, standard library, GPU driver, or dependency closure.” Every inherited first-party unsafe boundary, especially storage VFS code, must also be inventoried. The native binding gate is a release blocker; wrapping an unsound ABI in a safe function does not satisfy the requirement.

### 3.2 No outside libraries: a real closure rule

The shipping Rust dependency graph must contain only Rust's standard/toolchain libraries, this project's crates, Asupersync, FrankenMarkdown, and explicitly selected first-party FrankenSuite crates. This rule applies to the **transitive normal dependency graph**, not merely to lines in the top-level manifest.

Several reviewed sibling projects currently pull third-party packages unconditionally. In particular, Asupersync's inspected manifest includes substantial unconditional dependencies even with default features disabled; the whole FrankenTerm GUI, CASS, and the general FrankenNetworkX algorithm crate are not clean imports under this rule. [R3] [R4] [R5] [R6]

Consequently, the plan includes actual upstream dependency-profile and factoring work. It does not pretend `default-features = false` removes unconditional dependencies. It does not rename or vendor an outside package and call it a first-party implementation. It does not silently treat all inherited packages as exempt.

A less strict “no new direct third-party dependencies” policy would be easier, but it is **not** the shipping policy specified here. G0 produces an exact dependency closure and closes the violations needed by the chosen product slice. Until then, the project is architecture-ready, not dependency-compliant. Feature and factoring work is limited to the functionality FCB actually needs; it is not a demand to rewrite unrelated network, crypto, Python, or scientific stacks before displaying source.

For the standalone application, audit its complete resolved build and runtime closure. For an embedded library, audit the closure attributable to the selected FCB features and the integration boundary. The mere presence of a host's unrelated dependency is not an FCB import, but Cargo feature unification can change FCB's resolved dependency path and must be tested. A clean manifest is not evidence of a clean compiled graph. [B10]

### 3.3 Explicit platform allowances

The following are named system/toolchain boundaries, not third-party Rust library allowances:

- Apple system frameworks required for windows, input, accessibility, file events, font fallback, display timing, and Metal GPU submission.
- The Rust compiler, standard library, pinned nightly toolchain, linker, Apple SDK, and Apple's offline Metal shader compiler.
- Small project-authored Metal Shading Language programs compiled into the application bundle. Their role is GPU drawing/compute; authoritative product semantics remain in Rust.
- Signing and notarization tooling used to distribute a native Mac application.

Do not add `wgpu`, `winit`, `metal`, `objc2`, `egui`, `iced`, `skia`, `cosmic-text`, HarfBuzz, FreeType, Tree-sitter, Tantivy, Tokio, Rayon, an embedded browser, or a new serialization framework as a convenience shortcut. Do not compile project C/C++/Objective-C support libraries to hide the native boundary. Any need that cannot be met by the specified first-party/platform boundary is a recorded architectural decision, not an implicit exception.

### 3.4 One concurrency foundation

Asupersync is the sole asynchronous foundation for FCB-owned orchestration. No unrelated executor, hidden thread-per-file strategy, competing Rayon pool, or independently installed indexing daemon is introduced. Native event-loop and driver callbacks are host boundaries, not alternate async runtimes. In embedded mode the host supplies the compatible Asupersync region/runtime integration; the pure synchronous components need no runtime.

Distinguish **one runtime implementation/version family** from **one runtime instance or thread**. A thread-affine database adapter may need a separately owned Asupersync worker context. That is not automatically an unrelated executor, but its lifetime, scheduling, version, and budget must be explicit. G0 must inspect the chosen facade's actual construction path; compiling one version of Asupersync does not prove that every service shares one instance. FCB neither starts an extra runtime secretly nor shuts down a host runtime it did not create.

Asupersync scopes, cancellation, budgets, and terminal outcomes must remain visible at integration seams. Merely passing `Cx` around does not make a blocking operation cancelable or a CPU loop cooperative. [R6]

### 3.5 Evidence and authority

Source bytes are authoritative for source content. User annotations and preferences are authoritative for personal state. Parsers, indexes, layouts, atlases, graph projections, and GPU resources are derived artifacts.

A result must identify the source revision and analysis capability that produced it. “No results” is different from “search incomplete.” “Definition” is different from “candidate identifier match.” “Available in a sibling repository” is different from “integrated and tested here.” A performance target is different from a measurement.

### 3.6 The critical-path prohibition

During camera motion, pointer updates, and ordinary redraw:

- No filesystem access, database query, font-file parse, Markdown parse, full-file lex, index build, or synchronous shader compilation.
- No work proportional to the whole repository merely to move the camera.
- No synchronous GPU readback for hover or selection.
- No wait for an obsolete background job before responding to fresh input.
- No full scene rebuild because one color or selection changed.

These are executable instrumentation assertions in the qualification harness, not just design aspirations.

---

## 4. Repository-by-repository findings and reuse decisions

### 4.1 Review method and limits

All nine requested repositories were inspected through GitHub. The review covered the reference plan, READMEs, relevant manifests, and selected implementation files. It was not an exhaustive source audit, a clone/build verification, or a benchmark run. Default-branch reads were not an atomic multi-repository snapshot. Exact reviewed blob identities and the oversized-file limitation are recorded in §32.

README claims were checked against implementation where important. This matters especially for FrankenManim, whose README explicitly describes much of its target state in the present tense, and FrankenThreeD, whose README explicitly says its GPU renderer is not implemented. [R9] [R10]

### 4.2 Reuse matrix

| Repository | Useful material actually found | Decision | Work required before production use |
|---|---|---|---|
| `franken_markdown` | Parser/AST; currently top-level spanned document wrappers; reusable highlighting spans; `fmd-font` and `fmd-math`. | Sole owner of reusable Markdown, highlighting, document flow, and shared typography improvements. | Implement nested provenance, bounded/resumable engines, renderer-neutral flow/display output, and shared text extensions upstream; FCB supplies only host/view integration. |
| `asupersync` | Structured runtime, capabilities, bounded cooperative cancellation model, explicit blocking-pool configuration, deterministic testing. | Sole orchestration foundation. | Produce a genuinely first-party-only desktop profile; reconcile all consumed versions and adapters; verify main-loop integration. |
| `frankenterm` | Real glyph-cache keys, borrowed lookups, atlas-budget concepts, native rendering tests, input-to-photon harness concepts. | Extract narrow algorithms and test patterns; do not link the complete GUI. | Remove inherited unrelated/native dependency forests from any extracted library; adapt from terminal cells to proportional/pixel geometry. |
| `franken_networkx` | Integer adjacency, actual bidirectional `DiCsr`, graph revision tracking, deterministic semantics. | Selected graph kernels and checked compact views. | Factor a dependency-clean native subset; preserve edge provenance separately when a structural projection collapses parallel edges. |
| `coding_agent_session_search` | Initial/refined phases, readiness, publication discipline, actual bounded evidence-pack planner. | Progressive discovery and source reading/evidence-pack workflows; optional inverse embedding of FCB as a source viewer. | Generalize reusable selection/readiness policy upstream; do not import the dependency-heavy CASS app or mistake heuristic token estimates for exact counts. |
| `frankensqlite` | Safe engine core; public facade; dedicated-worker async connection; explicit cancellation/commit semantics. | Persistence for metadata, manifests, navigation state, and qualified index metadata. | Audit dependency and VFS boundaries; test exact SQL subset and version; isolate thread-affine connection ownership. |
| `frankentui` | Model/update pattern, pane/focus/virtualization concepts, concrete Fenwick implementation. | Extract non-terminal state/layout utilities. | Use checked wide coordinates; do not import ANSI presenters or a second runtime. |
| `franken_manim` | Retained IR/revision axes, CPU reference primitives, implemented SHA-256/canonical envelopes and namespaced cache/pin machinery. | Selected rendering, canonical-key, integrity, and resource-lifecycle primitives. | Factor digest/envelope code away from unnecessary scientific/RNG edges; adapt cache ownership and confinement rather than inheriting stronger security claims than the code supports. |
| `franken_threed` | Implemented generational handles, device generations, capability/feature manifests; persistent-schedule design. | Reuse `f3d-core` where its no-serde profile fits; borrow render-schedule principles. | Native Metal drawing is new work. Do not depend on the unimplemented WebGPU renderer or compiler. |

### 4.3 FrankenMarkdown: the closest direct fit

The inspected root library already exposes `ast`, `parse`, `span`, `highlight`, `layout`, `scanner`, `diagrams`, and related modules. The fresh inspection of `src/span.rs` establishes an important limitation: `SpannedDocument` stores top-level block spans; an exported `SpannedInline` alias does not establish a fully nested inline provenance tree. `to_document()` clones blocks, while `into_document()` drops span wrappers. Precise preview selection requires a new upstream contract, not an assumption that those names already supply it. [B1] `fmd-font` and `fmd-math` are factored first-party crates. The inspected no-default renderer path avoids the CLI's `clap` dependency; the GUI must also avoid enabling the Markdown `batch` feature because the GUI owns orchestration itself. [R2]

The current highlighter offers `highlight(lang, code)` and `highlight_into(lang, code, spans)`, with classified byte ranges that tile the input. This is exactly the representation needed for native source rendering. However, the inspected implementation is a focused block highlighter, not an incremental editor lexer. Its documented limitations include JS/TS regular-expression versus division ambiguity. A resumable language state is new upstream work. [R2]

`fmd-font` explicitly describes itself as Latin-first. Its current TrueType metrics/outlines, focused GPOS, and GSUB ligatures are valuable; they do not establish universal complex-script shaping or complete CFF support. The font plan below addresses that honestly. [R2]

### 4.4 Asupersync: architecture fit, dependency prerequisite

The runtime builder exposes worker, blocking-pool, queue and admission controls. The inspected documented defaults include an unbounded global queue and no blocking-pool threads. Those defaults are inappropriate for this UI without explicit configuration. [R6]

The project must consume a coherent, pinned Asupersync slice. Current sibling consumers do not automatically agree: the reviewed CASS manifest pins 0.4.11, the reviewed Asupersync root is 0.5.0, and other suite projects carry their own older pins. Record the selected commit and adapter tests; do not assume nominally similar `Cx` types are interchangeable.

### 4.5 FrankenTerm: glyph engineering, not a ready-made clean GUI

The concrete `GlyphKey` includes font identity, style, metrics, feature-axis information, and subpixel positioning; a borrowed key avoids allocations during lookup. This is useful guidance for a warm text-rendering path. The GUI manifest, however, includes the large terminal/WezTerm family and many outside dependencies. Importing it wholesale would violate the central dependency requirement and import a product model we do not need. [R3]

The new GUI should share a small, dependency-clean atlas/key package when practical. It should not translate source into terminal cells simply to reuse more of FrankenTerm.

The inspected `atlas_tiered_swap.rs` is explicitly a **policy substrate**: actual GPU blits, disk I/O, and parts of integration are deferred in that source. Reuse its region bookkeeping, bounded pressure decisions, and eviction concepts, not an assumed working GPU-paging implementation. On Apple unified memory, moving a texture to a CPU allocation may duplicate bytes rather than relieve physical pressure. FCB selects discard/recompute/compress/retain actions by measured end-to-end cost and actual released allocations. [B3]

### 4.6 FrankenNetworkX: compact identities and deterministic graphs

The inspected graph class already stores integer-indexed adjacency rows and revision-keyed derived caches, with explicit care that independently mutated clones must not share caches keyed only by a coincidentally equal revision. Adopt that identity discipline directly. [R4]

Use native selected algorithms over immutable integer snapshots. The freshly inspected `DiCsr` already stores outgoing and incoming offsets/targets; its parallel-edge-collapsing variant is expressly a structural projection. Extract that useful view with validated size/index contracts and preserve an independent multiplicity/provenance table when counts matter. A source browser does not need arbitrary Python object labels, every NetworkX algorithm, or full attribute maps in every edge. [B4] The existing general algorithm manifest still brings external matching, random, parallelism, and serialization packages, so a smaller first-party extraction is necessary.

### 4.7 CASS: progressive discovery without silent degradation

CASS distinguishes canonical records from rebuildable lexical/vector assets and publishes asset generations rather than mutating what readers are already using. Its progressive search implementation separates initial results, refined results, and refinement failure. These are strong patterns for a responsive code browser. [R5]

FrankenCodeBrowser borrows the pattern, not mandatory neural search. Fast path/path-prefix/literal matches should work with no downloaded model. Later ranking improvements must not reorder the row under a user's pointer or change the selected identity.

CASS's inspected `pack_planner.rs` additionally supplies a concrete pattern for bounded evidence selection, freshness, source readiness, and omitted-item accounting. FCB adapts it into reading trails and source-context packs: selected source plus relevant declarations/documentation and an explicit explanation of what was omitted. Its existing character-per-token estimate is not a tokenizer. Byte/character budgets can be exact; token counts are marked estimates unless an explicitly selected exact tokenizer exists. The planner currently imports outside packages and needs upstream factoring. [B2]

### 4.8 FrankenSQLite: actor ownership is not optional

The inspected `Connection` is deliberately `!Send`. `AsyncConnection` owns it on a dedicated worker and exchanges commands and responses. Its cancellation path distinguishes an interrupted operation from a publication that already committed. This is a better starting point than inventing a pool of freely movable connections. [R7]

The project will use a dedicated persistence service, or the exact qualified async facade, and bridge the actual `fsqlite_types::cx::Cx` and native Asupersync context contracts. No DB access belongs in frame assembly. The current VFS's unsafe/native boundary and the selected extensions remain independently auditable.

### 4.9 FrankenTUI: reuse behavior, widen arithmetic

The concrete Fenwick tree uses a contiguous `Vec<u32>` and wrapping arithmetic. That is useful for its original domain but not suitable as-is for a height index over extremely large documents. Factor the algorithm **upstream in FrankenTUI** into a checked wide-domain primitive with `u64` fixed-point sums and validated updates. General document-layout rules that consume it remain upstream in FrankenMarkdown. [R8]

Also reuse pane-tree transactions, focus graph concepts, deterministic model transitions, and virtualization policies where clean separation is possible. The pane stability contract offers a useful curated facade with `PaneTransaction`, semantic input, reversible versioned arrangements, focus, and accessibility vocabulary. Its stable re-export promise does not automatically extend to raw internal modules. FCB should obtain a supported non-terminal extraction upstream, not depend on whichever private path happens to compile. The current `ftui-layout` manifest still has unconditional outside dependencies even with empty defaults. [B5]

A Fenwick tree supports point updates and prefix queries, not arbitrary inserted/deleted rows in logarithmic time by itself. Document structural changes use paged leaves plus a checked prefix directory, or a measured bounded rebuild. Native accessibility and proportional text layout remain separate requirements.

### 4.10 FrankenManim and FrankenThreeD: the right renderer lessons

FrankenManim's implemented renderer separates topology, geometry, transform, style, order, image, and camera revisions. A color change need not rebuild geometry; a camera move need not decode a font. FrankenCodeBrowser adopts the same principle, with its own source/layout/text/atlas/device dependencies. [R9]

FrankenThreeD's implemented core provides generational handles and device generations, including tests that retire exhausted slots rather than resurrect stale handles after wraparound. However, the inspected basic handle has only slot and generation, not an arena/owner identity. Two independent embedded browsers can create the same pair. FCB therefore validates an owning instance/arena identity in addition to the reused handle; GPU handles also identify the renderer/device owner, not merely a generation number. [B9] Its core's inspected manifest permits a `std` profile without its optional serde dependencies. This is a plausible narrow direct reuse. The actual GPU renderer remains absent, so this plan assigns the native renderer real work packages instead of assuming it can be imported. [R10]

### 4.11 New reusable substrate discovered in FrankenManim

`fmn-hash` contains an actual in-house SHA-256 and versioned, bounded canonical serialization. These are stronger starting points than inventing a hash or binary envelope for each FCB subsystem. Its manifest currently depends on `fmn-core`, whose own graph includes deterministic-math and random-core packages. Factor the reusable digest/envelope layer upstream where necessary; do not call the existing root graph `std`-only without resolving it. [B8]

`fmn-cache` provides complete-input keys, namespaced schemas, immutable entries, checksums, pins, and generation-aware cache clearing. Preserve those ideas while replacing unsuitable assumptions: its own source does not promise immunity to hostile same-user path replacement, and a wall-clock stale maintenance lock is not proof that a process is dead. FCB's native cache authority and process coordination must meet their own stricter contract. These caches are optimizations, never the authority for source or personal annotations. [B8]

---

## 5. User experience and interaction specification

### 5.1 Window structure

```text
┌─ native title bar / repository switcher / command search ────────────────────┐
│ Back  Forward  Root › crates › module     Search…    2D / City    View       │
├──────────────────────────────────────────────────────┬──────────────────────┤
│                                                      │ Inspector | Results  │
│              SPATIAL REPOSITORY ATLAS                 │ History | Outline    │
│                                                      │                      │
│   nested directory regions                           │ selected file        │
│   file parcels / source bands                        │ source facts         │
│   semantic labels / match overlays                   │ exact source anchors │
│                                                      │ relationship details │
│        ┌─ optional pinned reading lens ─────────┐    │                      │
│        │ Source | Markdown preview | Split     │    │                      │
│        │ selectable, pixel-crisp content        │    │                      │
│        └────────────────────────────────────────┘    │                      │
├──────────────────────────────────────────────────────┴──────────────────────┤
│ index coverage • current scope • source revision • optional performance HUD │
└─────────────────────────────────────────────────────────────────────────────┘
```

The sidebar is collapsible and resizable. All panels can be navigated without a mouse. The central surface remains visually dominant. A conventional outline/file-tree view is available for precision navigation and accessibility; it is an alternate projection of the same model, not a second database of files.

### 5.2 Camera controls

Two-finger scrolling pans the atlas by default. Pinch zooms around the pointer or gesture centroid. A configurable modifier plus wheel provides zoom for conventional mice. Space-drag pans. Double-click focuses a directory/file, while an explicit action promotes its content into a reading lens.

In City mode, orbit requires a deliberate modified drag so it does not conflict with text selection or ordinary panning. Escape backs out of an active gesture or lens before changing workspace scope. “Fit selection,” “fit parent,” and “fit project” are first-class commands. Reduced-motion mode replaces flight animation with a direct transition and a short, nonmoving location highlight.

Zoom retains the world point under the gesture anchor. Interrupted camera animations immediately hand control to the user; they do not fight subsequent input. Camera history records intentional navigation endpoints, not every wheel tick.

### 5.3 Selection and reading

A single click selects a parcel or source entity. Selection is stable while analysis is refined. Enter opens the reading lens. The lens supports line numbers, horizontal scrolling, optional wrapping, find-in-file, visible whitespace, bracket/indent guides, selected-range copying, and exact line/column navigation.

A source location is represented internally by file identity, source revision, and byte range. Displayed line/column coordinates are derived. Copying a source selection returns the exact selected source text under a documented line-ending policy; rendering must not silently alter it. A separate “copy with location” action includes provenance.

Dragging inside a reading surface selects text. Dragging its title bar moves the pane. Moving a pane does not change the atlas layout. Pinning two or more files permits side-by-side reading and a restrained relationship tether on the map.

### 5.4 Search surfaces

Global search starts with an obvious scope chip: workspace, selected directory, selected file, or current visible region. Path, text, symbol, and Markdown-heading modes are distinct. Results display a path, source excerpt, matched range, and completeness state. A result click updates selection and source position before any decorative camera motion completes.

Results and atlas overlays share the same query generation. Stale result batches never repaint current results. Search refinement preserves selected identity and only moves rows when the user is not interacting, or on an explicit refresh action.

### 5.5 Inspector

The Inspector shows useful source facts: path, language capability, byte/line count, current revision, indexing status, outline, inbound/outbound relationships, and nearby documentation. Heuristic facts have an explicit badge and explanation. Unknown facts are unknown, not zero.

Optional diagnostics can expose render cost and data residency, but normal users should not need to understand cache generations to navigate code.

### 5.6 Markdown and documentation

Markdown files offer Source, Preview, and Split. Source and preview synchronize through spans, not through percentages of scroll height. README discovery is explicit: the user can open a directory's primary documentation without hiding its source files. Links to source paths and heading anchors resolve within the current root's capabilities.

Code fences share the source highlighter and color palette. Math and diagram support is reported by capability. Unsupported syntax remains visible in a useful form rather than disappearing.

### 5.7 Visual system

Provide a carefully designed light theme, a reference-inspired charcoal theme, and a high-contrast theme. Follow the user or host appearance preference; the recording is not a reason to force a dark-only interface. Directory borders, language color accents, selection, search matches, and diagnostics must have distinct visual roles. Never rely solely on hue.

Choose a small spacing/type scale, restrained elevation, readable labels, and crisp code. Avoid constant bloom, animated background effects, excessive depth of field, blurred reading text, and gratuitous camera easing. The scene should feel precise and responsive, not like a game engine demo wearing editor chrome.

---

## 6. System architecture and crate boundaries

### 6.1 One engine, three embedding levels, one application

```text
                 external Rust consumer                       fcb executable
             /             |              \                         |
     source/search     semantic view    native Mac view               |
     without GUI       + FramePlan      host integration              |
             \             |              /                         |
                       public fcb facade <----------------------------+
                              |
              source / analysis / map / search / UI model
                       |                      |
           FrankenMarkdown upstream     immutable FramePlan
           parse / flow / source maps   + matching interaction snapshot
           fonts / math / display              |
                       |               renderer / Metal adapter
                       +----------------------|
                              franken-macos safe system boundary

     optional fcb-runtime → host-supplied Asupersync parent region
     optional fcb-store   → qualified FrankenSQLite persistence actor
```

The standalone app creates the host resources. An embedding host supplies or explicitly delegates them. The renderer consumes immutable snapshots and bounded update packets; it does not own filesystem truth. A Markdown renderer does not know about FCB windows. A database service never holds a lock required to paint.

### 6.2 Proposed workspace and upstream ownership

These names specify future boundaries, not already available APIs. Publish only independently useful components; small implementation modules do not each need a crate.

| Crate or upstream component | Responsibility | Boundary |
|---|---|---|
| `fcb` | Curated public Rust facade, capability discovery, stable public re-exports. | Library only; no implicit startup or effects. |
| `fcb-core` | Owned IDs, validated ranges, limits, errors, snapshots, semantic requests. | `std` and a qualified narrow handle primitive; no OS GUI/runtime/database. |
| `fcb-source` | Immutable byte captures, range/encoding maps, sparse line index, source-provider interfaces. | Provider-driven; no implicit host filesystem authority. |
| `fcb-analysis` | Source-specific outlines, facts, checkpoint orchestration and graph integration. | Uses upstream FMD lexers; does not implement a second lexer family. |
| `fcb-map` | Stable hierarchy layout, camera, spatial queries, semantic LOD, summaries. | Pure bounded algorithms on snapshots. |
| `fcb-document` | FCB lens/session state, source-ID translation, asset requests, scheduling and integration of FMD output. | **No Markdown parser, generic flow engine, typesetter, diagram engine, or copied highlighter.** |
| `fcb-search` | Source/path query contracts, exact verification, source-specific indexing and progressive results. | Headless use; selected first-party kernels only. |
| `fcb-ui` | Deterministic reducer, focus, panels, source selection, commands, immutable frame/interactivity plans. | No direct I/O, GPU waits, global logging, or application lifecycle. |
| `fcb-render` | Backend-neutral batching/resource requests and native Metal execution adapter under an explicit feature. | CPU plan use without native backend; host-provided render target and ownership. |
| `fcb-runtime` | Asupersync request ownership, deduplication, budgets, services and publication. | Accepts a compatible host runtime/parent region; does not silently construct one. |
| `fcb-store` | Optional persistence service, schema, manifests and migration. | Qualified FrankenSQLite actor; not required for ephemeral library instances. |
| `fcb-app` | Composition root; **binary name `fcb`**; CLI and Mac application startup. | Only layer permitted to own app-wide event loop, runtime creation, menus, and default storage. |
| `fcb-conformance` | Test corpus, embedding consumers, replay, native/hardware qualification. | Test tooling; not linked into normal application. |
| FrankenMarkdown root modules, proposed `flow`, `display`, `source_map` | Reusable document parsing/provenance, continuous flow, layout, renderer-neutral drawing/interaction output. | Implemented and maintained **inside `franken_markdown`**. No dependency on FCB. |
| `fmd-font`, `fmd-math`; optional proposed `fmd-font-macos` | Shared text/font/math APIs; system shaping/raster adapter behind an optional Mac boundary. | All in FrankenMarkdown ownership. Base engine stays platform-neutral; no forced Asupersync. |
| Shared `franken-macos` | Safe AppKit/Metal/CoreText/CoreGraphics/input/accessibility/filesystem platform primitives. | Narrow system-ABI crate at `native/macos/` in this repository, separate from product semantics. The facade does not depend on FCB or FMD; FCB and `fmd-font-macos` may consume it. |

Avoid an upstream cycle: start with flow/provenance/display modules in the existing FrankenMarkdown root. Do not create an `fmd-flow` crate that depends on that root and then make the root depend back on it. A later split first extracts shared document types downward and must preserve acyclic imports. The native font adapter can depend on `fmd-font` plus the system bridge; the system bridge does not depend on the adapter.

### 6.3 Public library contract

The following conceptual types describe required roles, not compile-tested signatures:

```text
SourceProvider       → enumerate/open exact source captures within granted scope
BrowserSession       → independent owner of one browser model and its request generations
BrowserView          → one viewport, focus/selection and reading-lens arrangement
HostServices         → explicit resource, clock, wake and capability interfaces
FramePlan            → immutable bounded drawing plan + matching interaction snapshot
NativeViewBinding    → optional host-owned Mac view/render integration
SessionClose         → explicit asynchronous drain/report for owned work
```

Constructing a pure session is inert: no global runtime, thread, window, filesystem scan, environment-variable read, signal installation, model download, logging subscriber, allocator replacement, or process exit. Effects are requested through host services or returned as commands. A zero-provider instance may display host-supplied in-memory source immediately.

The facade exposes the smallest coherent vocabulary. Upstream syntax/document engines stay accessible through their own public APIs rather than FCB publishing a competing Markdown abstraction. Public source anchors and result types do not expose SQLite rows, native pointers, private renderer structs, or Asupersync internals unless a deliberately runtime-specific module requires them.

Public errors retain stable codes and context. Use non-exhaustive enums where future variants are expected, explicit constructors for invariants, additive methods/configuration, and versioned persisted/wire formats. A Rust library ABI is not promised stable between compiler builds; compatibility means a supported source/API and artifact-schema policy.

### 6.4 Feature matrix

Cargo features are additive and may unify through another dependency. They must enable capabilities, never act as mutually contradictory `disable_*` policy toggles. `default-features = false` on one edge cannot erase a feature enabled elsewhere. Test actual resolved graphs. [B10]

| Consumer profile | Enables | Must not require |
|---|---|---|
| `fcb` with default features empty | Core/session vocabulary and explicit in-memory primitives. | AppKit, Metal, SQLite, a runtime, disk assets, CLI parsing. |
| `source` / `search` / `map` | Independently useful headless functionality. | A GPU, a window, persistent storage, an installed app. |
| `markdown` | `fcb-document` and no-default FrankenMarkdown engine/flow integration. | FMD CLI/batch/wasm-bindgen features or a WebView. |
| `view` | Deterministic UI model and renderer-neutral frame plans. | Taking over the event loop or creating a device. |
| `runtime` | Compatible Asupersync host integration, or an explicitly requested owned runtime (§16.9). | A secretly created global executor. |
| `persistence` | Qualified `fcb-store`; explicit runtime/context integration. | Mandatory persistent state for unrelated profiles. |
| `macos-metal` | Native rendering/view adapter and optional system text support. | Whole FrankenTerm GUI, third-party graphics bindings. |
| `fcb-app` release | Explicit fixed feature set for the full standalone product. | Any undeclared runtime, downloaded model, or separately installed GUI companion. |

Choose exact feature names after G0 prototypes, but these separations are required. Native features are target-gated; unsupported target requests produce a clear compile-time or capability error, not an apparently functional stub. `all-features` is not the release configuration. Isolated consumer workspaces test each profile without receiving accidental features from dev dependencies or another workspace member.

### 6.5 Host lifecycle and rendering ownership

A native embedding host owns its `NSApplication` run loop, window/view hierarchy, and display lifecycle. FCB attaches a view or encodes into a safe borrowed render-target lease, using typed main-thread/device tokens supplied by the first-party bridge. A safe API never accepts a raw pointer disguised as `usize` and treats it as a valid native object.

Exactly one owner acquires/presents each drawable. The host either delegates this role to the FCB binding or retains it and supplies the current target. FCB never calls a second `nextDrawable` or presents the host's frame again. Target format, size, sample count, color space, clip, device identity, and completion ownership are validated before encoding. Arbitrary external Metal pointers are not a supported safe integration route; a separately audited unsafe adapter would be a named host responsibility, not hidden in the core.

`BrowserSession::close` conceptually stops admission, invalidates subscriptions, cancels/drains its own requests, and returns an ownership outcome. It never shuts down the host runtime or device. Dropping a view cannot synchronously wait on a GPU, filesystem, or database operation. Host teardown must continue servicing required completion/drain callbacks until ownership is discharged; an explicit abandonment/fail-stop policy handles foreign operations that cannot finish. There is no universal deadline promise for a stuck driver.

Instances are independently namespaced. Fonts and immutable source content may be shared only through an explicitly supplied cache domain with consent and accounting. Shared providers do not imply shared private annotations or root access. A host may use unrelated libraries elsewhere; FCB's safety and closure claims apply to its actual selected path and declared boundary.

### 6.6 Thread and data contracts

Pure immutable snapshots can be `Send + Sync` only when every field's type and semantics justify it. UI reducer state has one owner. Native view objects and thread-affine font/device operations keep explicit restrictions; no blanket unsafe `Send`/`Sync` implementation is added to make a trait convenient.

`DisplayMetrics` carries logical points, physical pixels, backing scale, color configuration, and a generation. A frame is immutable after submission. The matching hit-test and accessibility snapshot is published with that frame, not independently ahead of its pixels. Changes become visible through bounded deltas, not a global write lock.

Callbacks enqueue bounded typed events or signal preallocated completion cells. Reentrant platform calls never reborrow the reducer mutably. Public synchronous methods are nonblocking and bounded; potentially large operations are resumable commands or explicitly asynchronous services.

### 6.7 Minimal external-consumer qualification

Maintain real examples outside the primary workspace feature graph: an in-memory source/search tool with no native libraries; a headless map/frame-plan consumer; a Mac host embedding two independent FCB views with one compatible runtime; and the standalone `fcb` binary. The app must not access a private convenience API that the embedding example cannot use.

Measure code size/build graph, startup effects, bounded close, multi-instance IDs, cache privacy, host event-loop ownership, and selected features. Test that dropping one browser leaves the other and the host operational. This is the acceptance criterion for “modular library,” not merely adding `lib.rs` next to `main.rs`.

### 6.8 Reuse policy

Prefer a narrow supported upstream API over source copying. All reusable Markdown-related changes belong to FrankenMarkdown immediately, without a second-consumer exception. Other reusable algorithms are factored in their appropriate owner repository with clean profiles and tests. FCB-specific geography, source browsing, selection coordination, and app policy remain in FCB. The shared system bridge owns only low-level platform obligations.

Do not wait for every ambition of FrankenThreeD, FrankenManim, or FrankenNetworkX. Consume the smallest implemented and qualified contract. No uncommitted sibling patch or local path override is accepted as a release dependency.

---

## 7. Identity, snapshots, and invalidation

### 7.1 Distinct identities

Keep these domains separate:

| Identity | Meaning |
|---|---|
| `BrowserInstanceId` / `ArenaOwnerId` | A distinct library instance and ownership domain; not merely a slot-generation pair. |
| `WorkspaceId` | A user's logical workspace configuration. |
| `RootId` | One authorized source root and its namespace. |
| `FileId` | Session/persisted logical file identity within a root. |
| `SourceRevision` / `CaptureExtentId` | Identity of a complete captured byte sequence or explicitly bounded captured extents; never an unstated atomic live-file guarantee. |
| `AnalysisRevision` | Parser/highlighter capability and configuration applied to that source. |
| `LayoutRevision` | Spatial placement or document layout generation. |
| `QueryGeneration` | One search request and its immutable scope/options. |
| `WindowGeneration` | A particular live window incarnation. |
| `DeviceId` + `DeviceGeneration` | The actual device owner plus its resource-lifetime generation. |
| `PresentedFrameId` | The accepted visual/interaction snapshot and display-metrics generation. |

Paths are labels and lookup keys, not permanent source identities. A rename may preserve identity when the filesystem observation supports it; a delete followed by a new file at the same path must not inherit old annotations or stale analysis blindly. Content hashes deduplicate bytes but do not identify logical files by themselves.

### 7.2 Publication token

Every background result carries enough identity to reject obsolete work:

```text
PublicationToken = (
    browser/session owner and workspace/root identity,
    delivery endpoint incarnation and request generation,
    file identity when applicable,
    source revision,
    analysis/configuration revision,
    layout/display/renderer generation when the result depends on them
)
```

The consumer validates the owner, delivery incarnation, and every relevant dependency in the token against its current request and captured source. Independent analysis may omit camera state; a geometry/hit-test result cannot omit the layout/display identity that makes its coordinates meaningful. A stale result can be admitted into a content-addressed cache when valid for that exact content, but cannot replace current visible state. Closing a window invalidates its delivery endpoint; it does not necessarily cancel shared workspace indexing used by another window.

### 7.3 Immutable snapshots, bounded publication

Background work constructs immutable artifacts off-thread. A single designated owner publishes an `Arc` snapshot at a frame boundary or applies a bounded delta to the model. Do not hide a global `RwLock` behind the word “snapshot.” The UI must never wait while a worker holds a write lock across parsing or disk I/O.

Snapshot retention is budgeted. An old query or pinned source view may retain an older source revision, but its bytes count toward memory. A slow consumer is eventually told its optional stream was compacted and must resnapshot; unbounded historical versions are forbidden.

### 7.4 Invalidation matrix

| Change | Must invalidate | Must not invalidate |
|---|---|---|
| Camera pan | Visible-set query and camera uniforms. | Source, lexical spans, font outlines, document paragraph layout. |
| Camera zoom within one LOD band | Projection/uniforms and selected raster scale if needed. | Full repository layout or parser state. |
| Selection color | Selection overlay/style references. | Geometry and text shaping. |
| Theme color | Style palette and affected cached colored tiles. | Source token classification or glyph outlines. |
| Font size in reader | Reader line layout, glyph scale entries, height index. | Source bytes, lexical state, repository geometry. |
| File content update | That source's derived analysis and relevant indexes. | Unrelated files' source or glyph outlines. |
| Directory membership change | A bounded layout patch and membership indexes. | Every unaffected neighborhood's placement. |
| Markdown reference definition change | Dependent resolved links/layout where affected. | Independent source files. |
| Device reset | All GPU handles/resources for that generation. | CPU source, analysis, navigation, or user annotations. |

Cache keys include the identity of the owning snapshot as well as its revision. Generation counters do not wrap into previously valid handles; retire exhausted IDs or perform an explicit whole-domain reset. [R4] [R9] [R10]

### 7.5 Owner-qualified identities, not just generation counters

A `(slot, generation)` pair is unique only inside one arena. The reused FrankenThreeD basic handle does not itself identify that arena. Wrap it in an FCB-owned identity domain that includes browser/session or arena identity. GPU resources additionally carry renderer owner, actual device identity, device generation, and allocation generation. Never compare a generation number from two independent devices as if equality implied identity. [B9]

Persisted source/user IDs are distinct from ephemeral arena handles. OS file identifiers can be reused and cannot replace logical identity indefinitely. Raw untrusted wire handles are validated against the receiving owner and granted capabilities before lookup. Counter exhaustion retires a domain or slot; no wraparound resurrection is allowed.

### 7.6 Presented-frame coherence

`FramePlan` bundles drawing data with the exact camera, scene, source, layout, display-metrics, and interaction generations used to produce it. The application maintains a bounded record of the last confirmed presented frame and, where needed, pending frames. A click is resolved against the frame the user could see, with a clearly defined timing policy, not a newer model whose objects have moved but whose pixels have not appeared.

CPU hit testing, text selection geometry, link activation, and accessibility hit-test geometry use the same accepted snapshot. Hover may show a provisional response during an update; activating an exact source range revalidates its captured identity. A resize or backing-scale transition cannot combine old pixel coordinates with new logical geometry. Multiple queued frames are not multiple authorities for one input event.

GPU presentation timestamps and event timestamps require a verified monotonic-clock conversion before associating frames and input. When the platform cannot identify the exact presented frame, use the latest conservatively known frame and expose this limitation in timing evidence; do not invent nanosecond precision.

### 7.7 Destruction is work, too

Building an immutable snapshot off-thread does not make dropping it cheap. Replacing the last `Arc` on the event/render thread can recursively free a huge document, vector tree, or source chunk set. Publication therefore reserves a bounded retirement slot and retains a retirement owner before swapping large artifacts. The maintenance service releases them outside the interaction path, under the global byte and queue budget.

Do not solve this with an unbounded garbage queue. If retirement capacity is exhausted, defer the optional publication, retain the old frame, or reclaim through bounded work off the UI thread. GPU resources use completion-owned retirement; CPU object destruction is not evidence that a device has finished reading a buffer. Resource lifetime tests measure teardown stalls as well as leaks.

### 7.8 Shared work without shared cancellation mistakes

Use a bounded single-flight table keyed by exact source capture, analysis options, and operation kind. Two readers asking for the same lexical checkpoint or glyph result can share work. Each subscriber has its own delivery generation and cancellation. Canceling one view detaches that subscriber; the shared operation is canceled only when no permitted consumer or retained service requires it.

A closed endpoint cannot be reused to deliver to a new view with coincidentally equal counters. Deduplication includes root/privacy domain where needed. Failed and incomplete results are not cached as final successful facts. Bound the number and byte cost of obsolete draining requests so rapid typing cannot accumulate unlimited cancellation work.

---

## 8. Filesystem ingestion and live updates

### 8.1 Staged discovery

Opening a root starts three bounded waves: immediate root/directory discovery; metadata and coarse map population; source analysis and index enrichment. The first useful frame depends only on the first wave. Coarse map entries can exist before their full source is read.

Walk directories with an explicit work queue, not recursive call-stack traversal. Stream directory entries, cap open descriptors, validate path lengths and depth, and bound every batch's byte and entry count. Sort only the portion needed for stable publication; do not allocate an entire million-file listing as a prerequisite to rendering anything.

Prioritize selected/visible directories, recent files, and active search candidates. Background discovery is still fair: a user repeatedly interacting with one file must not permanently starve the rest of the repository.

### 8.2 Exclusion semantics

Provide first-party ignore matching for an explicitly tested Git-style pattern subset, plus application exclusions. Its conformance corpus covers anchored patterns, directory patterns, `**`, negation, escaping, and nested rule precedence. Unsupported patterns are reported, not guessed.

Default policy excludes source-control object databases, dependency caches, common build outputs, and binary payloads from source analysis, with a visible “excluded” count and an easy scope override. A user can browse excluded paths deliberately. Exclusion is not deletion and must never cause user annotations to be discarded.

No invocation of `git`, `find`, `rg`, a shell, package managers, or compiler tools is required to discover and read a directory tree. The initial product works on ordinary non-Git folders.

### 8.3 File identity and consistency

A file read records relevant file identity and metadata before and after reading. If the file changes, retry within a bounded policy or publish a clearly identified observed snapshot, then schedule another read. A before/after stat match is a useful detector but not proof of an atomic filesystem snapshot; the digest identifies the bytes actually read. Never claim a cross-file-consistent repository commit from an ordinary live-tree scan.

Use owned read buffers for mutable source. Do not memory-map a working-tree file and assume it cannot be truncated or replaced. This avoids turning external file modification into a process-level memory fault and keeps the semantic core's memory safety straightforward.

For source operations requiring stronger coherence, operate on explicitly captured immutable application-owned snapshots or an independently specified version-control provider. Do not invent Git-history capability from simple file observation.

### 8.4 Watcher semantics

Native file notifications are **hints to reconcile**, not a complete authoritative event log. Coalesce bursts by identity/path; recognize overflow, dropped events, root movement, volume disappearance, and permissions changes; and schedule bounded reconciliation scans. The bridge exposes these conditions explicitly.

A failed directory read is not evidence that its files were deleted. Preserve the last-known entries with an unavailable/stale status until a successful reconciliation establishes their state. Atomic editor saves, rename chains, symlink swaps, and rapid create/delete cycles have dedicated fixtures.

### 8.5 Root capabilities and symlinks

Opening a root creates an explicit read capability. Symlink traversal beyond that root is disabled by default; following one requires a separate approved root capability. Normalize paths for display without conflating case-sensitive identities or lossy Unicode strings. Keep raw native path bytes where needed.

For operations requiring strict path confinement under concurrent mutation, use a reviewed descriptor-relative/no-follow native service in `franken-macos`. A lexical `starts_with(root)` check is not a security boundary against symlink replacement. Source roots are read-only to FrankenCodeBrowser unless a future, separately specified editing feature is enabled.

### 8.6 Reconciliation epochs and non-file objects

A reconciliation pass has a directory identity, scan epoch, start observation, and explicit completion status. Absence becomes deletion evidence only after that directory's relevant enumeration completes successfully and intervening dirty hints have been reconciled. A partially read directory, budget exhaustion, permission failure, or canceled scan cannot tombstone unseen entries.

Do not read FIFOs, sockets, character/block devices, or unknown special objects as source files. Validate the opened object, not just an earlier path metadata result, and use the platform service's safe nonblocking/no-follow admission where required. A malicious repository must not turn opening a file preview into an indefinitely blocking named-pipe read.

Never index FCB's own cache, capture store, database sidecars, export scratch, or recovery directories when they happen to lie under an authorized source root. Track their actual ownership/identity, not only a conventional basename. This prevents self-generating watcher/index loops. Multiple names for one file remain distinct namespace entries where appropriate; shared bytes do not erase path meaning.

### 8.7 Stable discovery without pretending unordered enumeration is sorted

Streaming discovery and deterministic sorted initial placement are different operations. For a very large directory, gather a bounded page, spill sortable metadata to owned scratch if necessary, and publish a provisional aggregate until a stable child-order generation is ready. Alternatively, use a documented deterministic bucket/partition scheme with reserved capacity. Do not sort each arriving batch and claim the final layout is independent of directory enumeration order.

A committed layout generation freezes its effective weights and ordering. Updating a provisional byte estimate to an exact line count can refresh a legend without automatically moving every neighboring file. Reweighting that changes placement is an explicit bounded layout transaction. The selected file stays identifiable and readable while discovery/layout generations settle.

### 8.8 Authorization, identity continuity, and optional providers

Separate a logical source provider from a native path. In-memory or host-supplied immutable sources work without granting filesystem access. A provider states its capture, ordering, range-read, and cancellation guarantees; FCB cannot infer stronger consistency from an interface returning bytes.

For atomic-save replacements, preserving a logical file identity is an explicit namespace-continuity decision, not an assumption that its inode stayed the same. Reattach annotations to new bytes only when the chosen exact or qualified anchor-mapping rule succeeds. Otherwise keep an orphaned/stale annotation with its original source evidence. Never attach an old note to an unrelated new file simply because a path was recycled.

### 8.9 Root-grant lifecycle and native permissions

A persisted root path or `RootId` is not itself a current access grant. On reopen, restore only the scope authorized by the user's saved policy and revalidate the native access route. G0 records the standalone and embedded sandbox/entitlement models; it must not assume an in-process root capability grants OS permission. For a security-scoped bookmark route, resolve the bookmark, handle stale data, and balance successful access acquisition with release after outstanding native users finish. A navigation bookmark is a different object from an OS access bookmark. Failed restoration leaves a visible unavailable root and a deliberate reauthorization action, not an empty successful workspace. [B14]

The sandbox decision is cross-cutting, not a packaging detail. App Sandbox constrains the local IPC endpoints in §19.8, the external editor/link handoff in §22.6, watcher and font access scope, and makes security-scoped bookmarks the only durable access route across launches. An unsandboxed notarized distribution has none of those constraints and no bookmark requirement, but still needs the grant lifecycle above. G0 records the choice together with its consequences for each of those sections; no section may assume the other model.

Every root grant has a revocation generation. Revoking it stops new admission and invalidates pending deliveries, link activation and new exports from that grant. Serialize grant revalidation with the export's publication decision so revocation cannot slip between a permission check and a new authorized effect; an already published destination retains its completed outcome. Drain foreign reads safely while discarding their results; another explicitly authorized root/session may retain independently permitted data. State whether already displayed captures are withdrawn under the selected host policy, and do not promise to retract bytes already returned to a host, copied to the clipboard or exported. Root access revocation, source-cache clearing and annotation deletion are separate actions. Test revocation during a read/query/export, unavailable volumes, and root restoration under changed native permissions.

### 8.10 Traversal cycles and path presentation

Following an authorized in-root symlink still requires cycle control. Detect repeated opened directory identities along the current traversal ancestry, bound alias expansion, and report a cycle/alias limitation rather than repeating the same subtree until memory is exhausted. Do not globally deduplicate every path by file identity: two legitimate namespace entries may share bytes while retaining distinct path meaning. A depth limit is a fallback budget, not proof of a cycle-free traversal.

Keep raw paths for identity and reversible interchange; render control characters and bidi formatting characters in filenames through an explicit escaped display form. A filename containing a newline must not forge extra result rows or diagnostics. Resolve link URI decoding and native path components once under a defined policy before the confined open; relative-looking encoded traversal and nonlocal `file:` authorities do not acquire a new grant. Test aliases, loops, raw bytes, control characters and encoded path traversal without executing repository content.

---

## 9. Spatial layout and semantic zoom

### 9.1 The atlas is a retained spatial index

The repository hierarchy is stored separately from its layout. A layout node contains compact identity, parent/child references, a local rectangle, aggregate metrics, and the revision of the source hierarchy used to produce it. Children use parent-local coordinates. A camera-relative transform produces small GPU coordinates close to the viewport origin.

Use a packed hierarchy and a bounding-volume/spatial index so viewport traversal can reject entire subtrees. The visible work target is proportional to visible aggregate nodes and admitted details, not the total number of files. Very distant subtrees are represented by aggregates even when individual leaves are technically inside the viewport.

### 9.2 Stable treemap, not constantly re-sorted squarification

A conventional size-sorted treemap can move many nodes after a small size change. That is unacceptable for a tool whose value depends on spatial memory. The initial algorithm should combine deterministic directory ordering, bounded-aspect-ratio subdivision, reserved slack, and local subdivision policies.

Recommended first implementation:

1. Establish a deterministic initial order using normalized display names with a raw-path tie-break.
2. Assign each directory a retained partition tree. Initial splits balance bounded weights while penalizing extreme aspect ratios.
3. Keep existing partition relationships when file sizes change. Allocate small additions from retained slack or locally subdivide the affected leaf/neighborhood.
4. Apply bounded local repair when occupancy or aspect-ratio limits are violated. Preview substantial movements, and do not commit them during direct camera/text interaction.
5. Offer an explicit **Repack layout** command for a global improvement. Preserve the old map as a restorable layout generation.

Do not claim that every insertion preserves every coordinate forever. The contract is stable unaffected neighborhoods, deterministic changes, a measurable displacement budget, and explicit global repacking.

### 9.3 Weights and pathological files

Raw byte size can let generated or minified files dominate the entire map. Offer named size metrics, with a sensible bounded default. An initial candidate is a capped, sublinear transformation of analyzed source lines, with metadata-byte fallback while analysis is incomplete. The legend names the metric and its treatment of unanalyzed files.

Unknown weight is not zero. A file that is present but unreadable retains a visible placeholder parcel. A huge file receives a useful atlas representation plus a paged reader; the atlas is not required to reproduce its full physical text area at overview scale.

Function blocks are nested subdivisions only for languages and sources with actual qualified structure. Otherwise use line-range blocks. Never manufacture a function boundary from indentation alone and label it as definitive.

### 9.4 LOD ladder

LOD admission depends on projected physical-pixel size and information utility, not on arbitrary world-space distance alone.

| Level | Representation | Admitted information |
|---|---|---|
| L0: repository | A few aggregate directory regions. | Major directory names, aggregate counts, selected/search indicators. |
| L1: neighborhood | Directory/file rectangles. | Important filenames, language/category accents, coarse metric legend. |
| L2: file structure | Line-density strips and qualified structural blocks. | Selected outline names, search-match density, large comments/doc sections. |
| L3: code texture | Precomputed token/line bands, sparse labels. | Approximate visual structure explicitly not a readable text substitute. |
| L4: readable source | Glyph instances from exact source and token spans. | Line text, selection, matching ranges, source navigation. |
| L5: reading lens | Frontal pixel-crisp source/document surface. | Full selectable reading experience with independent scrolling and tools. |

Starting thresholds are tuning parameters, stored in the qualification record. For example, admitting glyph text below a projected x-height that is legible on the current display is forbidden even when doing so would inflate a “glyphs rendered” benchmark. Use upper and lower thresholds around every transition so small zoom oscillations do not churn resources.

### 9.5 Label placement

Candidate labels are ranked by selection, search relevance, navigation context, importance within the visible hierarchy, and projected size. Pack accepted labels into a screen-space collision grid. Retain the previous accepted set where still valid to avoid nondeterministic flicker. A hard label budget applies per viewport.

Do not decode/shape every filename to decide whether it might be visible. Cache measured labels keyed by exact text/font/style and use conservative width estimates for low-priority candidates. The selected item always receives an accessible visible identity even when ordinary labels are culled.

### 9.6 Precision and camera math

Use `f64` camera transforms and hierarchical local coordinates on the CPU. Use checked wide integers for discrete document positions and line counts. Convert to camera-relative `f32` only after subtracting a nearby origin and validating the range. Large absolute world coordinates must never be sent directly to shaders and expected to retain single-pixel precision.

Pointer-anchored zoom follows a simple invariant: the world point obtained by inverting the old transform at the pointer must project to the same pointer position under the new transform. Round only at the appropriate raster boundary; do not repeatedly round camera positions and accumulate drift.

### 9.7 Navigation animation

A navigation flight is an optional interpolation between two explicit camera states with a maximum duration. It is not an autonomous simulation. A critically damped or bounded easing model can be extracted from first-party animation code, but the finished endpoint must be exact and deterministic for replay.

The camera update loop consumes elapsed monotonic time, not an assumed fixed number of frames. It clamps extreme elapsed intervals after sleep. User input cancels or retargets motion immediately. Reduced-motion mode uses direct transitions.

### 9.8 City mode

City mode extrudes existing rectangles rather than running a new force-directed layout. Height is a bounded transform of a chosen metric such as line count, change size between captured snapshots, or match count. Files retain their 2D footprints and source identity. A flat footprint remains available for context.

Use instanced cuboids/side faces, a restrained directional-light model, and outlines. Expensive shadows, transparency, and ambient effects are optional and default off until demonstrated useful. Text appears on a frontal lens or appropriately oriented readable surface; it is not forced onto unreadable oblique faces.

Picking first intersects the spatial hierarchy and the relevant extruded bounds on the CPU. More complex GPU picking can be added asynchronously, with camera/device/request generations, but must not stall the interface. A fast provisional hover must never be mistaken for a final exact text location.

### 9.9 Multi-resolution source summaries and focus islands

Build a bounded summary pyramid for each captured file and directory: line extent, token-class density, qualified structure, match counts, and source-range coverage. Distant atlas tiles and a reader minimap consume the same summaries. Palette-indexed semantic summaries survive color-theme changes without re-lexing; colored raster tiles remain separately keyed by palette/version.

A summary states whether it is exact, sampled, or incomplete. Estimated line density cannot advertise exact line counts. On a query update, publish a compact match overlay rather than recomputing static source summaries. On source replacement, invalidate only summaries dependent on the changed capture, retaining older ones solely for explicitly old-snapshot views.

Finite floating-point precision still imposes a limit. Parent-local `f64` plus camera rebasing prevents many deep-zoom errors but does not make arbitrarily deep, exponentially tiny partitions representable. Promote a selected neighborhood to a new **focus island** with an explicit transform/ancestor breadcrumb before precision would be lost. Preserve source identity and the route back; never keep subdividing until widths underflow to zero.

A focus island and reading lens are complementary: the first gives usable spatial scale to a deep subtree, the second gives usable text dimensions to a narrow/huge file. Both have keyboard equivalents and recorded semantic navigation states.

### 9.10 Cheap contextual insight without destabilizing geography

Offer orthogonal overlays for language, search coverage, documentation links, qualified dependency direction, and captured-snapshot changes. An overlay changes styles or bounded aggregate data, not the base partition. Pin a small set of landmarks and recent source anchors to help return navigation. A path or relation trail is retained as source identities, not screen coordinates.

Prefetch only the next bounded neighborhood or selected trail destination, deduplicated across consumers. A prediction miss wastes a capped amount of work and is canceled under pressure. No learned controller is necessary to obtain this benefit.

---

## 10. Source storage and large-file reading

### 10.1 Byte-first representation

Source storage owns immutable byte chunks and a checked `u64` logical length. Text decoding, line boundaries, token spans, and visual glyph runs are derived layers. Original bytes are never reconstructed from rendered glyphs.

For ordinary files, an owned contiguous snapshot is efficient. For large files, use bounded chunks and a sparse line index, with hot ranges retained separately. Chunk size is selected empirically from a small candidate set rather than hard-coded as an alleged universal optimum. Cross-chunk UTF-8 sequences, CRLF pairs, lexical tokens, and search matches are explicit boundary cases.

Source storage exposes range reads that return either exact bytes plus revision or a typed unavailable/canceled/error result. It must not silently load a multi-gigabyte file because a caller asked for its first visible screen.

### 10.2 Exact and observed snapshots

An in-memory owned snapshot is immutable. A file-backed, application-owned snapshot generation may be evicted and reread from that immutable generation. A live source file is not an immutable backing store. When retaining the full source is too expensive, record that only certain ranges were captured and do not pretend unrelated later reads belong to one atomic revision.

A digest is computed by an inspected first-party implementation when required for persistent identity. A noncryptographic fingerprint can accelerate cache lookup but cannot serve as the sole authority for untrusted content identity or tamper detection. Candidate byte deduplication verifies the actual captured bytes before treating distinct captures as interchangeable; a digest is not a globally unique logical FileId. Artifact integrity using SHA-256 has the stated cryptographic assumption, not a mathematical proof that collisions cannot exist. Authoritative source associations remain explicit even when storage is shared.

### 10.3 Line index

A line index stores sparse absolute offsets plus dense local offsets within selected chunks. `line → byte` and `byte → line` have explicit complexity bounds and return checked results. The representation supports files with more than 2³² bytes and long logical line counts without truncation.

Building the line index is a resumable scan. The first visible screen can be located without a full-file pass. An exact jump to a far line may require background indexing; expose that state rather than freezing the window. Exact byte-range jumps supplied by search can bypass a full line-number index.

### 10.4 Huge lines and horizontal virtualization

A one-line 500 MB generated file must not allocate one shape object per character. Separate line identity from visible horizontal ranges. Materialize glyphs only for a bounded window plus overscan **after obtaining the required shaping context under §10.9**. Search and copying operate on source bytes, not the currently shaped subsection.

Wrap mode on pathological lines is budgeted and may publish a progressive result. A long-line indicator explains that only a viewport window is materialized. The application must never truncate the source silently. Binary-like or invalidly encoded content gets a byte/escaped view with an honest mode label.

### 10.5 Reader height indexing

For fixed-pitch unwrapped source, line-to-y arithmetic is direct and checked. For wrapped source and Markdown, use a checked `u64` prefix-sum structure over measured/estimated block heights. For large datasets, organize it in pages so replacing a group of blocks does not rebuild a monolithic array.

Unknown heights begin as explicit estimates. When background layout refines them, preserve the top visible source anchor plus its local offset. Do not simply preserve scrollbar percentage, which would cause jumps as total height changes.

The extracted FrankenTUI Fenwick algorithm is a starting point, not a drop-in: its `u32` wrapping sums and assertion-based index preconditions must be replaced or safely enclosed by the new domain contract. [R8]

### 10.6 Text offsets and selection

Maintain distinct byte offsets, Unicode scalar boundaries, grapheme boundaries, logical line/column positions, and visual glyph positions. No API named merely `column` may ambiguously combine them. GUI text selection uses grapheme boundaries; exact source operations use validated byte ranges.

Bidirectional runs preserve logical source order for copying and a cluster/source association sufficient for hit testing and explicit caret affinity. This is not generally a one-to-one reversible mapping: a visual boundary may need upstream/downstream or leading/trailing affinity, and several characters may form one glyph cluster. Directional controls and confusable-risk characters can be revealed in a security-oriented source display mode. Syntax colors must not hide control characters or change what is copied.

### 10.7 Capture extents, not fictional whole-file immutability

Distinguish `CompleteCapture` from `ExtentCapture`. The former has immutable owned backing for the entire captured byte sequence and can carry a whole-content digest. The latter identifies exactly the captured ranges, their observations and hashes, plus unknown holes. It does not assign later reads from a changing live file to the same supposedly immutable complete revision.

A complete captured stream can still be an observation made while another process wrote the file; a digest identifies that exact observed sequence, not an atomic filesystem instant. Stronger snapshot guarantees belong to a provider that actually supplies them. Cross-file coherent search operates on a closed manifest of captured revisions, not a claim that a live-tree walk is a Git commit.

When an old exact capture has been evicted, an anchor resolves only if its immutable backing remains or new bytes verify as the same capture. Otherwise return a stale/unavailable result with a deliberate “open current source” action. Never silently resolve an old search hit against different live bytes.

### 10.8 Encoding and line semantics

Preserve original bytes. UTF-8 is the ordinary route; explicit supported BOM-marked UTF-16 routes use a checked original-byte ↔ decoded-text mapping. Other encodings require an explicit qualified decoder or show a labeled escaped/byte view. No statistical guess becomes a promise of exact source text. Newline handling defines empty files, final newlines, CRLF, CR-only, and invalid byte sequences in a fixture-backed contract.

`ByteOffset`, `DecodedUtf8Offset`, `Utf16CodeUnitOffset`, `ScalarIndex`, `GraphemeBoundary`, and `VisualPosition` are distinct domains. Native text/IME/accessibility adapters validate their SDK range conventions and sentinel values before converting. Do not cast an unsigned “not found” sentinel or UTF-16 range directly into a source-byte slice. Original byte copy, Unicode text clipboard copy, and escaped display copy are different explicit operations.

### 10.9 Horizontal virtualization has a shaping-context boundary

Visible-only glyph materialization is valid; arbitrary substring shaping is not always equivalent to shaping the full context. Bidirectional ordering may depend on the paragraph, ligatures and joining may cross a viewport edge, tabs depend on preceding advances, and a grapheme may contain many combining characters. Unicode's bidi and grapheme algorithms operate on their defined context, not arbitrary byte tiles. [B11]

Provide a qualified fixed-pitch ASCII/code fast path with checkpoints for tabs and display controls. For general text, obtain necessary paragraph/directional/shaping context through a bounded resumable preparation pass, retain reusable context checkpoints where sound, and materialize only the visible runs after that context exists. A run cannot be labeled exact because it merely looks plausible.

For a pathological line whose required context exceeds the active work budget, keep the UI responsive and state `context pending`, or offer an explicitly labeled logical/escaped view. Exact byte access and search remain available. Do not promise constant-time random visual access to an arbitrary 500 MB bidi paragraph. CoreText calls themselves are foreign operations; never hand them an unbounded line on the main thread.

### 10.10 Height index units and structural edits

Document heights use a specified nonnegative fixed-point logical unit with checked `u64` accumulation; integer pixels alone cannot faithfully express fractional point layout. GPU conversion uses a local visible origin and validated finite `f32` values. Tests cover overflow, negative adjustments, insertion/removal of blocks, and widths changing after fallback fonts arrive.

Use bounded leaves and a prefix directory so changing a measured block does not clone or shift a million-entry array. Reserve old/new structural pages before applying a transaction. Scroll anchoring uses the stable source/semantic block and intra-block offset, not a global percentage or an obsolete visual row number.

### 10.11 Large selections, copying, and export

A source selection is an anchor/range description, not an eagerly concatenated string. Selecting an entire giant file must remain cheap. Materializing text for clipboard, export or accessibility happens through a separately budgeted request with explicit destination and format.

The ordinary clipboard route has a documented byte budget and a bounded native handoff; exceeding it offers a streamed file export or explicit larger admitted operation, not a silently truncated copy. The UI remains responsive while bytes are gathered. A canceled export reports whether its private candidate was discarded or an explicit destination publication already completed. No background operation writes into the source tree unless the user explicitly chose that destination and overwrite policy.

Exact copied bytes, logical Unicode text, and rendered Markdown text have different semantics and names. A noncontiguous rendered selection can offer selected logical text or an explicitly enclosing original-source range; it cannot claim invented concatenation is the literal original Markdown. Selection persistence records capture identity and resolves staleness rather than copying current live bytes under an old selection label.

---

## 11. Syntax highlighting and language understanding

### 11.1 One highlighter, multiple consumers

FrankenMarkdown remains the owner of the reusable lexical classification engine. Source views, Markdown fences, search excerpts, and exported snippets consume the same token classes and themes. Do not maintain separate regex collections for every UI surface.

The existing `highlight_into` API is useful for bounded whole blocks. The proposed incremental API must preserve its strongest property: classification does not add, remove, or reorder source bytes. [R2]

### 11.2 Proposed incremental contract

The following is a contract sketch, not a claim about an existing callable API:

```text
LexInput:
    source_revision, language_id, exact_byte_range,
    starting_state, work_budget

LexOutput:
    classified_spans,
    ending_state,
    scanned_through_byte,
    checkpoint_candidates,
    completeness,
    source_revision
```

A checkpoint includes enough language state to restart correctly: comment nesting, active string delimiter, raw-string terminator, interpolation stack, preprocessor/string context where relevant, and any mode needed to distinguish significant ambiguous tokens. It also carries lexer ABI/version, language/options, validated source correspondence, and an explicit end-of-input flag. A chunk boundary is not EOF: the lexer may retain a bounded unresolved suffix instead of prematurely classifying a split delimiter or code point. Checkpoint state itself has depth/size limits and is not serialized as trusted native memory.

A viewport cannot begin lexing arbitrary lines with an empty state and claim correctness. It starts from a valid preceding checkpoint, or displays a provisional/plain layer while a bounded scan establishes the state. Known context is never replaced by an invented default.

### 11.3 Incremental invalidation

After a source change, find the preceding valid checkpoint. Re-lex forward until both source correspondence and lexical state converge with a retained checkpoint in the unchanged suffix. A changed multiline delimiter may invalidate a large suffix; the algorithm must permit that, remain cancelable, and avoid pretending every edit is O(1).

For this read-only browser, updates are usually external file replacements rather than keystrokes. Use a bounded change detector to discover unchanged prefixes/suffixes; if that detector costs more than a sequential scan for a small file, use the simple path. Massive-file edits get a progressive reanalysis state.

### 11.4 Language coverage

Release coverage should include Rust, Python, JavaScript, TypeScript/TSX/JSX, C, C++, C#, Go, Java, Swift, shell, JSON, TOML, YAML, SQL, HTML, CSS, and Markdown, with exact lexical capabilities declared per language. Existing support is inventoried at G0 rather than assumed from an aspirational list.

Each language receives fixtures for comments, strings, escapes, numeric literals, Unicode, malformed/truncated input, and its important multiline constructs. Rust raw strings/lifetimes/nested comments and JavaScript regex/template/interpolation contexts deserve dedicated adversarial suites. Shell heredocs, YAML block scalars, Python triple strings, and C/C++ preprocessor continuation cannot be approximated as independent single-line tokens.

Unknown languages remain readable as plain source. The release capability report states which lexers are implemented and qualified, which are provisional, and which are absent. The application must not refuse to open a file because it cannot color it.

### 11.5 Semantics are a separate capability

A highlighter is not a compiler. Use a capability ladder:

| Level | Permitted claim | Examples |
|---|---|---|
| Bytes | Exact source representation. | Range copy, exact literal match, line navigation. |
| Lexical | Qualified token classification. | Comment/string boundaries, keyword coloring. |
| Structural | Parser-supported syntax entities. | Rust item outline, Markdown heading tree. |
| Resolved local | A proven relationship within the parser's modeled scope. | A uniquely resolved import/module link under stated rules. |
| External semantic | Facts supplied by an explicitly enabled, independently qualified provider. | Compiler/LSP references, with provider/version/project configuration. |
| Heuristic | A candidate only. | Same-name identifier link, approximate related-file suggestion. |

External compiler/LSP processes are not necessary for the release's core features. They remain an optional, separately permissioned integration; no silent executable launch when opening a repository. “Find all references” cannot be advertised as complete unless the active provider and analysis scope justify it.

### 11.6 Parse budgets and reuse

All language scans have input, nesting, output-span, and work limits. A malformed file may degrade to plain text but must not hang, blow the stack, or allocate unlimited token objects. Coalesce adjacent equal-class spans. Store compact offsets relative to chunks when their range fits, with checked conversion back to `u64` source offsets.

Use native CPU scanning and safe portable SIMD only after scalar reference equivalence tests. GPU lexing is not a prerequisite: its statefulness, dispatch cost, and CPU-visible result requirements may make it slower for interactive ranges.

### 11.7 Upstream ownership and lexical correctness ladder

All new lexers, state machines, checkpoint formats, scanner optimizations, syntax classes, and reusable theme behavior land in FrankenMarkdown. FCB owns request priority and source-snapshot association, not the lexical implementation. Existing whole-block consumers must retain source-preserving behavior through an additive compatibility layer and upstream regression tests.

Lexical correctness is not the same as perfect semantic coloring. Some syntax ambiguities require parser context; a small previous-token flag does not fully implement JavaScript/TypeScript/JSX grammar. Define per-language qualified behavior and conservative/plain classifications for unresolved contexts. A lexer may be source-exact while a color choice is provisional; those are separate states. Highlighting never becomes permission to claim compiler-level definitions or references.

Chunk/full equivalence compares token meaning after coalescing adjacent same-class spans, not incidental chunk-local span boundaries. Tests split delimiters, UTF-8 sequences, escaped newlines, raw-string terminators, interpolation braces, and EOF at every relevant position. Incremental convergence requires both equal lexical state and a verified correspondence into an unchanged suffix; equal state alone is insufficient.

Persisted checkpoints are versioned bounded data, validated on load, and never used across a different source capture merely because byte offsets line up. Large offsets use checked local-to-global conversions. No source frontend implements a different lexer to compensate for an upstream missing feature.

---

## 12. Native Markdown rendering

### 12.1 Correct pipeline

```text
immutable Markdown source
    → FrankenMarkdown spanned parse
    → resolved document semantics / assets
    → FrankenMarkdown native flow-layout tree
    → FrankenMarkdown visible block layout + nested source provenance
    → FrankenMarkdown renderer-neutral display primitives
    → FCB view/asset integration and retained display-list batches
    → the same text/vector/image Metal renderer
```

Do not render HTML into a WebView, render PDFs to screenshots, or extract text back out of generated HTML. Those paths discard the advantages of native retained layout and make precise source navigation harder.

The existing FrankenMarkdown parser, font/math components, and document structures are reused. A continuous-scroll flow engine and complete nested provenance are **new upstream FrankenMarkdown work**. A PDF page-layout implementation is not automatically an appropriate screen layout engine, and the inspected top-level span wrappers do not supply every inline mapping needed here. [R2] [B1]

The reusable pipeline belongs to FrankenMarkdown all the way through resolved semantics, text shaping contracts, intrinsic measurement, flow layout, source mapping, and backend-neutral drawing/interaction output. `fcb-document` only supplies source identity translation, view width/scroll state, explicitly authorized asset responses, work scheduling, and integration with the browser renderer. This is an ownership rule now, not a future extraction contingent on another customer.

### 12.2 Required document surface

The release supports headings, paragraphs, emphasis, strong text, inline code, fenced code, lists, nested lists, blockquotes, thematic breaks, links, images, GFM-style tables, task lists, strikethrough, and the selected qualified extensions exposed by FrankenMarkdown. Mathematics uses `fmd-math`; supported diagram forms use the first-party diagram/vector path.

Footnotes, callouts, reference links, HTML treatment, and extension compatibility are explicit capability rows with fixtures. The parser's actual conformance determines the advertised dialect; do not claim full CommonMark/GFM conformance merely because the common cases look correct.

### 12.3 Flow layout

Use a block tree with stable semantic anchors and measurable intrinsic widths. Paragraph shaping and line breaking are cached by source/style/font/available width. Simple interactive line breaking is acceptable during active resize if a later refinement is visibly stable and source-equivalent. Expensive paragraph optimization must never sit on the frame thread.

Tables compute column constraints in a bounded measure phase. Wide tables scroll horizontally rather than crushing text into unusable columns. Large tables virtualize rows while preserving headers and logical accessibility. Code blocks have independent horizontal scrolling and optional wrapping, not surprise global document width changes.

Lists preserve marker alignment, continuation indentation, nesting, and task state. Task-checkbox rendering is read-only unless an explicitly separate editing feature exists. Clicking a document must not mutate its source accidentally.

### 12.4 Source-to-layout provenance

Every interactive rendered element identifies the source spans from which it was derived, using an upstream nested provenance graph rather than a top-level block envelope. A paragraph may map to several spans; a ligature may map to several source characters; a generated list marker may not correspond to literal source bytes. The model preserves these distinctions instead of forcing every glyph into a false one-to-one mapping.

The graph records escapes, entities, stripped emphasis/link delimiters, code-fence dedentation, soft/hard line breaks, generated numbering, and reference-derived content. Transcluded content identifies its separate source capture. Source mapping can return a disjoint ordered set of ranges; the caller must not invent contiguous source by concatenating unrelated spans and presenting it as the original literal slice. “Copy enclosing Markdown block” is a separate useful command.

Preview selection offers two explicit actions: copy rendered reading text and copy corresponding Markdown source. Source/preview synchronization chooses a nearby shared source anchor and local offset. Following a heading link resolves the heading identity, not a cached pixel position.

### 12.5 Incremental document updates

Block-level updates reuse unchanged subtrees when parser/source correspondence supports it. Reference definitions, footnotes, include dependencies, and heading-link resolution can affect distant blocks; maintain those dependencies or invalidate conservatively. A line-based change detector alone cannot establish Markdown semantic independence.

For a huge document, the new upstream bounded parser/layout APIs publish progress. Existing synchronous APIs do not become cooperative merely because FCB runs them inside an async task. Until the upstream resumable path exists, admit only explicitly bounded whole-document jobs on an appropriate worker; do not advertise giant-document responsiveness through a wrapper alone.

Already available exact blocks remain readable. A partial parse cannot assert that an unseen reference is missing or that a table is complete. The UI distinguishes provisional boundaries from established ones. Changes to references, list grouping, or heading IDs carry dependency invalidation beyond a local line range. Refined pagination-free flow must preserve the top visible semantic/source anchor.

### 12.6 Images, mathematics, and diagrams

Local images resolve through root capabilities. Decode them off-thread under byte, dimension, frame-count, and total-decoded-memory limits. Decode to an appropriate display resolution; do not retain a full-resolution 100-megapixel image merely to draw a thumbnail. Existing first-party image/codec components are candidates, but each format needs an actual decoder and hostile-input tests before capability is advertised.

Math layout emits reusable glyph/path runs plus text/source provenance. It does not shell out to TeX. Diagram output must flow through qualified first-party vector parsing/geometry, not execute embedded script or arbitrary markup. Unsupported elements display a source-preserving fallback and a concise capability explanation.

### 12.7 Safe document policy

Network fetching is off by default. Raw HTML is escaped or handled by a deliberately small, inert allowlist; no JavaScript, event attributes, `javascript:` URLs, arbitrary CSS, iframes, shell commands, or automatic filesystem includes. Transclusion, where supported, requires root capabilities, recursion limits, cycle detection, and visible provenance.

Opening an external web link is an explicit user action handed to the OS. A link cannot request arbitrary application-local file access merely by looking like a relative URL. Font, image, and diagram caches are private derived state and have the same workspace deletion policy as other source-derived assets.

### 12.8 Renderer-neutral upstream contract

FrankenMarkdown's proposed flow API consumes an immutable spanned document/capture, `FlowConstraints`, theme/font environment identities, a finite work budget, and a caller-supplied resource resolver. It produces either bounded progress or a completed layout revision, along with display primitives, source/semantic maps, link/hit regions, accessible reading structure, warnings, and unresolved asset requests.

The core is synchronous-resumable and host-neutral. A `step` operation accounts for actual bounded work units and can yield without losing parser/layout state. It does not spawn tasks or perform filesystem/network calls. FCB runs steps inside its Asupersync scope; other consumers may use the same engine synchronously or under another explicitly chosen host without forcing an FCB dependency.

An unresolved image emits a typed request and stable placeholder with known or bounded estimated dimensions. Supplying an asset result identifies the document and request generation; a stale image response cannot reflow a new document. Reflow invalidates only affected layout dependencies when that can be established, otherwise it invalidates conservatively. The resolver's capability is separate from the Markdown text's requested URL.

Display output expresses text runs, vector paths, image references, clipping, logical geometry, semantic reading order, and selection provenance. It contains no Metal texture pointers, AppKit types, FCB window IDs, arbitrary script, or live closures that can perform ambient I/O. A host maps upstream asset/source IDs to its own resource and authorization domains.

### 12.9 Compatibility and ownership tests

Land upstream APIs and implementation with FrankenMarkdown's own parser, HTML, PDF, font/math, and core/WASM compile/regression tests as applicable. FCB's native screen layout does not replace or reinterpret print pagination. Shared parsing and typography improvements must not accidentally change documented HTML/PDF behavior without an explicit compatibility decision and fixtures.

Add a tiny **FrankenMarkdown-owned headless flow consumer** before the FCB integration passes. It measures/layouts the same document, validates nested provenance and budgets, and serializes a deterministic semantic layout fixture with no FCB dependency. FCB then consumes a committed upstream revision through the public API and renders real documents. This proves upstream ownership in code, not only in a work-ticket label.

Reusable Markdown export, diff semantics, accessible reading text, diagram/math layout, font handling, and code highlighting discovered during development follow the same rule. Browser-specific source selection coordination, lens placement, camera navigation, root grants, and frame scheduling remain FCB responsibilities. Raw image codec improvements can belong to an existing shared codec owner, but Markdown asset semantics and integration contracts remain in FrankenMarkdown.

### 12.10 Bounded layout is not just viewport clipping

A parser can build an unbounded AST before the renderer clips anything. A table can require whole-table intrinsic measurement, and a single deeply nested inline structure can monopolize a supposedly small block. Bound parse state, node counts, output expansion, nesting, measurement iterations, and per-step work before allocating or recursing.

Viewport rows may initially use qualified width estimates, but changes are explicit anchored reflow. Do not claim a table's final optimal column widths from inspecting only its first visible rows. Large code blocks reuse bounded lexical/text state; no whole-fence clone is required for each viewport. A giant unsupported structure retains a useful source view and a clear limitation instead of disappearing.

---

## 13. Fonts, shaping, selection, and text rendering

### 13.1 Shared typography engine

Use `fmd-font` for inspected bundled TrueType faces, metrics, outlines, kerning, and its qualified shaping subset. Use one text-run representation across source, Markdown, labels, and search excerpts. Cache font data once per font identity, not per file or pane. [R2]

Curated fonts are application assets with recorded licenses and content identities. This plan file does not redistribute font binaries. User-supplied fonts are untrusted parsed inputs and must receive the same bounds and complexity checks as source assets.

### 13.2 Complete native text behavior without false shaping claims

The inspected `fmd-font` is Latin-first and does not establish full complex-script support. For the native Mac product, specify a first-party CoreText adapter in the platform boundary for fallback shaping/font coverage where required. The adapter must return owned glyph/cluster/position information and preserve mapping back to the original source range. Rendering still uses the application's GPU path.

CoreText is a named Apple system service, not a newly admitted third-party Rust crate. The capability record distinguishes deterministic bundled-face runs from system-shaped fallback runs. Full cross-machine pixel determinism is not promised for the latter. Broader clean-room shaping, font parsing, run maps, and common raster contracts are implemented in `fmd-font` inside FrankenMarkdown from the outset.

The proposed optional `fmd-font-macos` adapter is also owned by FrankenMarkdown and uses the safe `franken-macos` system service. It translates owned native shaping/raster results into the shared upstream run contract. Low-level ABI code remains solely in the system bridge; `fmd-font` and the adapter's authoritative conversion logic forbid unsafe code. The base engine does not gain Apple or runtime dependencies merely because the native adapter exists.

Baseline release qualification includes Latin, combining marks, CJK, right-to-left text, mixed-direction runs, emoji sequences, fallback fonts, and input-method composition in search fields. A readable source browser cannot silently turn every unsupported character into an indistinguishable empty box.

### 13.3 Two rendering regimes

For reading-size text, favor crisp grayscale coverage masks at the actual display scale. For distant/transformed labels, use a separately qualified scalable representation or bounded raster-scale ladder. A single low-resolution glyph atlas stretched through the entire zoom range is not acceptable.

Do not assume multi-channel distance fields automatically look better for small code. Compare actual code and punctuation at 1×/2× backing scales, fractional positions, dark/light backgrounds, and transformed views. Use analytic/vector techniques from the first-party rendering stack only where they win measured quality/cost.

### 13.4 Raster identity is different from GPU residence

Separate the immutable rasterization key from the mutable GPU residency handle:

```text
GlyphRasterKey:
    exact font/face identity, variation coordinates, glyph ID
    raster mode, size/scale tier, subpixel phase, hinting/coverage policy
    shaping/raster implementation version and relevant feature identity

GpuGlyphRef:
    renderer owner, actual device identity and device generation
    atlas page/slot identity, slot generation, validated rectangle
    reference to the GlyphRasterKey it currently contains
```

Changing an unrelated atlas page does not invalidate every CPU glyph raster. The previous proposal's undifferentiated `atlas generation` in the raster key risked global cache churn. Shape-run keys additionally include exact text, script/direction/language, font cascade, features, and context. Raster keys include only inputs that affect the glyph raster; many shaping features are already reflected in the selected glyph/position and must not cause gratuitous duplication.

Text color generally belongs in a shader/style palette rather than the monochrome glyph cache key. Color glyphs have a separate representation. The FrankenTerm borrowed-key pattern is useful for avoiding allocation during lookup, but its terminal-cell-specific fields are not copied blindly. [R3]

### 13.5 Atlas management

Use small paged atlases with measured packing behavior, bounded fragmentation, and explicit eviction. Separate common UI/code glyph pages from transient large labels and color images. Pin resources referenced by in-flight frames until GPU completion. An atlas slot generation changes on reuse so retained draw data cannot sample a different glyph accidentally.

Rasterization misses are serviced by a bounded background queue. A temporary missing-glyph state may use a known fallback without changing source identity; the UI should not block waiting for a large new font corpus. Selected reading text has higher raster priority than far-away labels.

### 13.6 Hit testing and carets

Retain cluster advance arrays and a line-level hit-test index. Text hit testing uses those arrays on the CPU. A glyph quad is a rendering primitive, not a source-character boundary. Selection rectangles may cover discontiguous visual runs for one logical range.

Search fields implement composition ranges and marked text without prematurely issuing a new finalized query for every intermediate input-method event. Source views are read-only, but selection, accessibility text ranges, and keyboard movement must still be correct.

### 13.7 Fonts and color glyphs are complete resources

A native glyph ID has meaning only with the actual fallback font/run that produced it. The native adapter retains or converts that font identity and returns owned metrics, cluster associations, and either validated outlines or a raster source suitable for the renderer. It must not assume that a glyph from a fallback face exists in the primary bundled font.

Color emoji and bitmap-only fonts need a qualified color-raster route; shaping into glyph IDs alone is insufficient. CoreText/CoreGraphics may provide this through the named system boundary, with byte/dimension/format limits and correct color metadata. No new outside font/raster library is implicitly admitted. Unsupported color-glyph formats have an explicit meaningful fallback rather than blank text.

Font installation/substitution and appearance/display changes revise the font environment. They can change line metrics, so source-anchored reflow is necessary. Share immutable font backing rather than cloning complete `Font` byte vectors into each run. User-supplied fonts remain untrusted and cannot expand cached glyphs or font tables without an admission lease.

### 13.8 Text-focused quality gates

The small-text gate includes punctuation, thin strokes, underscores, ligatures on/off, visible whitespace, combining marks, selection through mixed bidi, code on light and dark backgrounds, fractional scrolling, and backing-scale migration. Pin reference output by shaping route. A GPU screenshot similarity score alone cannot prove exact copy or caret semantics.

Source code uses conservative ligature defaults and visible controls where helpful; a pretty ligature must not obscure distinct source characters during selection. FCB and Markdown fences share lexical classes, while their font/line-breaking presentation can differ through explicit upstream options.

---

## 14. Native Metal renderer

### 14.1 Why native Metal

The chosen Mac-only execution path is a first-party native Metal renderer. It avoids an embedded browser, a general-purpose widget renderer, and a broad portable graphics dependency. This is an architectural choice for the requested hardware and dependency policy, not a claim that merely choosing Metal guarantees performance. Metal is Apple's GPU interface for graphics and parallel computation. [A1]

Use the smallest qualified feature set that supports the product. More recent Metal features may be enabled by runtime capability and measurements, but basic rectangles, text, paths, and simple 3D do not need to depend on the newest optional GPU feature. M4 and M5 are qualification targets, not substitutes for runtime capability checks.

### 14.2 GPU-neutral retained representation

Store instances and resources in packed arrays, with immutable or versioned resource tables. Initial primitive families are solid rectangles, rounded/bordered rectangles, line segments, glyph quads, image quads, clipped vector geometry, and optional extruded parcels.

The semantic display list identifies style, clip, transform, z/layer ordering, resource handles, and source/interaction IDs. The render preparation layer partitions compatible draw batches without changing blending or overlap semantics. Transparent primitives cannot be arbitrarily reordered just to improve a draw-call count.

Render preparation is incremental. Ordinary camera motion updates camera uniforms and visible/admitted instance lists; it does not reconstruct all source geometry. Repeated static panels can retain complete sublists. The design borrows independent invalidation axes from FrankenManim and typed device/resource generations from FrankenThreeD. [R9] [R10]

### 14.3 Initial pass schedule

A practical baseline schedule is:

1. Upload or reference already prepared bounded dirty resource ranges.
2. Draw atlas background, retained directory/file primitives, and optional City geometry.
3. Draw source-density/structural bands and qualified diagram/image content.
4. Draw text and labels with the appropriate clip and scale policy.
5. Draw selection, search, navigation, and focus overlays.
6. Composite reading panels and fixed UI chrome; present a complete frame.

Combine passes where ordering and attachment behavior permit. The list is semantic, not a demand for six render encoders. Avoid rendering the same opaque viewport through unnecessary full-screen intermediate textures.

### 14.4 Culling and batching

Start with CPU hierarchical culling and instanced draws. The visible hierarchy is already valuable for hit testing and semantic LOD; it should not be discarded merely to advertise GPU culling.

Add GPU culling/compaction or indirect drawing only after a measured crossover shows benefit at relevant visible-object counts. GPU-generated count/readback must not introduce a CPU synchronization barrier. A small number of known primitive families permits compact, stable pipelines and precomputed binding layouts.

Use screen-space clipping and LOD to control fill rate. Drawing a million subpixel rectangles whose coverage overlaps is not a meaningful proof of useful scalability.

### 14.5 Buffer ownership and synchronization

The safe bridge exposes owned resource handles and explicit upload/submission leases. CPU writable staging memory cannot remain mutably borrowed while the GPU might consume it. Submitted resources remain alive and immutable for the lifetime required by the command buffer. Completion releases leases through a bounded callback path.

Never expose a safe `&mut [u8]` to memory that may concurrently be read or written by the GPU. Never infer GPU completion from the Rust lifetime of a temporary command encoder. Use explicit completion/fence state and device generations.

For uploads, shared storage is a candidate for CPU-produced instance data; GPU-private resources are candidates for long-lived textures and other GPU-only data. On Apple GPUs, storage choice and synchronization still matter despite unified memory. Benchmark storage choices rather than treating “unified” as “copies and hazards do not exist.” [A2]

### 14.6 Serialization and shader ABI

No `bytemuck`, unchecked struct casts, or unaligned reference creation. Serialize GPU records with explicit field encoders into preallocated byte buffers, or use a tightly reviewed typed allocation/upload boundary whose layout is proved. The authoritative representation remains safe Rust.

Record field offsets, padding, alignment, scalar widths, coordinate units, and endianness for every shader-visible record. Host-side layout tests and an actual GPU round-trip fixture verify the agreement. The shader receives explicit counts and bounds; it does not trust an index merely because it came from a Rust vector at an earlier time.

### 14.7 Dirty regions and drawable semantics

Retain scene data and optionally offscreen tiles, not an assumption that a newly acquired drawable contains the prior frame. Every presented drawable must receive a complete valid image. Partial redraw optimization is allowed only through retained intermediate surfaces or a documented preservation guarantee that is actually qualified.

A resize, backing-scale change, display migration, color-configuration change, or device-generation change invalidates the appropriate target and raster resources. Avoid one giant render-target allocation per full repository extent; only viewport-sized and bounded tile resources exist.

### 14.8 Shader compilation and pipeline warmup

Compile shipped MSL sources offline using the pinned Apple toolchain. Load and create the small initial pipeline set before claiming the interactive scene is ready. Cache pipeline state by device, shader content, attachment format, and specialization parameters.

No synchronous shader compilation after a user pinch begins. Optional feature pipelines can warm asynchronously, but their unavailable state must have a legitimate existing rendering route or a visible feature-loading state. Cold-start qualification includes pipeline creation cost rather than hiding it from the startup number.

### 14.9 Device failure and teardown

Device/command errors invalidate that GPU generation. Retain source and UI state, stop admitting dependent frames, and attempt a bounded renderer reconstruction. All old resource handles fail validation. A software/reference recovery surface may keep the user informed, but it must not be described as the qualified GPU experience.

Window close, display removal, application sleep, and device failure are separate events. Each has a defined ownership path. Driver work that cannot be interrupted remains a foreign-operation boundary; the system does not free buffers early to manufacture a cancellation guarantee.

### 14.10 Explicit color, alpha, clipping, and depth contracts

Adopt a documented SDR baseline: theme/image input color is converted according to its declared encoding; shader lighting/compositing uses linear values; alpha is premultiplied exactly once; grayscale glyph coverage is linear coverage, not sRGB color. With premultiplied output use the matching source/destination blend factors and an appropriately configured sRGB presentation target. Validate the complete host-target configuration rather than silently assuming it.

A sampled premultiplied color glyph/image must not be premultiplied again. A glyph mask must not pass through color-gamma decoding. CPU reference and GPU fixtures cover translucent overlaps, antialiased edges, overlapping selections, color emoji, dark/light themes, and image color profiles. System wide-gamut/HDR behavior is an explicitly qualified optional route; no extended-range brightness claim is inferred from an SDR screenshot.

Coordinate conventions are fixed: logical top-left UI coordinates, physical drawable pixels, local map/world coordinates, and Metal clip/depth coordinates have explicit transforms. Clip-stack intersections are validated; scissor rectangles use clamped physical integer bounds. Large clip hierarchies are bounded. Transparent City overlays retain semantic order and depth policy; changing projection must not reverse front/back interpretation or make hidden surfaces clickable.

Reject nonfinite camera/geometry values before GPU upload. Never normalize NaN merely to make a malformed camera state hash consistently. Shader ABI tests exercise alignment, strides, bounds, color conversion, clip/depth, and device limits on an actual GPU.

### 14.11 Completion is a conservation channel, not a lossy notification

Reserve one terminal completion record for every admitted GPU submission before it can be committed. The callback records success/error and signals the owner through a preallocated, bounded mechanism. Wakeups may coalesce; the underlying completion state may not be dropped. A saturated latest-value event queue is not a safe place to put the only notification that releases in-flight buffers.

Completion records survive window close until all submitted resources are retired. The owner processes them without allocating inside the foreign callback or calling arbitrary user code. A canceled display request can be discarded before submission; a submitted command retains its leases until its terminal device contract permits release.

Distinguish invalid-command/programming errors, resource-admission failures, drawable unavailability, and actual device loss. Do not automatically reset the entire renderer for every recoverable command error or loop forever recreating a broken pipeline. Bound recovery attempts and preserve an accurate diagnostic/state route. Safe reclamation is based on qualified terminal ownership, not the desired cancellation deadline.

### 14.12 GPU acceleration beyond drawing: measured candidates only

The mandatory accelerator is the native retained rendering path. Optional compute candidates are summary-tile generation, large visible-set compaction, and batched numeric overlay transformations. A candidate must beat the CPU path including data preparation, synchronization, upload, and consumption at the realistic crossover; keep a scalar/reference result for correctness.

Do not send source lexing to the GPU merely to remove its name from a CPU profile. State-dependent parsing, bidi resolution, and exact source indexing often need CPU-visible results. Retained summaries and precise invalidation eliminate more work than accelerating unnecessary work. Keep frame preparation independent of full repository size.

---

## 15. Frame pacing and interaction latency

### 15.1 Scheduling model

AppKit owns the native event loop. A small main-thread reducer ingests native input, updates immediate state, and publishes the latest camera/selection snapshot. A render owner assembles and submits frames without blocking the main thread on disk, DB, parsing, or GPU completion.

Use a qualified display-link integration. Apple's `CAMetalDisplayLink` provides display-synchronized updates and frame-rate/latency controls; requests are not a guarantee that every connected display will run at 120 Hz. Its chosen run-loop/delegate route determines callback behavior. Do not assert an arbitrary callback thread or add a competing drawable-acquisition path. The application or embedding host designates one acquisition/presentation owner and validates its update/drawable contract on the pinned SDK. [A3] [B12]

The exact callback threading, availability, and deployment-version contract is established against the pinned SDK in G0. Do not invent an availability shim from an untested selector. This route bounds the deployment floor: `CAMetalDisplayLink` requires macOS 14, and `CVDisplayLink` is deprecated from macOS 15, so a lower floor needs a separately qualified pacing route (§26.8).

### 15.2 Queue latency

Start with at most two application frames in flight, subject to the drawable contract. Compare two versus three retained buffer slots under real load; do not assume triple buffering is always optimal. More buffering can improve throughput while worsening interaction latency.

A queued frame representing an obsolete camera state can be dropped before submission. Input deltas may be coalesced into the latest camera state, but discrete commands and text-edit/composition events are not arbitrarily discarded. Generation and ownership checks apply to every frame.

Drawable acquisition and completion waits must not block AppKit's event handling. The native bridge must qualify where acquisition can wait and how that waiting interacts with shutdown and display changes.

### 15.3 Work budget inside a frame

Each frame has bounded time for model-delta consumption, visible-set traversal, layout-ready artifact admission, batch preparation, uploads, and GPU execution. The frame does not drain an arbitrarily large background-results queue before drawing.

When the budget is exhausted, defer low-priority detail: distant labels, optional graph edges, high-resolution thumbnails, and City decoration. Preserve selected source, caret/selection accuracy, command responsiveness, and obvious progress/completeness indicators.

### 15.4 Motion versus stationary state

Active camera motion keeps the geometry and source already resident useful immediately. Prefetch follows a bounded predicted neighborhood based on direction/velocity and current memory pressure. It does not load every file on the ray toward a distant target.

When stationary, the display link can pause or use a minimal necessary wake policy. No continuous 120 Hz redraw for a static source window. Cursor blink, indexing progress, and small animations trigger only the required invalidation; hidden/occluded windows do not render at full rate.

### 15.5 End-to-end latency accounting

Measure input event timestamp, reducer acceptance, chosen frame, CPU submission, GPU start/end where available, and actual presentation. Software timing is useful but is not identical to physical input-to-photon latency. Hardware camera/display measurement is a separate qualification lane.

Report event-to-present and physical input-to-photon metrics under their correct names. Include late/dropped frames and cold glyph/pipeline events. A benchmark that times only command encoding must not claim to measure the user's complete interaction experience.

### 15.6 Gesture ownership and clean wakeups

The hit region selected at gesture start owns the gesture until completion/cancellation. Scrolling inside a Markdown table or code block must not also pan the atlas. Text selection, panel dragging, camera pan, pinch, orbit, and native IME events have an explicit arbitration table. Native scroll/momentum phases are converted faithfully; do not add a second inertial simulation on top of platform momentum.

Coalesce motion into a latest-state mailbox, but preserve ordered discrete commands, focus changes, marked-text commits, and button transitions. Bound event processing per frame and provide backpressure or explicit resynchronization for producers. A wake flag is reset with an order that cannot lose a concurrent event; test the producer/consumer interleavings under the lab runtime and real native callbacks.

During active animation or motion, schedule the next needed frame. When static, pause rendering without losing the notification that should wake it. An indexing progress counter does not require a 120 Hz full redraw. Completion processing and lifecycle maintenance remain live even when a window is occluded.

---

## 16. Asupersync orchestration and cancellation

### 16.1 Scope hierarchy

```text
Application region
├── Native host integration lifetime
├── Shared font/resource service
├── Workspace region A
│   ├── Discovery/reconciliation service
│   ├── Source snapshot service
│   ├── Analysis/index service
│   ├── Persistence service
│   ├── Query generation regions
│   └── Window subscription regions
└── Workspace region B
```

A window owns subscriptions and window-local requests. Shared workspace work remains alive while another window still uses it. Closing the last owner requests cancellation and drains the workspace through a controlled asynchronous shutdown path.

### 16.2 Admission, priorities, and pools

Configure explicit bounded application queues and root admission limits. Do not rely on the inspected runtime's default unbounded queue. Configure a blocking pool deliberately; do not assume `spawn_blocking` is isolated when no pool exists. [R6]

Initial service classes:

| Class | Examples | Policy |
|---|---|---|
| Immediate | Input, camera, selection, panel focus. | Main-thread bounded reducer; no await. |
| Visible | Selected source range, visible Markdown blocks, missing glyphs. | Highest background service priority, strict size/time quanta. |
| Interactive | Search, outline, navigation target preparation. | Canceled/replaced by request generation; bounded result stream. |
| Maintenance | Discovery, indexing, graph enrichment, DB compaction. | Lower-priority fair budget; throttled under interaction or pressure. |

Use a measured small worker count and an explicitly limited blocking pool. “There are many cores” is not permission to occupy them all. Reserve headroom for the event/render path and other applications. No assumptions about fixed P-core/E-core counts or unofficial affinity APIs are baked into correctness.

### 16.3 Cooperative CPU work

Lexing, parsing, line scans, graph traversals, and index construction divide work into bounded quanta and checkpoint cancellation. A candidate starting target is a sub-millisecond to low-single-millisecond uninterrupted CPU quantum, measured on the qualification machine. The exact value is a parameter, not a theorem from `Cx` ownership.

Avoid cancellation checks per byte when a bounded chunk check is adequate. Conversely, a pathological inner loop with unbounded work inside one “chunk” is not cooperative. Every parser recursion/expansion path receives its own complexity bound.

### 16.4 Cancellation states and publication

Represent success, error, cancellation, and panic distinctly until the application boundary. An obsolete query should stop spending resources, but cancellation does not retroactively undo a durable operation that already committed. Preserve authoritative terminal effect outcomes from the storage adapter. [R6] [R7]

Two-phase admission and publication prevent half-built indexes from becoming visible. A worker can create a candidate artifact, check generation, reserve publication capacity, and publish atomically. If the generation is stale, discard or cache the exact artifact without displaying it.

### 16.5 Foreign operations

Filesystem calls, font-system calls, driver calls, and submitted GPU work are not universally interruptible. Their wrappers specify bounded admission and result-discard semantics, not an invented hard deadline on the operating system.

A source read that cannot be interrupted may finish in its blocking worker while its output is discarded. A submitted frame retains resources until completion even after its window is closing. Shutdown waits asynchronously; no finalizer performs an unbounded join on AppKit's event thread.

### 16.6 Deterministic concurrency tests

Use Asupersync's lab/replay facilities for task ordering, cancellation races, queue saturation, stale publication, and resource release. Inject delayed completions, simultaneous close and search, duplicate watcher hints, and failed persistence publications.

The lab proves the modeled orchestration contracts. Native event loops and GPU drivers still require real-host tests; deterministic scheduler tests do not certify an unmodeled ABI callback or display pipeline.

### 16.7 Three budgets with different meanings

Keep separate contracts:

| Budget | Owner | What it limits |
|---|---|---|
| Asupersync `Budget` | Runtime scope/context. | Deadline, scheduling polls, abstract cost, and priority. |
| Engine `WorkBudget` | Source/FMD/search/layout step. | Bounded bytes, nodes, transitions, expansion, and checkpoints inside one operation. |
| `ResourceBudget` with owned leases | FCB instance or explicitly shared host resource domain. | Managed allocation capacity, retained generations, queue payload bytes, GPU resources, and peak overlap. |

Asupersync's inspected budget is not an allocation quota and a poll count is not milliseconds. A future that performs an unbounded synchronous parse inside one poll defeats a naive poll-budget latency claim. Resource reservations precede allocation, and engine loops consume actual work units. The runtime deadline uses an absolute compatible clock or an explicit duration-to-deadline conversion; never pass a duration as an absolute timestamp. [B6]

Priority is also not a hard scheduling guarantee. The inspected `Budget::meet` takes the **maximum** urgency; nesting maintenance under an urgent request and assigning a lower priority does not necessarily lower the effective priority. Create appropriate service scopes and use application admission/fairness controls rather than assuming child budgets can weaken a parent. Frame deadlines remain protected by bounded work and ownership, not just a numeric priority. [B6]

### 16.8 Bounded admission without resource deadlock

Establish a global order for acquiring publication slots, byte leases, source/asset pins, and service permits. Never reserve all free memory for candidates that then wait for a publication queue whose existing entries require that same memory to drain. Completion/retirement progress has protected capacity independent of ordinary work admission.

Visible work can borrow a bounded maintenance allowance, but it cannot overrun host limits. Age maintenance requests to ensure discovery/indexing eventually progress. Separate maximum admitted tasks from maximum running workers and from payload bytes: a queue of ten requests can still own gigabytes.

Request replacement creates a new generation immediately; it does not await an obsolete request's drain on AppKit. The retiring-request count and its retained bytes are bounded. When foreign operations are slow, stop admitting more equivalent work, deduplicate subscribers, and report pending state. Cancelling many requests is itself a workload and receives bounded processing.

### 16.9 Embedded runtime contract

The application creates and closes its Asupersync runtime explicitly. An embedding host supplies a compatible runtime/context, time source and wake integration. A host without its own Asupersync integration may instead call an explicit facade constructor that builds a private owned runtime with the same bounded configuration the application uses; that call is never implicit, is the only route to a runtime FCB owns inside a host, and the host closes it through the same session drain path. The library creates only owned child regions and services named in its configuration. It does not infer that a process-wide default exists or install one on first method call.

The FrankenSQLite adapter's dedicated worker is owned by the persistence service, with any required runtime instance/context explicitly declared. A `Cx` from another crate/version or logical clock is not interchangeable because its type name looks similar. Consumer tests cover matching versions, close while writes finish, cancellation after publication, and a host remaining alive after an embedded FCB session closes.

---

## 17. Search and progressive results

### 17.1 Search is a core navigation primitive

Search must be useful before every optional analysis layer is ready. Separate path lookup, literal text search, qualified symbol lookup, heading search, and optional advanced query modes. The default path requires no network, downloaded model, compiler, external search binary, or embedding service.

A query owns immutable scope/options and a generation. Its results carry exact source ranges and completeness metadata. “Top results available” and “all matches counted” are distinct states.

### 17.2 Path search

Build a compact path-component index with normalized search keys and preserved raw identities. Rank exact filename, exact path segment, prefix, subsequence/fuzzy match, recent navigation, and selected scope by a deterministic scoring function. Tie-break by stable path/file identity.

Case-folding and normalization used for matching do not replace the actual source path. A case-insensitive query may match two distinct files on a case-sensitive volume. The UI must preserve that distinction.

Use cached candidate sets for incremental query refinement where sound. If a change in query mode or normalization breaks the subset relation, recompute rather than incorrectly narrowing from the old candidate list.

### 17.3 Literal source search

For sufficiently large indexed collections, a compact trigram or equivalent substring candidate index is appropriate. Store postings by source/content generation and verify every candidate against the captured source under the selected byte or decoded-text semantics (§17.11). The prefilter may return false positives; it must not create false negatives inside its declared complete coverage.

Queries shorter than the index's gram width, Unicode normalization modes, case-insensitive modes, and cross-chunk matches need explicit routes. A byte trigram index cannot simply answer a normalized Unicode query without a compatible indexed representation or a verification scan over the correct candidate universe.

Small scopes can use a fast bounded direct scan instead of paying index overhead. Partially indexed workspaces combine indexed candidates with a budgeted scan of uncovered files, while showing the covered scope and pending work. A user can stop after useful results without waiting for an exhaustive total.

### 17.4 Progressive publication

Publish an initial bounded result batch, then append/refine under the same generation. The result object includes:

```text
query_generation
requested_mode / realized_mode
scope_identity
source_generations
hits[] with file/source/range/score/explanation
scanned_files / eligible_files_known
coverage_state: discovering | partial | complete | canceled | failed
truncated_by_limit
pending_refinement
warnings[]
```

Do not let semantic or graph ranking hold the lexical results hostage. Borrow CASS's explicit initial/refined/failure separation and source-versus-derived asset discipline, but not its illustrative latency numbers. [R5]

### 17.5 Ranking stability

Selection is by hit identity, not row index. Once a user hovers, selects, or starts keyboard navigation, protect the active ordering region. New high-scoring results can appear in a “new results” area or settle after interaction pauses. Camera navigation follows the selected hit's exact source range regardless of later score changes.

A search overlay on the map is a separate compact aggregate: matching-file bits, directory counts, and bounded match bands. Do not upload every text hit as an independent GPU object when millions of matches exist.

### 17.6 Advanced search

A bounded first-party query parser can support explicit field filters, conjunctions, exclusions, language/path scope, and exact phrases. Each query mode has defined escaping and Unicode behavior. Configuration and query text have size/nesting limits.

Regular-expression search is optional until a clean first-party linear-time engine and its compatibility matrix are qualified. Do not introduce backtracking expressions into untrusted whole-repository scans or call a new outside regex crate to avoid this work. Unsupported constructs receive a specific error rather than a different silent interpretation.

### 17.7 Index lifecycle

Source bytes are canonical. Search postings, document frequencies, and rankings are rebuildable. Build new index segments independently, validate them, and publish a manifest referencing one coherent generation. Queries pin the manifest they started with; newer queries can see a later generation.

Tombstones and replacement segments must publish together. A failed replacement cannot publish only the tombstone and make a live file vanish. Keep bounded prior generations until all readers release them; garbage collection has explicit age/reference/space rules. Corruption yields a rebuildable-index error, not loss of bookmarks or source.

### 17.8 A complete result needs a closed source universe

Pin a `SearchManifest` containing authorized roots, complete eligible membership, source capture identities, query normalization/version, and index coverage. “Complete” means the exact search has finished over that declared universe. A changing live directory does not provide a timeless global no-match result; newly discovered/changed sources form a new query/refresh generation.

Report unavailable files, intentional exclusions, uncovered captures, interrupted scans, result truncation, and requested scope separately. Top-k results can be immediately useful without being the proven global top-k, and displaying fifty hits is not an exact match count. Track `matches_seen`, `stored_hits`, `truncated`, and coverage independently. Ranking refinement cannot change which source bytes an existing hit names.

A literal byte index, case-folded index, and normalized Unicode index are different semantic objects. Persist the exact normalization algorithm/version and a mapping back to original bytes. Reusing an incompatible candidate index can cause false negatives even when final verification is exact. Route unsupported normalization or short queries to a bounded complete scan of the correct universe, not to an empty candidate set.

### 17.9 Index amplification and artifact limits

A high-entropy or huge generated file can produce a much larger posting structure than its source size. Cap per-file and per-generation index construction bytes, posting counts, scratch space, merge overlap, and disk output. A quota failure leaves explicit uncovered state plus the direct-scan route; it must not silently remove that file from the meaning of workspace search.

Use immutable bounded segments and compact posting encodings with checked decoding. Query hot sets pin only required pages. Exact source verification reads captured immutable ranges under I/O budgets; it must not validate a hit against newly changed working-tree bytes and then retain the old revision label.

### 17.10 Reading trails and source-evidence packs

Use the CASS evidence-selection pattern to collect a user's selected ranges, nearby declarations, relevant Markdown sections, and qualified relationship evidence into a bounded **reading trail**. A trail is a sequence of exact source anchors plus short rationale, not a generated claim about code behavior. The user can follow it in FCB, pin it beside the atlas, or explicitly export it. [B2]

The pack records the source manifest, per-item capture/ranges, evidence type, readiness/staleness, deduplication, exact byte/character budget, and every omitted or truncated category. Token estimates identify their estimator and are never described as exact token counts. Rank by explicit user selection first, then bounded coverage/diversity and relevance; no downloaded model is required.

An exact character budget counts Unicode scalar values in the declared decoded/export representation, not glyphs, graphemes or UTF-16 code units. Count encoded export bytes separately, including headers and provenance, so fitting selected source alone does not permit an over-budget final pack. Malformed input uses the named escaped representation or reports unavailable decoding; it cannot invent an exact character count.

The general readiness/selection policy is factored in CASS where appropriate. Markdown pack formatting/export improvements belong to FrankenMarkdown. FCB owns source-specific candidate generation and navigation. A CASS host can in turn embed FCB to inspect the exact source behind a result through a granted provider, without launching an external editor or granting access to unrelated roots.

Export is an explicit operation with destination and privacy review. A pack can contain secrets present in source; redaction is best-effort unless a precise rule establishes otherwise. Hashes prove integrity relationships, not authorization or safe disclosure. No agent instructions found in source are executed as part of collecting a pack.

### 17.11 Text search and original-byte search are distinct

The human `--text` route searches the declared decoded text representation, with matching and normalization options recorded. A UTF-8 byte needle cannot search UTF-16 source correctly: even ordinary ASCII characters have a different byte encoding. Keep an explicitly named raw-byte query route separate if offered. Unsupported/invalid decoding remains an explicit coverage condition or a separately selected byte/escaped mode, never a successful no-match result for the requested text semantics.

Decoded indexes carry decoder/normalization versions and maps back to the original captured byte ranges. Verify text hits by decoding the same capture under those semantics, then validate the mapped original ranges; do not require the UTF-8 query bytes to occur literally inside UTF-16 backing. Define matches within expansions or normalization units and map them to the complete contributing source units. The G2 oracle corpus includes UTF-8, UTF-16LE/BE BOMs, surrogate pairs, chunk boundaries, malformed sequences and normalized expansions. Exact byte copying continues to use the original capture.

### 17.12 Ephemeral indexing precedes persistence

G2's candidate index works over an in-memory closed capture manifest with bounded immutable segments and no database. It proves candidate completeness and exact verification independently of disk publication. Persistent segment encoding, manifest transactions, recovery and out-of-core merging arrive through G4's artifact/store integration. Both routes implement the same search semantics and are compared with the same reference scan. This separation prevents the early indexed-search gate from depending on later persistence work; it does not remove persistent indexing from the full product.

---

## 18. Dependency graphs and code intelligence

### 18.1 Graphs answer questions, not just draw edges

The primary map is hierarchical. Graph relationships augment it when the user asks a question: “What imports this module?”, “What does this file depend on?”, “Which files form a cycle?”, or “Where are these related definitions?” Do not default to drawing every relationship over the entire atlas.

Each edge contains source/target identity, kind, source revision, evidence span, extractor version, and confidence/capability class. A co-occurrence edge is not a call edge. A lexical import name is not a resolved external package dependency until resolution rules support that conclusion.

### 18.2 Representation

Build compact integer node IDs and immutable adjacency snapshots. Store edge attributes in parallel arrays keyed by compact edge IDs. Use CSR-style snapshots for analytics and a small update overlay when necessary; rebuild/compact under budget rather than exposing a mutation-heavy general graph on every frame.

Map integer graph identities back to source identities through one versioned table. Node slots cannot be silently reused while a displayed path or selected relationship still references them.

### 18.3 Selected analyses

The useful initial analysis set is reachability, inbound/outbound neighbors, connected components, strongly connected components and condensation, bounded shortest relationship paths, and topological order for qualified acyclic projections. More expensive centrality or community analysis is optional enrichment, not a startup dependency.

Use selected FrankenNetworkX kernels after factoring and conformance tests. Its general graph semantics and deterministic tie-break discipline are valuable, but importing every algorithm and its dependency closure is not justified by these needs. [R4]

### 18.4 Incremental analysis

Changes invalidate incident facts and the analyses depending on them. Do not incrementally update an SCC condensation with an unproved shortcut that can miss a newly formed cycle. A bounded full recomputation of a relevant subgraph may be safer and cheaper than a complex dynamic algorithm.

Long-running analysis publishes an immutable graph version and states its coverage. The UI remains responsive while it runs. A bounded local view is always available even when global metrics are pending.

### 18.5 Visual treatment

Relationship edges are admitted by selection, query, and screen-space budget. Aggregate edges between collapsed directories, reveal local fan-in/fan-out, and provide a list equivalent in the Inspector. Curves have deterministic routing and avoid obscuring source where possible.

Graph metrics may change color/height overlays without relocating the atlas. A force-directed graph, if ever added, is a separate explicit analytic view with its own position semantics, not a replacement for stable source geography.

### 18.6 Snapshot comparison

A useful first-party extension compares two application-captured source snapshots and overlays added, removed, and changed files/ranges. A text-diff implementation must be bounded on adversarial inputs and produce exact anchors.

This does not imply Git history, blame, rename detection across commits, or repository status support. Those require a separately qualified first-party version-control provider. The baseline never shells out to Git silently or pretends local file timestamps are commit history.

### 18.7 Directed views and query-specific graph facts

Reuse the actual bidirectional CSR representation in FrankenNetworkX after upstream checked-construction and closure work. Validate sorted/unsorted row semantics, offset monotonicity, terminal lengths, node bounds, and overflow at the boundary. Incremental mutations build a new immutable view rather than changing the arrays a query has pinned. [B4]

Preserve separate graph projections for lexical imports, qualified resolved imports, structural containment, document links, and heuristics. The projection/evidence filter is part of every analytics cache key. Combining heuristic and resolved edges can change a cycle or reachability conclusion; it cannot happen invisibly.

A multiplicity-insensitive view may collapse parallel edges for SCC/reachability. The original facts and counts remain available for fan-in metrics, source spans, and explanations. A shortest relationship path is minimal under a declared edge cost and tie-break policy, not a proof of runtime behavior. Present “why this edge/path exists” from the underlying source evidence.

### 18.8 Documentation back-links and stable comparison

Index qualified Markdown-to-source links and nearby documentation anchors alongside source-to-source facts. This creates a useful bidirectional code/documentation navigation surface without pretending comments are compiler semantics. Resolvers name their language/root configuration and avoid executing package tooling.

Captured-snapshot comparison retains both old and new source anchors and a bounded range correspondence. Stable geography can show changes in place, while moved/reattached annotations require explicit verified mapping. A reading trail may pin both sides of a comparison. No Git blame, remote history, or build graph is inferred from these local captures.

---

## 19. Persistence and FrankenSQLite integration

### 19.1 What belongs in the database

Use FrankenSQLite for small authoritative personal state and qualified metadata/manifests: workspace/root settings, navigation history, bookmarks, annotations, layout generations, file observations, analysis manifests, and persistent cache references. Large immutable source chunks and search segments can live in bounded application-owned files referenced by manifests.

Do not turn every rendered line or glyph into a database row queried during paint. The database is not the frame scheduler or the live scene graph.

### 19.2 Separate user state from rebuildable state

Maintain an explicit distinction, preferably separate databases or independently recoverable stores:

- **User state:** bookmarks, annotations, preferences, manually pinned arrangements. Never discard this during “rebuild index.”
- **Derived state:** file observations, analysis receipts, search manifests, density tiles, cached layouts. Rebuildable with a clear scope and cost.

A cache repair that deletes a user's note is a severe correctness defect. Migration and recovery tests must exercise that separation.

### 19.3 Thread-affine connection ownership

One persistence service owns each connection on its qualified thread. The inspected synchronous facade is `!Send`; the async facade provides dedicated worker ownership and actual cancellation propagation. Use that model instead of sharing `Connection` among generic tasks. [R7]

The selected facade version, runtime version, and `Cx` conversion/ownership rules are pinned and tested together. Do not compile multiple incompatible Asupersync versions into the standalone process merely to satisfy old dependency constraints. Upgrade or factor upstream consumers coherently. Inspect any runtime constructed inside the facade; a dedicated actor may own an explicitly admitted Asupersync context, but an embedding call must not silently install a process-global runtime. A matching type name is not proof that its clock, capabilities, or lifetime belong to the host.

### 19.4 Schema outline

The following is a logical schema, not a promise that every advanced SQL feature is needed:

```text
workspace(workspace_id, schema_version, configuration)
root(root_id, workspace_id, root_identity, capability_policy)
file(file_id, root_id, raw_path, observation_generation, status)
source_version(source_id, file_id, byte_length, digest, capture_policy)
analysis_manifest(source_id, engine_version, feature_set, artifact_refs)
index_generation(generation_id, scope_id, manifest_ref, publication_state)
layout_generation(layout_id, root_id, hierarchy_generation, parameters, artifact_ref)
bookmark(bookmark_id, file_id, source_anchor, user_label, created_sequence)
annotation(annotation_id, source_anchor, body, update_sequence)
window_state(window_key, selected_anchor, camera, pane_configuration)
```

Prefer a small SQL subset whose behavior is directly tested. Do not require the most ambitious MVCC, native durability, FTS, or extension feature merely because it exists in the broader repository. Use simple short transactions and one write owner initially. The browser's responsiveness does not require concurrent writers to the same metadata store.

### 19.5 Atomic artifact publication

A derived artifact is written to a unique temporary generation, validated, synchronized as required by the durability contract, then published by a small manifest/transaction step. Existing readers retain the old generation until release.

A crash can leave an unreferenced candidate, a committed manifest, or a recoverable interrupted publication. Startup distinguishes these states. It does not blindly choose whichever file has the newest modification time. Cross-file publication cannot be called atomic merely because each individual rename is atomic; the manifest is the authority.

### 19.6 Cancellation, failures, and recovery

A canceled read can be discarded. A canceled write may already have committed; return the authoritative terminal outcome, then decide whether its artifact remains referenced. Never convert every cancellation into “nothing happened.” [R7]

On corruption, quarantine derived assets and rebuild them. For user-state damage, preserve the original bytes and use an explicit recover/restore path. Do not market RaptorQ recovery or page-level encryption as active application guarantees unless those exact integrated paths have been qualified. Filesystem permissions and OS storage protections remain the baseline privacy boundary.

### 19.7 Storage limits

Set disk-cache quotas, per-root quotas, and retention policies. Do not retain every observed source version indefinitely. The user can clear source-derived caches for a workspace, inspect disk usage, and preserve personal annotations separately. The diagnostic surface reports pinned generations that prevent immediate reclamation.

### 19.8 GUI, CLI, and embedded-process ownership

A single thread owning a connection does not establish a single writer across multiple `fcb` processes. Define a per-user store-owner role with OS-backed lifetime/lock semantics, schema/protocol version, and explicit attach behavior. A second GUI or CLI invocation either sends a bounded authenticated request to the live local owner, uses a separately qualified read-only mode, or obtains exclusive ownership after the OS establishes the prior owner is gone.

The owner is part of the running application/service instance, not an independently installed mandatory daemon. Headless commands can own their ephemeral store while running. An embedded library does not attach to the standalone app's store without the host explicitly authorizing that namespace. No PID file or wall-clock heartbeat alone permits stealing an active writer's ownership.

Local IPC uses owner-private endpoints and validated peer/user/session identity through the system boundary, bounded messages, version negotiation, capability-scoped roots, and explicit timeouts. Endpoint names are not authentication. The client cannot send an arbitrary path and thereby grant itself a new root. Recovery and schema migration are serialized with writes; an older client cannot reinterpret a newer schema silently.

### 19.9 Concrete publication and recovery ordering

For a new immutable artifact: reserve byte/disk/publication capacity; write a unique owned candidate; validate its version, length, content identity and invariants; perform the durability synchronization required by the selected native policy; publish the immutable file/name and directory state; then commit the database manifest that references that complete artifact; only then publish the in-memory head.

Never commit a manifest pointing to a candidate that is still being written. A pre-manifest crash leaves an unreferenced candidate eligible for bounded later cleanup. A post-manifest crash leaves a recoverable committed generation. A commit with uncertain caller outcome is reconciled by operation/generation ID before retry; do not duplicate an annotation or publish a tombstone twice because a response was lost.

Specify separately process-crash safety and power-loss durability, including the actual selected macOS filesystem synchronization contract. A rename is not by itself proof that both file contents and parent-directory changes survive power loss. The hardware/native recovery tests establish the supported claim; the plan does not presume it from `std::fs::rename`.

### 19.10 Numeric and format boundaries

FCB's `u64` byte offsets and identity domains cannot be blindly cast to a signed database integer. Use a validated representable bound where appropriate, or a documented fixed-width binary representation for full-width IDs/offsets. Ordering over encoded values is explicit. Text/native path bytes are length-tagged blobs where lossless text encoding is not guaranteed.

Schema rows reference immutable artifact schema/version, source capture, content identity and completeness. Decode under size/count limits, reject unexpected trailing/truncated sections according to the format policy, and keep checksum failure distinct from authorization. Borrow the inspected first-party canonical envelope/hash primitives after dependency factoring instead of writing unrelated ad-hoc formats. [B8]

Back up authoritative personal state before migration using a qualified mechanism. Index generation cleanup cannot remove user-state backups as if they were disposable caches. Cache clear, store migration, and source export have different authorities and cancellation/commit contracts.

---

## 20. Unified-memory budgets and pressure control

### 20.1 Budget philosophy

A 24 GB Mac is not a license to reserve most of the machine. CPU and GPU resource accounting must recognize unified memory and avoid adding the same shared allocation twice. Budgets use binary MiB/GiB; hardware capacity labels such as 24 GB are recorded as marketed, with actual physical bytes queried at runtime. The native Metal interface exposes device resource-budget information such as `recommendedMaxWorkingSetSize`; it is a runtime input, not a fixed fraction assumed from the chip name. [A2]

Initial standard-profile targets are **3 GiB of tracked live application resources** and a **6 GiB hard admission guard** for owned/managed resources. These are design targets, not measured process-footprint guarantees. Allocator overhead, system objects, drivers, file cache, and framework allocations require separate measured headroom and pressure response.

### 20.2 Standard profile allocation plan

| Category | Initial target | Included resources |
|---|---:|---|
| Owned source snapshots/hot chunks | 768 MiB | Shared across views; old revisions count until released. |
| Lexical/structural analysis | 384 MiB | Checkpoints, token runs, outlines, compact facts. |
| Search hot state | 320 MiB | Posting caches, path index, query working sets. |
| Atlas hierarchy/spatial/graph state | 192 MiB | Compact maps and selected graph snapshots. |
| Document/reader layout | 384 MiB | Visible/nearby shaped runs, block layout, height indexes. |
| CPU font/image caches | 256 MiB | Font data, raster staging, bounded decoded images. |
| All application GPU resources | 512 MiB | Atlases, instance buffers, targets, textures, in-flight generations. |
| Database page/metadata caches | 64 MiB | Explicitly configured persistent-service working sets. |
| UI/runtime/control state | 64 MiB | Queues, tasks, history projections, telemetry rings. |
| Shared transient scratch | 128 MiB | Bounded parse/index/upload workspaces. |
| **Total** | **3,072 MiB = 3 GiB** | Shared allocations assigned to one accounting owner. |

These are admission targets, not isolated reservations that must all be filled. Idle subsystems can lend budget within the global guard. A single operation cannot borrow beyond the guard simply because its own subsystem has no cap.

### 20.3 Accounting rules

Every substantial allocation family has an owner and a budget class. CPU/GPU-shared storage is counted once in the managed-resource ledger; OS-reported metrics are displayed separately with an explanation of overlap. Resource leases count until GPU completion, not until the CPU drops a drawing command.

Peak transient demand matters. Replacing a 400 MiB artifact may temporarily require old and new generations together. Admission reserves for that overlap before construction. Generation compaction and cache rebuilds must fit inside the same peak budget, not just their steady state.

### 20.4 Pressure state machine

| State | Trigger | Response |
|---|---|---|
| Normal | Within soft target; OS pressure low. | Ordinary prefetch and maintenance budgets. |
| Constrained | Soft target exceeded or rising OS pressure. | Stop speculative prefetch; reduce optional labels/3D assets; shrink caches. |
| Critical | OS pressure high or admission approaches hard guard. | Cancel discretionary work; evict cold derived state; lower indexing concurrency; keep selected reading state. |
| Recovery | Pressure subsides for a hysteresis interval. | Rebuild budgets gradually; do not immediately refill all caches. |

The selected source range, user's state, and required UI controls have protected minimum working sets. Optional visual quality degrades before basic text legibility or source correctness. **Controlled admission failures** are typed states. This is not a claim that every allocation anywhere in the standard library, framework, driver, or host can report recoverable OOM, or that the OS cannot terminate the process. Use fallible reservation for owned large collections, validate actual capacity, and keep measured headroom. [B13]

### 20.5 Scale profiles

A larger-memory machine may increase cold-cache retention and background throughput, but the algorithms must not depend on it for correctness. The stress profile remains out-of-core on a 24 GB machine. Larger scene metadata must be paged/aggregated instead of becoming an unbounded `Vec` whose size tracks every possible source token.

Multiple windows share source, font, and index caches while retaining separate viewport/display resources. Opening a second window must not duplicate an entire repository snapshot. Closing a workspace releases all derived resources once their leases and shared owners are gone.

### 20.6 Display resolution costs

Render targets scale with physical pixel count, not the logical window's point count. A 4K RGBA8 surface alone is about 31.6 MiB; multiple targets, depth buffers, and retained frames multiply that cost. At higher resolutions, the GPU budget may need to reduce intermediate surfaces, tile retention, or optional City effects rather than silently exceed its allowance.

Text remains rendered at the intended physical resolution. Any deliberate dynamic-resolution policy applies only to explicitly optional visual layers and is reported; it must not be used to claim native-resolution benchmark results for a lower-resolution image.

### 20.7 Budget ownership is a foundation, not a final optimization

Introduce `ResourceBudget` and lease accounting with the first source/GPU allocations in G0/G1. The later memory-pressure milestone verifies the complete system, not the first appearance of admission control. Otherwise every earlier API can hide allocations that are expensive to recover from retrospectively.

Charge allocated capacity, not just logical length. Include container capacity, compressed and decompressed versions, decoded font/image state, native retained objects where measurable, pending request payloads, retired CPU snapshots, in-flight GPU resources, and old/new generation overlap. Fallible `try_reserve` protects specific allocations, not the entire process; audit upstream output construction and actual allocator capacity growth. [B13]

Large results carry a lease until their last retained owner releases them. Count a shared allocation once within the accounting domain, but do not use an `Arc` clone to hide that it remains live. No second view receives a free separate 3 GiB allowance: the app profile is process/shared-domain-wide, while an embedding host chooses its own smaller explicit allowance.

Prefer bounded chunks and controlled exact-reservation paths to unconstrained geometric collection growth. Reserve a conservative managed-capacity bound before allocation; reconcile returned capacity before publication. An unexpected oversize allocation is recorded, rejected from publication and retired, not silently retained under a smaller charge. Allocator bookkeeping, fragmentation, framework allocations and OS residency are separately measured headroom: the managed admission ceiling is not a kernel-enforced resident-memory maximum.

### 20.8 Physical memory versus policy labels

The standard 3 GiB target and 6 GiB admission ceiling are adjustable standalone defaults, not required reservations and not a minimum viable footprint. Empty/headless/library use should scale with actual inputs. A constrained embedding may use tens of MiB for a small source view without creating database/font/map caches it did not request.

On unified memory, `GPU`, `CPU`, and `staging` describe access/use, not necessarily distinct physical pools. A demotion counts as pressure relief only when relevant physical/managed allocations are actually released or reduced. Retain compressed summaries or recompute cheap glyphs rather than copying every evicted atlas page to another equally large allocation. The inspected FrankenTerm policy is adapted, not copied with its separate-tier assumptions. [B3]

Use OS pressure and device budget telemetry as independent signals with defined sampling cost. A recommended device working-set value is not permission to consume all host memory. Avoid repeatedly filling a cache during the recovery hysteresis interval. Do not call an untracked framework allocation “zero-copy” merely because the CPU and GPU share silicon.

### 20.9 Bounded disk and reclamation progress

Disk quotas include capture generations, partial candidates, search segments, compaction scratch, retained backups, and export staging. Account the worst admitted overlap before a merge or snapshot capture starts. Slow pinned readers can prevent reclamation; expose that state and refuse optional new work instead of deleting a live generation.

Reclamation has a protected progress path. Terminal GPU records, retirement slots and cancellation-drain bookkeeping must remain available even when ordinary work exhausts its soft budget. Prevent a deadlock in which freeing memory requires a queue entry that itself waits for free memory. Test repeated source replacements, two readers pinned to old revisions, a full disk, and device completion arriving during critical pressure.

---

## 21. Performance objectives and measurement

### 21.1 Targets, not claims

No M4/M5 application benchmark has been run for this plan. Every number in this section is an initial engineering objective to test and revise with evidence. The recording does not establish any of them.

The benchmark result must name the physical Mac, chip variant, memory, OS/SDK, build and suite pins, display resolution/backing scale/refresh, power mode, thermal state, corpus, and cache state. “Runs on an M5” is not a meaningful performance report by itself.

### 21.2 Corpus classes

| Class | Intended scale | Purpose |
|---|---|---|
| Interactive small | 1–5k files; ordinary mixed-language project. | Cold startup, typography, interactions, correctness. |
| Standard large | About 100k files, 10 million lines, and 1–2 GiB source payload, measured exactly per fixture. | Primary 24 GB smooth-navigation and indexing coexistence target. |
| Stress | About 1 million files and 10–20 GiB source payload; actual line/byte counts recorded. | Out-of-core behavior, discovery, memory bounds, no UI collapse. |
| Pathological | Huge single line, giant Markdown table, deeply nested tree, invalid text/font/image, rapid replacements. | Complexity and failure behavior, not ordinary throughput marketing. |
| Real suite | Pinned representative Franken repositories and mixed real projects. | Real names, source distributions, documentation, dependency trees. |

Generated counts must be verified. An empty-file million-node map is not a substitute for a million-file text workload. Separate metadata-only, source-loaded, and index-complete runs.

### 21.3 Initial latency objectives

| Operation | Initial objective | Measurement boundary |
|---|---|---|
| Warm frame during pan/zoom on qualified 120 Hz display | p95 delivered frame interval near one 8.33 ms refresh; p99 no more than two refreshes in standard trace. | Presented frames, including missed refreshes. |
| CPU frame preparation/submission | p95 ≤ 3 ms in the standard visible envelope. | Reducer-delta admission through submit; native waits reported separately. |
| GPU execution | p95 ≤ 4 ms in the standard visible envelope. | Actual GPU interval where available; not CPU encoding time. |
| Event-to-present | p95 ≤ 25 ms at 120 Hz; ≤ 45 ms at 60 Hz. | Input timestamp to associated presentation, with queue depth. |
| Physical input-to-photon | Separate target matching the above where measurement permits. | High-speed-camera or equivalent physical measurement; never inferred from software alone. |
| Warm open of indexed workspace | Useful interactive atlas within 300 ms, full visible detail progressively. | User open action to usable frame; includes loading persisted state. |
| Cold open | Window/controls useful within 500 ms; first meaningful partial atlas within 1 second for local standard root. | Includes real pipeline initialization; full scan/index not hidden in this number. |
| Warm file/path search | Initial useful results p95 ≤ 30 ms. | Finalized input to visible result batch. |
| Warm indexed literal search | Initial useful results p95 ≤ 100 ms. | Includes exact-hit verification and UI publication. |
| Reading a selected ordinary source/Markdown file | First useful exact content p95 ≤ 100 ms warm; cold costs broken down. | Selection to visible readable content. |
| Canceling obsolete cooperative work | Work stops within one measured bounded quantum plus scheduling delay. | Cancellation request to last useful CPU work; foreign calls excluded explicitly. |
| Static idle window | No continuous full-rate redraw; near-idle application CPU after work completes. | Sustained trace with update causes recorded. |

CPU and GPU timing overlap. Their individual objectives do not prove a frame objective by simple addition. Startup objectives are especially sensitive to cold filesystem/SDK costs and must be reported honestly if unmet.

### 21.4 Visible-work envelope

Initial standard-scene budgets should be tested around 20–30k simple visible parcel/overlay instances, up to roughly 150k admitted glyph instances across visible panels, and a few thousand labels at most, with clipping and content chosen for legibility. These are candidate admission limits, not hardware capacities.

The point is to bound useful per-frame work independent of a repository containing millions of files. If a realistic view exceeds a budget, aggregation/LOD should reduce it. Never hide omitted selected text; expose a reading view instead.

### 21.5 Benchmark methodology

Run cold-start and warm-steady-state lanes separately. Use repeatable camera/search/navigation traces and retain the actual trace plus all version/configuration metadata. Compare the same resolution, font, corpus, and power mode. Keep symbols/frame pointers in a dedicated performance profile.

Report median, p95, p99, worst relevant stalls, missed-refresh counts, peak tracked allocations, measured footprint, GPU resource bytes, bytes read, index coverage, and idle behavior. Include at least one sustained run long enough to expose thermal or cache-growth behavior.

A Linux headless test can establish semantic correctness and some CPU costs, but cannot qualify native Mac display pacing, GPU text quality, Metal lifetimes, or 24 GB unified-memory behavior.

### 21.6 Optimization order

Optimize in this order: eliminate unnecessary work; bound work by visibility; retain and invalidate precisely; improve data layout; reduce allocation/copying; tune CPU concurrency; improve GPU batching/fill; then consider specialized SIMD/compute techniques.

Each significant optimization gets a before/after trace on the same workload, a correctness comparison, a memory/latency tradeoff statement, and a retained regression fixture. Reject changes that improve average throughput by creating interaction tail stalls or materially degrading text.

### 21.7 Adaptive policies

Begin with understandable bounded heuristics. If later traces justify statistical/adaptive policies from FrankenTUI or Asupersync, deploy them in observe-only mode first. Their output may tune discretionary budgets; it must not relax source correctness, memory guards, or safety.

Do not introduce a Bayesian controller, e-process, or learned ranking model merely because a sibling project contains one. A deterministic threshold with hysteresis is often the right initial policy for a GUI cache.

### 21.8 Define “useful” before timing it

A first useful atlas frame contains a navigable real root/neighborhood with honest coverage, working input, and a path to readable real source; an empty window with a spinner does not satisfy it. A useful source frame contains exact visible content plus explicit pending analysis state, not a texture of placeholder lines. A search latency number includes the corresponding visible result generation.

Delivered-frame intervals are measured during a continuous active-input/animation trace, not across intentional static idle. Include missed refreshes and classify why a frame was late. Event-to-present uses a verified timestamp domain and matching presented-frame identity. No timing scope omits synchronous destruction, drawable acquisition, or cold font costs merely because those occur outside a function named `render`.

Compare matched visible content and quality: same physical pixel count, source/Markdown features, font route, motion trace, opacity/clip workload, cache condition, power/thermal state and background indexing load. Pixel scaling or skipping selected text defines a different quality lane, not a speedup. Report CPU and GPU overlap rather than adding unrelated percentile figures.

### 21.9 Optimization experiments with high expected value

Prioritize five measurable candidates: shared multi-resolution token summaries; content-keyed single-flight source/text work; page-local atlas updates with independent raster keys; retained immutable layout tiles plus bounded retirement; and visibility-weighted prefetch within strict byte/I/O limits. Each removes repeated work while preserving a simple scalar/semantic reference.

Only after those are characterized should optional GPU compaction, alternative glyph representations, or statistical budget selection enter an experiment lane. Adaptive policies run shadow/observe-only against deterministic baseline rules first, retain a bounded decision trace, and cannot relax correctness or memory limits. This is an explicit use of the suite's replay/evidence ideas without requiring every controller in the ecosystem to ship in a source browser.

---

## 22. Safety, security, privacy, and trust boundaries

### 22.1 Threat model

Treat repository content, paths, Markdown, fonts, images, query strings, cached artifacts, and filesystem events as untrusted inputs. A repository can be intentionally adversarial without the user expecting to execute it. Opening a directory must not run code from that directory.

Protect against memory-unsound native access, parser denial of service, unbounded expansion, path traversal, stale-result substitution, cross-workspace data leakage, GPU use-after-free, corrupted index publication, and accidental disclosure through logs or previews.

### 22.2 Native bridge safety obligations

The bridge must maintain a per-API safety ledger covering:

| Area | Required invariant |
|---|---|
| Objective-C object ownership | Correct retained/borrowed conventions; no double release; lifetime survives callbacks. |
| Thread affinity | Main-thread-only objects cannot be sent/shared through an unsound blanket implementation. |
| ABI signatures | Types, calling conventions, struct returns, integer widths, and nullability checked against the pinned SDK. |
| Callback context | Callback state remains alive; shutdown prevents late use; no borrowed stack pointer escapes. |
| Panic/exception boundary | No Rust unwinding through foreign frames; no unsafe assumption that a foreign exception can cross Rust. |
| Buffer access | CPU/GPU aliasing and lifetime tracked by leases/completion, not wishful borrowing. |
| Reentrancy | Native callbacks cannot obtain conflicting mutable access to application state. |
| External strings/data | Lengths and encodings validated before owned conversion. |

Favor narrowly generated or hand-reviewed signatures over a generic variadic message dispatcher exposed throughout the codebase. Keep all raw-pointer code local. A C++-style exception catcher is not smuggled into the build. APIs used by the bridge must have qualified nonthrowing/error-return behavior under its validated inputs. Where an API can throw even for admitted inputs, G0 must establish a sound fail-stop boundary or reject that entry point; simply hoping an exception never crosses Rust frames is not an accepted safety argument. Unexpected native exceptions must not unwind through Rust.

### 22.3 Safe resource wrappers

Use opaque handles with generation checks, validated sizes/usages, and limited operations. Do not export raw pointers, unrestricted mapped slices, or `Send`/`Sync` implementations without a documented thread/lifetime proof. A resource released by the user is retired, not necessarily immediately destroyed while frames still reference it.

Sanitizers/native diagnostics and hostile lifecycle tests complement review. They do not establish soundness alone; every unsafe block still needs a reason and invariant.

### 22.4 Parsing and decompression limits

Every parser/decoder has finite limits on input size, nesting, output allocation, total work, and decoded payload. An image's compressed byte size does not bound its decoded size. Repeated references to the same data must not defeat expansion limits. Checked arithmetic precedes allocations and index calculations.

Source failures degrade to useful exact/escaped content when possible. Font/image failures show placeholders and diagnostics. Nothing in an untrusted repository may enable arbitrary shaders or dynamic native code.

### 22.5 Local privacy

No telemetry or source upload by default. Logs record timings, counts, IDs, and error classes without full source text or secret-bearing paths unless explicitly requested. Diagnostic exports offer a redacted mode and disclose when source snippets/images will be included.

Cache directories are private to the user and logically partitioned by workspace/root. Clearing a workspace cache removes source-derived text, previews, and index payloads for that workspace subject to active leases; it does not silently erase personal notes. Crash artifacts should avoid dumping entire source buffers by default.

### 22.6 External actions

Opening an editor or external link requires an explicit user action and a configured trusted target. Arguments are passed structurally, not interpolated into a shell string. The app does not auto-run repository build scripts, language servers, hooks, package commands, or agent instructions found in files.

Optional agent-facing control is local, authenticated to the user's session, and capability-scoped. Read-only source discovery does not imply permission to mutate files or navigate another user's workspace.

### 22.7 Cache clearing and capability revocation

A clear request names an owned cache namespace and generation, never an arbitrary recursive filesystem path. Refuse filesystem roots, home/source roots, ancestors of protected paths, missing/mismatched owner markers, and ambiguous ownership. Use the native descriptor-relative/no-follow service where strict confinement is required; a prior lexical check does not defend against concurrent path replacement.

Rotate the logical ownership generation before admitting new writes into a cleared namespace. Old handles cannot repopulate the new cache after a delayed read completes. Active immutable leases may outlive the logical clear until safely released; report deferred reclamation. Shared content remains only while other authorized namespaces hold references, and never becomes discoverable across roots just because its hash matches.

Clearing a cache is a logical deletion/reclamation operation, not a guarantee of forensic erasure from APFS snapshots, OS caches, backups, or storage media. Never imply that clearing derived state erases user annotations or backups. The inspected FrankenManim cache is useful prior art, but its own same-user race/stale-lock caveats are not replaced by stronger words in FCB documentation. [B8]

### 22.8 Host data and automation are untrusted boundaries

An embedding host is a separate authority: it grants providers and storage/render services explicitly. Validate provider lengths, paths, span offsets, graph indices, and result generations even if the types came from a safe Rust caller; memory safety alone does not establish semantic validity or reasonable complexity.

Deep links and robot requests identify a source anchor plus a previously granted scope. They never silently grant access to arbitrary `file:` paths. Only documented external-link schemes are admitted, with no shell interpolation. Reading trails, source previews, and export content are data, not instructions to launch an agent or execute a repository command.

Native callback state, Objective-C runtime class registration, and completion ownership are instance-safe. Repeated embedding cannot register incompatible callbacks under a colliding process-global class name or tear down another instance's shared platform state. Such platform-global details remain in the audited bridge, not scattered through FCB views.

---

## 23. Accessibility and native macOS behavior

### 23.1 Accessibility is a parallel representation

A custom GPU surface does not automatically provide usable accessibility. The platform adapter must expose an accessibility tree derived from the semantic UI/source model, with stable identities, roles, names, values, actions, selection, and text ranges.

Do not instantiate one native accessibility object for every character or every file in a million-file repository. Virtualize the tree, provide requested ranges/subtrees on demand, and present a conventional outline/results/reader route that does not require spatial vision.

### 23.2 Keyboard equivalence

Every pointer operation has a keyboard equivalent: change scope, focus parent/child/sibling, search, select a result, open/pin a reader, switch source/preview, follow/backtrack a link, fit selection, toggle projection, and inspect relationships.

Focus is visible and independent of selection. Opening a new panel preserves a predictable return focus. Modal surfaces do not leak keyboard events into the atlas. Native text-editing shortcuts work in search fields, and system-reserved shortcuts are not stolen.

### 23.3 Text and screen readers

The reader exposes logical text and selection through the same source maps used for copying. A screen reader should be able to navigate headings, lines, links, and selected source without hearing every atlas label. The same result and source anchor are available in a linear reading mode.

Test marked-text input, right-to-left selection, combining marks, and mixed scripts in both visible and accessibility surfaces. Accessibility queries must not synchronously parse an entire file on the main thread.

### 23.4 Native behavior

Support opening folders/files through the standard dialog and drag-and-drop; multiple windows; standard menu commands; clipboard; backing-scale changes; moving between displays; full screen; sleep/resume; appearance changes; and graceful app termination with a defined persistent-state outcome.

Observe reduced motion, increased contrast, and larger text preferences. Resize remains interactive while expensive content reflows in bounded work. On a hidden or minimized window, suspend unnecessary rendering and prefetch.

### 23.5 Accessibility starts with the first reader

G0 defines semantic node identities, source/decoded/native range mapping, focus return, and host accessibility attachment. G1 proves a small real reader's keyboard selection and native accessibility text route. Later phases expand the corpus and polish all controls; they do not begin accessibility design after the renderer is entrenched.

Virtualized accessibility requests use the same accepted semantic layout as the displayed frame, and resolve only bounded requested ranges. A screen reader action may schedule source context preparation and receive a clear pending state; it must not synchronously shape an entire giant paragraph inside an AppKit callback. Expose document headings, table structure, link roles, and source lines through the upstream FMD semantic tree and FCB source model.

Host embedding preserves the host's menu, responder chain and focus hierarchy. Attaching one FCB view cannot install an application-global event monitor that intercepts unrelated input. All native text ranges and sentinel conversions receive explicit UTF-16/source-byte tests against the selected SDK.

---

## 24. CLI, agent interface, and observability

### 24.1 Proposed CLI surface

These commands define the intended product contract; they are not installed by this plan:

```bash
fcb
fcb /path/to/repository
fcb open /path/to/repository
fcb open /path/to/file.rs --line 120
fcb search /path/to/repository --text "cancel" --json --limit 50
fcb inspect /path/to/repository --json
fcb capabilities --json
fcb doctor --json
fcb index /path/to/repository --json
fcb cache status --json
fcb cache clear --workspace WORKSPACE_ID --derived-only
fcb trail export TRAIL_ID --format markdown --out /path/to/context.md
fcb bench replay TRACE_FILE --output RESULTS_DIRECTORY
```

GUI operations and robot commands share the public library service APIs. The CLI must not implement a second search/index policy. Normal machine output goes to stdout; diagnostics go to stderr. JSON schemas and error codes are versioned. A first-party codec or small bounded schema-specific encoder is used only after inspection; a new serialization crate is not assumed exempt.

`fcb open FILE` grants a read root at the file's parent directory unless `--root` names an authorized ancestor; a file inside an already-open workspace root reuses that root. The effective root is displayed, and opening a file never widens scope silently. Bare `fcb` is explicitly the human GUI launcher; machine callers use a subcommand with `--json`. A bare machine flag such as `fcb --json` resolves to capabilities/readiness without opening a window. Document this distinction. `capabilities` works without a selected source root, database, GPU, or network. Argument parsing supports `--`, raw native paths where possible, spaces, non-ASCII filenames, deterministic errors, bounded input, and no shell execution. JSON encoders escape all input correctly and round-trip arbitrary supported text under tests.

`fcb` is a real standalone executable, not a shim that requires a separately installed `.app`. The Mac release embeds its required shaders/font/default resources or ships a self-contained declared artifact with the binary as its actual runtime; the strict single-file lane embeds required assets. The optional `.app` wraps the same executable/resources for Finder integration. Both launch routes exercise the same library and are qualified independently. An embedding library never parses process arguments on construction.

### 24.2 Capability record

Expose implemented and qualified status separately for native renderer, display-link mode, text shaping routes, Markdown features, languages, structural analysis, search modes, persistence, optional integrations, and dependency policy.

A proposed capability state is one of `qualified`, `implemented-unqualified`, `unavailable`, or `blocked`, with version/evidence/reason. Do not call a stub “available” because its symbol exists. Unknown hardware or a failed pipeline has an explicit capability result.

### 24.3 Error contract

Errors include a stable code, subsystem, human message, retryability, affected scope, source/request generation where relevant, and a safe next action. Examples:

```text
SOURCE_CHANGED_DURING_READ
SOURCE_ENCODING_UNSUPPORTED
ROOT_PERMISSION_DENIED
INDEX_COVERAGE_PARTIAL
QUERY_BUDGET_EXCEEDED
LEXER_CONTEXT_PENDING
DOCUMENT_ASSET_BLOCKED
GPU_DEVICE_GENERATION_LOST
RESOURCE_BUDGET_DENIED
DEPENDENCY_PROFILE_NONCOMPLIANT
PERSISTENCE_PUBLICATION_RECOVERY_REQUIRED
```

An error can coexist with useful partial output, but its completeness metadata must make that obvious. `doctor` is read-only unless a repair is explicitly requested. Repairs describe what can be rebuilt and what personal state will be preserved.

### 24.4 Performance HUD

A development HUD shows visible/admitted objects, glyph misses, frame times, queue depths, dropped stale results, source/index coverage, managed memory, GPU bytes, and reasons for degraded optional detail. Production can keep it hidden while retaining bounded counters.

Use fixed-capacity event rings and aggregate counters. Observability must not allocate a log record per glyph or emit source payloads at 120 Hz. Expensive tracing is explicitly enabled and its overhead reported in benchmarks.

### 24.5 Replay and receipts

Record a replayable stream of semantic input events, not opaque native pointer addresses. A receipt includes suite/build IDs, corpus identity, application options, display metrics, source-generation changes, outcome, and metrics. Native display timing may differ across runs; semantic selection and source outcomes must still agree.

A replay that reproduces a visual state is not evidence that a native accessibility path was exercised. Receipts identify the route tested.

### 24.6 Capability census must not pass vacuously

Maintain a fixed required-feature registry for each supported consumer/release profile. Every required feature has an implementation route, conformance evidence, safety/dependency classification, and a separate performance qualification state. An empty capability vector, omitted row, or untested symbol cannot make the profile “all green.” The implemented feature-manifest discipline in FrankenThreeD is a useful model, not permission to credit absent renderers. [R10]

Headless capability output is meaningful without native resources. A Mac renderer may be implemented but unqualified for the current device/display configuration. Source coloring may be source-exact with provisional lexical classification. Machine clients can distinguish those dimensions rather than interpreting a single `available=true` as every guarantee.

Diagnostic requests are bounded and read-only by default. Rendering a doctor report must not launch a benchmark, touch every repository file, or rebuild an index unexpectedly. High-cardinality source/path contents remain redacted unless the user explicitly requests an unredacted export.

### 24.7 Lossless wire identities and bounded response framing

JSON uses UTF-8 Unicode strings; native paths need not be valid UTF-8. Provide a tagged reversible native-path payload (for example, bounded hexadecimal Unix path bytes) alongside an escaped display label. Never round-trip a source identity through lossy text or serialize Rust's unspecified `OsStr::as_encoded_bytes` representation as a portable format. Unsupported platform tags refuse explicitly. [B15]

Serialize full-width integer IDs, generations, lengths and offsets as canonical decimal strings in the machine schema, with checked bounds and no sign/leading-zero ambiguity. Ordinary small counters may use numbers only with declared bounds. This avoids precision loss in clients using binary64 JSON numbers above the interoperable exact-integer range. Reject duplicate object fields and nonfinite numeric geometry rather than depending on parser-specific interpretation. [B16]

Ordinary `--json` returns one bounded complete JSON document. Progressive GUI publication does not silently change stdout into multiple JSON documents. A separately selected streaming mode, if implemented, has versioned NDJSON records with sequence/query identity and a terminal coverage/outcome record. EOF without that terminal record is interrupted output, not complete search. Output backpressure has byte/time limits and cannot block AppKit or hold shared publication locks; a broken pipe cancels this client's subscription while preserving authoritative outcomes of already committed effects. Tests cover raw paths, integers around 2^53 and u64 limits, escaping, duplicate keys, truncated streams and slow/closed consumers.

---

## 25. Testing and qualification

### 25.1 Test layers

| Layer | What it proves | Examples |
|---|---|---|
| Pure unit/property tests | Local invariants and arithmetic. | Checked ranges, ID exhaustion, line mapping, treemap containment, token coverage. |
| Corpus/differential tests | Content semantics and compatibility. | Highlight fixtures, Markdown dialect, search exactness, selected graph algorithms. |
| Deterministic orchestration | Ownership, cancellation, stale publication. | Close/query races, queue pressure, interrupted artifact publication. |
| CPU reference rendering | Display-list semantics, clipping, selection geometry. | Primitive/path/glyph-run fixtures with stable inputs. |
| Native integration | Real ABI/input/window/persistence behavior. | IME, display changes, Metal resources, file events, app lifecycle. |
| GPU visual qualification | Rendering quality and bounded equivalence. | Text atlases, opacity/order, clip, source/Markdown screenshots. |
| Performance/pressure | Measured target-class operating envelope. | M4/M5 traces, 24 GB pressure, cold/warm indexing, sustained navigation. |

All release-critical cases use production code paths. A mock-rendered treemap cannot satisfy a Metal test, and a generated JSON response cannot stand in for opening a real source file.

### 25.2 Essential invariant fixtures

Test that highlight spans tile exact source bytes; no UTF-8 or CRLF boundary is sliced incorrectly; line/byte/grapheme mappings round-trip where defined; stale handles cannot resurrect; cache keys include snapshot identity; fixed-height calculations cannot overflow; and a source generation never mixes different captured bytes silently.

Treemap tests cover deep hierarchies, empty/unknown files, huge outliers, local insertion/deletion, containment, overlap, deterministic order, and displacement budgets. LOD tests oscillate around thresholds and verify resource churn remains bounded.

### 25.3 Search correctness

For each query mode, compare indexed results against a straightforward exact reference scan over the same captured source set. Include chunk-boundary matches, short queries, case variants, Unicode normalization modes, deleted/replaced files, index-generation changes, and cancellation.

A partial-index test must never emit `complete` accidentally. A failed replacement publication must not remove an existing live document from all current results. A result click must resolve its exact snapshot or clearly report that it is stale.

### 25.4 Markdown and text

Maintain a visual/document corpus covering tables, nested lists, long code fences, math, diagrams, local images, malformed markup, huge paragraphs, Unicode and bidi, multiple fonts, and linked source ranges. Test source/preview round-trip navigation independently of pixel images.

Use deterministic bundled fonts for bit-stable CPU fixtures. System fallback shaping has a declared platform/version matrix and visual/semantic expectations rather than cross-platform bit identity. GPU results are checked with both structural invariants and bounded visual comparisons; do not require floating-point GPU output to be bit-identical across every chip.

### 25.5 Native safety/lifecycle stress

Exercise window close during glyph upload; device failure with frames in flight; repeated display migration; sleep during indexing; rapid source replacement; root disconnection; memory pressure while a DB write commits; late native callbacks after cancellation; and repeated open/close cycles.

The important result is conservation: no resource freed too early, no orphaned task, no stale content substituted, no indefinite main-thread wait, and no growing leaked resource set. A “no crash in one run” report is insufficient.

### 25.6 Fuzz and hostile inputs

Create deterministic in-tree generators for malformed source/Markdown, deeply nested patterns, arbitrary path bytes, corrupt artifacts, font tables, and image metadata. Optional external fuzz runners can be development tools without entering the shipping dependency graph, but the core regression corpus must run with the project toolchain alone.

Bound test workloads so CI/local qualification cannot be hung by one case. Store minimized reproductions and the seed/input that triggered every defect. Test negative paths as thoroughly as normal examples.

### 25.7 Release proof on real hardware

At least one qualifying M4 24 GB machine and one M5 configuration must run the named native corpus and traces before claims covering both are made. A 60 Hz display run and a high-refresh display run are distinct lanes; a 4K/native-resolution run is not interchangeable with a small logical window.

When only one class is available, certify that class and leave the other explicitly unqualified. Do not extrapolate M5 numbers from M4, or high-end Max/Pro results to a base chip with a different memory configuration.

### 25.8 Test tooling discipline

Tests and performance runs can execute on the owner's local/remote machines; GitHub Actions is not a product dependency. Version and archive the test command, artifact manifest, trace, and result. Fast host-independent tests run on every change; native visual/performance suites gate renderer and platform changes at appropriate cadence.

Performance thresholds are accompanied by correctness assertions. A regression fix cannot improve the test result by lowering resolution, reducing the corpus, disabling syntax/Markdown, or excluding slow frames without an explicit new test lane.

### 25.9 Additional review-driven regression matrix

| Defect class | Required adversarial case | Passing result |
|---|---|---|
| Owner collision | Two browser arenas use the same slot/generation, then exchange handles. | Cross-owner lookup rejected; one instance closing cannot corrupt the other. |
| Pixel/model mismatch | Move a parcel or reflow text while a previous frame is still presented. | Click and a11y geometry use an explicitly compatible presented snapshot. |
| Destruction stall | Drop the last reference to a large AST/source graph during frame publication. | Large destruction runs off the UI thread; bounded retirement bytes remain charged. |
| Lost completion | Saturate ordinary event queues while GPU submissions complete and windows close. | Every submission reaches a terminal record and retires exactly once. |
| Scope priority mistake | Nest nominally low-priority maintenance under an urgent context. | Effective priority is understood; admission still protects interaction/fair progress. |
| Incomplete Markdown maps | Escapes, entities, nested links, dedented fences, generated markers, includes. | Upstream maps retain truthful disjoint/multi-source provenance. |
| General huge-line shaping | Large bidi paragraph, joining script, tabs, ligatures, combining sequence. | Exact context preparation or explicit logical/escaped route; no false visual-exact claim. |
| Live-tree search | Add/change/remove files while a search and index publication run. | Complete means a closed named manifest; newer content is a new generation. |
| Discovery omission | Cancel or fail a directory scan after only some entries. | Unseen entries are not marked deleted. |
| Special source object | Replace a regular file with a FIFO or symlink during preview. | Safe admission rejects or bounds the operation without hanging the UI. |
| Memory overlap | Reflow, compaction, old pinned captures, and upload compete for the final allowance. | Reservations fail/defer cleanly; completion/reclamation cannot deadlock. |
| Multi-process store | GUI and CLI open/migrate/write the same store simultaneously. | One explicitly authorized owner or qualified isolation; no stale-lock takeover. |
| Artifact commit ambiguity | Kill the process before/after every publication and lost response point. | Old or new complete generation recovered; no half-index or duplicated user mutation. |
| Feature leakage | Build external consumers without workspace/dev feature assistance. | Headless profile stays headless; chosen closure is reported accurately. |
| Upstream ownership | Build the FMD flow consumer without FCB, then FCB against the landed FMD API. | Reusable Markdown behavior exists only in its upstream owner. |

### 25.10 Required embedding and binary tests

The library test matrix includes two sessions with separate grants, one shared source provider, optional shared font caches, separate device owners, independent close, and a host that remains operational. The standalone matrix includes direct `fcb` launch with no companion app, `.app` launch, headless CLI with no display, resources discovered from paths containing spaces, and no unexpected startup effects in library consumers.

Run pure-core and deterministic semantic tests without native frameworks, then native tests against actual Apple text/input/rendering paths. Neither substitutes for the other. Qualification evidence is generated by test commands and real artifacts; unchecked boxes and target figures remain unchecked until those routes actually pass.

---

## 26. Build, dependency closure, and distribution

### 26.1 Coherent suite pin

Create a `SUITE.lock`-style manifest containing each selected repository commit, package identity, enabled features, source origin, license, unsafe boundary classification, and dependency evidence. The reviewed blob hashes in §32 are research provenance, not that future resolved lockfile.

Resolve the entire native application's normal/build dependency graph against the pinned toolchain. A new package or feature edge fails the closure gate until reviewed. The build must not fetch moving branches, use uncommitted sibling working trees, or silently select a different Asupersync version.

### 26.2 First-party-only closure work

The critical upstream task is to factor or feature-gate Asupersync's required desktop runtime so unused networking/security/serialization/adapter subsystems do not force outside packages into this application. Essential externally implemented primitives must be replaced by actual first-party/std-backed implementations with maintained safety and performance tests, not merely hidden behind a new package name.

Repeat the same process for selected FrankenSQLite, graph, layout, and renderer pieces. A dependency-clean core library is only useful if its platform and facade path also satisfies the selected policy. Record any inherited system-ABI code separately from ordinary first-party engine code.

The closure gate distinguishes shipping runtime packages, compile-time generators/macros, development-only tools, and system frameworks. No runtime dependency gets reclassified as “tooling” simply because a build script bundles it into the binary.

### 26.3 Toolchain and optimization

Use a dated nightly Rust toolchain, edition 2024, and a pinned Apple SDK/Metal compiler configuration. Pin the nightly only after the required safe SIMD and ABI tests pass. Release builds use optimized code with a measured LTO/codegen configuration; performance builds retain symbols and profiling support.

Do not assume that a size-optimized profile inherited from a terminal library is optimal for this renderer. Use `opt-level=3` as the starting point for hot crates, then measure. Avoid `target-cpu=native` in a redistributable build unless the artifact clearly declares and enforces that exact CPU requirement. Prefer a broadly valid Apple Silicon baseline and explicitly qualified optional code paths.

### 26.4 Application bundle

Ship a normal `.app` containing the native executable, offline shader library, curated licensed font/visual assets, schema/capability versions, and license/provenance material. No Python, Node, browser engine, external terminal, or model download is required.

The binary is named `fcb`, built from `fcb-app` and using the public `fcb` library. It is not a CLI shim dependent on another installed executable. Provide a single-file native lane with required compiled shader/default font assets embedded, plus a `.app` distribution of the same engine for standard Mac integration. Optional large resources are optional in both routes; missing them cannot break baseline source/Markdown reading.

Installation and launch work from paths containing spaces and non-ASCII characters, arbitrary working directories, and no neighboring development checkout. Cache locations are user-private and explicitly excluded from source discovery. Library-only builds do not embed application assets or create cache locations unless their chosen features and host configuration request them.

### 26.5 Signing and updates

Provide signed/notarized distributions through the owner's chosen release process. Verify artifact integrity and version before activation. Updates preserve user state and can leave the old version available for rollback where compatible.

Do not overwrite a live source tree, silently launch privileged installers, or require a shell pipeline as the only installation route. A manual native bundle installation is sufficient for the first qualified release; an automatic updater is a separate security-sensitive component, not a prerequisite to fast browsing.

### 26.6 Build/release gates

Required checks include formatting/lints, no-unsafe authoritative crates, dependency-closure compliance, coherent suite/runtime pins, pure-core tests, parser/search/source invariants, native bridge safety tests, production-path visual tests, and named target-class performance/pressure qualification.

Use the exact supported feature matrix rather than `--all-features` as a substitute for understanding mutually exclusive or test-only sibling features. A library's unrelated experimental feature failing elsewhere is not silently enabled in the application to broaden a marketing claim.

### 26.7 Library packaging and source compatibility

Publish the `fcb` facade and only the independently useful subordinate crates. Keep all released component versions coherent and document required upstream revisions/features. Never rely on a sibling project's `[patch]` section being inherited by a downstream consumer; prove the external consumer resolves the intended graph. Root application suite pins and public dependency constraints are separate artifacts with separate tests.

Examples use only the supported facade or intentionally documented low-level modules. The standalone binary cannot hide missing public functionality behind private cross-crate imports. Preserve a concise migration guide for changing public contracts and a separate schema migration path for stored trails/anchors/layouts. Do not promise binary plugin ABI compatibility or dynamic library unloading; Rust source-level modularity is the required product surface.

Build evidence records normal dependencies, build dependencies/proc macros, selected target features, native framework links, shader compiler/SDK, and license provenance. Test-only third-party oracle tools may run outside the shipping closure, but cannot be compiled into shipped adapters under a misleading label. A renamed or copied outside library is not a new first-party implementation.

### 26.8 Single-file runtime versus distribution container

A standalone `fcb` executable can embed its runtime assets without requiring its downloadable distribution to be one bare Mach-O file. Apple issues notarization tickets for standalone binaries but does not support stapling directly to them or to ZIP archives. Select and qualify a supported stapled distribution container, such as a disk image or installer package, for offline installation; an app bundle has its own staple route. Do not promise identical offline Gatekeeper behavior for a bare downloaded binary merely because an online launch succeeded. [B17]

Test the final signed/notarized downloads on a clean Mac, including quarantine, online and offline first launch, extraction/installation and arbitrary working directories. Record which artifact and route passed. Do not clear quarantine or disable Gatekeeper to manufacture acceptance. G0 chooses the deployment floor, architecture baseline, signing identity/entitlement model and SDK availability policy; G7 proves the complete distribution. The floor is already bounded below by the display-link route selected in §15.1 (macOS 14 for `CAMetalDisplayLink`) and interacts with the sandbox decision in §8.9. The standalone runtime remains independent of a companion `.app`.

---

## 27. Upstream extension contracts

### 27.1 Shared ownership and landing discipline

All named projects are owned by the user. Cross-repository work is part of this implementation plan, not a request to persuade an unrelated vendor. Each extension names its actual home, API, dependency delta, upstream tests, and FCB consumer test. Improve the original component; do not maintain browser-local substitutes for reusable functionality.

For every extension, land the owner-repository implementation and tests first or in coordinated dependency order, then advance FCB to that committed revision. A development worktree may temporarily depend on a local change, but the accepted/released path must resolve committed compatible versions. Do not change broad default behavior or unrelated consumers just to make one FCB experiment easy.

### 27.2 U1: Asupersync desktop and embedding foundation

**Owner:** Asupersync. **Consumers:** `fcb-runtime`, standalone composition, and the qualified persistence adapter.

Provide a narrow compliant profile containing owned tasks/regions, needed channels and synchronization, explicit time/deadline semantics, cooperative cancellation, bounded admission, blocking-service integration, and deterministic tests. Separate unused networking/security/serialization integrations at both module and dependency levels. Replace only essential noncompliant primitives with real first-party/std-backed implementations while retaining semantics.

Expose host-supplied runtime/region integration and clearly document any dedicated worker-context requirement. Publish the limits of cancellation, poll/cost quotas, priority meet, and foreign operations. FCB's resource-byte ledger remains a separate application/host contract; Asupersync need not become a GUI allocator.

Acceptance: minimal native and embedded consumers, resolved graph, queue/byte-admission integration, cancellation/drain conservation, foreign-call handling, and no hidden global initialization. Maintenance priority tests must exercise the actual `Budget::meet` rule. [B6]

### 27.3 U2: FrankenMarkdown reusable lexical engine

**Owner:** FrankenMarkdown. **Consumers:** source readers, Markdown fences, search excerpts, exports, other Franken apps.

Implement language-state/checkpoint APIs, bounded scratch/work, explicit chunk-versus-EOF semantics, validated source correspondence, compact tiled spans, and per-language capability classification. All required lexer expansions and optimizations land here. Existing whole-block APIs retain a compatible implementation path using the shared engine.

Acceptance: full/chunked equivalence after coalescing spans, arbitrary split boundaries, malformed inputs, delimiter edits, convergence with unchanged suffix evidence, checkpoint version validation, complexity caps, and regression fixtures for existing FMD outputs. FCB owns scheduling and capture association, not tokenization.

### 27.4 U3: FrankenMarkdown document semantics, flow, and provenance

**Owner:** FrankenMarkdown, including root modules or its own appropriately factored workspace crates. **Consumer:** the thin `fcb-document` adapter.

The entire reusable Markdown pipeline belongs upstream: parsing/dialect fixes, nested inline/source provenance, references/heading IDs, block semantics, safe asset requests, reusable style rules, continuous-flow layout, text/math/diagram integration, tables/lists/code, renderer-neutral display output, reading order/accessibility semantics, incremental dependencies, and selection/copy maps. There is **no “until a second consumer exists” exception**.

The inspected top-level `SpannedDocument` is a starting point, not a complete inline map. Add a proper provenance representation without forcing per-frame AST cloning. Support many-to-many and generated/multi-source relationships truthfully. Keep print pagination and HTML emission distinct consumers of shared semantics; do not retrofit a browser engine into FMD. [B1]

Start with acyclic root modules `flow`, `display`, and `source_map` as proposed in §6; split later only when dependency direction is sound. Keep core steps host-neutral and synchronous-resumable. FCB supplies request scopes, resource responses, view constraints, and maps upstream output into its retained renderer. FMD does not depend on FCB, AppKit, a runtime, a filesystem, or a GPU in its base profile.

Acceptance: a FrankenMarkdown-owned headless flow consumer; nested provenance and asset-generation tests; giant/hostile document budgets; upstream HTML/PDF/font/math/core-WASM regressions; then FCB source/preview/selection with a committed upstream API. Completion requires both upstream behavior and a real consumer, not a public trait alone.

### 27.5 U4: FrankenMarkdown shared text/font/math and optional Mac adapter

**Owner:** `fmd-font`, `fmd-math`, and an optional `fmd-font-macos` component inside FrankenMarkdown. **System primitive owner:** the `franken-macos` crate at `native/macos/` in this repository (§6.2), delivered by FCB-003.

Improve immutable shared font backing, checked parsers, shaping/context APIs, glyph/cluster provenance, run metrics, font fallback identification, and reusable raster/display contracts upstream. The Mac adapter obtains owned platform results from the safe system bridge and returns the common run/raster representation. Base font/math engines remain platform-independent and unsafe-forbidden.

Qualification includes Latin/CJK/bidi/joining/combining text, ligatures, emoji/color/bitmap glyph routes, invalid fonts, native UTF-16 range conversion, bounded giant paragraphs, and exact logical-source selection. Different shaping routes state determinism and coverage honestly. A future clean-room shaping improvement replaces the implementation behind this upstream contract, not a parallel FCB-only font system.

### 27.6 U5: FrankenTUI non-terminal panes, focus, and wide virtualization

**Owner:** FrankenTUI. **Consumers:** FCB UI/document integration and other hosts.

Extract a supported dependency-clean non-terminal pane/focus transaction surface and a checked wide prefix/virtualization primitive. Preserve semantic input, reversible layouts, focus/announcements, versioning, and bounded retention. Avoid importing terminal presenters or cell-only geometry, and do not assume raw private module paths inherit the curated facade's stability contract. [B5]

Use checked fixed-point logical heights and explicit structural-edit behavior. Acceptance covers sums beyond 2³², invalid indexes, negative adjustments, inserted/deleted blocks, deterministic transactions, scale conversion, accessibility focus, and a resolved no-terminal profile. Refactor upstream dependencies as needed; empty default features alone do not remove the inspected unconditional dependencies. **Work package:** FCB-097, consumed by FCB-032 (heights) and FCB-050 (panes).

### 27.7 U6: FrankenNetworkX compact directed kernels

**Owner:** FrankenNetworkX. **Consumers:** FCB source-relationship and contextual-analysis services.

Factor checked immutable integer graph views including the actual outgoing/incoming CSR structure and selected reachability/SCC/condensation/path/topology kernels. Preserve deterministic tie-breaks and explicit directed/multiplicity semantics. Remove unrelated Python, random-generation, external matching, and alternate parallel runtime edges from the selected path. [B4]

Acceptance includes graph validation, limits/cooperative checkpoints, exact selected-kernel conformance, row-order stability, parallel-edge projection tests, and immutable generation association. Do not claim all algorithms were inspected simply because one oversized source file exists. A narrow complete set is better than a broad unqualified dependency.

### 27.8 U7: FrankenSQLite bounded persistence/host integration

**Owner:** FrankenSQLite. **Consumer:** optional `fcb-store` and the standalone app.

Qualify a clean native facade with the actual thread-affine connection model, compatible context/runtime ownership, bounded cache/query/result behavior, short-transaction semantics, and an inventoried native VFS boundary. Disable unused extensions and keep the required SQL subset explicit. Document rather than obscure any actor-local runtime instance. [B7]

Acceptance: external embedding with no global runtime takeover, cancellation after admission/commit, reader/writer policy, interrupted manifest publication, crash/reopen/migration, numeric/schema representation, bounded rows, and preservation of user state through index rebuild/clear. FCB owns process-store coordination and application schema; reusable engine/facade fixes land upstream.

### 27.9 U8: Retained rendering, handles, and atlas reuse

**Owners:** FrankenManim for retained/reference rendering primitives; FrankenThreeD for reusable typed handles; FrankenTerm for atlas/key/eviction policy.

Reuse independent revision axes, geometry/reference fixtures, compact stable handles, borrowed glyph-key lookups, packing/eviction decisions, and resource accounting. The native FCB Metal renderer is new work, not an import from an absent FrankenThreeD renderer. Atlas policy is not assumed to include the deferred native transfer implementation. [R9] [R10] [B3]

Add upstream support for owner-qualified handles where generally useful; FCB wraps session/device domains regardless. Separate raster identity from GPU residence. Completion-owned retirement and unified-memory accounting remain hard consumer requirements. Acceptance includes two independent arenas/devices, slot exhaustion, stale atlas references, style/camera invalidation, and actual native resource lifetimes.

### 27.10 U9: FrankenManim digest, canonical envelopes, and cache primitives

**Owner:** FrankenManim's `fmn-hash`/`fmn-cache` or a narrowly extracted first-party substrate maintained there.

Use the inspected safe SHA-256 and bounded canonical serialization rather than creating an unrelated FCB hashing implementation. Factor unnecessary `fmn-core` scientific/RNG edges downward where required. Keep schema/version, endianness, checksums, finite geometry validation, and complete-input keys explicit. [B8]

Reuse namespaced caches, pins, immutable entries, and ownership generations. Do not import synchronous disk work into the frame path, wall-clock lock takeover, or a stronger hostile-path guarantee than the provider supplies. FCB uses its native root authority and byte/disk quotas. Acceptance compares cache hits to recomputation, rejects wrong-key/invalid envelopes, preserves pins, and prevents cleared/retired handles from writing into a fresh namespace.

### 27.11 U10: CASS readiness/evidence selection and reciprocal embedding

**Owner:** CASS for reusable readiness, bounded evidence selection, and omission policy. **FCB owns:** source candidates, source trails, and navigation. **FrankenMarkdown owns:** reusable Markdown formatting/export improvements.

Generalize the inspected pack-planner concepts without importing CASS's app or its outside dependency graph. Preserve source readiness/freshness, deterministic bounded selection, explicit approximate token estimates, and missing/truncated evidence. Reading trails require no semantic model. [B2]

Acceptance: bounded source-context pack, exact anchor navigation, privacy-preserving defaults, deterministic omission ledger, FMD export integration, and an external consumer capable of using FCB as the granted-source preview. CASS UI integration beyond the stable source-view consumer is an optional rollout, not a hidden mandatory CASS dependency.

### 27.12 Extension ledger and scope control

Each ledger entry records owner repository, exact API proposal, current inspected status, committed implementation revision, selected features, dependency delta, upstream fixtures, FCB consumer evidence, and performance qualification. Existing implementation, correctly integrated route, and measured acceleration are different statuses.

The first goal is the real source/Markdown browsing experience. Generalize only what has a concrete consumer and policy/value benefit. Do not turn this plan into mandatory completion of all scientific, network, animation, or graphics features in the suite. Conversely, do not declare a reusable Markdown improvement “browser-specific” merely to avoid landing it in FrankenMarkdown.

---

## 28. Implementation phases and release gates

### 28.1 Sequencing principle

Work proceeds through end-to-end capability slices. Infrastructure is justified by the next real user interaction. Avoid spending a phase on dashboards, framework generalization, manifest ceremony, or dozens of empty crates while source reading remains absent.

All ordinary product features specified here remain in the release plan. City mode is optional for the user to turn on, not an excuse to replace its planned implementation with a screenshot. The same distinction applies to light/dark themes, multiple readers, and source/preview split mode.

### 28.2 Phase 0: retire architectural uncertainties

**Goal:** prove native, upstream, resource-ownership, and embeddable-library foundations before freezing high-level APIs. FCB-065–072 are foundation work, not release-end polish.

Required experiments:

- Resolve the exact desired shipping and isolated-library consumer closures; identify necessary narrow first-party refactors. Produce a minimal Asupersync desktop/embedding path with required semantics, not a blanket exception.
- Establish owner-qualified IDs, allocation/publication/completion leases, bounded retirement, and an inert public `fcb` facade. Exercise two headless instances and one host-owned close path.
- Define semantic accessibility nodes and source/UTF-16/visual range conversions before committing to reader storage or hit-test APIs.
- Create a native AppKit window and Metal surface through the proposed first-party safe bridge. Exercise input, resize, a display-link loop, and deterministic teardown.
- Draw real source text using `fmd-font`/qualified native fallback and the proposed GPU representation at physical 4K and reading-size typography. Measure warm/cold glyph behavior and selection geometry.
- Compile a coherent selected Asupersync/FrankenSQLite integration and verify connection/thread/cancellation behavior.
- Land an initial FrankenMarkdown-owned nested-provenance/flow experiment and its headless upstream consumer. Feed its committed public output into a small native FCB view with source hit testing. A generated HTML screenshot or FCB-local flow engine does not pass.
- Run a compact hierarchy/LOD/precision prototype over a real source tree and a synthetic large hierarchy, measuring visible traversal and coordinate accuracy.

**G0 passes when:** there is a committed, reproducible dependency/safety design; the required narrow runtime profile is implemented and passes focused host/owned-lifecycle tests; early byte/completion/retirement admission is enforced; the inert library and owner-qualified instances work; native window/text/Metal lifetimes and initial accessibility range contracts work; the key upstream-owned Markdown contracts are validated; and a strict-compliant foundation build is demonstrated. Remaining closure violations in components not yet integrated are explicitly tracked and block their admission. Missing runtime or native-boundary proof blocks G0 itself. No release can pass while any shipping closure violation remains.

A failed experiment changes the implementation strategy, not the user requirement. For example, if a chosen glyph technique fails small-text quality, switch technique; do not quietly remove syntax-highlighted reading.

### 28.3 Phase 1: real atlas-to-source loop

**Goal:** open a real root, progressively populate a stable atlas, select a real file, and read/copy exact source through the GPU UI.

Implement core identities, source snapshots, bounded discovery, basic stable layout, CPU culling/LOD, camera controls, main UI reducer, a reading lens, glyph resources, and lifecycle ownership. Use plain source before a missing lexer is ready; do not block reading on semantic analysis.

**G1:** a real repository can be opened/navigated/read with no synchronous filesystem/parse or bulk-destruction work in camera frames; source copy and a small real native accessibility/text route pass; hit testing matches accepted frame state; basic memory/queue admission works; window close does not orphan resources or shut down an embedding host; partial discovery is honest. The standalone shell and external view consumer use the same public library path.

### 28.4 Phase 2: syntax and immediate search

Implement resumable lexers for the first qualified language set, token themes, checkpoint invalidation, path search, literal search, result-to-source jumps, matching overlays, and generation-safe progressive results.

**G2:** full/chunked lexical equivalence passes; bounded ephemeral indexed search over closed captures matches exact reference scans, including supported text encodings; rapid query changes cannot display stale results; source updates invalidate only affected data; selected result identity survives refinement. Persistent publication/recovery is G4 work and is not a prerequisite to this gate.

### 28.5 Phase 3: native documentation

Complete FrankenMarkdown-owned native flow, nested provenance, paragraph/code/table/list layout, math, supported diagram/image semantics, safe asset requests, and bounded large-document behavior; integrate through the thin FCB document adapter for source/preview/split and exact navigation. All reusable fixes land upstream, including regressions discovered by the FCB consumer.

**G3:** real project READMEs and comprehensive plans render attractively in the native GPU path; nested source selection/navigation remain truthful; hostile documents respect budgets; the upstream headless FMD consumer and existing FMD output regressions pass; no WebView, TeX, external renderer, or FCB-local Markdown engine is required.

### 28.6 Phase 4: persistence and large-scale live workspaces

Complete user/derived-state separation, persisted navigation/layout, process-store ownership, crash-safe artifact publication/recovery, watcher reconciliation, out-of-core sources/indexes, and integrated pressure management. Build on resource admission already present since G0/G1. Integrate the strict-compliant FrankenSQLite profile with explicit host/worker context ownership.

**G4:** warm reopen restores meaningful state, index rebuild preserves annotations, source/root churn is correct, and standard/stress fixtures remain usable under the managed memory envelope.

### 28.7 Phase 5: relationships and multi-surface workflows

Implement qualified structural outlines and selected relationship analyses, documentation back-links, evidence badges, contextual graph overlays, multiple pinned readers, bookmarks/history, exact reading trails and explicit pack exports, captured-snapshot comparison, and refined navigation/focus behavior. CASS supplies reusable policy concepts, not a mandatory application dependency.

**G5:** every displayed semantic relationship identifies its evidence level; users can perform the core analysis workflows without losing source/place; heuristic links cannot be mistaken for complete compiler knowledge.

### 28.8 Phase 6: City view, accessibility, native polish

Complete retained-map extrusion/tilt/orbit, legends, clipping/picking, projection transitions, and the full accessibility/keyboard/IME/display/reduced-motion/contrast qualification matrix. This expands the semantic and native foundations already required in G0/G1; it is not the first implementation of accessibility or text input.

**G6:** the recorded 2D→code→search→City→2D interaction family is implemented with real content, and its essential information is also accessible through a nonspatial keyboard/screen-reader route.

### 28.9 Phase 7: performance qualification and packaging

Tune only with retained evidence. Run the full native M4/M5 hardware matrix, cold/warm/stress/pressure/idle traces, typography gallery, resource-lifetime tests, and dependency/source/license checks. Build and qualify both the direct standalone `fcb` executable and signed application bundle, plus independently built external library consumers; exercise install/update/reopen/rollback behavior where implemented.

**G7:** every advertised feature is a real qualified route; safety/closure/user-state requirements pass; named hardware targets have actual measurements; remaining limitations are explicit, not hidden behind a “complete” label.

### 28.10 Release authority

Functional completeness, memory safety, dependency compliance, visual quality, and performance qualification are independent gates. A fast prototype cannot waive dependency policy. A clean dependency tree cannot waive broken selection. A pretty screenshot cannot waive actual native integration.

Avoid an arbitrary schedule promised before G0. The largest uncertainty is not rectangle drawing; it is satisfying strict dependency closure and a sound native boundary while integrating real text, source semantics, and lifecycle behavior.

### 28.11 Critical-path priorities after this review

Do not defer allocation leases, instance identity, frame/interactivity coherence, upstream ownership, or basic accessibility until later feature polish. They determine the shape of every subsystem. The revised work graph introduces small foundation packages for these contracts; later packages validate their complete integration under scale.

Proceed in parallel where dependencies permit: native bridge/text and upstream FMD flow; source/capture/search primitives and headless embedding; clean runtime/storage profiles and deterministic resource tests. Do not serialize all work behind a full ecosystem dependency purge, but do not label a noncompliant prototype a compliant shipping build. Each admitted production slice has an exact dependency/safety gate.

More elaborate accelerators, statistical controllers, optional external semantic/Git providers, and actual reciprocal CASS UI rollout remain separately qualified enhancements. The core library, native source/Markdown views, recorded atlas/City workflow, and bounded reading-trail/export surface are included in this plan's full product acceptance.

---

## 29. Implementation work packages

The IDs below are ready to translate into the project's issue/bead system. They are **planned work**, not claims that issues were created or tasks completed. Dependencies name concrete packages, not calendar dates. Each completion requires its production-path test or retained evidence.

### 29.1 Foundation and native execution

| ID | Deliverable | Dependencies | Completion evidence |
|---|---|---|---|
| FCB-001 | Pin reviewed suite inputs and inspect exact selected package graph. | None | Machine-readable graph identifies all normal/build packages and incompatible runtime pins. |
| FCB-002 | Asupersync dependency-clean desktop profile and bounded-runtime sample. | FCB-001 | Native sample has compliant closure and passes region/channel/cancel tests. |
| FCB-003 | First-party macOS object/ABI ownership kernel under `native/macos/` in this repository. | FCB-001 | Signature/ownership ledger plus retain/release/reentry/lifetime tests; consumable by FCB and `fmd-font-macos` without either hosting it. |
| FCB-004 | Native window, event conversion, resize, and close. | FCB-003 | Real AppKit lifecycle and input smoke tests with no late callback use. |
| FCB-005 | Safe Metal resource/upload/submission layer. | FCB-003, FCB-065, FCB-068 | Real GPU round-trip; owned upload/submission leases, cross-owner and stale handles rejected. |
| FCB-006 | Display-link integration and bounded frame ownership. | FCB-004, FCB-005, FCB-070, FCB-072 | Single drawable owner, verified callback route, native pacing/teardown; no event-thread completion wait. |
| FCB-007 | Core typed IDs, checked ranges, snapshots, limits, errors. | FCB-001 | Round-trip, exhaustion, overflow, and stale-publication unit tests. |
| FCB-008 | Standalone composition using the inert public facade and explicitly owned runtime. | FCB-002, FCB-004, FCB-007, FCB-065, FCB-066, FCB-068, FCB-070, FCB-094 | Integrated native app uses a single qualified runtime and clean closure. |

### 29.2 Source, atlas, and reader

| ID | Deliverable | Dependencies | Completion evidence |
|---|---|---|---|
| FCB-009 | Root read capabilities, native permission lifecycle and raw-path identities. | FCB-003, FCB-007, FCB-067, FCB-068 | Symlink/rename/case/raw-byte corpus; unavailable/stale native grants and revocation during reads. |
| FCB-010 | Bounded directory discovery and ignore matcher. | FCB-002, FCB-009, FCB-065, FCB-066 | Large/deep tree gives progressive output with descriptor/queue limits. |
| FCB-011 | Immutable source chunks and observed-snapshot contract. | FCB-007, FCB-009, FCB-065, FCB-067 | Concurrent replacement/truncation tests without mutable-source mmap. |
| FCB-012 | Sparse line index, exact byte/line translation and capture encoding maps. | FCB-011 | Huge-line/chunk/CRLF/UTF-8, UTF-16 BOM decoding-map and far-jump fixtures. |
| FCB-013 | Stable retained partition-tree layout. | FCB-007, FCB-010, FCB-065 | Containment, deterministic stable generation, provisional discovery, local insertion/displacement tests. |
| FCB-014 | Spatial hierarchy query and LOD admission. | FCB-013 | Visible-cost traces independent of total hidden leaves; threshold hysteresis. |
| FCB-015 | Camera math, pointer anchoring, focus/back navigation. | FCB-004, FCB-014 | Deep-zoom precision and deterministic interaction replay. |
| FCB-016 | FrankenMarkdown-owned shared text/font/run contract and native-route interface. | FCB-001, FCB-003, FCB-007, FCB-065 | Upstream contract plus initial font/cluster/source mapping and ownership fixtures. |
| FCB-017 | Glyph atlas, raster queues, safe resource retirement. | FCB-005, FCB-016, FCB-065, FCB-072, FCB-075 | Separate raster/residency keys, owned atlas slots, completion-safe churn and real pixel tests. |
| FCB-018 | Retained rectangle/glyph/clip renderer. | FCB-005, FCB-014, FCB-017, FCB-071, FCB-072 | Real atlas and source text on native GPU, correct ordering/clipping. |
| FCB-019 | Reading lens, horizontal virtualization, exact selection/copy. | FCB-012, FCB-016, FCB-018, FCB-065, FCB-071, FCB-082 | Real bounded source reading; exact copy and declared general-text context/fallback semantics. |
| FCB-020 | UI reducer, sidebar, command routing, focus model. | FCB-007, FCB-015, FCB-019, FCB-066, FCB-069, FCB-071 | Public-library open→atlas→source→back; coherent selection, bounded event/retirement work. |

### 29.3 Syntax, search, and language facts

| ID | Deliverable | Dependencies | Completion evidence |
|---|---|---|---|
| FCB-021 | FrankenMarkdown-owned resumable lexer and explicit EOF/checkpoint interface. | FCB-001, FCB-011, FCB-065 | Whole/chunked equivalence and exact byte tiling. |
| FCB-022 | FrankenMarkdown language-state expansions and qualified lexical capabilities. | FCB-021 | Rust/JS/Python/shell/YAML and remaining release-language adversarial corpus. |
| FCB-023 | Upstream FMD checkpoint convergence plus FCB bounded request/publication integration. | FCB-002, FCB-021, FCB-065, FCB-066, FCB-068 | External edits converge correctly without UI stalls or stale spans. |
| FCB-024 | Shared upstream token/theme outputs in FCB readers and excerpts. | FCB-018, FCB-019, FCB-023 | Same source tokenization across surfaces; recolor does not re-lex. |
| FCB-025 | Path search index and stable fuzzy ranking. | FCB-007, FCB-010 | Exact/prefix/fuzzy results with distinct case-sensitive identities. |
| FCB-026 | Bounded exact source scan and query scopes. | FCB-011, FCB-012 | Reference corpus including cross-chunk/short queries and decoded UTF-16 text versus raw-byte semantics. |
| FCB-027 | Ephemeral immutable substring candidate segments and exact verification. | FCB-026, FCB-065, FCB-085 | G2 bounded in-memory closed-manifest index; no database dependency or false negatives in admitted semantics. |
| FCB-028 | Query-generation streams, cancellation, completeness UI. | FCB-002, FCB-025, FCB-026, FCB-066 | Rapid query changes, protected selection, truthful partial results. |
| FCB-029 | Result navigation and compact spatial match overlays. | FCB-014, FCB-024, FCB-028 | Exact range landing and bounded million-match aggregation. |
| FCB-030 | Source-specific structural outlines and evidence-class schema; reusable lexer fixes upstream. | FCB-007, FCB-022 | Language-scoped facts with source spans; no heuristic-as-exact claims. |

### 29.4 Markdown and text quality

| ID | Deliverable | Dependencies | Completion evidence |
|---|---|---|---|
| FCB-031 | Consume upstream FMD nested document/flow APIs through thin fcb-document integration. | FCB-011, FCB-016, FCB-073, FCB-074 | Committed upstream headless flow API drives native FCB output; no local Markdown engine. |
| FCB-032 | FrankenMarkdown-owned paragraph/list/quote/heading flow and checked height contracts. | FCB-031, FCB-065, FCB-074, FCB-097 | Resize, long paragraph, deep list, totals above 2³² fixtures. |
| FCB-033 | FrankenMarkdown-owned code-fence and large/wide table layout. | FCB-024, FCB-032 | Independent scrolling, row virtualization, exact code copy. |
| FCB-034 | FrankenMarkdown math/diagram display output plus FCB renderer mapping. | FCB-018, FCB-031 | fmd-math/diagram fixtures rendered natively with source anchors. |
| FCB-035 | FMD asset semantics, FCB capability I/O, and admitted first-party image decoders. | FCB-009, FCB-005, FCB-031, FCB-065 | Upstream asset-request semantics; confined bounded decoding and stale asset-response tests. |
| FCB-036 | FCB source/preview/split panels consuming upstream selection/provenance. | FCB-019, FCB-032, FCB-033, FCB-073 | Anchor synchronization and rendered-text versus Markdown-copy semantics. |
| FCB-037 | FMD incremental document dependencies and FCB source-anchored viewport reflow. | FCB-023, FCB-031, FCB-036 | Distant reference edits and height refinement preserve visible anchor. |
| FCB-038 | Shared upstream typography and FCB actual-scale theme/gallery qualification. | FCB-016, FCB-017, FCB-036 | Actual-scale light/dark/high-contrast and mixed-script gallery. |

### 29.5 Persistence, live state, and scale

| ID | Deliverable | Dependencies | Completion evidence |
|---|---|---|---|
| FCB-039 | Qualified FrankenSQLite profile and runtime adapter. | FCB-001, FCB-002, FCB-065, FCB-066 | Clean selected closure; explicit !Send worker/runtime/context ownership and commit/cancel tests. |
| FCB-040 | Separate user and derived schemas with migrations. | FCB-007, FCB-039 | Upgrade/reopen and annotation-preservation tests. |
| FCB-041 | Durable bookmark/history/window-state service. | FCB-020, FCB-040, FCB-088 | Crash/reopen, exact anchor reattachment or preserved stale state; no note loss. |
| FCB-042 | Atomic artifact generation publication and GC. | FCB-011, FCB-040, FCB-065, FCB-080, FCB-081, FCB-087 | Manifest-last publication, crash/retry identities, pins, protected GC and quotas. |
| FCB-043 | Native watcher hints and reconciliation. | FCB-003, FCB-010, FCB-042, FCB-083 | Scan epochs, overflow/root loss/rename storms, special objects and self-cache exclusion. |
| FCB-044 | Source/index out-of-core paging and large-workspace snapshots. | FCB-012, FCB-027, FCB-042, FCB-065, FCB-082, FCB-086 | Stress corpus navigable inside tracked budgets. |
| FCB-045 | Integrated memory-pressure qualification on the existing foundation ledger. | FCB-017, FCB-037, FCB-039, FCB-044, FCB-065, FCB-072, FCB-081 | Peak/retirement/in-flight overlap, unified-memory accounting, protected reclamation and reader behavior. |
| FCB-046 | Integrated interactive/maintenance fairness and bounded delivery qualification. | FCB-008, FCB-028, FCB-043, FCB-045, FCB-065, FCB-094 | Indexing under camera/search load does not starve either interaction or progress. |

### 29.6 Analysis, City view, and native polish

| ID | Deliverable | Dependencies | Completion evidence |
|---|---|---|---|
| FCB-047 | Dependency-clean checked FrankenNetworkX directed views and selected kernels. | FCB-001, FCB-007, FCB-065 | Qualified graph corpus and deterministic/cancelable outputs. |
| FCB-048 | Source relationship extraction and compact graph snapshots. | FCB-030, FCB-047 | Evidence-separated directed CSR; parallel multiplicity/provenance preserved; exact generation invalidation. |
| FCB-049 | Contextual graph overlays and Inspector relationships. | FCB-020, FCB-048 | Bounded edges, linear list equivalent, no fake semantic completeness. |
| FCB-050 | Pinned multi-reader workflow and pane transactions. | FCB-019, FCB-036, FCB-041, FCB-066, FCB-079, FCB-097 | Shared source memory, focus correctness, reversible arrangements. |
| FCB-051 | City extrusion, projection transitions, metric legends. | FCB-015, FCB-018, FCB-065, FCB-071, FCB-084 | Same map positions/selection retained across 2D/City transitions. |
| FCB-052 | City picking, clip/depth/order, safe optional visual quality. | FCB-005, FCB-051 | Correct selection without synchronous GPU readback; pressure degradation. |
| FCB-053 | Complete native text input, IME, clipboard, menu and open/drop qualification. | FCB-004, FCB-016, FCB-020, FCB-069, FCB-070, FCB-077 | Real native behavior and composition tests. |
| FCB-054 | Full native accessibility/virtualized outline and reader qualification. | FCB-019, FCB-020, FCB-032, FCB-053, FCB-069, FCB-077 | Screen-reader and keyboard-only end-to-end workflows. |
| FCB-055 | Reduced motion, display migration, sleep/resume, appearance. | FCB-006, FCB-038, FCB-052, FCB-054 | Native lifecycle/contrast/resolution matrix. |
| FCB-056 | Captured-source snapshot comparison. | FCB-011, FCB-041, FCB-049 | Exact bounded diffs with no implied unsupported Git history. |

### 29.7 Qualification and delivery

| ID | Deliverable | Dependencies | Completion evidence |
|---|---|---|---|
| FCB-057 | Versioned standalone CLI/robot service schemas, public-facade capabilities. | FCB-008, FCB-028, FCB-039, FCB-066, FCB-087 | Real services, inert capabilities, single-file launcher behavior, lossless raw paths/full-width IDs, bounded JSON and explicit stream terminal/backpressure behavior. |
| FCB-058 | Bounded telemetry/HUD and semantic replay. | FCB-020, FCB-046, FCB-057, FCB-094 | Reproducible trace with source-redaction and overhead reports. |
| FCB-059 | Source/parser/asset hostile corpus and regression minimizer. | FCB-022, FCB-035, FCB-043 | Reproducible seeds, budgets, minimized failure fixtures. |
| FCB-060 | GPU/native resource-lifetime and failure injection suite. | FCB-005, FCB-006, FCB-052, FCB-055, FCB-070, FCB-071, FCB-072, FCB-079, FCB-089 | Close/device/pressure/callback races preserve resource ownership. |
| FCB-061 | CPU/GPU visual and source-semantic comparison suite. | FCB-038, FCB-049, FCB-054, FCB-071, FCB-076, FCB-089 | Qualified content/layout/pixel evidence by rendering route. |
| FCB-062 | Real M4/M5 cold/warm/pressure/idle qualification. | FCB-044, FCB-045, FCB-058, FCB-060, FCB-061, FCB-063, FCB-095 | Named hardware traces and honest SLO results, including missed frames. |
| FCB-063 | Real standalone fcb binary, optional .app, embedded assets, signing/install. | FCB-008, FCB-053, FCB-057, FCB-066, FCB-080 | Direct executable/bundle and quarantined online/offline distribution launches; supported stapling route; licensed embedded resources; no companion app or checkout. |
| FCB-064 | Full-product release acceptance across app, library and upstream owners. | FCB-050, FCB-056, FCB-059, FCB-062, FCB-063, FCB-076, FCB-078, FCB-079, FCB-080, FCB-081, FCB-084, FCB-085, FCB-086, FCB-088, FCB-089, FCB-091, FCB-092, FCB-093, FCB-095, FCB-096 | All mandatory work reachable; functional/ownership/safety/closure/visual/hardware gates assessed without vacuous passes. |

### 29.8 Review-driven foundation and integration packages

IDs 065 onward are additions from this review, **not later scheduling priority**. In particular 065–072 and 094 precede dependent native/source work. Dependencies below are delivery dependencies, not instructions for an upstream repository to import FCB types. All reusable FMD tasks are owned and tested upstream.

| ID | Deliverable | Dependencies | Completion evidence |
|---|---|---|---|
| FCB-065 | Early managed-byte, queue and peak-overlap admission leases. | FCB-007 | G0 refusal/conservation tests; allocations charged by capacity; protected completion/reclamation capacity. |
| FCB-066 | Inert public fcb facade, additive component profiles, and API/lifecycle contracts. | FCB-007 | Independent tiny consumer constructs without I/O, threads, global state, GUI, DB or runtime. |
| FCB-067 | Host source-provider and complete/extent-capture interface. | FCB-007, FCB-065, FCB-066 | In-memory and native-provider boundaries validate ranges, ownership and capture guarantees. |
| FCB-068 | Owner-qualified instance/arena/device identities and epoch validation. | FCB-007 | Two independent owners with equal slot/generation cannot alias; exhaustion cannot resurrect handles. |
| FCB-069 | Early semantic accessibility, focus and source/native range vocabulary. | FCB-007, FCB-068 | UTF-8/UTF-16/grapheme/visual domains and sentinel/error cases specified and tested. |
| FCB-070 | Safe native embedding and host-owned loop/device/presentation binding. | FCB-003, FCB-004, FCB-005, FCB-066, FCB-068 | Attach/detach does not own host globals; one drawable owner; device/thread tokens validated. |
| FCB-071 | Immutable FramePlan with coherent hit-test/source/accessibility snapshot. | FCB-007, FCB-065, FCB-068, FCB-069 | Queued/presented-frame mismatches and resize races cannot activate different visible source. |
| FCB-072 | Early CPU retirement and lossless GPU terminal-completion ownership. | FCB-005, FCB-065, FCB-068 | Full ordinary queues cannot lose a completion; bounded retirement prevents UI destruction stalls. |
| FCB-073 | FrankenMarkdown nested source provenance and compatible spanned APIs. | FCB-001, FCB-065 | Escapes/entities/fences/generated/multi-source maps; no per-frame AST cloning or invented bijection. |
| FCB-074 | FrankenMarkdown resumable flow/display API and upstream headless consumer. | FCB-016, FCB-065, FCB-073 | No FCB dependency; bounded real document flow, resource requests, deterministic semantic fixtures. |
| FCB-075 | FrankenMarkdown font/context/raster extensions and optional Mac adapter. | FCB-003, FCB-016, FCB-065 | Owned fallback faces, color glyphs, bounded general-text context, no native dependencies in base engine. |
| FCB-076 | Upstream FrankenMarkdown compatibility and independent-consumer gate. | FCB-022, FCB-033, FCB-034, FCB-035, FCB-037, FCB-073, FCB-074, FCB-075 | Relevant HTML/PDF/font/math/core-WASM regressions plus FCB consuming committed public APIs. |
| FCB-077 | Early real native source-reader accessibility and keyboard/IME smoke path. | FCB-004, FCB-012, FCB-016, FCB-019, FCB-069, FCB-070 | G1 VoiceOver/text-range/focus smoke using actual source and host responder chain. |
| FCB-078 | Isolated headless source/search/map external library consumers. | FCB-011, FCB-025, FCB-026, FCB-066 | Builds outside workspace feature graph; no GUI/DB/native asset startup; real provider/query behavior. |
| FCB-079 | Two-instance native embedding and independent host/session shutdown tests. | FCB-019, FCB-020, FCB-070, FCB-071, FCB-072, FCB-077 | One view closes during work; other view, host loop, device and runtime remain valid. |
| FCB-080 | Upstream reusable digest/canonical-envelope factoring and conformance. | FCB-001 | Inspected fmn-hash primitives selected with actual closure; bounded serialization and digest vectors. |
| FCB-081 | Owned cache namespaces, pins, revocation and protected reclamation primitives. | FCB-009, FCB-065, FCB-068, FCB-080 | Wrong-root/retired writes rejected; no wall-clock liveness assumption; cold/hot equality tests. |
| FCB-082 | Old-capture anchor resolution and qualified huge-line visual-context routes. | FCB-011, FCB-012, FCB-075 | No live-byte substitution under old captures; bidi/tabs/combining pathology stays bounded and truthful. |
| FCB-083 | Scan epochs, special-object admission, self-cache exclusion and continuity rules. | FCB-009, FCB-010, FCB-011 | Partial scan never deletes unseen files; FIFO/symlink races, traversal cycles/aliases, path display controls and atomic saves handled. |
| FCB-084 | Retained semantic summary pyramids and precision-safe focus islands. | FCB-013, FCB-014, FCB-023 | Palette change avoids re-lex; bounded LOD work, deep hierarchy does not underflow into invisible parcels. |
| FCB-085 | Closed search manifests, normalization maps and completeness semantics. | FCB-011, FCB-026, FCB-028 | Live-tree changes, unknown membership, normalized/short queries and exact counts distinguished. |
| FCB-086 | Persistent index encoding/publication, disk quota and bounded merge/paging contracts. | FCB-027, FCB-042, FCB-065, FCB-085 | G4 persistent segments match ephemeral/reference search; crash publication and high-entropy quotas preserve uncovered direct-scan semantics. |
| FCB-087 | Explicit multi-process store ownership and authenticated local protocol. | FCB-003, FCB-008, FCB-039, FCB-040, FCB-066 | GUI/CLI attach, owner death and schema negotiation without active-writer theft or root escalation. |
| FCB-088 | User-state migration, full-width IDs and uncertain-commit recovery. | FCB-040, FCB-042, FCB-080, FCB-087 | Signed-integer boundaries, manifest crash points, duplicate retry and backup preservation tested. |
| FCB-089 | Color/alpha/clip/depth and host-target shader ABI qualification. | FCB-005, FCB-017, FCB-018, FCB-070, FCB-071 | Real GPU/CPU fixtures; no double premultiply/gamma, incorrect clip, or invalid target reuse. |
| FCB-090 | CASS-owned reusable bounded evidence/readiness selection policy. | FCB-001, FCB-066, FCB-080 | Narrow first-party extraction, deterministic omissions, explicit non-exact token estimates. |
| FCB-091 | FCB reading trails, documentation back-links and provenance navigation. | FCB-029, FCB-036, FCB-041, FCB-048, FCB-090 | Exact captured anchors; evidence labels; stable map/reader navigation with bounded pack selection. |
| FCB-092 | Explicit source-pack export with upstream Markdown formatting and privacy. | FCB-036, FCB-080, FCB-088, FCB-091 | Destination/consent, exact byte budgets, omission ledger, no silent disclosure or command execution. |
| FCB-093 | Resolved library/app closure and feature-leak external-consumer matrix. | FCB-001, FCB-002, FCB-039, FCB-047, FCB-066, FCB-078, FCB-079, FCB-080, FCB-097 | Additive profiles, actual unification/patch behavior, no accidental native/runtime/dependency leakage. |
| FCB-094 | Early bounded tracing, admission fairness and clock-domain fixtures. | FCB-007, FCB-065, FCB-068 | G0/G1 evidence for wake/coalescing, priority meet, deadline conversion and zero per-glyph logging. |
| FCB-095 | Expanded review-driven ownership/concurrency/hostile regression matrix. | FCB-059, FCB-060, FCB-077, FCB-079, FCB-081, FCB-082, FCB-083, FCB-085, FCB-086, FCB-087, FCB-088, FCB-089 | Every §25.9 defect class exercised with production routes and bounded reproducible artifacts. |
| FCB-096 | Joint standalone/library/upstream delivery contract acceptance. | FCB-057, FCB-063, FCB-076, FCB-079, FCB-093 | Real standalone executable, app bundle, independent embedding, committed FMD APIs and exact features. |
| FCB-097 | FrankenTUI-owned non-terminal pane/focus facade and checked wide-prefix height primitive (U5). | FCB-001, FCB-007, FCB-065 | Upstream no-terminal profile with resolved clean closure; sums beyond 2³², invalid-index, negative-adjustment and structural-edit fixtures; FCB consumers in FCB-032/050. |

### 29.9 Immediate implementation order

Start FCB-001/002/003/007, then the inert facade, owner IDs, byte leases, coherent frame model and completion/retirement ownership in FCB-065–072/094 as their dependencies become available. In parallel, build the real native font/text route and **upstream** FrankenMarkdown provenance/flow in FCB-073–075. These are contracts and production-path experiments, not a phase of empty framework crates.

The first complete user loop is FCB-020 with the early accessibility and embedding proofs: discover a real file, locate it spatially, read/copy it, and return. Prioritize shared highlighting/search and upstream native Markdown next. Persistence/out-of-core work proceeds without blocking ephemeral reading; strict-compliant production admission still applies to each selected component.

The added reading-trail and summary work compounds existing source/search/graph primitives; it does not introduce a second application or a model requirement. More speculative compute/adaptive/provider integrations remain outside the mandatory work graph until separately justified.

Do not close a work package with a trait, stub return, screenshot, or test that never reaches the real implementation. A cross-repo task is complete only when the upstream commit and public FCB consumer both satisfy the row's evidence. The release package depends on every mandatory package transitively. `scripts/check_plan_graph.py` checks dependency references, cycles, release reachability, headless-lane isolation, citations, anchors and numbering; run it after every edit to this plan.

---

## 30. Risks, rejected approaches, and decision rules

### 30.1 Major risks

| Risk | Why it matters | Mitigation and decision gate |
|---|---|---|
| Strict transitive dependency policy requires more upstream work than expected. | The currently inspected manifests are not already compliant. | Gate exact closure early; factor only required paths; never hide outside packages or relax policy silently. |
| Handwritten native ABI is unsound or expands into a general framework. | Memory safety depends on correct ownership/threading/signatures. | Narrow safe API, per-call ledger, pinned SDK, lifecycle tests; reject generic pointer escape. |
| Text is fast but visually inferior. | A code browser is primarily a reading tool. | Real-size typography gallery before committing to a glyph technique; keep crisp reading route. |
| General language semantics are overpromised. | Lexical color and name matching do not prove references. | Capability/evidence ladder; language-scoped parsers; explicit heuristic labels. |
| Stable map degenerates after many updates. | Spatial memory conflicts with optimal packing. | Local repair with displacement budgets; explicit restorable global repack. |
| Unified-memory peaks exceed steady targets. | Generations, uploads, and reflows overlap. | Reserve peak leases before construction; global accounting; out-of-core algorithms. |
| Background work produces tail latency. | Average throughput can hide bad interaction stalls. | Priority/admission limits; bounded quanta; latest-result publication; native traces. |
| Markdown adapter accidentally becomes a browser engine. | General CSS/HTML and scripting violate scope and simplicity. | Native document semantics, restricted inert HTML policy, explicit unsupported cases. |
| Sibling feature/API/version drift breaks integration. | Reviewed repositories evolve independently. | Coherent suite pins, consumer tests, extension ledger; no moving branch releases. |
| CPU reference and GPU implementations drift. | Optimizations can change clipping, ordering, or text mapping. | Shared display-list semantics, source invariants, bounded visual comparisons. |
| Accessibility is deferred until custom rendering is entrenched. | Retrofitting source/text ranges becomes expensive. | Semantic IDs/source maps from the beginning; native accessibility gate before release. |
| Device failure or foreign blocking breaks shutdown. | GPU and OS work is not universally cancelable. | Completion-owned leases, async drain, typed degraded states, no premature frees. |

### 30.2 Rejected defaults

**Embedding a WebView or Electron.** It would provide document layout quickly but adds a different renderer/runtime and abandons the native dependency/performance design. The core product is not an HTML app wrapped in a window.

**Importing the whole FrankenTerm GUI.** It brings a terminal product model and substantial third-party closure. Extract the pieces with independent value.

**Waiting for all of FrankenThreeD.** The inspected native-relevant renderer does not exist. Basic source GUI drawing must be owned and scheduled here; reuse the implemented handles and design insights now.

**GPU everything.** GPU parsing, graph layout, and search are not automatically faster after data transfer, synchronization, and result consumption. Use GPU acceleration where it actually improves the user-visible pipeline.

**Full repository draw lists every frame.** This converts total repository size into interaction cost and defeats semantic zoom. Retain structure and query a bounded visible hierarchy.

**Mutable-source mmap and unchecked SIMD.** Neither is necessary to make visible-source reading fast. They expand safety risk before a measured bottleneck justifies even considering a different owned-data strategy.

**A fixed 120 Hz busy loop.** It wastes power and does not establish smooth presentation. Use display timing and demand-driven invalidation.

**A full SQL/database dependency on the frame path.** Persistence must not become a global synchronization point for a camera or caret.

**A force-directed layout as the primary map.** Global rearrangement destroys spatial memory and makes exact source location unnecessarily unstable.

**Universal compiler semantics from a highlighter.** This is a correctness failure, not a feature shortcut.

**Neural search as a startup requirement.** It introduces model acquisition, memory, and latency before the basic path/text/heading workflows are complete. Progressive enrichment remains possible later through a separately qualified first-party provider.

**Premature adaptive controllers and process ceremony.** Add them only when a measured failure of simpler bounded rules demands them. Feature completion and real-source workflows take priority over impressive internal terminology.

### 30.3 Decision rules for hard tradeoffs

When speed conflicts with exact source content, preserve content and reduce optional detail. When spatial beauty conflicts with readability, open the reading lens. When packing efficiency conflicts with established location, preserve location until an explicit repack. When a claimed semantic fact lacks evidence, downgrade the claim rather than manufacture certainty.

When a dependency fails policy, factor/replace the necessary implementation or leave that path blocked. When a new optimization exceeds peak memory, reject it even if its steady state looks good. When a feature passes in simulation but not on the native host, report only the simulated result.

### 30.4 Review findings and their corrected contracts

This table records substantive changes already integrated above; it is not an alternative specification that leaves the old behavior in force.

| Finding | Correction in this revision | Primary design/test location |
|---|---|---|
| Product was application-centric, not a real reusable library. | Public inert `fcb` facade, isolated features, host-owned lifecycle, external consumers. | §§6, 24, 26; FCB-066/070/078/079/096. |
| Standalone CLI could have been only a companion-app shim. | Actual `fcb` executable plus optional bundle; embedded-resource single-file lane. | §§24, 26; FCB-063/096. |
| Reusable Markdown flow was assigned downstream until another consumer appeared. | Entire reusable pipeline is owned by FrankenMarkdown immediately. | §§6, 12, 27; FCB-031–038/073–076. |
| Spanned type names were stronger evidence than their actual implementation. | Explicit nested source maps; top-level wrappers and cloning API limitations recorded. | §§4.3, 12; FCB-073. |
| Slot/generation identifiers could alias across embedded instances. | Owner-qualified arena/device/instance identity and cross-owner validation. | §§7, 14; FCB-068/079. |
| UI model could lead presented pixels during asynchronous updates. | Immutable frame plus matching interaction/a11y snapshot, explicit presentation association. | §§7.6, 15; FCB-071/089. |
| Last-reference destruction could stall an otherwise async frame. | Budgeted off-UI retirement and protected reclaim progress. | §§7.7, 20; FCB-072. |
| GPU terminal completions could be treated like droppable camera events. | Reserved lossless terminal records; wake coalescing never discards ownership state. | §14.11; FCB-072/095. |
| Resource accounting was sequenced after most allocating subsystems. | Managed byte/queue/peak leases begin in G0/G1. | §§16, 20, 28; FCB-065. |
| Runtime budget could be confused with heap or wall-clock enforcement. | Separate scheduling, engine-step and byte budgets; real priority/deadline semantics. | §16.7; FCB-065/094. |
| “One runtime” could hide actor-local runtime construction. | Single compatible foundation, explicitly owned/injected instances and worker contexts. | §§3.4, 16.9, 19; FCB-039/087. |
| Default-feature suppression could be mistaken for transitive closure isolation. | Additive Cargo matrix and independent downstream consumers/actual graphs. | §§3.2, 6.4, 26; FCB-093. |
| Whole-file source identity could be inferred from unrelated captured ranges. | Complete versus extent captures and immutable backing validation. | §10.7; FCB-067/082. |
| Huge-line virtualization implied context-free exact text shaping. | Qualified ASCII fast path and bounded paragraph/context preparation or labeled fallback. | §10.9; FCB-075/082. |
| Byte, scalar, grapheme, UTF-16 and visual offsets risked conflation. | Separate typed domains, provenance conversions and sentinel tests. | §§10, 13, 23; FCB-012/069/073/077. |
| A Fenwick tree could be read as a general logarithmic dynamic sequence. | Checked wide fixed-point heights with paged structural edits. | §§4.9, 10.10, 27.6; FCB-097/032. |
| Partial directory scans could imply deletions. | Successful reconciliation epochs and dirty-hint accounting. | §8.6; FCB-083. |
| Source enumeration omitted FIFOs/devices and self-generated cache loops. | Opened-object validation, bounded native admission, cache identity exclusion. | §8.6; FCB-083. |
| Deterministic layout could depend on unordered streamed arrival or changed estimates. | Stable committed ordering/weights, provisional aggregates and explicit repack. | §§8.7, 9; FCB-013/084. |
| `f64` local coordinates alone were not an unlimited deep-zoom guarantee. | Focus-island promotion before finite precision/underflow limits. | §9.9; FCB-084. |
| One atlas generation in every raster key could flush unrelated glyph work. | Immutable raster keys separate from GPU page/slot residence. | §13.4; FCB-017. |
| Unified-memory tier names could imply nonexistent pressure relief. | Measure actual released allocations; avoid duplicate CPU/GPU demotion copies. | §§4.5, 20.8; FCB-045. |
| Shaped glyph IDs alone did not implement color/bitmap fallback fonts. | Owned fallback-font and qualified raster resources through upstream native adapter. | §13.7; FCB-075. |
| Color/alpha/clip/depth semantics were under-specified. | Explicit SDR linear/premultiplied pipeline, host-target and actual GPU fixtures. | §14.10; FCB-089. |
| Search completeness could be asserted over a changing/open universe. | Closed source manifest, independent coverage/count/truncation semantics. | §17.8; FCB-085. |
| Exact verification could not repair incompatible prefilter false negatives. | Matching normalization/version and complete-scan fallback universe. | §§17.3, 17.8; FCB-085/086. |
| Index storage could amplify without a source-size-related bound. | Posting/scratch/disk/merge quotas and explicit uncovered route. | §17.9; FCB-086. |
| Graph projections could lose multiplicity or mix evidence levels. | Separate structural CSR, provenance/count tables and projection cache identities. | §18.7; FCB-047/048. |
| Persistence actor did not establish multi-process writer ownership. | Explicit store owner/IPC and OS-backed liveness, no heartbeat-only lock theft. | §19.8; FCB-087. |
| Full-width IDs and publication/uncertain commit details were missing. | Defined database encodings, artifact-before-manifest ordering and operation recovery. | §§19.9–19.10; FCB-088. |
| Cache checksum/pinning could imply authorization or stronger path safety. | Native root authority, generation revocation, checksum/security distinctions. | §22.7; FCB-080/081. |
| Accessibility appeared too late in phase ordering. | G0 semantic/range contracts and G1 real reader path, then full polish. | §§23, 28; FCB-069/077. |
| Missing capability rows could yield a vacuous “complete” report. | Profile-specific required registry with independent evidence dimensions. | §24.6; FCB-057/096. |
| Several suite primitives were overlooked or only described abstractly. | Actual bidirectional CSR, evidence-pack policy, digest/envelope/cache and pane facade added. | §§4, 17–19, 27; B2/B4/B5/B8. |

### 30.5 Deliberately bounded enhancements

The useful accretions are source-summary pyramids, precision-safe focus islands, content-keyed shared work, reading trails, source-context export, documentation back-links, compact directed graph views, and modular embedding. Each compounds an existing product surface and has a specific work package.

RaptorQ repair, neural search, general Git/LSP providers, full 3D scene-engine compatibility, arbitrary shader execution, full dynamic plugin loading, and speculative statistical controllers are not required dependencies of this source browser. Existing suite code can inform a later explicitly scoped addition; it does not justify adding an unrelated subsystem now. CPU/GPU correctness and readable source retain priority over fashionable mechanisms.

---

## 31. Requirement traceability and completion checklist

### 31.1 User requirements mapped to implementation

| Requirement | Design coverage | Principal work/evidence |
|---|---|---|
| Study and reflect the supplied UI. | §§2, 5, 9, 14–15. | FCB-013–020, FCB-029, FCB-051–052; real 2D/code/search/City replay. |
| Memory-safe Rust. | §§3, 7, 10, 14, 22, 26. | FCB-003–008, FCB-059–060; authoritative forbid-unsafe and native safety audit. |
| No outside libraries beyond allowed Franken foundations. | §§3.2–3.3, 4, 26–27. | FCB-001–002, FCB-039, FCB-047, FCB-064; complete resolved shipping closure. |
| Asupersync foundation. | §§6, 16, 19, 27.2. | FCB-002, FCB-008, FCB-039, FCB-046; scoped ownership/cancel tests. |
| Hyperoptimized GPU GUI. | §§9, 13–15, 20–21. | FCB-005–006, FCB-014, FCB-017–018, FCB-058–062; actual native metrics. |
| Late-model M4/M5 Macs with 24 GB or more. | §§20–21, 23, 25–26. | FCB-045, FCB-055, FCB-062; named physical hardware/display matrix. |
| Syntax highlighting. | §§10–11, 13. | FCB-021–024; exact-span and chunk-equivalence corpus. |
| Markdown rendering. | §§12–13. | FCB-031–038; native document and source/preview qualification. |
| Study every named repository. | §§4 and 32. | Explicit nine-repository findings, selected source evidence, and reuse boundaries. |
| Comprehensive plan like FrankenMarkdown's. | This document. | Product, architecture, subsystems, performance, safety, work graph, gates, and provenance. |
| Rename to FrankenCodeBrowser / `fcb`. | Header, §§1, 6, 24, 26. | Canonical executable/library names and revised work IDs. |
| Real standalone binary and modular Rust library. | §§6, 24–26, 28. | FCB-066/070/078/079/093/096; isolated consumers and direct executable. |
| All reusable Markdown improvements implemented upstream. | §§4.3, 6, 11–13, 27.3–27.5. | FCB-021–024/031–038/073–076; FMD-owned implementation and independent consumer. |
| Thorough fresh review and useful additional suite reuse. | §§4, 7–30, 32.5. | Review findings in §30.4 and §§32.7–32.8; new source ledger; 97-package acyclic dependency graph. |

### 31.2 Release checklist

The boxes are deliberately unchecked. They express future acceptance, not work completed while writing this plan.

- [ ] A real source tree opens into a progressively populated atlas and exact reading view.
- [ ] `fcb` works as an actual standalone executable and through the optional `.app`, without a companion installation.
- [ ] The public `fcb` library is inert by default and independently usable headlessly or inside a host-owned native view.
- [ ] Closing one embedded instance leaves other instances and host-owned resources operational.
- [ ] Every reusable Markdown/highlighting/flow/font/provenance improvement lives upstream in FrankenMarkdown and passes upstream plus consumer tests.
- [ ] Frame pixels, hit testing and accessibility geometry use compatible accepted snapshots.
- [ ] Memory admission, lossless completion and bounded off-UI retirement are enforced from the first production slice.
- [ ] 2D, readable source, search navigation, City mode, and return-to-place all use real content.
- [ ] Syntax classification is shared, source-preserving, incremental, and capability-qualified.
- [ ] Markdown is rendered natively with source maps, math/code/tables/assets, and safe fallbacks.
- [ ] The shipping dependency graph satisfies the strict first-party rule, including transitive edges.
- [ ] All authoritative application crates forbid unsafe code; every native/inherited boundary is named and audited.
- [ ] Asupersync is the sole qualified async foundation; every runtime/worker context is explicit, compatible, and owned by the proper host/service.
- [ ] No file/DB/parser/compiler/driver wait sits on ordinary input/camera processing.
- [ ] Generations prevent stale query, source, atlas, and GPU handle substitution.
- [ ] User annotations survive index rebuilds, crashes, and cache cleanup.
- [ ] Out-of-core behavior and peak memory admission are tested on the 24 GB target class.
- [ ] Text selection, IME, screen-reader access, keyboard workflows, and display changes are qualified.
- [ ] Native performance reports identify measured hardware, displays, corpus, cache state, and failures.
- [ ] The application bundle runs without external Python/Node/browser/model/terminal prerequisites.
- [ ] No planned subsystem, simulated backend, or target number is presented as an implemented result.

### 31.3 Central architectural conclusion

The highest-leverage design is not to make one enormous scene draw faster. It is to ensure that a user gesture almost never asks the machine to redo work it already knows the answer to.

The hierarchy is retained. Source bytes are versioned. Lexical context is checkpointed. Document blocks are cached. Glyph resources are shared. Search results publish by generation. GPU resources have explicit lifetimes. Camera motion changes projection and visibility, not the meaning of the source.

That combination is how this becomes a genuinely fast source browser rather than a visually impressive benchmark that stalls as soon as the user opens a real repository.

---

## 32. Research provenance and source ledger

### 32.1 Scope and interpretation

Review date: **September 12, 2026**. All nine user-specified repositories were accessed through GitHub. Selected file contents and manifests informed the concrete findings above. No application implementation, source build, exhaustive repository audit, native Mac benchmark, or dependency-closure compilation was performed as part of producing this document.

The supplied video was inspected using extracted frames across its duration. Its pixels establish the observed interaction family, not hidden implementation details or performance.

The reference FrankenMarkdown plan was read in full. Other entries below are full manifest reads or indicated source/README ranges. The blob hashes identify the reviewed file content returned by GitHub; **they are not repository commit hashes**. Default-branch links can evolve. Implementation must resolve a coherent set of commit pins separately.

One attempted read of `franken_networkx/crates/fnx-algorithms/src/lib.rs` returned no content because the source was too large for the available file route; a follow-up raw-file route also failed. Accordingly, this plan bases its concrete graph implementation observations on the inspected graph-class source and algorithm manifest, not on a claimed reading of that oversized algorithm file. Other repository findings are similarly limited to the identified reviewed material.

### 32.2 Primary repository references

**[R1] FrankenMarkdown planning precedent.**  
[Comprehensive plan](https://github.com/Dicklesworthstone/franken_markdown/blob/main/docs/planning/COMPREHENSIVE_PLAN_FOR_FRANKEN_MARKDOWN.md). Reviewed the whole document. Blob: `1cfb1e02488a83a5ab8cc38beb574ca4596a0d4e`. Used for product/subsystem/performance/phase structure, not as a current implementation census.

**[R2] FrankenMarkdown implementation and dependencies.**

| Reviewed file | Range | Blob SHA |
|---|---|---|
| [Cargo.toml](https://github.com/Dicklesworthstone/franken_markdown/blob/main/Cargo.toml) | Whole file | `4998fd4c1f1b98fdf1e5510a678041a4a4d5295a` |
| [src/lib.rs](https://github.com/Dicklesworthstone/franken_markdown/blob/main/src/lib.rs) | 1–220 | `920aadf93c6281fe83ed9a7f030dabe10834394f` |
| [src/highlight.rs](https://github.com/Dicklesworthstone/franken_markdown/blob/main/src/highlight.rs) | 1–160 | `4b936f7730aab8cf3e7372d16cdf401c13d6c026` |
| [fmd-font/src/lib.rs](https://github.com/Dicklesworthstone/franken_markdown/blob/main/fmd-font/src/lib.rs) | 1–130 | `f53ae489a2c067e4d64eaacc254485c9a6e8bab0` |

Key supported findings: no-default library separation from CLI dependencies; existing source-span/AST modules; reusable exact-byte highlighting; missing resumable language state; Latin-first font scope and remaining broader shaping/CFF work.

**[R3] FrankenTerm rendering ideas and closure costs.**

| Reviewed file | Range | Blob SHA |
|---|---|---|
| [README.md](https://github.com/Dicklesworthstone/frankenterm/blob/main/README.md) | 1–210 | `b202532f1ee351212852a62a698d9dea8f8d72eb` |
| [crates/frankenterm-gui/Cargo.toml](https://github.com/Dicklesworthstone/frankenterm/blob/main/crates/frankenterm-gui/Cargo.toml) | 1–180 | `e237363e1fde21cc7fe8f0c001f64297212d27fe` |
| [crates/frankenterm-gui/src/glyphcache.rs](https://github.com/Dicklesworthstone/frankenterm/blob/main/crates/frankenterm-gui/src/glyphcache.rs) | 1–150 | `8bb8c956640ac35537c2f291ce375cb877e07453` |

Key supported findings: composite/borrowed glyph cache keys; atlas-budget integration references; rendering/lifecycle test surfaces in the manifest; large noncompliant whole-GUI dependency closure. Search excerpts also identified atlas packing/telemetry and tiered-swap documentation, but those entire files were not audited.

**[R4] FrankenNetworkX graph representation and dependencies.**

| Reviewed file | Range | Blob SHA |
|---|---|---|
| [README.md](https://github.com/Dicklesworthstone/franken_networkx/blob/main/README.md) | Opening section through graph/catalog material; output tail truncated | Not used as a pinned implementation claim |
| [Cargo.toml](https://github.com/Dicklesworthstone/franken_networkx/blob/main/Cargo.toml) | Whole file | `eed8e33843d5f31a497d551aa7b77d12fb72756e` |
| [crates/fnx-algorithms/Cargo.toml](https://github.com/Dicklesworthstone/franken_networkx/blob/main/crates/fnx-algorithms/Cargo.toml) | Whole file | `8aa8ae17558f651b2fe7869e716909aa24c1b8c8` |
| [crates/fnx-classes/src/lib.rs](https://github.com/Dicklesworthstone/franken_networkx/blob/main/crates/fnx-classes/src/lib.rs) | 1–170 | `ac201a1c6004c192e65b2f7261e516e6b1073bfa` |

Key supported findings: integer adjacency, revision-keyed derived caches, clone/cache identity care, external dependencies in the general algorithms path. Selected graph algorithms still require exact-kernel qualification; the oversized algorithm source was not read successfully.

**[R5] CASS progressive search and publication discipline.**

| Reviewed file | Range | Blob SHA |
|---|---|---|
| [README.md](https://github.com/Dicklesworthstone/coding_agent_session_search/blob/main/README.md) | 1–170 | `e148a536dd3d8915fe88a8b3c724b35fcca1229e` |
| [Cargo.toml](https://github.com/Dicklesworthstone/coding_agent_session_search/blob/main/Cargo.toml) | 1–180 | `dadcd1471a22b912c731cc82445eda065476c46f` |
| [src/search/mod.rs](https://github.com/Dicklesworthstone/coding_agent_session_search/blob/main/src/search/mod.rs) | Returned module facade | `eea65d7c5f962639e14c7fc005c6f19c273720a5` |
| [src/search/two_tier_search.rs](https://github.com/Dicklesworthstone/coding_agent_session_search/blob/main/src/search/two_tier_search.rs) | 1–150 | `6d9ae843dbead25888b53f2c2cafddf8a6e91aca` |

Key supported findings: explicit initial/refined/failure phases; canonical-versus-derived asset policy; exact pinned sibling versions and significant whole-application dependency cost. Historical module names and illustrative timing comments were not treated as independent current implementation/performance proof.

**[R6] Asupersync runtime and dependency reality.**

| Reviewed file | Range | Blob SHA |
|---|---|---|
| [README.md](https://github.com/Dicklesworthstone/asupersync/blob/main/README.md) | 1–190 | `ba9436494c7ceb9d7cb5b3c5bf30b61ab7c60eb1` |
| [Cargo.toml](https://github.com/Dicklesworthstone/asupersync/blob/main/Cargo.toml) | 1–230 and 420–620 | `2245088e54e263273e50d557a3e3cd9716f62800` |
| [src/runtime/builder.rs](https://github.com/Dicklesworthstone/asupersync/blob/main/src/runtime/builder.rs) | 1–140 | `2357fe2e30514a1b8a95a6daf7255b9e44805507` |

Key supported findings: cooperative scope/cancellation semantics, explicit blocking-pool and queue defaults, native runtime host distinction, substantial unconditional outside dependencies. No existing fully dependency-clean desktop profile was established by this review.

**[R7] FrankenSQLite facade, thread ownership, and cancellation.**

| Reviewed file | Range | Blob SHA |
|---|---|---|
| [README.md](https://github.com/Dicklesworthstone/frankensqlite/blob/main/README.md) | 1–150 | `a660ca94cac08db19884563ff1760c8839654595` |
| [crates/fsqlite/Cargo.toml](https://github.com/Dicklesworthstone/frankensqlite/blob/main/crates/fsqlite/Cargo.toml) | Whole file | `ee78cd896ae6873ddd73a10ddc041f183c1bbce6` |
| [crates/fsqlite/src/lib.rs](https://github.com/Dicklesworthstone/frankensqlite/blob/main/crates/fsqlite/src/lib.rs) | 1–190 | `b5f9a2edefebbd5e02fd8a224c8bab94dce0534e` |
| [crates/fsqlite/src/async_api.rs](https://github.com/Dicklesworthstone/frankensqlite/blob/main/crates/fsqlite/src/async_api.rs) | 1–170 | `44ba8949d51d9d418dcdd6c0e91004cd212cc188` |

Key supported findings: `!Send` connection, dedicated-worker async facade, distinct native/FrankenSQLite context types, cancellation after command admission, qualified safe-core versus VFS/C-ABI boundaries. No assumption that every broad database feature is wired and qualified for this application.

**[R8] FrankenTUI non-terminal behavior and height arithmetic.**

| Reviewed file | Range | Blob SHA |
|---|---|---|
| [README.md](https://github.com/Dicklesworthstone/frankentui/blob/main/README.md) | 1–180 | `2a3ae8463dd09b49d9bc89e874009542d1f7f38d` |
| [Cargo.toml](https://github.com/Dicklesworthstone/frankentui/blob/main/Cargo.toml) | Returned workspace/profile section | `e6aee77f2524f51c4e0c478126160f2e34316584` |
| [crates/ftui-widgets/src/fenwick.rs](https://github.com/Dicklesworthstone/frankentui/blob/main/crates/ftui-widgets/src/fenwick.rs) | 1–190 | `68c10c37d2b221fffeb4d3261283a9b462b68b46` |

Key supported findings: useful pane/focus/virtualization design surfaces; concrete contiguous prefix-sum implementation with wrapping `u32` arithmetic that must be adapted for the proposed wide pixel-height domain.

**[R9] FrankenManim retained renderer versus target-state prose.**

| Reviewed file | Range | Blob SHA |
|---|---|---|
| [README.md](https://github.com/Dicklesworthstone/franken_manim/blob/main/README.md) | 1–180 | `59b3c35cc2467e98d3fe3aa9479cba3b114bdbca` |
| [Cargo.toml](https://github.com/Dicklesworthstone/franken_manim/blob/main/Cargo.toml) | Returned workspace/dependency/profile section | `1b90465fd46e0d142d1b81734deeb4e09d9f7869` |
| [crates/fmn-render/src/lib.rs](https://github.com/Dicklesworthstone/franken_manim/blob/main/crates/fmn-render/src/lib.rs) | Returned renderer facade | `7aa8c10540f392b5b79464e6b56c3204bd5b71d9` |
| [crates/fmn-render/src/revision.rs](https://github.com/Dicklesworthstone/franken_manim/blob/main/crates/fmn-render/src/revision.rs) | 1–160 | `6d51f7f0cbea6c2624af58ecaacfc09e5b7fe471` |

Key supported findings: actual retained render IR modules, independent invalidation axes, source-backed cache discipline, and explicit README warning that broad target-state claims must not be mistaken for completed qualification.

**[R10] FrankenThreeD implemented core and absent renderer.**

| Reviewed file | Range | Blob SHA |
|---|---|---|
| [README.md](https://github.com/Dicklesworthstone/franken_threed/blob/main/README.md) | 1–170 | `2c583e396f99412b7562185e4a013a78d6c91d09` |
| [Cargo.toml](https://github.com/Dicklesworthstone/franken_threed/blob/main/Cargo.toml) | Whole file | `5f0431fda369fc2b8af7cfa5ba47fd3d99d26dff` |
| [crates/f3d-core/src/lib.rs](https://github.com/Dicklesworthstone/franken_threed/blob/main/crates/f3d-core/src/lib.rs) | 1–200 | `85d771b7b2a03573e195a52b425cfee9b1fdd60b` |
| [crates/f3d-core/Cargo.toml](https://github.com/Dicklesworthstone/franken_threed/blob/main/crates/f3d-core/Cargo.toml) | Whole file | `61d17a0d073d62419ad8c11ffd4caf525546f3bc` |

Key supported findings: real typed/generational handles and device-generation tests, optional serde dependency profile in the core, and explicit current absence of the WebGPU renderer/compiler. No physical-device speedup is borrowed from this repository.

### 32.3 Platform references

**[A1] Apple Metal.** [Metal overview](https://developer.apple.com/metal/) and [MTLDevice documentation](https://developer.apple.com/documentation/metal/mtldevice). Used for the basic native GPU execution boundary, not a speedup claim.

**[A2] Apple resource/storage documentation.** [Choosing a resource storage mode for Apple GPUs](https://developer.apple.com/documentation/metal/choosing-a-resource-storage-mode-for-apple-gpus), [shared storage](https://developer.apple.com/documentation/metal/mtlstoragemode/shared), and [recommendedMaxWorkingSetSize](https://developer.apple.com/documentation/metal/mtldevice/recommendedmaxworkingsetsize). The documentation pages were located; some full page bodies required JavaScript and their Markdown route was not retrievable through the research tool. Exact availability and operational details must therefore be verified against the pinned SDK in G0. No fixed hardware-memory fraction or unsupported synchronization guarantee is asserted here.

**[A3] Apple display timing.** [CAMetalDisplayLink](https://developer.apple.com/documentation/quartzcore/cametaldisplaylink), [preferredFrameRateRange](https://developer.apple.com/documentation/quartzcore/cametaldisplaylink/preferredframeraterange), [preferredFrameLatency](https://developer.apple.com/documentation/quartzcore/cametaldisplaylink/preferredframelatency), and [Optimize for variable refresh rate displays](https://developer.apple.com/videos/play/wwdc2021/10147/). Used to choose display-synchronized, capability-qualified pacing, not to promise a particular refresh rate on every Mac/display combination.

**[A4] Rust native boundary requirements.** [Rust 2024 unsafe extern blocks](https://doc.rust-lang.org/edition-guide/rust-2024/unsafe-extern.html) and [Rustonomicon FFI](https://doc.rust-lang.org/nomicon/ffi.html). Used for the explicit ABI/signature/safety boundary. Safe application callers do not eliminate the binding implementation's safety obligations.

### 32.4 What has and has not been delivered

The earlier planning pass delivered the UI study, nine-repository review, architecture and its original work graph. This revision replaces that graph with 96 dependency-linked packages (97 after R4 added FCB-097), adds the standalone/library and strict upstream-ownership contracts, and corrects the issues summarized in §30.4. The fresh source-review record is in §32.5.

Not delivered or claimed: a running FrankenCodeBrowser binary, a new native bridge, committed upstream refactors, a compliant resolved application dependency graph, GitHub repository modifications, measured M4/M5 performance, or completed release qualification.

### 32.5 Fresh-review sources and limitations

The original 32.2 ledger is retained as the earlier research record, not silently updated into an exhaustive current audit. This revision re-read the complete plan and returned to every named repository for targeted source/contract review. The table below records newly inspected files and ranges. SHA values are **file blob hashes**, not repository commits or a coherent multi-repo lockfile. Default-branch URLs remain navigational; the recorded content identity is the research evidence.

| Ref | Repository and inspected source | Range / returned content | Blob SHA and finding |
|---|---|---|---|
| B1 | [FrankenMarkdown `src/span.rs`](https://github.com/Dicklesworthstone/franken_markdown/blob/main/src/span.rs) | Requested 1–200; complete returned file. | `fbbf2b4b557f9c0cf31d6dcca5ba84a1576d440c` — top-level spanned blocks, unchecked public span construction with safe slicing, cloning `to_document`; no full nested inline provenance established. |
| B2 | [CASS `src/search/pack_planner.rs`](https://github.com/Dicklesworthstone/coding_agent_session_search/blob/main/src/search/pack_planner.rs) | 1–220. | `8da40fafe3a838dba5854817ed6aacf50cb5d8ea` — bounded evidence/readiness/freshness model, heuristic token estimator and outside imports; not a clean drop-in library. |
| B3 | [FrankenTerm `atlas_tiered_swap.rs`](https://github.com/Dicklesworthstone/frankenterm/blob/main/crates/frankenterm-core/src/atlas_tiered_swap.rs) | 1–180. | `30d8bfd31893e2db3092b6c8b7dda5d798e850ee` — policy substrate with explicit deferred blit/I/O/native integration; adapt its tier model for unified memory. |
| B4 | [FrankenNetworkX `digraph.rs`](https://github.com/Dicklesworthstone/franken_networkx/blob/main/crates/fnx-classes/src/digraph.rs) | 1–145. | `4d0ca61a72179d0b07be4aacdb9122299de3a06a` — real bidirectional CSR arrays and multiplicity-insensitive projection; selected full algorithm bodies not re-audited. |
| B5 | [FrankenTUI pane stability contract](https://github.com/Dicklesworthstone/frankentui/blob/main/docs/api/pane-stability-contract.md) | 1–170. | `a780cfe3361615aac40b25e663d6fe7787eedb15` — curated pane facade, transactions, focus, semantic input and versioning; document version prose is not package-version proof. |
| B5 | [FrankenTUI `ftui-layout/Cargo.toml`](https://github.com/Dicklesworthstone/frankentui/blob/main/crates/ftui-layout/Cargo.toml) | Whole file. | `336209340d107f7df7aa681d386f67be320b4840` — 0.7.0 with unconditional outside dependencies despite empty defaults. |
| B6 | [Asupersync `src/types/budget.rs`](https://github.com/Dicklesworthstone/asupersync/blob/main/src/types/budget.rs) | 1–170. | `d744bc9fab2b9581cc7102baf4f041524759a29c` — scheduling budget dimensions, priority-max meet, absolute/relative deadline distinction; not heap admission. |
| B7 | [FrankenSQLite `async_api.rs`](https://github.com/Dicklesworthstone/frankensqlite/blob/main/crates/fsqlite/src/async_api.rs) | 1–95, refreshed. | `44ba8949d51d9d418dcdd6c0e91004cd212cc188` — actor ownership and post-admission cancellation/terminal outcome; internal runtime construction still requires integration audit. |
| B8 | [FrankenManim `fmn-cache/src/lib.rs`](https://github.com/Dicklesworthstone/franken_manim/blob/main/crates/fmn-cache/src/lib.rs) | 1–155. | `606117fe24fec75529031e966a9f161cd427fd3c` — canonical cache/pin/generation concepts with explicit same-user path-race and maintenance-lock caveats. |
| B8 | [FrankenManim `fmn-hash/src/lib.rs`](https://github.com/Dicklesworthstone/franken_manim/blob/main/crates/fmn-hash/src/lib.rs) | Complete returned facade. | `4054db21d37ceebeff59bf6da2647b2945301529` — in-house SHA-256 and bounded canonical serialization facade; full implementation not independently cryptographically audited here. |
| B8 | [FrankenManim `fmn-hash/Cargo.toml`](https://github.com/Dicklesworthstone/franken_manim/blob/main/crates/fmn-hash/Cargo.toml) | Whole file. | `11676aa05043d638e612a9e6f752531c3b12a860` — dependency on `fmn-core`. |
| B8 | [FrankenManim `fmn-core/Cargo.toml`](https://github.com/Dicklesworthstone/franken_manim/blob/main/crates/fmn-core/Cargo.toml) | Whole file. | `e96128c1e023381503793ce0b57e0db623102950` — deterministic-math/random-core edges require actual closure review or factoring. |
| B9 | [FrankenThreeD `handle.rs`](https://github.com/Dicklesworthstone/franken_threed/blob/main/crates/f3d-core/src/handle.rs) | 1–160. | `af6b434038487bff18cf1eddd0071d4788552a47` — basic typed slot/generation handles have no embedded arena-owner identity. |

Additional search excerpts identified existing incremental run shaping in FMD, the CASS planner's performance surfaces, and FrankenTUI pane integration. Excerpts were used to select files, not to assert unseen implementation or measured FCB performance. The NetworkX source directory inspection showed a roughly 3.34 MB algorithm `lib.rs`; its then-reported blob was `78b14b9a0d46ffb5ec85743fb52f4ef42b252ef6`. Metadata does not establish that its whole body was read.

**[B10] Cargo dependency/feature semantics.** [Cargo Book: Features](https://doc.rust-lang.org/cargo/reference/features.html). Used for additive features, feature union, and why one `default-features = false` edge does not guarantee isolation. The feature profile and external-consumer designs are FCB proposals, not statements that Cargo implements FCB policy automatically.

**[B11] Text context.** [Unicode UAX #9](https://www.unicode.org/reports/tr9/) and [UAX #29](https://www.unicode.org/reports/tr29/). Used for paragraph/direction context and grapheme boundaries; exact Unicode data/algorithm revisions are pinned in the future implementation. No universal constant-time visual random-access result is asserted.

**[B12] Native pacing.** [Apple CAMetalDisplayLink](https://developer.apple.com/documentation/quartzcore/cametaldisplaylink) and [run-loop registration](https://developer.apple.com/documentation/quartzcore/cametaldisplaylink/add%28to%3Aformode%3A%29). Used to avoid inventing callback/loop ownership and guaranteed refresh. Some documentation bodies require JavaScript; exact SDK availability, update/drawable ownership and callback behavior remain explicit G0 native proof obligations, not silently assumed verified here.

**[B13] Fallible allocation scope.** [Rust `Vec` documentation](https://doc.rust-lang.org/std/vec/struct.Vec.html), especially `try_reserve`/`try_reserve_exact` and capacity behavior. Used to distinguish controlled allocation admission from a promise that all OS/framework/allocator failure is recoverable.

### 32.6 R2 revision completion statement (historical)

This revision delivers the fully revised **FrankenCodeBrowser (`fcb`)** plan, its standalone/library design, mandatory upstream Markdown ownership, corrected identity/source/rendering/search/persistence/resource contracts, additional source-grounded reuse, and a mechanically checked 96-package work graph. It retains the full original product scope while replacing conflicting old ownership and lifecycle rules in place.

No running browser, compiled public API, upstream implementation commit, repository mutation, strict-compliant build closure, Mac GPU result, or native performance qualification is claimed. Structural document checks validate references, naming and work dependencies; they do not prove the future implementation's correctness. All code/API sketches and unimplemented names remain explicitly proposed.

### 32.7 R3 review: delivery and boundary corrections

The September 12, 2026 follow-up review checked the 96-package graph and the newly created repository documentation. The original graph was acyclic, with no missing dependency references and all packages reachable from FCB-064. Its scheduling defect was subtler: G2 indexed search depended through FCB-027 → FCB-042 → FCB-040 → FCB-039 on later persistent-store work. R3 separates the bounded ephemeral index from G4 persistent integration while preserving the full release scope.

R3 also specifies native root-grant restoration/revocation, traversal cycles and safe path labels, decoded-text versus raw-byte search, full-width wire numbers and raw paths, response framing/backpressure, export accounting, and standalone notarization containers. Small counterexamples demonstrated why UTF-8 byte matching misses UTF-16 text, lossy path conversion cannot round-trip arbitrary Unix bytes, and binary64 number consumers can merge adjacent large IDs. These are specification counterexamples, not executions of an FCB implementation.

The original source/blob ledger remains historical. This pass did not rebuild or re-audit every sibling implementation. New primary references checked for the boundary corrections are:

- **[B14] Apple root-access lifecycle:** [resolving bookmark data](https://developer.apple.com/documentation/foundation/nsurl/urlbyresolvingbookmarkdata%3Aoptions%3Arelativetourl%3Abookmarkdataisstale%3Aerror%3A) and [accessing a security-scoped resource](https://developer.apple.com/documentation/foundation/url/startaccessingsecurityscopedresource%28%29). These establish explicit resolution/staleness and access lifecycle; the selected FCB sandbox/entitlement route still needs G0 native qualification.
- **[B15] Native path representation:** [Rust std::ffi](https://doc.rust-lang.org/std/ffi/) and [OsStr::as_encoded_bytes](https://doc.rust-lang.org/std/ffi/struct.OsStr.html#method.as_encoded_bytes). Native strings are not necessarily UTF-8; the unspecified encoded representation is not a portable wire format. FCB's tagged reversible payload is a proposed schema choice.
- **[B16] JSON interoperability:** [RFC 8259, §§6–8](https://www.rfc-editor.org/rfc/rfc8259.html). JSON number precision and Unicode interoperability motivate explicit full-width string fields and lossless path payloads; versioned framing is FCB policy.
- **[B17] Native distribution:** [Apple's custom notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow) and [packaging Mac software](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution). Standalone binaries receive tickets but cannot be directly stapled; distribution-container and offline-launch claims require their own tests.

The public repository and supporting documentation now exist. This remains a specification revision: no runtime, native bridge, upstream integration, compliant compiled closure or performance result has been delivered by this review. All product gates remain pending.

### 32.8 R4 review: headless sequencing, bridge ownership, and gate ordering

A second September 12, 2026 follow-up re-read the whole R3 plan and re-ran the structural checks. The graph was still acyclic and fully reachable, but R3's claim that the FCB-027 chain was the only scheduling defect was incomplete. FCB-010, FCB-023 and FCB-028 depended on FCB-008, the standalone composition, which transitively required FCB-004, FCB-005 and FCB-070. Discovery, layout, path search, the ephemeral index, closed manifests and the headless consumers of FCB-078 therefore all waited on the native window and Metal layer, contradicting §6.4 and §28.11. R4 points those packages at FCB-002 and FCB-066 instead, moves FCB-025 off the UI reducer, and moves capture encoding maps from FCB-082 into FCB-012 so FCB-026 no longer depends on font shaping through FCB-075.

R4 originally gave `franken-macos` a separate repository home (§§3.1, 6.2, 27.5). The user's 2026-09-22 one-repository decision supersedes that packaging choice: its source now lives under `native/macos/` here. R4 also added FCB-097 for the FrankenTUI extraction that §27.6 required without a package, removed the inverted FCB-062 → FCB-096 edge, defined the root granted by `fcb open FILE` (§24.1), recorded the cross-cutting consequences of the sandbox decision (§8.9), tied the deployment floor to the display-link route (§§15.1, 26.8), named the explicit owned-runtime route for hosts without Asupersync (§16.9), and replaced the unbacked “mechanically checked” statement with the repository script `scripts/check_plan_graph.py`. That script checks the document, not any implementation. Product-gate status still depends on executed qualification.

**End of plan.**
