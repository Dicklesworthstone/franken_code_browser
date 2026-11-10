#!/bin/sh
# Exact native selection production decoders/models and C marshaling.
# Invoke through strict RCH at the reviewed source revision. Injected foreign
# replies do not execute Rust; AppKit cases require macOS and use private
# pasteboards only. No physical-Mac presentation qualification is claimed.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-reader-text-selection.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
if [ "$(uname -s)" = Darwin ]; then set -- "$@" "$src/AtlasSourceReader.swift"; fi
set -- "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" "$src/AtlasReaderDocument.swift" \
    "$src/AtlasReaderTextSelection.swift" "$src/AtlasReaderCoordinator.swift" \
    "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift"
swiftc "$@" "$src/tests/AtlasReaderTextSelectionTests.swift" -o "$out/selection-tests"
"$out/selection-tests"

clang -std=gnu11 -Wall -Wextra -Werror \
    -c "$src/tests/AtlasReaderTextSelectionFixtures.c" -o "$out/selection-fixture.o"
swiftc "$@" "$src/AtlasReaderTextSelectionBridge.swift" \
    "$src/tests/AtlasReaderTextSelectionBridgeTests.swift" "$out/selection-fixture.o" \
    -o "$out/selection-bridge-tests"
"$out/selection-bridge-tests"

if [ "$(uname -s)" = Darwin ]; then
    swiftc -swift-version 6 -warnings-as-errors "$src/AtlasSource.swift" \
        "$src/AtlasSourceReader.swift" "$src/tests/AtlasSourceReaderTests.swift" \
        -o "$out/native-source-tests"
    "$out/native-source-tests"
    swiftc -swift-version 6 -warnings-as-errors "$src/AtlasReaderSelectionClipboard.swift" \
        "$src/tests/AtlasReaderSelectionClipboardTests.swift" -o "$out/native-clipboard-tests"
    "$out/native-clipboard-tests"
else
    printf 'AppKit source selection and private-pasteboard tests not executed on this worker.\n'
fi
printf 'Retained test artifacts: %s\n' "$out"
