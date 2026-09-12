# FrankenCodeBrowser architecture

This is a design summary of the [comprehensive plan](COMPREHENSIVE_PLAN_FOR_FRANKEN_CODE_BROWSER.md),
especially §§3, 6–20 and 27. All component names and APIs below are proposed. No engine or native
application is implemented at bootstrap.

## Product boundary

FrankenCodeBrowser is a native macOS source browser and a modular Rust library. Its atlas is a
stable spatial representation of directories and files. Its reader exposes exact captured source.
Search, Markdown, graph facts, history and bookmarks share those source identities.

The standalone app creates its event loop, runtime and default storage. An embedding host supplies
or explicitly delegates those resources. Pure components operate on host-provided data without
filesystem authority. The native view uses AppKit and Metal through a narrow first-party bridge;
there is no web browser engine in the application.

## Proposed components

| Component | Owns | Must not own |
|---|---|---|
| `fcb` | Curated public facade and capability vocabulary | Implicit startup or global state |
| `fcb-core` | Owner-qualified IDs, checked ranges, limits, errors, snapshots | OS GUI, database, runtime |
| `fcb-source` | Providers, immutable complete/extent captures, decoding maps, sparse line indexes | Implicit filesystem grants |
| `fcb-analysis` | Source-specific outlines, facts and analysis coordination | A copied lexer family |
| `fcb-map` | Stable partition layout, camera, culling, LOD, summaries | Source truth or parsing |
| `fcb-document` | Lens state, source translation, granted assets, FMD output integration | Generic Markdown parsing, flow or typesetting |
| `fcb-search` | Source/path queries, candidate indexes, exact verification, coverage | A second source revision model |
| `fcb-ui` | Deterministic reducer, focus, selection, panels, immutable frame plans | I/O or GPU waits |
| `fcb-render` | Batching and an explicitly enabled native Metal adapter | Source discovery or host device takeover |
| `fcb-runtime` | Asupersync scopes, requests, budgets, publication | Hidden runtime construction |
| `fcb-store` | Optional FrankenSQLite actor, schema and manifests | Paint-time database queries |
| `fcb-app` | CLI and Mac composition; binary named `fcb` | Private engine functionality unavailable to hosts |
| `fcb-conformance` | Fixtures, external consumers, replay, native qualification | Shipping runtime dependencies |
| `franken-macos` | Audited safe Apple system boundary | FCB/FMD semantics or arbitrary pointer escape |

Create components when a real slice needs them. The table does not require thirteen empty crates.
Independently useful crates may be published; internal implementation modules need not be packages.

## Three consumer levels

1. Headless source/search/map use with in-memory or explicitly granted providers.
2. Semantic views and renderer-neutral frame plans inside another application.
3. Native Mac view embedding under host-owned event-loop/device/presentation lifetimes.

The default `fcb` feature set is empty. Proposed additive features include `source`, `search`,
`map`, `markdown`, `view`, `runtime`, `persistence` and `macos-metal`. Exact names are settled by
G0 consumer prototypes. Isolated consumers must verify actual dependency/feature resolution;
workspace tests alone can hide accidental feature leakage.

## Source identity and publication

A logical file is distinct from its path and content hash. Each capture names the bytes actually
retained. A complete capture has immutable complete backing; an extent capture explicitly records
holes. A live-file stat check cannot prove an atomic cross-file repository snapshot.

Every background publication identifies its browser/root, delivery incarnation, request,
capture and relevant analysis/layout/display/device generations. Reject obsolete delivery while
allowing correctly keyed immutable cache reuse where authorized. Closing one subscriber does not
cancel shared work still needed by another permitted subscriber.

Search pins a closed source manifest. Candidate indexes cannot exclude valid matches under the
declared query semantics; every candidate hit is checked against captured source. Unknown scope,
unavailable captures, result limits and canceled work remain explicit.

## Presented pixels and input

`FramePlan` bundles drawing with matching camera, layout, source, display and interaction identity.
Hit testing, selection, links and accessibility geometry use a conservatively accepted presented
snapshot. A newer model is not automatically the frame the user saw.

The map retains stable partitions and local coordinates. Visible-set traversal admits bounded
aggregates, labels and glyphs. A focus island promotes a deep subtree before floating-point
precision fails; a reading lens provides useful text dimensions for narrow parcels and huge files.
City mode extrudes the same footprint and names the chosen height metric.

## Markdown and typography ownership

FrankenMarkdown owns parsing, nested provenance, flow, text/font/math/diagram behavior and
renderer-neutral display/accessibility output. FCB translates upstream IDs, supplies view
constraints and authorized asset responses, and schedules resumable work. Reusable extensions
are implemented upstream first with a FrankenMarkdown-owned headless consumer, then consumed
through committed APIs here. Keep imports acyclic; the shared platform bridge depends on neither
FMD nor FCB.

Original bytes, logical reading text and visual glyphs remain separate. Selection must account for
graphemes, ligatures, bidi, UTF-16 native ranges and generated Markdown text. Disjoint source maps
must not pretend to be a contiguous literal slice. Huge-line visual access needs valid shaping
context or an explicitly limited display route.

## Work and resource lifetime

Asupersync provides the sole orchestration foundation. Explicit bounded service classes prioritize
visible reading and interactive queries while allowing maintenance progress. Foreign filesystem,
font and driver calls have admission/discard semantics; they are not universally interruptible.

Scheduling, engine work and managed-byte budgets are different contracts. Reserve allocations by
capacity, including old/new overlap, source pins, queue payloads and in-flight resources. CPU/GPU
shared memory is charged once within its accounting domain, while OS footprint is measured
separately. The standalone standard profile starts with a 3 GiB target and 6 GiB managed admission
guard, subject to measurement; small embedded consumers need no such minimum allocation.

Large CPU snapshots retire off the UI thread through bounded queues. GPU submissions reserve
lossless terminal records before commit and retain resources until completion. Wakes can coalesce;
ownership outcomes cannot disappear. Reclamation retains capacity even under pressure.

## Persistent state

Bookmarks, annotations and preferences are authoritative personal data. Indexes, previews and
layouts are derived. Rebuild and cache clear never erase the former. FrankenSQLite connections
have explicit thread-affine ownership, and GUI/CLI processes need separate store-owner coordination.

Write and validate immutable artifact candidates, complete the selected durability steps, then
commit their manifest and publish the in-memory head. Reconcile uncertain outcomes by operation ID.
Crash safety, power-loss durability and caller response delivery are separate claims.

## Acceptance

The first real loop is open → atlas → exact source → copy → return, including a native accessible
reader and an external host consumer. Later gates add syntax/search, native documents, persistence
and scale, relationships/trails, City/native polish and measured release qualification. See
[ROADMAP.md](ROADMAP.md). A screenshot or `lib.rs` alone cannot establish either product surface.
