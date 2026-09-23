# Mac App Store listing — FrankenCodeBrowser 0.1.0

This copy describes the sandboxed Mac build. It is source for the App Store Connect listing,
not evidence that Apple has accepted or published the app.

| Field | Value |
|---|---|
| Name | FrankenCodeBrowser |
| Subtitle | Explore code spatially |
| Primary category | Developer Tools |
| Price | Free |
| Primary language | English (U.S.) |
| Copyright | 2026 Jeffrey Emanuel |
| Support URL | https://www.jeffreyemanuel.com/contact |
| Marketing URL | https://github.com/Dicklesworthstone/franken_code_browser |
| Privacy policy URL | https://github.com/Dicklesworthstone/franken_code_browser/blob/main/PRIVACY.md |
| Keywords | source code,repository,visualization,treemap,syntax highlighting,search,developer tools |

## Description

Explore a codebase as a map of its source files. FrankenCodeBrowser lays out a folder as a dense
atlas of text-filled rectangles, grouped by directory. Pan and zoom to move between the shape of
the repository and individual lines of code without losing your place.

Choose a local project folder to begin. The app shows syntax-highlighted source in the atlas and
in a selectable file reader. Search for exact text across the selected project, highlight returned
matches, and narrow the view by file type when you want to focus on a language or document set.

The native Mac app uses Metal for atlas presentation and keeps a local prepared-text cache for
subsequent openings. The Mac App Store edition uses a read-only folder grant and saves access to
recently selected projects with macOS security-scoped bookmarks. It does not need an account or
upload your source. Opening a project does not build or execute its code.

FrankenCodeBrowser is an early release. Large repositories can take time to prepare, and searches
report partial results when files are unavailable or a result limit is reached.

## App Review notes

No sign-in or demo account is needed. On first launch, choose a folder containing source files.
The reviewer can use any local source project; the public
https://github.com/Dicklesworthstone/franken_code_browser repository is a representative example.
Drag the atlas to pan, scroll to zoom, use Command-F for exact-text search, choose a file-type
filter from the toolbar, and click a file to open its selectable source reader. The app reads the
chosen folder and writes derived cache data only inside its own macOS sandbox container.

The screenshot in `assets/app-store/01-asupersync-atlas.png` is a real capture of the signed Mac
App Store build viewing the public Asupersync repository. It is 2880 × 1800 pixels with no alpha
channel, matching Apple's Mac screenshot specification.
