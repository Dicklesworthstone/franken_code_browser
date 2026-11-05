#!/bin/sh
# Actual change-admission model and bounded mailbox; actual FSEvents only on
# macOS. Retain all artifacts. Sandbox grants require the native app lane.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-project-changes.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors "$src/AtlasProjectChanges.swift" \
    "$src/tests/AtlasProjectChangesTests.swift" -o "$out/project-changes"
"$out/project-changes"
if [ "$(uname -s)" = Darwin ]; then
    swiftc -swift-version 6 -warnings-as-errors "$src/AtlasProjectChanges.swift" \
        "$src/AtlasProjectChangesNative.swift" "$src/tests/AtlasProjectChangesNativeTests.swift" \
        -framework CoreServices -o "$out/native-project-changes"
    "$out/native-project-changes"
else
    printf 'Native FSEvents was not exercised; run this script on macOS for that evidence.\n'
fi
printf 'Retained test artifacts: %s\n' "$out"
