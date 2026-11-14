#!/bin/sh
# Native retained-reader keyboard and focus regressions. Invoke through strict
# RCH at the reviewed source revision on a physical Mac.
set -eu
if [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'Reader command tests require native AppKit on a physical Mac.' >&2
    exit 2
fi
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-reader-commands.XXXXXX")
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasAppearance.swift" "$src/AtlasSource.swift" "$src/AtlasSourceReader.swift" \
    "$src/tests/AtlasReaderCommandTests.swift" -o "$out/reader-command-tests"
"$out/reader-command-tests" >"$out/positive.log" 2>&1
cat "$out/positive.log"
if "$out/reader-command-tests" --negative-control >"$out/negative-control.log" 2>&1; then
    printf '%s\n' 'Reader command negative control unexpectedly passed.' >&2
    exit 1
fi
if ! grep -q 'FAIL AtlasReaderCommands: all three commands belong to the focused pin' "$out/negative-control.log"; then
    cat "$out/negative-control.log" >&2
    printf '%s\n' 'Negative control failed before reaching the keyboard ownership oracle.' >&2
    exit 1
fi
printf 'Retained native test artifacts: %s\n' "$out"
