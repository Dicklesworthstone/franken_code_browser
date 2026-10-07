#!/bin/sh
# UI-independent presentation logic over the production document value types.
# This does not exercise the Rust decoder/FFI or qualify native macOS rendering.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-document-preview.XXXXXX")
# Retain outputs for diagnosis; do not remove files from a shared workspace.
python3 - "$src" "$out/Vocabulary.swift" <<'PY'
from pathlib import Path
import sys
src = Path(sys.argv[1])
reader = (src / 'AtlasReader.swift').read_text()
document = (src / 'AtlasReaderDocument.swift').read_text()
# Extract unchanged declarations, not stub readers or alternate implementations.
# Full reader/decoder/coordinator integration has its own suites. These tests
# inject typed coordinator results at the presentation boundary only.
parts = [reader[:reader.index('enum AtlasReaderRequest:')],
         reader[reader.index('struct AtlasReaderIdentity:'):reader.index('struct AtlasReaderInfo:')],
         reader[reader.index('enum AtlasReaderWire {'):],
         document[:document.index('/// Immutable worker descriptor.')]]
Path(sys.argv[2]).write_text('\n'.join(parts))
PY
set --
# Swift 6.2.1's Linux shared Observation runtime can miss a threading symbol;
# its shipped static runtime provides it without changing observable source.
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$out/Vocabulary.swift" "$src/AtlasDocumentPreviewModel.swift" \
    "$src/tests/AtlasDocumentPreviewTests.swift" -o "$out/tests"
"$out/tests"
printf 'Retained test artifacts: %s\n' "$out"
