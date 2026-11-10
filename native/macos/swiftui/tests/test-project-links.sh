#!/bin/sh
# Production native documentation-link decoder/model and Swift-to-C marshaling.
# Run through strict RCH at a reviewed source revision. Scripted envelopes do
# not qualify Rust extraction, a real project capture or physical-Mac UI work.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-project-links.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
set -- "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasIndexedSearch.swift" "$src/AtlasIndexedSearchWire.swift" "$src/AtlasIndexedSource.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" "$src/AtlasReaderDocument.swift" \
    "$src/AtlasReaderTextSelection.swift" "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/AtlasProjectLinks.swift" "$src/AtlasProjectLinksModel.swift" "$src/AtlasProjectLinksReader.swift" \
    "$src/tests/AtlasIndexedSearchFixture.swift" "$src/tests/AtlasProjectLinksFixture.swift"
swiftc "$@" "$src/tests/AtlasProjectLinksTests.swift" -o "$out/model-tests"
"$out/model-tests"
cc -std=c11 -Wall -Wextra -Werror -I"$src/../../../crates/fcb-bridge/include" \
    -c "$src/tests/AtlasProjectLinksFixtures.c" -o "$out/links-fixture.o"
swiftc "$@" "$src/AtlasProjectLinksBridge.swift" "$src/tests/AtlasProjectLinksBridgeTests.swift" \
    "$out/links-fixture.o" -o "$out/bridge-tests"
"$out/bridge-tests"
printf 'Retained test artifacts: %s\n' "$out"
