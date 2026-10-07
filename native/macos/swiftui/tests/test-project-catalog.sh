#!/bin/sh
# Actual catalog decoder/adapter with fixed C symbols, not Rust execution or a
# macOS UI qualification. Keep outputs for diagnosis. No source parser stubs.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-project-catalog.XXXXXX")
swiftc -swift-version 6 -warnings-as-errors "$src/AtlasProjectCatalog.swift" \
    "$src/tests/AtlasProjectCatalogTests.swift" -o "$out/catalog"
"$out/catalog"
python3 - "$src" "$out/Support.swift" <<'PY'
from pathlib import Path
import sys
src = Path(sys.argv[1])
io = (src / 'AtlasProjectIO.swift').read_text()
search = (src / 'AtlasSearchCoordinator.swift').read_text()
# Unchanged production cancellation/error declarations only. Tests exercise
# the complete catalog/worker source, not the unrelated scheduling/UI modules.
Path(sys.argv[2]).write_text(search[:search.index('/// Immutable after admission.')]
    + io[io.index('enum AtlasProjectIOError:'):io.index('/// Only immutable input')])
PY
clang -std=gnu11 -Wall -Wextra -Werror -c "$src/tests/AtlasProjectCatalogFixture.c" -o "$out/fixture.o"
swiftc -swift-version 6 -warnings-as-errors "$out/Support.swift" "$src/AtlasProjectCatalog.swift" \
    "$src/AtlasProjectWorker.swift" "$src/tests/AtlasProjectCatalogBridgeTests.swift" "$out/fixture.o" -o "$out/bridge"
"$out/bridge"
printf 'Retained test artifacts: %s\n' "$out"
python3 - "$src" "$out/Context.swift" <<'PY'
from pathlib import Path
import sys
text = (Path(sys.argv[1]) / 'AtlasSearch.swift').read_text()
start = text.index('struct AtlasSearchContext:')
end = text.index('/// Hit indices are only unique', start)
Path(sys.argv[2]).write_text('import Foundation\n' + text[start:end])
PY
swiftc -swift-version 6 -warnings-as-errors "$out/Context.swift" "$src/AtlasProjectCatalog.swift" \
    "$src/AtlasProjectOpening.swift" "$src/tests/AtlasProjectOpeningTests.swift" -o "$out/opening"
"$out/opening"
