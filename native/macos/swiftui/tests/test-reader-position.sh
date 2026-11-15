#!/bin/sh
# Position protocol/source-mapping/navigation regressions. Invoke through strict
# RCH at the reviewed source revision; this does not execute the Rust engine.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-reader-position.XXXXXX")
for suite in reader-position reader-startup workspace-history; do
    set --
    if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
    if [ "$(uname -s)" = Darwin ]; then set -- "$@" "$src/AtlasAppearance.swift" "$src/AtlasSourceReader.swift"; fi
    if [ "$suite" = reader-position ]; then
        set -- "$@" "$src/tests/AtlasReaderPositionTests.swift"
    elif [ "$suite" = reader-startup ]; then
        set -- "$@" "$src/AtlasProjectCatalog.swift" "$src/../tests/AtlasReaderRefreshFixture.swift" \
            "$src/../tests/AtlasReaderStartupRegressionTests.swift"
    else
        set -- "$@" "$src/AtlasDocumentLinkHistory.swift" "$src/AtlasWorkspaceReaderHistory.swift" \
            "$src/../tests/AtlasReaderRefreshFixture.swift" "$src/../tests/AtlasWorkspaceReaderHistoryTests.swift"
    fi
    swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" "$src/AtlasReaderDocument.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/AtlasReaderTextSelection.swift" "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    -o "$out/$suite-tests"
    "$out/$suite-tests"
done
printf 'Retained test artifacts: %s\n' "$out"
