#!/bin/sh
# Production reader locators, semantic folder history and camera anchors on macOS.
# Retain artifacts for the independent strict-RCH verification lane.
set -eu
cd "$(dirname "$0")/.."
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-folder-navigation.XXXXXX")
python3 - swiftui "$out/Scope.swift" <<'PY'
from pathlib import Path
import sys
text = (Path(sys.argv[1]) / "AtlasSearch.swift").read_text()
# Unchanged production declarations, not substitute file-scope predicates.
# This keeps the test independent of unrelated reader/search-session linkage.
def declaration(start, end):
    first = text.index(start)
    return text[first:text.index(end, first)]
reader = (Path(sys.argv[1]) / "AtlasReader.swift").read_text()
identity_start = reader.index("struct AtlasReaderIdentity:")
identity = reader[identity_start:reader.index("struct AtlasReaderInfo:", identity_start)]
Path(sys.argv[2]).write_text("import Foundation\n"
    + declaration("enum AtlasSearchError:", "struct AtlasSearchProgress:")
    + declaration("struct AtlasSearchFileScope:", "struct AtlasSearchContext:")
    + declaration("enum AtlasFileScope:", "final class AtlasSearchAccessLease:")
    + identity)
PY
for suite in folder-navigation reader-atlas-navigation; do
    case "$suite" in
        folder-navigation) test_source=swiftui/tests/AtlasFolderNavigationTests.swift ;;
        reader-atlas-navigation) test_source=swiftui/tests/AtlasReaderAtlasNavigationTests.swift ;;
    esac
    swiftc -swift-version 6 -warnings-as-errors -parse-as-library \
        "$out/Scope.swift" swiftui/AtlasNavigationHistory.swift swiftui/AtlasCamera.swift \
        swiftui/AtlasProjectCatalog.swift swiftui/AtlasFolderNavigation.swift swiftui/AtlasReaderAtlasNavigation.swift \
        "$test_source" -o "$out/$suite"
    "$out/$suite"
done
printf 'Retained test artifacts: %s\n' "$out"
