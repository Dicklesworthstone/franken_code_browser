#!/bin/sh
# Bounded temporary-reader ownership, concurrency and failed-close recovery.
# Run by the independent verifier through the repository's strict-RCH lane.
# This portable lifecycle suite does not qualify native rendering or Rust I/O.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-parcel-markdown-retirement.XXXXXX")
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasParcelMarkdownRetirement.swift" \
    "$src/tests/AtlasParcelMarkdownRetirementTests.swift" -o "$out/retirement"
"$out/retirement"
printf 'Retained test artifacts: %s\n' "$out"
