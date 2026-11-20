#!/bin/sh
# Native adapter/CoreText/source geometry checks. These typed display fixtures
# do not substitute for upstream Markdown, physical-Mac pixels or frame cadence.
# Run through the repository's independent strict-RCH verification lane.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-atlas-document-display.XXXXXX")
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
clang -std=gnu11 -Wall -Wextra -Werror -c "$src/tests/AtlasViewportFixture.c" -o "$out/fixture.o"
swiftc -swift-version 6 -warnings-as-errors \
    "$out/Vocabulary.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/AtlasAppearance.swift" "$src/AtlasSource.swift" "$src/AtlasPreparedText.swift" \
    "$src/AtlasDocument.swift" "$src/AtlasProjectCatalog.swift" "$src/AtlasParcelLayout.swift" \
    "$src/AtlasMatch.swift" "$src/AtlasParcelDocumentDisplay.swift" \
    "$src/tests/AtlasParcelDocumentDisplayTests.swift" "$out/fixture.o" -o "$out/atlas-document-display"
"$out/atlas-document-display"
printf 'Retained atlas document-display artifacts: %s\n' "$out"
