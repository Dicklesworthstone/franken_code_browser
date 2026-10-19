#!/bin/sh
# Portable model/wire tests. Execute through the independent strict-RCH verifier;
# injected transport is not native Mac/ABI qualification.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-reading-desk.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasReadingDesk.swift" "$src/AtlasReadingDeskModel.swift" \
    "$src/tests/AtlasReadingDeskTests.swift" -o "$out/reading-desk-tests"
"$out/reading-desk-tests"
printf 'Retained test artifacts: %s\n' "$out"
