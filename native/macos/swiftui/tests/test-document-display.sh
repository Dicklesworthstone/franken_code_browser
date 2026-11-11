#!/bin/sh
# Run through the independent strict-RCH lane. These production decoder checks
# do not substitute for Rust shaping or a physical-Mac visual test.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d)
python3 - "$src" "$out/Vocabulary.swift" <<'PY'
from pathlib import Path
import sys
src = Path(sys.argv[1])
reader = (src / 'AtlasReader.swift').read_text()
document = (src / 'AtlasReaderDocument.swift').read_text()
parts = [reader[:reader.index('enum AtlasReaderRequest:')],
         reader[reader.index('struct AtlasReaderIdentity:'):reader.index('struct AtlasReaderInfo:')],
         reader[reader.index('enum AtlasReaderWire {'):],
         document[:document.index('/// Immutable worker descriptor.')]]
Path(sys.argv[2]).write_text('\n'.join(parts))
PY
swiftc -swift-version 6 -warnings-as-errors \
    "$out/Vocabulary.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/tests/AtlasDocumentDisplayTests.swift" -o "$out/display-tests"
"$out/display-tests"
printf 'Retained display decoder artifacts: %s\n' "$out"
