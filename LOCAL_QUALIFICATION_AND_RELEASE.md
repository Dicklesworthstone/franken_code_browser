# Local qualification and release

This is the planned verification contract from plan §§21 and 25–28. No Cargo workspace,
qualification script, DSR registration or release build exists at bootstrap. Do not run invented
commands or describe document checks as product tests.

## Evidence by lane

| Lane | Establishes | Does not establish |
|---|---|---|
| Documentation checks | Existing links, naming, consistent status and work-package references | Executable behavior |
| Pure semantic tests | Checked IDs/ranges, capture/line semantics, layout invariants, exact search | Native text or GPU behavior |
| Upstream conformance | FMD lexical/flow/provenance behavior and supported compatibility | FCB integration without its consumer |
| Isolated consumers | Actual feature closure and headless/host ownership at tested profiles | Every possible host combination |
| Deterministic orchestration | Modeled cancellation, stale delivery, admission and ownership races | Unmodeled ABI/driver behavior |
| CPU reference rendering | Display-list, color/clip/order and source geometry semantics | Physical GPU presentation |
| Native integration | Actual AppKit, text, accessibility, watcher, Metal and teardown routes | Untested hardware or OS versions |
| Hardware performance | Named corpus/display/cache measurements on a physical Mac | Another chip, memory class or quality setting |
| Packaging/recovery | Exact standalone/bundle launches, artifact integrity, migration/reopen | Unexercised power-loss durability |

Functional, safety, dependency, visual and performance verdicts are independent. Missing required
capabilities must appear explicitly, rather than disappearing from a registry to make it green.

## Runner policy

Use owner-controlled DSR lanes for repository-level qualification and release once configured.
Use strict RCH for narrow CPU-heavy work that can run on an appropriate worker. Native Apple
tests need native hosts; Linux results are useful for headless semantics only. Verify registration,
selected target, exact source inputs and terminal outcomes before citing a runner result.

Repository scripts should own test logic so optional workflow wrappers contain no unique checks.
GitHub Actions is not a required release authority. A dry run, queue admission, remote compile
start, timeout or fallback is not a completed qualified run. Do not bypass the owner's Cargo shim
or disk/offload controls to turn an unavailable worker into a local workload.

For documentation-only changes, check internal links, status consistency, intended file inventory,
whitespace and Git ignore behavior. Once code exists, run formatter/lints and meaningful tests for
the changed contract, then the applicable broader lanes for a coherent release candidate.

## Required adversarial coverage

- Captures replaced or evicted while search/selection references old bytes; cross-chunk UTF-8,
  CRLF, huge offsets and encoding maps.
- Two owners exchanging equal slot/generation handles; closing one embedded view during work.
- Model/layout changes while old pixels remain presented; resize and backing-scale changes.
- GPU completion during full ordinary queues and window close; CPU last-reference retirement.
- Incomplete directory scans, permission failures, watcher overflow, FIFOs and symlink replacement.
- Partial search universes, short/normalized queries, exact reference comparisons and index quotas.
- UTF-16 decoded queries versus byte queries, raw non-UTF-8 paths, full-width JSON IDs, incomplete
  output streams, slow clients and export sizes including formatting/provenance.
- Revoked/restored root grants, stale native bookmarks, symlink loops and misleading filename controls.
- Bidi/ligature/combining context, native UTF-16 sentinels, generated/disjoint Markdown provenance.
- Memory pressure with old/new artifacts, in-flight frames and pinned captures competing for space.
- Store-owner collisions, crash points around artifact/manifest publication and uncertain commits.
- Upstream and downstream builds without accidental workspace feature or patch assistance.

Use the full matrix in plan §25.9; these examples do not replace it. Native reader keyboard and
accessibility tests begin in G1. Tests must reach production code with real files/resources.

## Performance record

Retain the exact command, source and suite commits, toolchain/SDK, features, physical chip/memory,
OS, display pixels/backing scale/refresh, power/thermal state, corpus identity and verified counts,
cache state, trace, raw samples, failures and terminal outcome.

Report median/p95/p99, stalls and missed refreshes, event-to-present, CPU/GPU timings, managed
allocation peaks, measured footprint, I/O, coverage and static idle behavior. Physical
input-to-photon requires separate hardware measurement. A frame encoding timer is not that metric.

Qualify small, standard large, stress, pathological and pinned real-repository corpora. Match
visible content and quality across comparisons. Do not lower resolution, hide selected text,
disable Markdown or omit late frames to claim a speedup. The plan's 120 Hz, latency and memory
figures remain objectives until measured. Certify only hardware classes actually tested.

## Release sequence

1. Select a coherent committed source/suite snapshot and the exact release feature set.
2. Verify transitive/build/runtime closure, unsafe ledger, licenses, toolchain/SDK and assets.
3. Run required semantic, upstream, consumer, native, visual, pressure and hardware lanes.
4. Build the actual standalone `fcb` binary with required assets and the optional `.app`.
5. Exercise launches without a companion app or development checkout, including paths with spaces
   and non-ASCII text; verify headless CLI without a display and host-independent library use.
6. Sign/notarize the declared distributions; retain integrity and provenance records.
   Distinguish the embedded-asset standalone runtime from its distribution container: direct
   stapling of a standalone binary is unsupported. Qualify the selected supported container and
   quarantined online/offline launch without disabling Gatekeeper. See plan §26.8 and its Apple sources.
7. Verify published assets by downloading and checking them, preserving authoritative user state
   through the supported install/update/reopen/rollback routes.

No tagged release or release-success badge should be created by this documentation bootstrap.
