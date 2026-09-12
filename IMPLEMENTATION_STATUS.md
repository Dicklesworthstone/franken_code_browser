# Implementation status

**Snapshot: September 12, 2026. Stage: documentation bootstrap, before G0.**

This inventory describes this repository. Sibling capabilities and the plan's source-review
findings do not establish an FCB implementation.

| Surface | Current state |
|---|---|
| Comprehensive specification | R3 plan, 32 sections and 96 planned work packages; delivery and boundary review applied |
| Project documentation | README, agent instructions, architecture, dependency policy, roadmap, qualification and security/privacy guidance |
| Repository support | License, issue-report templates, ignore and text-format configuration |
| Rust workspace/toolchain | Not created or qualified |
| Public `fcb` library / executable | Not implemented or published |
| `franken-macos` bridge / native Metal renderer | Not implemented here |
| Atlas, source reader, search, Markdown integration | Not implemented |
| Persistence, reading trails, City view | Not implemented |
| Committed upstream extension integrations | None selected by this bootstrap |
| Resolved suite/dependency closure | Not produced |
| Semantic/native/performance qualification | Not run; no product implementation to exercise |
| Installer, signed app, release artifacts | Not available |
| Beads task graph / DSR registration | Not established by this bootstrap |

All G0–G7 product gates remain pending. Documentation validation checks only the documentation;
it cannot qualify source correctness, embedding, native behavior, dependency closure or performance.
The `FCB-001`–`FCB-096` identifiers in the plan are work-package IDs, not evidence that issues exist.

## Next implementation boundary

Begin the foundation work in plan §29.9: exact suite inputs, the narrow Asupersync profile, audited
native ABI kernel and typed core, followed by owner-qualified IDs, inert embedding, byte budgets,
frame/accessibility coherence and lossless completion/retirement. Upstream FrankenMarkdown flow
and provenance require their own implementation and consumer proof.

## Updating this file

When a real surface lands, replace its row with the exact implemented scope and a path to its
production test/evidence. Keep implementation, integration and target-hardware qualification
distinct. Update the snapshot date and README consistently. Do not convert targets, empty crates,
proposed APIs or successful document checks into product completion.
