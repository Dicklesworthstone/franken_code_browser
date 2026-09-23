#!/bin/sh
# Real Metal pixel parity and paired GPU timestamps against the six-vertex baseline.
# Requires an external output directory and the existing native bridge archive.
set -eu
cd "$(dirname "$0")/.."
: "${FCB_PROFILE_OUTPUT:?Set a new external output directory}"
: "${FCB_BRIDGE_LIB:?Set the native bridge archive}"
mkdir "$FCB_PROFILE_OUTPUT"
git show 7589f2b:swiftui/AtlasMetalGlyphRenderer.swift | sed 's/AtlasMetalGlyphRenderer/AtlasMetalGlyphRendererControl/g' > "$FCB_PROFILE_OUTPUT/Control.swift"
xcrun swiftc -O -parse-as-library swiftui/AtlasSource.swift swiftui/AtlasMatch.swift swiftui/AtlasDocument.swift swiftui/AtlasPreparedText.swift "$FCB_PROFILE_OUTPUT/Control.swift" swiftui/AtlasMetalRasterRenderer.swift swiftui/AtlasMetalGlyphRenderer.swift swiftui/tests/AtlasQuadStripComparison.swift "$FCB_BRIDGE_LIB" -o "$FCB_PROFILE_OUTPUT/compare"
"$FCB_PROFILE_OUTPUT/compare" > "$FCB_PROFILE_OUTPUT/results.jsonl"
