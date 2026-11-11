#!/bin/sh
# Production occurrence decoder and retained-reader model regressions. Execute
# through strict RCH at the reviewed source revision. Injected foreign responses
# do not qualify Rust extraction, the actual C ABI, or physical-Mac presentation.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-reader-references.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
set -- "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" "$src/AtlasReaderDocument.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/AtlasReaderTextSelection.swift" "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift"
# Each suite owns its @main entry point; compile them as separate executables.
swiftc "$@" "$src/tests/AtlasReaderReferencesTests.swift" -o "$out/codec-tests"
"$out/codec-tests"
swiftc "$@" "$src/tests/AtlasReaderReferencesModelTests.swift" -o "$out/model-tests"
"$out/model-tests"
printf 'Retained test artifacts: %s\n' "$out"
