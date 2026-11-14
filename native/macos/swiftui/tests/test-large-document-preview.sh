#!/bin/sh
# Run through the independent strict-RCH lane at the reviewed revision. This
# checks production native admission/decoding/presentation, not Rust parsing or
# physical-Mac rendering. Keep artifacts available for diagnosis.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-large-document-preview.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
if [ "$(uname -s)" = Darwin ]; then set -- "$@" "$src/AtlasAppearance.swift" "$src/AtlasSourceReader.swift"; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" \
    "$src/AtlasReaderDocument.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/AtlasReaderTextSelection.swift" "$src/AtlasReaderCoordinator.swift" \
    "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/tests/AtlasReaderDocumentFixture.swift" "$src/tests/AtlasLargeDocumentPreviewTests.swift" \
    -o "$out/tests"
"$out/tests"
printf 'Retained large document artifacts: %s\n' "$out"
