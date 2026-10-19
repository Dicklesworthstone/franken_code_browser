#!/bin/sh
# Exact retained-pair integration over the production Swift models, worker lane,
# and decoders. Invoke through strict RCH at the source revision under review.
# The injected transport does not qualify the actual Rust ABI or native Mac UI;
# exported Rust C tests and the supplied-reader Mac bridge fixture own that seam.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-reader-comparison.XXXXXX")
set --
# Match the portable document suite's Linux Observation runtime handling.
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderDocument.swift" \
    "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/AtlasReaderComparisonModel.swift" "$src/tests/AtlasReaderComparisonTests.swift" \
    -o "$out/reader-comparison-tests"
"$out/reader-comparison-tests"
printf 'Retained test artifacts: %s\n' "$out"
