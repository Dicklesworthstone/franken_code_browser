# Changelog

This record is based on the repository's Git history, checked-in Beads exports, and the published
release. Version 0.1.0 is a developer preview, not a claim that all product gates are complete.
Scope window: September 12–23, 2026.

## Version Timeline

| Date | Milestone | Distribution |
|---|---|---|
| September 23, 2026 | [v0.1.0](https://github.com/Dicklesworthstone/franken_code_browser/releases/tag/v0.1.0) | Public notarized Mac developer preview |
| September 14–22, 2026 | One-repo source browser and Mac app | Source milestones; local test builds |
| September 12–13, 2026 | Documentation bootstrap | No binary release |

## v0.1.0 — notarized Mac developer preview (September 23, 2026)

### Delivered capability

- Published the [first GitHub release](https://github.com/Dicklesworthstone/franken_code_browser/releases/tag/v0.1.0) with a Developer ID-signed, Apple-notarized and stapled drag-to-Applications DMG and its SHA-256 sidecar. The release image's SHA-256 is `698293fc864d984fda3deea37769abd84adcc7fd44a9dbffd6bd9bc214244b76`.
- Added a verified one-line shell installer and a [Homebrew cask](https://github.com/Dicklesworthstone/homebrew-tap/blob/main/Casks/franken-code-browser.rb) for Apple Silicon Macs running macOS 14 or later. The cask passed `brew style`, strict audit, and fetch against the published artifact; the online installer passed an isolated install test.
- The release does not yet establish clean-machine first-launch, sustained smooth zoom, or Mac App Store qualification. See [distribution status](DISTRIBUTION.md).

## Earlier source milestones — one-repo Mac app (September 14–22, 2026)

### Engine and headless source tools

- Landed bounded root discovery, immutable source captures, encoding maps, source search and
  retained layout primitives. Representative commits: [root grants](https://github.com/Dicklesworthstone/franken_code_browser/commit/b8eebd7),
  [exact search](https://github.com/Dicklesworthstone/franken_code_browser/commit/502fd61),
  [retained parcel layout](https://github.com/Dicklesworthstone/franken_code_browser/commit/187c845).
- Added the `fcb` headless CLI for explicit file/workspace inspection, bounded reading and search;
  later source workflows include whole-file streaming, saved repositories and persistent reading
  desks. See [CLI reference](crates/fcb-app/README.md) and
  [desk sessions](https://github.com/Dicklesworthstone/franken_code_browser/commit/389aa2b).
- Exposed retained atlas, source, search and cache services over the `fcb-bridge` C ABI for a native
  host. See [atlas navigation](https://github.com/Dicklesworthstone/franken_code_browser/commit/0c93a66),
  [indexed query bridge](https://github.com/Dicklesworthstone/franken_code_browser/commit/6e23eb8),
  and [native artifact cache](https://github.com/Dicklesworthstone/franken_code_browser/commit/dd86f8a).

### Mac atlas and performance

- Integrated the SwiftUI/Metal app and typed Apple object facade under `native/macos/` so the
  engine and app build from this one repository. Root scripts now build the app, install a local
  copy, and make a drag-to-Applications DMG. This source migration preserves the former local
  `franken_macos` checkout as a history archive.
- The native code includes text-filled source parcels, directory strokes, Monokai-inspired colors,
  zoom/pan, captured-source search highlighting and retained Metal glyph presentation. The latest
  performance campaign also records unresolved display-delivery stutter; fast GPU draw timings
  alone do not establish smooth zoom. See [current distribution gates](DISTRIBUTION.md).
- Built [commit `3d8cece9ec98`](https://github.com/Dicklesworthstone/franken_code_browser/commit/3d8cece9ec98) on a physical Mac, opened the Asupersync repository, and produced a
  local Developer ID-signed DMG. Apple accepted its notarization, the ticket was stapled and
  validated, and the staged app passed local Gatekeeper assessment. The packager now supports the
  authenticated `asc` API-key route as well as `notarytool`; the exact artifact and remaining
  qualification work are recorded in [distribution status](DISTRIBUTION.md).

### Distribution state at that milestone

- A signed/notarized DMG was built locally before the public v0.1.0 release. Quarantined
  clean-machine installation remained unqualified. There was no App Store upload or App Review
  submission; that route still needs sandboxed project access and separate Mac distribution signing.

## Historical bootstrap — September 12–13, 2026

### Added

- Initial comprehensive plan (R2, revised in place to R3 and R4) for the native Apple Silicon source
  browser and public Rust library.
- Project-specific README, agent instructions, architecture, dependency constitution, roadmap,
  implementation status, qualification/release, security and privacy documentation.
- License matching the owner's example repositories, issue templates and repository text/ignore
  configuration.

At this historical point the repository was a documentation bootstrap. Later implementation is
recorded above; no binary release is claimed.

### Corrected

- Separated G2 ephemeral indexed search from G4 persistence so the early search gate does not
  require the later database milestone.
- Specified decoded UTF-16 text search, reversible native paths, full-width JSON IDs, bounded
  response framing and complete export-budget accounting.
- Added root-grant restoration/revocation, symlink-cycle and safe filename-presentation contracts.
- Distinguished standalone runtime assets from notarized distribution containers and offline launch.
- Scoped scratch database/log ignores to the repository root so curated regression fixtures remain
  visible to Git.
- Repointed FCB-010/023/025/028 away from the standalone composition and UI reducer so headless
  discovery, layout, search and external consumers no longer require the native window or Metal.
- Moved capture encoding maps into FCB-012 so the exact scan (FCB-026) does not depend on font shaping.
- Gave `franken-macos` its own repository home, added FCB-097 for the FrankenTUI extraction, and
  removed the inverted FCB-062 → FCB-096 dependency.
- Defined the root granted by `fcb open FILE`, the sandbox decision's consequences, the macOS 14
  display-link floor and the explicit owned-runtime route for non-Asupersync hosts.
- Added `scripts/check_plan_graph.py` and stopped ignoring `/doc/`.
