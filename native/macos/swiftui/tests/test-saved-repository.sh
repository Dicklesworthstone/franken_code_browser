#!/bin/sh
# Production saved-repository decoder/model and retained-reader contracts.
# Run later through the independent strict-RCH verifier at the source revision
# under review. Injected transport does not qualify the native C ABI or Mac UI.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-saved-repository.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderDocument.swift" \
    "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/AtlasSavedRepository.swift" "$src/AtlasSavedRepositorySearch.swift" \
    "$src/AtlasSavedRepositoryReader.swift" "$src/AtlasSavedRepositoryModel.swift" \
    "$src/tests/AtlasSavedRepositoryTests.swift" -o "$out/saved-repository-tests"
"$out/saved-repository-tests"
printf 'Retained test artifacts: %s\n' "$out"
