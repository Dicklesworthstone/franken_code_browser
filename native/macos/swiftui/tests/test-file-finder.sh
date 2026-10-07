#!/bin/sh
# Portable production path worker, C marshaling and panel-state checks.
# Fixed transports/C symbols are not Rust or physical-Mac qualification.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-file-finder.XXXXXX")
python3 - "$src" "$out/Cancellation.swift" <<'PY'
from pathlib import Path
import sys
source = (Path(sys.argv[1]) / 'AtlasSearchCoordinator.swift').read_text()
start = source.index('final class AtlasSearchCancellation:')
end = source.index('\n/// Immutable after admission.', start)
Path(sys.argv[2]).write_text('import Foundation\n' + source[start:end])
PY
swiftc -swift-version 6 -warnings-as-errors "$out/Cancellation.swift" \
    "$src/AtlasFileFinder.swift" "$src/tests/AtlasFileFinderFixture.swift" \
    "$src/tests/AtlasFileFinderTests.swift" -o "$out/worker-tests"
"$out/worker-tests"
clang -std=gnu11 -Wall -Wextra -Werror \
    -c "$src/tests/AtlasFileFinderBridgeFixtures.c" -o "$out/bridge-fixtures.o"
swiftc -swift-version 6 -warnings-as-errors "$out/Cancellation.swift" \
    "$src/AtlasFileFinder.swift" "$src/AtlasFileFinderBridge.swift" \
    "$src/tests/AtlasFileFinderBridgeTests.swift" "$out/bridge-fixtures.o" -o "$out/bridge-tests"
"$out/bridge-tests"
set --
# Use the shipped static runtime when Linux's shared Observation runtime lacks
# its threading symbol; do not remove production observation annotations.
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors "$out/Cancellation.swift" \
    "$src/AtlasProjectIO.swift" "$src/AtlasFileFinder.swift" "$src/AtlasQuickOpenModel.swift" \
    "$src/tests/AtlasFileFinderFixture.swift" "$src/tests/AtlasQuickOpenTests.swift" -o "$out/panel-tests"
"$out/panel-tests"
printf 'Retained test artifacts: %s\n' "$out"
