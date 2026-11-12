#!/bin/sh
# Execute only through the independent strict-RCH macOS lane. This fixture
# exercises ImageIO and Core Graphics; it does not claim a presented Mac frame.
set -eu
if [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'Native PNG/JPEG decoder fixtures require macOS ImageIO.' >&2
    exit 1
fi
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
    "$src/tests/AtlasDocumentImageTests.swift" -o "$out/image-tests"
"$out/image-tests"
printf 'Native image decoder artifacts: %s\n' "$out"
