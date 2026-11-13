#!/bin/sh
# Run in the strict RCH independent verification lane. These injected boundary
# fixtures do not launch applications or qualify native AppKit/sandbox behavior.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-editor-handoff.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasEditorHandoff.swift" "$src/tests/AtlasEditorHandoffTests.swift" -o "$out/editor-handoff"
"$out/editor-handoff"
printf 'Retained editor-handoff fixture artifacts: %s\n' "$out"
