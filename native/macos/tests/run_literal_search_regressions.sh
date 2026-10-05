#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
build_dir="${FCB_TEST_OUTPUT_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/fcb-literal-search.XXXXXX")}"
mkdir -p "$build_dir"
swiftc -swift-version 6 -warnings-as-errors \
  "$root/native/macos/swiftui/AtlasSearch.swift" \
  "$root/native/macos/swiftui/AtlasSearchCoordinator.swift" \
  "$root/native/macos/tests/AtlasLiteralSearchRegressionTests.swift" \
  -o "$build_dir/literal-search-tests"
"$build_dir/literal-search-tests"
# Syntax checks cover both distribution branches; they do not link the app.
swiftc -frontend -parse "$root/native/macos/swiftui/App.swift"
swiftc -frontend -parse -D FCB_APP_STORE "$root/native/macos/swiftui/App.swift"
printf 'SwiftUI syntax guards passed for both distributions (not a macOS build).\n'
printf 'Test executable retained at %s\n' "$build_dir/literal-search-tests"
