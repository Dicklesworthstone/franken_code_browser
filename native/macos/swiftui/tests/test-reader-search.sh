#!/bin/sh
# Production Find decoder/coordinator/model regressions. Invoke through strict
# RCH at the reviewed source revision. Injected responses do not qualify the
# Rust scanner, real foreign handoff or physical-Mac presentation.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-reader-search.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
if [ "$(uname -s)" = Darwin ]; then set -- "$@" "$src/AtlasAppearance.swift" "$src/AtlasSourceReader.swift"; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" "$src/AtlasReaderDocument.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/AtlasReaderTextSelection.swift" "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/tests/AtlasReaderSearchTests.swift" -o "$out/reader-search-tests"
"$out/reader-search-tests"
printf 'Retained test artifacts: %s\n' "$out"
