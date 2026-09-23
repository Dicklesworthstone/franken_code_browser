# Changelog

This record is based on the repository's Git history and checked-in Beads exports through
2026-09-22. There are no version tags or GitHub Releases. These are source milestones, not a
claim that the complete product or a signed app has shipped.

## Unreleased — source browser and one-repo Mac app (September 14–22, 2026)

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
- Built commit `3d8cece9ec98` on a physical Mac, opened the Asupersync repository, and produced a
  local Developer ID-signed DMG. Apple accepted its notarization, the ticket was stapled and
  validated, and the staged app passed local Gatekeeper assessment. The packager now supports the
  authenticated `asc` API-key route as well as `notarytool`; the exact artifact and remaining
  qualification work are recorded in [distribution status](DISTRIBUTION.md).

### Release state

- The signed/notarized DMG is local, with no GitHub Release download yet. Quarantined clean-machine
  installation remains unqualified. There is no App Store upload or App Review submission; that
  route still needs sandboxed project access and separate Mac distribution signing.

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
