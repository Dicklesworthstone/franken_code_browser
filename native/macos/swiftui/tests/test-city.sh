#!/bin/sh
# Run through the independent strict-RCH verifier on a physical Mac. This
# compiles the real City wire/worker/model/Metal/control sources and exercises
# injected transport scheduling. It is NOT a drawable/GPU/visual qualification.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-city.XXXXXX")
# Reuse the production retirement implementation exactly, without dragging the
# unrelated source/Markdown graph into this focused native scheduling lane.
python3 - "$src/AtlasDocument.swift" "$out/NativeRetirement.swift" <<'PY'
from pathlib import Path
import sys
text = Path(sys.argv[1]).read_text()
Path(sys.argv[2]).write_text(text[:text.index('enum Monokai {')])
PY
clang -std=c11 -Wall -Wextra -Werror -I"$src/../../../crates/fcb-bridge/include" \
    -c "$src/tests/AtlasCityFixture.c" -o "$out/fixture.o"
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasAppearance.swift" \
    "$out/NativeRetirement.swift" "$src/AtlasProjectCatalog.swift" \
    "$src/AtlasCity.swift" "$src/AtlasCityModel.swift" \
    "$src/AtlasCityMetalView.swift" "$src/AtlasCityView.swift" \
    "$src/tests/AtlasCityTests.swift" "$out/fixture.o" -o "$out/city-tests"
"$out/city-tests"
printf 'Retained City regression artifacts: %s\n' "$out"
