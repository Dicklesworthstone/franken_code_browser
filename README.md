<div align="center">

# FrankenCodeBrowser (`fcb`)

**A spatial source browser for Apple Silicon, designed as a native Metal application and an embeddable Rust library.**

Explore the repository. Zoom into exact source. Search without losing your place.

[![Status](https://img.shields.io/badge/status-foundations%20landing-d29922)](IMPLEMENTATION_STATUS.md)
[![Target](https://img.shields.io/badge/target-Apple%20Silicon-555555)](#platform-and-distribution)
[![Design](https://img.shields.io/badge/design-Rust%20%2B%20Metal-b7410e)](ARCHITECTURE.md)
[![License](https://img.shields.io/badge/license-MIT%20%2B%20rider-blue)](LICENSE)

</div>

> [!IMPORTANT]
> This repository contains the comprehensive design and project documentation plus the first
> foundation crates (typed core, inert facade, runtime/source seams, test-support tooling) with
> batch-verified test suites. There is no application, executable, installer, or benchmark result
> yet: the atlas, source reader, search, Markdown integration and native renderer described below
> remain unimplemented. Features and commands below describe the planned product. See
> [implementation status](IMPLEMENTATION_STATUS.md) for the exact boundary.

## Why a spatial source browser?

A file tree tells you how a project is organized, but following search hits and opening tabs can
make it difficult to remember where you are. FrankenCodeBrowser's planned repository atlas gives
directories and files stable places. Zooming reveals structure and then readable source; selecting
a file opens a crisp reading lens without losing its location on the map.

The atlas, source reader, Markdown preview, search results and relationship inspector share one
versioned source model. A search hit names captured bytes. A bookmark retains its source anchor.
A link follows an identified range rather than an approximate rectangle.

## Planned experience

| Task | Intended behavior |
|---|---|
| Get oriented | Open a real directory and explore a progressively populated 2D atlas before full indexing finishes. |
| Read precisely | Zoom into syntax-highlighted source or open a frontal reading lens with selection, copying, line navigation and wrapping. |
| Keep context | Pin multiple readers, follow history and bookmarks, and return to stable directory neighborhoods. |
| Search a large tree | Receive progressive path/text/heading results with exact source jumps and explicit coverage. |
| Read documentation | View native Markdown with source/preview/split, tables, code, mathematics and qualified diagrams/images. |
| Inspect relationships | Follow imports, documentation links and selected structural facts with their evidence level visible. |
| Explore a code city | Tilt and extrude the same map using a named metric, then return to flat reading without losing selection. |
| Share selected context | Assemble bounded reading trails and explicitly export source-evidence packs with provenance and omissions. |
| Embed the engine | Use headless source/search/map components or attach native views to host-owned resources. |

The core interaction is deliberately continuous:

```text
Open a repository → explore its atlas → select a file → read exact source
                            ↑                              │
                            └── search / links / history ──┘
```

This is a read-only browsing product. Editing, builds, debuggers, terminals and compiler execution
are outside the initial release. Optional editor handoff must be an explicit action.

## How the design stays responsive

The renderer retains source geometry, layout and resource identities. Camera movement changes
projection and visibility; it does not parse files or rebuild the whole repository. Distant
directories aggregate into bounded detail levels, while a selected file gets a readable lens.

Background discovery, analysis and search run under Asupersync scopes with bounded work and
generation checks. A stale search batch cannot replace the current query. CPU retirement and GPU
completion retain their own capacity so resource cleanup can continue under pressure.

Large files use immutable captures, chunked access and sparse line indexes. General text shaping
keeps the context needed for bidi and graphemes; pathological input can report context pending or
offer an explicit logical/escaped view while exact byte access remains available.

These are engineering contracts to implement and measure. Choosing Rust or Metal alone does not
establish latency, memory bounds or text quality.

## One library, one application

```text
Headless Rust consumer    Host-owned native view    Standalone fcb executable
          │                       │                         │
          └───────────────────────┼─────────────────────────┘
                                  ▼
                         public fcb library
                                  │
             source / search / analysis / map / UI model
                        │                     │
             FrankenMarkdown output     immutable FramePlan
                 flow + source maps     + interaction snapshot
                        └─────────────────────┤
                                      native Metal adapter
                                              │
                                safe franken-macos boundary

             optional Asupersync integration / FrankenSQLite store
```

The default library is intended to be inert: construction creates no window, runtime, thread,
database, filesystem scan or process-global handler. Hosts grant providers and explicitly supply
or delegate runtime, storage and rendering resources. Closing one view must leave its host and
other browser instances operational.

The planned `fcb-app` package produces the actual `fcb` executable using the public library.
The proposed component boundaries and consumer profiles are in [ARCHITECTURE.md](ARCHITECTURE.md).
Names and APIs are not yet published contracts.

## Shared components and ownership

| Owner | Planned role in FCB |
|---|---|
| Asupersync | Structured background work, cancellation, bounded admission and deterministic orchestration tests |
| FrankenMarkdown | All reusable highlighting, Markdown semantics, flow, source maps, fonts, text, mathematics and document display output |
| FrankenSQLite | Optional persistence with explicit connection/worker ownership and commit outcomes |
| FrankenTUI | Narrow non-terminal pane, focus and checked wide-height utilities |
| FrankenNetworkX | Selected compact directed graph views and algorithms |
| FrankenTerm | Narrow glyph-key and atlas-management primitives |
| FrankenManim | Selected retained-rendering, digest, canonical-envelope and cache primitives |
| FrankenThreeD | Qualified generational handles and resource identity primitives |
| CASS | Factored bounded evidence-selection/readiness policy for source trails |

Reusable improvements land in their owner repositories and are consumed through committed public
APIs. `fcb-document` remains a thin integration layer; it does not implement a private Markdown
engine. The native Metal renderer and proposed `franken-macos` bridge require real new work.

The shipping graph must satisfy a strict first-party dependency rule, including transitive edges.
The plan identifies upstream factoring still needed to achieve it. There is no blanket third-party
exception for sibling dependencies. See [DEPENDENCY_CONSTITUTION.md](DEPENDENCY_CONSTITUTION.md).

## Platform and distribution

The first GUI targets late-model Apple Silicon Macs, particularly M4/M5 configurations with at
least 24 GB of unified memory. Headless library components must also work on supported non-Mac
test hosts without Apple frameworks. Native Windows, Linux, iOS and browser UIs are not initial
release commitments.

Planned distribution includes a real standalone `fcb` binary with required embedded assets and
an optional signed/notarized `.app` using the same engine. Neither route should require a companion
application, a development checkout, Node, Python, a WebView or a downloaded model.

The standalone runtime can still be distributed inside a disk image or installer for offline
notarization support. A self-contained executable and a bare downloadable file are different
packaging choices; each launch route needs qualification.

No distribution is available yet. The Rust 2024 dated nightly, minimum macOS version and exact
SDK are foundation qualification decisions, not verified installation requirements today.

## Inspect the project today

```bash
git clone https://github.com/Dicklesworthstone/franken_code_browser.git
cd franken_code_browser
less README.md
less COMPREHENSIVE_PLAN_FOR_FRANKEN_CODE_BROWSER.md
```

There is no `cargo build` or installation command at this stage. The plan's proposed CLI includes:

```text
fcb /path/to/repository
fcb open /path/to/file.rs --line 120
fcb search /path/to/repository --text "cancel" --json --limit 50
fcb inspect /path/to/repository --json
fcb capabilities --json
fcb doctor --json
fcb trail export TRAIL_ID --format markdown --out /path/to/context.md
```

These commands are specifications, not runnable examples. The intended machine interface shares
the library's services, emits versioned JSON on stdout and diagnostics on stderr, and distinguishes
incomplete, canceled, unavailable and failed results. `doctor` is read-only by default.

## Performance objectives

The plan sets initial objectives for a qualified standard workload: roughly 100,000 files and
10 million lines, with bounded visible detail. It includes 120 Hz presentation goals, warm path
search results within 30 ms at p95, and a 3 GiB managed-resource target with a 6 GiB admission guard.

**None of these numbers has been measured in FCB.** Managed bytes are not total process footprint.
Stress qualification includes approximately one million files and 10–20 GiB of source, as well as
huge lines, malformed text and changing repositories. Every result must name its hardware, display,
corpus, source revision and cache state. See plan §21 and
[LOCAL_QUALIFICATION_AND_RELEASE.md](LOCAL_QUALIFICATION_AND_RELEASE.md).

## Roadmap and documentation

The plan defines 97 work packages and eight product gates, G0–G7. The next step is G0: qualify
dependency and native boundaries, inert embedding, resource ownership, initial accessibility,
and upstream Markdown contracts. The first complete user loop then opens a real tree and lets the
user navigate, read and copy real source. All gates remain pending.

| Read | For |
|---|---|
| [Comprehensive plan](COMPREHENSIVE_PLAN_FOR_FRANKEN_CODE_BROWSER.md) | Full product specification, research ledger and work-package dependencies |
| [Agent instructions](AGENTS.md) | Engineering rules, shared-tree safety and verification workflow |
| [Architecture](ARCHITECTURE.md) | Component boundaries, identity and lifecycle design |
| [Dependency constitution](DEPENDENCY_CONSTITUTION.md) | Allowed closure and upstream ownership |
| [Roadmap](ROADMAP.md) | Product milestones and initial implementation order |
| [Implementation status](IMPLEMENTATION_STATUS.md) | What exists and what remains unimplemented |
| [Qualification and release](LOCAL_QUALIFICATION_AND_RELEASE.md) | Semantic, native, performance and packaging evidence |
| [Security](SECURITY.md) / [Privacy](PRIVACY.md) | Root authority, untrusted source and export handling |
| [Changelog](CHANGELOG.md) | Durable repository changes |

## Limitations and common questions

**Can I run it now?** No. This is the design/bootstrap stage. A missing executable or Cargo
manifest is expected; the repository does not yet contain an installation path.

**Is it an IDE?** The planned product focuses on reading and navigation. It does not need to
execute source, compile projects or run language servers to open a directory.

**Will it understand every reference?** Exact bytes, lexical classifications, structural facts,
resolved relationships and heuristics have separate evidence levels. A highlighter is not a compiler.

**Will Markdown use a browser?** No. The design consumes native, renderer-neutral output from
FrankenMarkdown. Required flow and provenance extensions must land there first.

**Will source leave my machine?** The design has no default telemetry, uploads or network fetching.
Exports are explicit. These are required behaviors, not security guarantees of an existing app.

**Where should I report a problem?** Use the repository's issue templates for design defects and,
once implemented, reproducible bugs. Include the exact revision and expected behavior. Keep private
source and credentials out of public reports; see [SECURITY.md](SECURITY.md).

## About Contributions

*About Contributions:* Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos that seeks community contributions, but it's the only way I can move at this velocity and keep my sanity.

## License

[MIT License with OpenAI/Anthropic Rider](LICENSE), matching the owner's example repositories.
The rider is part of the license; see the exact terms. Future fonts, assets and dependencies must
retain their own license and provenance records.
