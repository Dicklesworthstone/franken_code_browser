#!/bin/sh
# Production intake/routing and reader fixtures. Run through strict RCH in the
# independent verification lane; this does not qualify AppKit or sandbox access.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-native-open.XXXXXX")
python3 - "$src" "$out/Cancellation.swift" <<'PY'
from pathlib import Path
import sys
source = (Path(sys.argv[1]) / 'AtlasSearchCoordinator.swift').read_text()
Path(sys.argv[2]).write_text(source[:source.index('/// Immutable after admission.')])
PY
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors "$out/Cancellation.swift" \
    "$src/AtlasProjectIO.swift" "$src/AtlasRootAccessLoader.swift" "$src/AppStoreRootAccess.swift" \
    "$src/AtlasNativeOpen.swift" "$src/tests/AtlasNativeOpenTests.swift" -o "$out/native-open"
"$out/native-open"
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" "$src/AtlasReaderDocument.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/AtlasReaderTextSelection.swift" "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/tests/AtlasReaderDocumentFixture.swift" "$src/tests/AtlasSelectedFileReaderTests.swift" -o "$out/selected-file-reader"
"$out/selected-file-reader"
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/AtlasProjectIO.swift" "$src/AtlasRootAccessLoader.swift" "$src/AppStoreRootAccess.swift" \
    "$src/AtlasSource.swift" "$src/AtlasNativeOpen.swift" \
    "$src/AtlasObservedReader.swift" "$src/AtlasObservedReaderSearch.swift" "$src/AtlasObservedReaderModel.swift" \
    "$src/tests/AtlasObservedReaderFixture.swift" "$src/tests/AtlasSelectedRangeReaderTests.swift" -o "$out/selected-range-reader"
"$out/selected-range-reader"
printf 'Retained native-open fixture artifacts: %s\n' "$out"
