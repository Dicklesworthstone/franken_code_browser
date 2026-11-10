#!/bin/sh
# Production whole-token workspace codecs/worker with injected C responses.
# Retained reader activation is also covered by test-captured-search.sh.
# Run through the independent strict verifier; no native/Rust execution claim.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-workspace-occurrences.XXXXXX")
clang -std=gnu11 -Wall -Wextra -Werror \
    -c "$src/tests/AtlasNativeSearchFixtures.c" -o "$out/fixture.o"
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" "$src/AtlasSearchWorker.swift" \
    "$src/tests/AtlasWorkspaceOccurrenceTests.swift" "$out/fixture.o" -o "$out/occurrences"
"$out/occurrences"
printf 'Retained test artifacts: %s\n' "$out"
