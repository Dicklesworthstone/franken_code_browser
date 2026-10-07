#!/bin/sh
# Portable host scheduling + fixed C-boundary tests, not macOS/Rust qualification.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/fcb-progressive-search.XXXXXX")
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/tests/AtlasSearchProgressTests.swift" -o "$tmp/coordinator"
"$tmp/coordinator"
clang -std=gnu11 -Wall -Wextra -Werror \
    -c "$src/tests/AtlasNativeSearchFixtures.c" -o "$tmp/fixture.o"
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasSearchWorker.swift" \
    "$src/tests/AtlasNativeSearchTests.swift" "$tmp/fixture.o" -o "$tmp/worker"
"$tmp/worker"
printf 'Retained test artifacts: %s\n' "$tmp"
