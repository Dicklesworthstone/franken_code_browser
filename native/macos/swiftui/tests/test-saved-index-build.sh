#!/bin/sh
# Saved index publication receipts, explicit attachment and entered-worker drain.
# Independent strict-RCH verifier owns execution; fixtures do not qualify real
# filesystem durability, grant handling, index semantics or physical Mac UI.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-saved-index-build.XXXXXX")
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
    "$src/AtlasSavedRepository.swift" "$src/AtlasSavedRepositoryIndexBuild.swift" \
    "$src/AtlasSavedRepositoryImports.swift" "$src/AtlasSavedRepositoryPaths.swift" \
    "$src/AtlasSavedRepositorySearch.swift" "$src/AtlasSavedRepositoryReader.swift" "$src/AtlasSavedRepositoryModel.swift" \
    "$src/tests/AtlasSavedRepositoryTests.swift"
swiftc "$@" "$src/tests/AtlasSavedIndexBuildTests.swift" -o "$out/saved-index-build-tests"
"$out/saved-index-build-tests"
printf 'Retained test artifacts: %s\n' "$out"
