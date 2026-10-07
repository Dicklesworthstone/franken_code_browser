#!/bin/sh
# Production source ownership and handoff validation against fixed envelopes.
# This is not a Rust ABI execution or physical-Mac qualification.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-captured-search.XXXXXX")
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/tests/AtlasSearchCaptureTests.swift" -o "$out/capture-tests"
"$out/capture-tests"
printf 'Retained test artifacts: %s\n' "$out"
