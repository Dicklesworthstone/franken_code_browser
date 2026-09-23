# AGENTS.md — FrankenCodeBrowser

Instructions for agents working on FrankenCodeBrowser, the planned `fcb` Rust library and native
source browser. Read this entire file, `README.md`, and the complete
`COMPREHENSIVE_PLAN_FOR_FRANKEN_CODE_BROWSER.md` before initial implementation or broad design work.
On later focused tasks, reread the relevant plan sections and any owning crate contracts.

## User direction and scope

The user's explicit instructions govern the task. These rules guide implementation; they do not
authorize unrelated work, publication, destructive cleanup, or changes to sibling repositories.
Carry authorized work through implementation and proportionate verification without repetitive
permission requests. Report real blockers and preserve the exact proof boundary.

Deliver the requested capability. Plans, Beads, coordination, manifests, and test harnesses support
that work. Do not replace source browsing with empty crates, generated capability claims, mock
rendering, or an elaborate process framework. Build the smallest complete production slice that
advances the plan's real open → atlas → source → search → documentation workflow.

## Protect the shared workspace

- Never delete a file or directory without the user's explicit authorization for the exact command
  in this session, including temporary files you created.
- Never run destructive reset/clean commands, force-push, or overwrite unrelated work.
- Do not stash, revert, unstage, move, or otherwise disturb another agent's changes. Inspect the
  current tree and make narrow edits to the files owned by your task.
- Work on `main`. Do not create branches, worktrees, or scratch clones unless the user explicitly
  changes that policy. File scope and coordination provide isolation.
- Stage explicit paths. A dirty sibling checkout is not permission to reset it to an old pin.
- Do not restart, stop, kill, or repair the shared Agent Mail service. If unavailable, retry once
  and continue independent work without claiming coordination succeeded.
- Never modify or remove CASS databases, backups, recovery directories, or session archives.
- For any Apple Simulator work on the owner's machine, first run
  `/Users/jemanuel/.local/bin/ensure-simulator-audio-safe prepare` and require success. Preserve the
  process-mute tap and BlackHole 2ch route; never play audio to test safety. If safety cannot be
  established, refuse Simulator actions while preserving visible windows and booted devices.
  Native Mac testing is the product lane; Simulator results cannot qualify Metal on a physical Mac.

## Design authority and implementation truth

The comprehensive plan is the normative product and technical specification. `AGENTS.md` governs
agent workflow. The supporting documents summarize specific parts of that plan:

| Document | Purpose |
|---|---|
| `ARCHITECTURE.md` | Library/app separation, ownership, source and frame invariants |
| `DEPENDENCY_CONSTITUTION.md` | First-party closure, native allowances, upstream landing rules |
| `ROADMAP.md` | G0–G7 sequencing and initial work packages |
| `IMPLEMENTATION_STATUS.md` | Dated inventory of what actually exists |
| `LOCAL_QUALIFICATION_AND_RELEASE.md` | Required evidence and build/release lanes |
| `SECURITY.md` / `PRIVACY.md` | Untrusted inputs, root grants, source handling and exports |

Code, tests, and retained execution evidence establish implementation status. A requirement does
not become implemented because it appears in a plan or registry. When artifacts disagree, identify
the owning requirement and repair the drift; never silently lower a gate. Preserve the plan's
research ledger as historical evidence, not a live dependency lockfile.

## Non-negotiable implementation contracts

### Rust and dependency closure

- Rust edition 2024; a dated nightly and Apple SDK are selected and qualified during G0. Do not
  invent a successful toolchain pin before those checks exist.
- All authoritative FCB source, analysis, search, layout, UI-state, and render-planning crates use
  `#![forbid(unsafe_code)]`.
- The first-party `franken-macos` bridge under `native/macos/` is the sole new application-side native unsafe
  exception: minimal audited Apple ABI, object ownership, callbacks and thread/device lifetimes.
  It lives in this repository so the app builds from one checkout; FrankenMarkdown does not host it.
  It owns no parser, search engine, or product policy. Inherited unsafe boundaries, including a
  selected storage VFS, need their own inventory and qualification.
- Shipping dependencies are std/toolchain libraries, FCB, Asupersync, FrankenMarkdown, and explicitly
  selected first-party FrankenSuite components. Audit transitive normal dependencies and the
  complete build/runtime closure, including macros, generated code and native assets.
- No Tokio, Rayon pool, serde convenience exception, Tree-sitter, Tantivy, wgpu, winit, objc2,
  WebView, Electron, or hidden foreign implementation. Renaming or vendoring an outside library
  does not make it first-party. `default-features = false` does not erase unconditional edges.
- Apple frameworks, the Rust/Apple toolchains, project-authored MSL shaders, and signing tools are
  the named platform allowances. Do not import either sibling example's different exception policy.

### Upstream ownership

Reusable Markdown parsing, highlighting, lexer checkpoints, typography, fonts, mathematics,
diagrams, document flow, source maps, accessibility reading structure, and Markdown export belong
in **FrankenMarkdown**, immediately. There is no temporary FCB-local implementation exception.
`fcb-document` adapts source identities, view state, granted assets, scheduling and display output.
It is not a second document engine.

Other shared changes land with their owners: Asupersync runtime, FrankenSQLite facade, FrankenTUI
pane/wide-prefix utilities, FrankenNetworkX directed kernels, FrankenManim hash/cache primitives,
FrankenTerm atlas policy, and FrankenThreeD handles. Inspect each owner's instructions before
editing there. Accepted integration requires a committed public upstream API, upstream tests,
and an FCB consumer test. Local path patches and research blob SHAs are not release pins.

### Library and host ownership

- `fcb` is the public library; `fcb-app` produces the real `fcb` executable. The app consumes the
  same supported APIs as external hosts.
- Default library construction is inert: no thread, runtime, window, filesystem scan, environment
  read, global logger, signal handler, allocator replacement, or process exit.
- Source/search/map and renderer-neutral consumers operate without Apple frameworks, persistent
  storage, or an installed application. Cargo features are additive; test them in isolated
  consumer workspaces, including feature unification.
- Hosts own their event loop, root grants, runtime and device unless they explicitly delegate
  particular resources. Exactly one owner acquires and presents a drawable.
- Two embedded instances must not alias IDs, leak private caches, or shut down each other's host
  resources. Closing a session cancels and drains only its owned work.

### Source and search correctness

- Keep original bytes authoritative. Distinguish complete captures from captured extents with
  unknown holes. Never resolve an old exact anchor against changed live bytes silently.
- Distinguish instance/root/file/capture/analysis/layout/query/window/device/presented-frame IDs.
  Slot and generation alone do not identify an owner. Retire exhausted identities.
- Use checked `u64` offsets and separate byte, decoded UTF-8, UTF-16, scalar, grapheme, and visual
  positions. Native sentinels and database signed integers need validated conversions.
- Do not mmap mutable working-tree source. File notifications trigger reconciliation; only a
  successfully completed scan can establish absence. Reject special objects such as FIFOs.
- Highlighting preserves bytes. Chunk boundaries are not EOF. Lexical colors and heuristic links
  do not establish compiler semantics.
- Search verifies candidates against the exact captured bytes. Complete results require a closed
  source manifest. Coverage, unavailable sources, match counts, truncation and refinement are
  separate states; an incompatible prefilter cannot be repaired by exact verification alone.
- Text search applies the declared decoder/normalization to that capture; a UTF-8 byte needle is
  not a UTF-16 text query. Preserve maps back to original bytes. Machine schemas use reversible
  native path payloads and canonical strings for full-width IDs/offsets, with bounded framing.
- Giant-line shaping requires valid text context or an explicit pending/logical/escaped mode.
  A clipped substring is not necessarily exact bidi or ligature layout.

### Interaction and resource ownership

- Camera/input/redraw work does no filesystem I/O, DB query, parsing, full-file lexing, shader
  compilation, GPU wait/readback, or whole-repository rebuild.
- Retain the hierarchy and bound traversal by admitted visible detail. Preserve unaffected
  neighborhoods; global repack is explicit. Use focus islands before precision underflows.
- Drawing, hit testing, source selection and accessibility share a compatible accepted frame.
  Do not activate a newer model whose pixels have not been presented.
- Asupersync is the sole FCB orchestration foundation. Runtime instances, actor contexts, clocks,
  worker counts and blocking pools are explicit. No detached work or unbounded queues/retries.
- Scheduling budgets, engine work budgets and managed-byte leases are distinct. Reserve capacity
  before allocation, including old/new overlap and queue payloads. Protect reclamation progress.
- Reserve lossless terminal records before GPU submission. Coalescing a wake must never lose a
  completion. GPU leases survive cancellation until actual terminal ownership permits release.
- Retire large CPU snapshots off the interaction thread through bounded queues. Dropping the last
  `Arc` can be expensive; async construction alone does not prevent a frame stall.
- Keep glyph raster identity separate from GPU residency. Validate shader ABI, color/alpha/clip
  conventions and CPU/GPU ownership on real hardware.
- Preserve selected exact source and user annotations before optional visual detail. The plan's
  3 GiB target and 6 GiB managed-resource admission guard are unmeasured design targets, not a
  promise about total process footprint.

### Storage, privacy and native usability

Separate authoritative bookmarks/annotations from rebuildable indexes and previews. A persistence
actor owns its thread-affine connection; separate processes also need explicit store ownership.
Publish validated immutable artifacts before their manifest, and reconcile uncertain commit
outcomes by operation ID. File rename alone does not prove power-loss durability.

Opening a repository grants read scope, not execution. No automatic build scripts, LSPs, shell
commands, network fetches or agent instructions from source. Root confinement must survive symlink
replacement where claimed. Exports and external actions require an explicit destination/action.
No source uploads or telemetry by default. Keep logs bounded and free of source payloads by default.

Saved paths do not confer current native access. Revalidate restored root grants, invalidate
pending deliveries on revocation, and drain native access leases safely. Bound symlink loops and
alias expansion; escape filename controls without replacing raw identity.

Keyboard, text selection, IME and native accessibility begin with the first reader. Light, dark,
high-contrast and reduced-motion behavior are required product work. City mode preserves the same
map and source identities and provides an explicit height metric.

## Working method

1. Read governing documents, the current status, task contract and relevant implementation/tests.
2. Inspect `git status` and file ownership. If concurrent agents and Agent Mail are available,
   reserve your narrow paths; do not treat advisory reservations as proof of a clean tree.
3. Implement a coherent user-visible slice with relevant success, failure and cancellation tests.
   Prefer focused patches; avoid broad scripted rewrites and duplicate `_v2` modules.
4. Run the checks appropriate to the change. Update status and docs only to the scope proven.
5. Commit and push authorized work using explicit paths. Verify the remote commit and report the
   exact remaining limitations. Do not stop at a proposed command when execution is authorized.

When `.beads/` exists, use `br` for mutations and `bv --robot-triage` / `bv --robot-next` for triage.
Read the whole selected issue and dependencies. Use `br sync --flush-only` for durable exports;
it does not commit. Never hand-edit the database or claim the plan's FCB IDs are existing Beads.
Close work only with its specified implementation and evidence, respecting any independent review
requirement. Avoid bare interactive `bv` or `cass` in automated sessions.

## Verification and release discipline

At bootstrap this is a documentation-only repository. There is no Cargo workspace, pinned
toolchain, runnable `fcb`, or configured qualification runner. Check links, document consistency,
plan preservation and Git hygiene. Run `python3 scripts/check_plan_graph.py` after editing the plan
or any doc it cross-references; it checks documents only. Do not invent build/test successes.

When code exists, run formatting, lints, focused semantic tests and isolated consumer checks for
the selected features. Run UBS on changed supported code before committing; scanner success is
supplementary evidence. Add native tests for ABI/render/text/lifecycle changes. A Linux worker,
CPU renderer, static audit, or screenshot cannot qualify a physical Mac's complete native route.

Prefer DSR for configured repository qualification and release lanes, with strict RCH for narrow
CPU-heavy probes. Verify runner registration and commands before claiming they exist. Do not
create GitHub Actions workflows merely to obtain a green badge; hosted workflows are optional
wrappers, not release authority. Preserve exact command, source/suite pins, worker, exit outcome
and evidence paths. Admission, dry runs and local fallback are not remote completion.

On the owner's Mac, honor the Cargo shim and run `sbh check --need 20G` before heavy lanes. Never
bypass offload using an absolute toolchain Cargo path or hardcode a target directory under `/tmp`.
Keep the inherited target directory; isolated targets must use the configured external
`RCH_TARGET_BASE`. A missing native worker is a reported blocker, not permission to bypass controls.

The first implementation order is FCB-001/002/003/007, then foundational owner IDs, inert facade,
byte budgets, accessibility/frame vocabulary and retirement/completion contracts as dependencies
permit. Read plan §29.9. G0–G7 mean this project's product gates, **not FrankenSim's Gauntlet tiers**.
Do not claim a gate passed from a trait, stub, fabricated response, or an unexecuted test.

## Active swarm: code-first work and independent verification

These rules apply to the user-authorized NTM implementation campaign and subsequent swarms.
BlackCedar coordinates this campaign; a scheduled verifier may act under its explicit handoff.

- Write real production code and relevant positive, boundary and failure tests in the same work
  item. No placeholder macros, fabricated capability rows, fixture-as-live proof, weakened
  assertions, regenerated goldens to force green, or source/spec edits that lower acceptance.
- During code-first waves, workers do not run builds or test compilations. A narrowly authorized
  syntax check is the maximum exception and still uses strict RCH. Commit coherent owned changes
  with the bead ID and "code-first, batch verification pending"; this is not completion evidence.
- Only the independent batch verifier closes work after reviewing production and test diffs and
  executing the relevant tests through strict RCH at an exact source revision. Preserve every
  failed attempt. Missing native hardware, skipped cases, compile-aborted suites and timeouts
  cannot qualify the product. Never close merely to unblock dependents.
- Use `br` exclusively for tracker changes and `--actor <AgentMailName>` on every mutation.
  Atomically claim one ready implementation task per worker; epics are navigation, not work.
  Workers may change their assignee/status/comments, never acceptance criteria or dependencies.
  Keep committed verification debt explicit with `batch-pending` labels until the verifier acts.
- Reserve narrow paths with Agent Mail and use bead-ID threads. Inspect the actual tree before
  every edit/commit; never include a peer's staged or unstaged files. Coordinate shared manifests
  through one owner. No branches, worktrees, stash, resets or file deletion.
- Batch verification runs centrally when a prerequisite can unlock work, the ready pool dries,
  verification debt reaches 24 items, or a wave reaches 30 minutes. Incomplete work stays open.
  RCH must report remote execution; local fallback is refused, not accepted as remote proof.
- Process artifacts require a named consumer, gated feature, observed defect and retirement
  condition. They earn no capability credit. Refusal-only work stays open unless the bead itself
  owns that boundary, with a near-identical permitted case that succeeds.
- Gate self-weakening, proof laundering, refusal farming, commit pumping, follow-up laundering,
  dependency smuggling and demo hardcoding are defects. The coordinator inspects for them every
  few ticks and reopens unsupported closes with an incident comment. Commit count is not a KPI.
- Report measured denominators and countermetrics, actual execution/effect outcomes, and exact
  remaining limitations. Linux/unit/static proof never substitutes for native Mac qualification.
