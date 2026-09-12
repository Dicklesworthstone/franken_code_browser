# Changelog

## Unreleased

### Added

- Initial comprehensive plan (R2, revised in place to R3 and R4) for the native Apple Silicon source
  browser and public Rust library.
- Project-specific README, agent instructions, architecture, dependency constitution, roadmap,
  implementation status, qualification/release, security and privacy documentation.
- License matching the owner's example repositories, issue templates and repository text/ignore
  configuration.

This is a documentation bootstrap. No executable, library implementation or release is claimed.

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
