#!/bin/sh
# Actual native worker/model/CoreText preparation with injected immutable source
# packets. No Rust engine execution or window/GPU performance claim.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-viewport-residency.XXXXXX")
python3 - "$src" "$out/Support.swift" <<'PY'
from pathlib import Path
import sys
src = Path(sys.argv[1])
search = (src / 'AtlasSearchCoordinator.swift').read_text()
io = (src / 'AtlasProjectIO.swift').read_text()
Path(sys.argv[2]).write_text(search[:search.index('/// Immutable after admission.')]
    + io[io.index('enum AtlasProjectIOError:'):io.index('/// Only immutable input')])
PY
clang -std=gnu11 -Wall -Wextra -Werror -c "$src/tests/AtlasViewportFixture.c" -o "$out/fixture.o"
swiftc -swift-version 6 -warnings-as-errors \
    "$out/Support.swift" "$src/AtlasProjectCatalog.swift" "$src/AtlasProjectWorker.swift" \
    "$src/AtlasSource.swift" "$src/AtlasPreparedText.swift" "$src/AtlasDocument.swift" "$src/AtlasParcelLayout.swift" \
    "$src/AtlasViewportWorker.swift" "$src/AtlasViewportResidency.swift" \
    "$src/tests/AtlasViewportResidencyTests.swift" "$out/fixture.o" -o "$out/viewport-residency"
"$out/viewport-residency"
printf 'Retained test artifacts: %s\n' "$out"
