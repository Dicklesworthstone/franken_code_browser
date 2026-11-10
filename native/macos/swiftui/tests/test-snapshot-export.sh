#!/bin/sh
# Production save-receipt/model and native ABI adapters with injected foreign
# results. Run through the independent strict-RCH verifier at the exact source
# revision; these fixtures do not qualify filesystem effects or physical Mac UI.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-snapshot-export.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/AtlasSnapshotExport.swift" "$src/AtlasSnapshotExportModel.swift" \
    "$src/tests/AtlasSnapshotExportTests.swift" -o "$out/model"
"$out/model"
clang -std=gnu11 -Wall -Wextra -Werror -c "$src/tests/AtlasSnapshotExportBridgeFixtures.c" -o "$out/fixture.o"
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/AtlasSnapshotExport.swift" "$src/AtlasSnapshotExportBridge.swift" \
    "$src/tests/AtlasSnapshotExportBridgeTests.swift" "$out/fixture.o" -o "$out/bridge"
"$out/bridge"
printf 'Retained test artifacts: %s\n' "$out"
