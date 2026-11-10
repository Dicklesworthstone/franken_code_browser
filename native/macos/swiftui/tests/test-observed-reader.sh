#!/bin/sh
# Production observed-range wire/model and ABI adapters with injected receipts.
# Run only through the independent strict-RCH verifier at the exact revision.
# These tests do not qualify filesystem policy, native shaping or physical Mac UI.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-observed-reader.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasSource.swift" "$src/AtlasObservedReader.swift" "$src/AtlasObservedReaderSearch.swift" "$src/AtlasObservedReaderModel.swift" \
    "$src/tests/AtlasObservedReaderFixture.swift" "$src/tests/AtlasObservedReaderTests.swift" -o "$out/model"
"$out/model"
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasSource.swift" "$src/AtlasObservedReader.swift" "$src/AtlasObservedReaderSearch.swift" "$src/AtlasObservedReaderModel.swift" \
    "$src/tests/AtlasObservedReaderFixture.swift" "$src/tests/AtlasObservedReaderSearchTests.swift" -o "$out/search"
"$out/search"
clang -std=gnu11 -Wall -Wextra -Werror -c "$src/tests/AtlasObservedReaderBridgeFixtures.c" -o "$out/fixture.o"
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/AtlasSource.swift" "$src/AtlasObservedReader.swift" "$src/AtlasObservedReaderSearch.swift" "$src/AtlasObservedReaderBridge.swift" \
    "$src/tests/AtlasObservedReaderBridgeTests.swift" "$out/fixture.o" -o "$out/bridge"
"$out/bridge"
printf 'Retained test artifacts: %s\n' "$out"
