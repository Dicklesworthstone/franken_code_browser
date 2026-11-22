#!/bin/sh
# Native personal atlas-view store and worker ownership on macOS.
# Retain artifacts for the independent strict-RCH verification lane.
set -eu
cd "$(dirname "$0")/.."
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-folder-persistence.XXXXXX")
python3 - swiftui "$out/Scope.swift" <<'PY'
from pathlib import Path
import sys
text = (Path(sys.argv[1]) / "AtlasSearch.swift").read_text()
# Unchanged production declarations, not substitute file-scope predicates.
# This keeps the test independent of unrelated reader/search-session linkage.
def declaration(start, end):
    first = text.index(start)
    return text[first:text.index(end, first)]
Path(sys.argv[2]).write_text("import Foundation\n"
    + declaration("enum AtlasSearchError:", "struct AtlasSearchProgress:")
    + declaration("struct AtlasSearchFileScope:", "struct AtlasSearchContext:")
    + declaration("enum AtlasFileScope:", "final class AtlasSearchAccessLease:"))
PY
swiftc -swift-version 6 -warnings-as-errors -parse-as-library \
    "$out/Scope.swift" swiftui/AtlasNavigationHistory.swift swiftui/AtlasCamera.swift \
    swiftui/AtlasProjectCatalog.swift swiftui/AtlasFolderNavigation.swift \
    swiftui/AtlasFolderPersistence.swift swiftui/tests/AtlasFolderPersistenceTests.swift \
    -o "$out/folder-persistence"
"$out/folder-persistence"
printf 'Retained test artifacts: %s\n' "$out"
