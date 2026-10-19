#!/bin/sh
# Exact displayed-source import over production models/coordinator/decoders.
# Run through strict RCH at the source revision under review. The portable
# fixture does not qualify Rust; macOS additionally checks actual C marshaling
# and CryptoKit against fixed original-byte vectors, without a native UI claim.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-supplied-reader.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
set -- "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderDocument.swift" \
    "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift"
swiftc "$@" "$src/tests/AtlasSuppliedReaderFixture.swift" "$src/tests/AtlasSuppliedReaderTests.swift" \
    -o "$out/supplied-tests"
"$out/supplied-tests"
if [ "$(uname -s)" = Darwin ]; then
    clang -std=gnu11 -Wall -Wextra -Werror -c "$src/tests/AtlasSuppliedReaderBridgeFixtures.c" -o "$out/bridge-fixture.o"
    swiftc "$@" "$src/AtlasReaderBridge.swift" "$src/tests/AtlasSuppliedReaderBridgeTests.swift" \
        "$out/bridge-fixture.o" -o "$out/bridge-tests"
    "$out/bridge-tests"
else
    printf 'Mac CryptoKit/C bridge checks not executed on this worker.\n'
fi
printf 'Retained test artifacts: %s\n' "$out"
