#!/bin/sh
# Full native worker/codec and shared coordinator tests. Engine responses and
# C symbols are fixtures: this is not actual Rust or physical-Mac qualification.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-indexed-search.XXXXXX")
set -- "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/AtlasIndexedSearchWire.swift" "$src/AtlasIndexedSource.swift" "$src/AtlasIndexedSearch.swift"
swiftc -swift-version 6 -warnings-as-errors "$@" \
    "$src/tests/AtlasIndexedSearchFixture.swift" "$src/tests/AtlasIndexedSearchTests.swift" -o "$out/worker"
"$out/worker"
clang -std=gnu11 -Wall -Wextra -Werror -c "$src/tests/AtlasIndexedBridgeFixtures.c" -o "$out/fixture.o"
swiftc -swift-version 6 -warnings-as-errors "$@" "$src/AtlasIndexedSearchBridge.swift" \
    "$src/tests/AtlasIndexedBridgeTests.swift" "$out/fixture.o" -o "$out/bridge"
"$out/bridge"
swiftc -swift-version 6 -warnings-as-errors "$src/AtlasSearch.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/tests/AtlasIndexedSchedulingTests.swift" -o "$out/scheduling"
"$out/scheduling"
printf 'Retained test artifacts: %s\n' "$out"
