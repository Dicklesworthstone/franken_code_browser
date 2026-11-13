#!/bin/sh
# Production native ownership/history lifecycle with injected accepted readers.
# This does not qualify project-link resolution, the C ABI, or physical Mac UI.
# Invoke through strict RCH at the exact revision under independent review.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-document-link-history.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasDocumentLinkHistory.swift" "$src/tests/AtlasDocumentLinkHistoryTests.swift" \
    -o "$out/document-link-history"
"$out/document-link-history"
printf 'Retained test artifacts: %s\n' "$out"
