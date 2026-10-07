#!/bin/sh
# Portable host scheduling + fixed C-boundary tests, not macOS/Rust qualification.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/fcb-progressive-search.XXXXXX")
cleanup() {
    rm -f "$tmp/coordinator" "$tmp/worker" "$tmp/fixture.o"
    rmdir "$tmp"
}
trap cleanup EXIT HUP INT TERM
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/tests/AtlasSearchProgressTests.swift" -o "$tmp/coordinator"
"$tmp/coordinator"
clang -std=gnu11 -Wall -Wextra -Werror \
    -c "$src/tests/AtlasNativeSearchFixtures.c" -o "$tmp/fixture.o"
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCoordinator.swift" \
    "$src/AtlasSearchWorker.swift" "$src/tests/AtlasNativeSearchTests.swift" \
    "$tmp/fixture.o" -o "$tmp/worker"
"$tmp/worker"
