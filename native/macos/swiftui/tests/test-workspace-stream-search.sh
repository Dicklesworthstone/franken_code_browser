#!/bin/sh
# Production wire/model/ABI fixtures. Run only through the independent strict
# verifier at an exact revision; these do not qualify physical Mac rendering.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-workspace-stream.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasSource.swift" "$src/AtlasObservedReader.swift" "$src/AtlasObservedReaderSearch.swift" \
    "$src/AtlasObservedReaderModel.swift" "$src/AtlasWorkspaceStream.swift" "$src/AtlasWorkspaceStreamModel.swift" \
    "$src/tests/AtlasObservedReaderFixture.swift" "$src/tests/AtlasWorkspaceStreamTests.swift" -o "$out/model"
"$out/model"
clang -std=gnu11 -Wall -Wextra -Werror -c "$src/tests/AtlasWorkspaceStreamBridgeFixtures.c" -o "$out/fixture.o"
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/AtlasSource.swift" "$src/AtlasObservedReader.swift" "$src/AtlasObservedReaderSearch.swift" \
    "$src/AtlasWorkspaceStream.swift" "$src/AtlasWorkspaceStreamBridge.swift" \
    "$src/tests/AtlasWorkspaceStreamBridgeTests.swift" "$out/fixture.o" -o "$out/bridge"
"$out/bridge"
printf 'Retained test artifacts: %s\n' "$out"
