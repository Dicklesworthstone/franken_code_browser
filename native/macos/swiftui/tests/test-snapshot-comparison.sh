#!/bin/sh
# Run through strict RCH at a reviewed source revision. Scripted native
# receipts do not qualify real FCBS validation, Rust comparison or Mac UI.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-snapshot-comparison.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
set -- "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasIndexedSearch.swift" "$src/AtlasIndexedSearchWire.swift" "$src/AtlasIndexedSource.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" "$src/AtlasReaderTextSelection.swift" "$src/AtlasReaderDocument.swift" \
    "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/AtlasSnapshotComparison.swift" "$src/AtlasSnapshotComparisonModel.swift" "$src/AtlasSnapshotComparisonReader.swift" \
    "$src/tests/AtlasSnapshotComparisonFixture.swift"
swiftc "$@" "$src/tests/AtlasSnapshotComparisonTests.swift" -o "$out/model-tests"
"$out/model-tests"
cc -std=c11 -Wall -Wextra -Werror -I"$src/../../../crates/fcb-bridge/include" \
    -c "$src/tests/AtlasSnapshotComparisonFixtures.c" -o "$out/snapshot-fixture.o"
swiftc "$@" "$src/AtlasSnapshotComparisonBridge.swift" "$src/tests/AtlasSnapshotComparisonBridgeTests.swift" \
    "$out/snapshot-fixture.o" -o "$out/bridge-tests"
"$out/bridge-tests"
printf 'Retained test artifacts: %s\n' "$out"
