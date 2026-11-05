#!/bin/sh
# Production native-host admission and worker tests. OS security-scoped
# bookmark behavior still requires the sandboxed Mac application lane.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-root-access.XXXXXX")
python3 - "$src" "$out/Cancellation.swift" <<'PY'
from pathlib import Path
import sys
source = (Path(sys.argv[1]) / 'AtlasSearchCoordinator.swift').read_text()
Path(sys.argv[2]).write_text(source[:source.index('/// Immutable after admission.')])
PY
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors "$out/Cancellation.swift" \
    "$src/AtlasProjectIO.swift" "$src/AtlasRootAccessLoader.swift" \
    "$src/tests/AtlasRootAccessLoaderTests.swift" -o "$out/root-access"
"$out/root-access"
printf 'Retained test artifacts: %s\n' "$out"
