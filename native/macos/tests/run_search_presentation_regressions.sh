#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
build_dir="${FCB_TEST_OUTPUT_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/fcb-search-presentation.XXXXXX")}"
mkdir -p "$build_dir"
swiftc -swift-version 6 -warnings-as-errors \
  "$root/native/macos/swiftui/AtlasSearch.swift" \
  "$root/native/macos/tests/AtlasSearchPresentationRegressionTests.swift" \
  -o "$build_dir/search-presentation-tests"
"$build_dir/search-presentation-tests"
# These are syntax guards only, not a SwiftUI/AppKit typecheck or macOS build.
swiftc -frontend -parse "$root/native/macos/swiftui/App.swift"
swiftc -frontend -parse -D FCB_APP_STORE "$root/native/macos/swiftui/App.swift"
printf 'SwiftUI syntax guards passed for both distributions (not a macOS build).\n'
printf 'Test executable retained at %s\n' "$build_dir/search-presentation-tests"
