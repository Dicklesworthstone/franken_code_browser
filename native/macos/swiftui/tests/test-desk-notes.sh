#!/bin/sh
# Source fixtures for the independent strict-RCH verifier. This script is not
# native Mac/ABI qualification and is not executed during the code-first wave.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-desk-notes.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasReadingDesk.swift" "$src/AtlasReadingDeskModel.swift" \
    "$src/AtlasDeskNoteDraft.swift" "$src/tests/AtlasDeskNoteTests.swift" -o "$out/desk-note-tests"
"$out/desk-note-tests"
printf 'Retained test artifacts: %s\n' "$out"
