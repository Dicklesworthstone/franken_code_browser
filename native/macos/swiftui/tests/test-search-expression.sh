#!/bin/sh
# Production expression codecs/workers with scripted C/Rust boundary responses.
# This runner does not qualify Rust extraction or physical native presentation.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-search-expression.XXXXXX")
clang -std=gnu11 -Wall -Wextra -Werror \
    -c "$src/tests/AtlasNativeSearchFixtures.c" -o "$out/live-fixture.o"
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasSearchWorker.swift" \
    "$src/AtlasIndexedSearchWire.swift" "$src/AtlasIndexedSource.swift" "$src/AtlasIndexedSearch.swift" \
    "$src/tests/AtlasIndexedSearchFixture.swift" "$src/tests/AtlasSearchExpressionTests.swift" \
    "$out/live-fixture.o" -o "$out/expression"
"$out/expression"
printf 'Retained test artifacts: %s\n' "$out"
