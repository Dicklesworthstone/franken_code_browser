#!/bin/sh
# Source fixtures for the independent strict-RCH verifier. Pass --native-window
# on a Mac to additionally exercise AppKit delegate forwarding. Neither mode
# qualifies native rendering, the C ABI, or physical-Mac performance.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-desk-close.XXXXXX")
if [ "$(uname -s)" = Linux ]; then
    swiftc -static-stdlib -swift-version 6 -warnings-as-errors \
        "$src/AtlasReadingDesk.swift" "$src/AtlasReadingDeskModel.swift" \
        "$src/AtlasDeskCloseProtection.swift" "$src/tests/AtlasDeskCloseProtectionTests.swift" \
        -o "$out/desk-close-tests"
else
    swiftc -swift-version 6 -warnings-as-errors \
        "$src/AtlasReadingDesk.swift" "$src/AtlasReadingDeskModel.swift" \
        "$src/AtlasDeskCloseProtection.swift" "$src/tests/AtlasDeskCloseProtectionTests.swift" \
        -o "$out/desk-close-tests"
fi
"$out/desk-close-tests" "$@"
printf 'Retained test artifacts: %s\n' "$out"
