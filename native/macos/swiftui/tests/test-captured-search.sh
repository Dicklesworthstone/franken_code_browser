#!/bin/sh
# Production source ownership and typed reader activation, not real Rust ABI
# execution or physical-Mac qualification. Keep outputs for failure diagnosis.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-captured-search.XXXXXX")
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/tests/AtlasSearchCaptureTests.swift" -o "$out/capture-tests"
"$out/capture-tests"
# Compile the owning reader declarations directly: structural activation now
# returns a real reader request, and the transport carries comparison capability.
# The typed activation fixture still does not claim C ABI or native UI coverage.
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderDocument.swift" \
    "$src/AtlasReaderCoordinator.swift" \
    "$src/AtlasCapturedReader.swift" "$src/tests/AtlasCapturedReaderTests.swift" \
    -o "$out/reader-tests"
"$out/reader-tests"
printf 'Retained test artifacts: %s\n' "$out"
