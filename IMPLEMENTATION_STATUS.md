# Implementation status

**Snapshot: September 14, 2026. Stage: foundation crates landed and batch-verified; no product gates passed.**

This inventory describes this repository. Sibling capabilities and the plan's source-review
findings do not establish an FCB product. Rows state exact implemented scope and the production
test evidence; "verified" means executed via strict remote compilation and test runs at the cited
revisions (code-first batch-pending commits are verified by independent review lanes before closure).

| Surface | Current state |
|---|---|
| Comprehensive specification | R4 plan, 32 sections and 97 planned work packages; delivery, boundary and sequencing reviews applied |
| Project documentation | README, agent instructions, architecture, dependency policy, roadmap, qualification and security/privacy guidance |
| Repository support | License, issue-report templates, ignore/text-format configuration and `scripts/check_plan_graph.py` document checker |
| `crates/fcb-core` — typed core | Owner-qualified identity types, `IdAllocator` with persisted-counter collision rejection; resource budgets with ordinary/terminal/retirement reservation classes, global acquisition order, typed admission errors and capacity reconciliation; owner-qualified handles and tables; bounded event rings, verified absolute deadlines, checked monotonic conversions and pair-verified clock-domain adapters. Evidence: `crates/fcb-core/tests/` (checked_core, handles, resource_leases, resource_leases_adversarial, tracing), 61+ tests green via strict remote runs at revisions `333c01b`/`59a66d9`; bead `fcb-y2jq.1` closed on independent receipt |
| `crates/fcb` — inert public facade | Additive `FeatureSet` (compiled vs available), capability-gated API, host-selected services (`HostServices`/`HostRequest`), `FramePlan` carrying `FileId` separately from `SourceRevision`. Evidence: `crates/fcb/tests/` (inert_facade, inert_consumer, host_services), 17 tests green at revision `7f49bf89`; beads `fcb-z2go.1`/`fcb-z2go.2` closed on independent receipts |
| `crates/fcb-runtime` — runtime seam | Bounded desktop-profile seam: wake coalescing surface and lab probes. Evidence: `crates/fcb-runtime/tests/wake_probe.rs`, 11 tests green at revision `1c5539a7`; surfaces serve beads `fcb-y2jq.2` (closed) |
| `crates/fcb-source` — provider seam | Source provider and capture capability seam, plus grant-scoped bounded directory discovery (`BoundedDiscovery`) with descriptor/depth/path/batch/queue caps, deterministic paged publication, and incomplete-scan semantics that do not tombstone unseen entries. Discovery evidence: `crates/fcb-source/tests/bounded_discovery.rs`, 15/15 via strict remote `cargo test -p fcb-source --test bounded_discovery -j 3` on hz2 (bead `fcb-hh2.1`, code-first, batch verification pending; claim blocked on in-progress `fcb-ywx.2`). Prior seam evidence: `crates/fcb-source/tests/provider_seam.rs`, 23 tests green at revision `7f49bf89`; serves bead `fcb-6up6.2` (closed) |
| `crates/fcb-test-support` — evidence tooling | Deterministic corpus fixtures with digests/counts and bounded scenario-receipt codec (redacted event ring, seed/pin/route identifiers, expected-vs-actual failures, terminal records that survive truncation). Evidence: 33 tests green at the post-repair tip (`e40f638`); defect loop (3 codec failures isolated, then repaired under bead `fcb-wc0g`) documented on beads `fcb-2nzu`/`fcb-wc0g`, both closed |
| `franken_macos` sibling bridge | Separate repository (no Cargo dependencies): audited ownership kernel with thread tokens, retain/release counters, explicit exception policy. Evidence: 8/8 tests via strict remote runs at revision `45ee4ac`; bead `fcb-0lf.1` closed; callback qualification remains in `fcb-0lf.2` |
| Asupersync desktop profile (upstream) | Bounded desktop runtime profile, host-selected services and owned/host runtime samples in the asupersync repository. Evidence: 7/7 sample tests at revision `49a534882`; beads `fcb-ywx.1` (reopened for dependency-closure factoring) and `fcb-ywx.2` (batch-pending) |
| Public `fcb` library / executable | Facade crate exists; the standalone `fcb` executable and product CLI are not implemented |
| Native Metal renderer / atlas UI | Not implemented |
| Atlas, source reader UI, search, Markdown integration | Not implemented (seam crates above are boundaries, not features) |
| Persistence, reading trails, City view | Not implemented |
| Resolved suite/dependency closure | Not produced; the selected `desktop-runtime-profile` feature still selects non-first-party edges (bead `fcb-ywx.1` owns the factoring) |
| Semantic/native/performance qualification | Not run on target hardware; remote strict-RCH runs cover portable logic only |
| Installer, signed app, release artifacts | Not available |
| G0–G7 product gates | All pending; foundation crates are prerequisites, not gate evidence |

## Verification discipline

Implementation lands code-first, batch-pending, and is verified by independent review lanes
executing the relevant suites through strict remote compilation at exact source revisions.
Receipts and criterion mappings are retained as bead comments; closure requires executed
evidence, not commit presence. Known tooling gaps: the shared worker fleet lacks
clippy/rustfmt components (format/lint gates not yet runnable remotely), and the Agent Mail
service operated with a corruption circuit breaker open (writes routed through bead comments).

## Updating this file

When a real surface lands, replace its row with the exact implemented scope and a path to its
production test/evidence. Keep implementation, integration and target-hardware qualification
distinct. Update the snapshot date and README consistently. Do not convert targets, empty crates,
proposed APIs or successful document checks into product completion.
