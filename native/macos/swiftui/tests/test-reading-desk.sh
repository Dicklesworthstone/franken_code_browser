#!/bin/sh
# Portable model/wire tests. Execute through the independent strict-RCH verifier;
# injected transport is not native Mac/ABI qualification.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-reading-desk.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasReadingDesk.swift" "$src/AtlasDeskRecovery.swift" "$src/AtlasReadingDeskModel.swift" \
    "$src/tests/AtlasReadingDeskTests.swift" -o "$out/reading-desk-tests"
"$out/reading-desk-tests"
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasSource.swift" "$src/AtlasObservedReader.swift" "$src/AtlasObservedReaderSearch.swift" "$src/AtlasObservedReaderModel.swift" \
    "$src/AtlasReadingDesk.swift" "$src/AtlasDeskRecovery.swift" "$src/AtlasReadingDeskModel.swift" \
    "$src/AtlasDeskObservedReader.swift" "$src/AtlasDeskObservedNoteDraft.swift" \
    "$src/tests/AtlasObservedReaderFixture.swift" "$src/tests/AtlasDeskObservedPassagesTests.swift" -o "$out/observed-passages"
"$out/observed-passages"
clang -std=gnu11 -Wall -Wextra -Werror -c "$src/tests/AtlasDeskObservedBridgeFixtures.c" -o "$out/observed-fixture.o"
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasReadingDesk.swift" "$src/AtlasDeskRecovery.swift" "$src/AtlasReadingDeskModel.swift" \
    "$src/AtlasReadingDeskBridge.swift" "$src/tests/AtlasDeskObservedBridgeTests.swift" \
    "$out/observed-fixture.o" -o "$out/observed-bridge"
"$out/observed-bridge"
printf 'Retained test artifacts: %s\n' "$out"
