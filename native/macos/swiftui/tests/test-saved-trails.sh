#!/bin/sh
# Production trail admission, ordered navigation, recovery, retained ranges and
# public-header ABI marshaling. Independent strict-RCH execution remains
# required; these fixtures cannot qualify native panels or physical Mac UI.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-saved-trails.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
set -- "$@" -swift-version 6 -warnings-as-errors -D FCB_SAVED_SEARCH_TESTS \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" "$src/AtlasReaderDocument.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/AtlasReaderTextSelection.swift" "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/AtlasSavedRepository.swift" "$src/AtlasSavedRepositoryTrails.swift" "$src/AtlasSavedTrailAuthoring.swift" "$src/AtlasSavedRepositoryIndexBuild.swift" \
    "$src/AtlasSavedRepositoryImports.swift" "$src/AtlasSavedRepositoryPaths.swift" "$src/AtlasSavedRepositorySearch.swift" \
    "$src/AtlasSavedRepositoryReader.swift" "$src/AtlasSavedRepositoryModel.swift" \
    "$src/tests/AtlasSavedRepositoryTests.swift"
swiftc "$@" "$src/tests/AtlasSavedTrailTests.swift" "$src/tests/AtlasSavedTrailAuthoringTests.swift" \
    "$src/tests/AtlasSavedTrailExportTests.swift" "$src/tests/AtlasSavedTrailArchiveReaderTests.swift" -o "$out/saved-trail-tests"
"$out/saved-trail-tests"
cc -std=c11 -Wall -Wextra -Werror -I"$src/../../../crates/fcb-bridge/include" \
    -c "$src/tests/AtlasSavedTrailBridgeFixtures.c" -o "$out/saved-trail-fixture.o"
swiftc "$@" "$src/AtlasSavedRepositoryTrailBridge.swift" "$src/tests/AtlasSavedTrailBridgeTests.swift" \
    "$out/saved-trail-fixture.o" -o "$out/saved-trail-bridge-tests"
"$out/saved-trail-bridge-tests"
printf 'Retained test artifacts: %s\n' "$out"
