#!/bin/sh
# Portable production worker/codec checks at an injected transport boundary.
# No filesystem traversal, Rust execution or physical-Mac qualification claimed.
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
    "$src/tests/AtlasFileFinderTests.swift" -o "$out/tests"
"$out/tests"
printf 'Retained test artifacts: %s\n' "$out"
