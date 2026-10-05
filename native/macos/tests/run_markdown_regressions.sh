#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
OUT="${FCB_TEST_OUTPUT:-$(mktemp -d)}"
mkdir -p "$OUT"
printf 'Retaining Markdown test artifacts in %s\n' "$OUT"
swiftc -swift-version 6 -warnings-as-errors \
    "$ROOT/native/macos/swiftui/AtlasMarkdown.swift" \
    "$ROOT/native/macos/swiftui/AtlasMarkdownWorker.swift" \
    "$ROOT/native/macos/tests/AtlasMarkdownRegressionTests.swift" \
    -o "$OUT/markdown-regressions"
"$OUT/markdown-regressions"
