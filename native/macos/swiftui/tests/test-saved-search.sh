#!/bin/sh
# Saved decoded-search contracts over the production model and retained reader.
# The repository suite supplies one shared foreign-transport fixture; its own
# entry point is omitted only for this dedicated search executable.
# Run later through the independent strict-RCH verifier at the source revision
# under review. Injected transport does not qualify the native C ABI or Mac UI.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-saved-search.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors -D FCB_SAVED_SEARCH_TESTS \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderDocument.swift" \
    "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/AtlasSavedRepository.swift" "$src/AtlasSavedRepositorySearch.swift" \
    "$src/AtlasSavedRepositoryReader.swift" "$src/AtlasSavedRepositoryModel.swift" \
    "$src/tests/AtlasSavedRepositoryTests.swift" "$src/tests/AtlasSavedSearchTests.swift" -o "$out/saved-search-tests"
"$out/saved-search-tests"
printf 'Retained test artifacts: %s\n' "$out"
