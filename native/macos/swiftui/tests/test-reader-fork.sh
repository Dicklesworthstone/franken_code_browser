#!/bin/sh
# Exact retained-reader fork integration over the production Swift models and
# decoders. Invoke through strict RCH at the source revision under review.
# This injected C transport does not qualify the Swift-to-Rust ABI or Mac UI.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-reader-fork.XXXXXX")
set --
# Match the portable document suite's Linux Observation runtime handling.
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderDocument.swift" \
    "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/tests/AtlasReaderForkTests.swift" -o "$out/reader-fork-tests"
"$out/reader-fork-tests"
printf 'Retained test artifacts: %s\n' "$out"
