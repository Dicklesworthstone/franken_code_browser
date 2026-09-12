# Roadmap

The [comprehensive plan](COMPREHENSIVE_PLAN_FOR_FRANKEN_CODE_BROWSER.md), §§28–29, owns the complete
97-package dependency graph. This is its navigation summary. All gates are pending at bootstrap;
there is no promised release date before foundation uncertainties are resolved.

## Product gates

| Gate | User-visible or architectural result | Required evidence |
|---|---|---|
| G0: foundations | Sound native boundary, compliant foundation closure, inert library, explicit ownership and initial upstream document integration | Real native window/text/Metal, runtime lifecycle, byte/completion/retirement admission, initial accessibility and FMD-owned consumer |
| G1: atlas to source | Open a real tree, navigate its stable atlas, read/copy exact source and return | Production GPU/source path, coherent hit testing, keyboard/native accessibility, independent host lifecycle |
| G2: syntax and search | Shared resumable highlighting and immediate progressive search | Whole/chunk lexer equivalence, ephemeral indexed/reference search agreement across supported encodings, stale-publication rejection |
| G3: documentation | Native Markdown source/preview/split with qualified rich content | Upstream flow/provenance and compatibility tests plus real native FCB documents |
| G4: live workspaces at scale | Persist navigation, reconcile file changes and page large sources/indexes | Crash/reopen, annotation preservation, process-store ownership and pressure/stress fixtures |
| G5: analysis workflows | Qualified relationships, multiple readers, history, trails, exports and captured-source comparison | Evidence labels, exact source anchors, bounded exports and stable navigation |
| G6: City and native polish | Same-map 2.5D view and complete keyboard/accessibility/IME/display behavior | Real 2D → code → search → City → 2D trace, plus nonspatial access |
| G7: qualified distribution | Standalone executable, optional signed app and independent library consumers | Exact closure, functional/safety/visual gates and named hardware/performance results |

These labels are FCB product gates. They do not reuse FrankenSim's numerical Gauntlet definitions.

G2 indexes bounded in-memory captures without persistence. G4 adds persistent segment encoding,
publication, recovery and out-of-core merging using the same search semantics. FCB-027 therefore
does not depend on the later store; FCB-086 owns its persistent integration.

City mode is optional to activate, but remains part of the planned full product. Accessibility
starts in G0/G1; G6 expands it rather than introducing it late.

## Start here

The immediate starting packages are FCB-001 (suite inputs), FCB-002 (clean runtime), FCB-003
(native ownership kernel) and FCB-007 (typed core). Their dependencies still apply: for example,
FCB-002 and FCB-003 require FCB-001's inspection.

As prerequisites land, implement FCB-065–072 and FCB-094: managed byte/queue leases, inert facade,
source providers, owner IDs, native range/accessibility vocabulary, host embedding, coherent
frames, CPU retirement/GPU completions and bounded timing/admission fixtures. These are early
foundations despite their higher work-package numbers.

Develop the native text route and **upstream** FrankenMarkdown provenance/flow work
(FCB-073–075) as their dependencies permit. Do not create an FCB-only Markdown substitute.
The first complete browsing loop culminates in FCB-020 with early accessibility and embedding proof.

## Completion rules

A work package is complete only when its production behavior and specified evidence exist.
Cross-repository work needs the owning upstream commit, its tests and a public FCB consumer.
A trait, success-shaped JSON, mock renderer or screenshot is not sufficient.

If converting the plan to Beads, preserve IDs, dependency meaning and acceptance evidence. Run
`python3 scripts/check_plan_graph.py` after any plan edit, and never silently alter dependencies to
make a queue appear ready. The documentation bootstrap does not create or close those implementation tasks.

Optional neural search, external Git/LSP providers, broad GPU compute and adaptive controllers
remain separately justified enhancements. They must not displace the real source/Markdown/native
workflow or relax the closure, correctness and resource contracts.
