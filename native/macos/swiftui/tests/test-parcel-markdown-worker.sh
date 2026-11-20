#!/bin/sh
# Actual packet worker, document jobs, wire decoders and retirement over fixed
# supplied-byte vectors. No upstream parser/font/renderer execution is claimed.
# Run through the repository's independent strict-RCH verification lane.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-parcel-markdown-worker.XXXXXX")
python3 - "$src" "$out" <<'PY'
from pathlib import Path
import sys
src, out = Path(sys.argv[1]), Path(sys.argv[2])
reader = (src / 'AtlasReader.swift').read_text()
# Keep actual capture admission and info decoders without pulling UI or FFI.
parts = [reader[:reader.index('enum AtlasReaderRequest:')],
         reader[reader.index('struct AtlasReaderIdentity:'):reader.index('/// Styling is derived by the shared lexer')],
         reader[reader.index('enum AtlasReaderWire {'):]]
(out / 'ReaderVocabulary.swift').write_text('\n'.join(parts))
# Share the independently known BOM/NUL/Unicode/newline digest vectors.
supplied = (src / 'tests/AtlasSuppliedReaderFixture.swift').read_text()
(out / 'SuppliedVectors.swift').write_text(supplied[:supplied.index('final class AtlasSuppliedReaderFixture:')])
PY
swiftc -swift-version 6 -warnings-as-errors \
    "$out/ReaderVocabulary.swift" "$out/SuppliedVectors.swift" \
    "$src/AtlasReaderDocument.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/AtlasParcelMarkdownRetirement.swift" "$src/AtlasParcelMarkdownWorker.swift" \
    "$src/tests/AtlasParcelMarkdownWorkerTests.swift" -o "$out/parcel-markdown-worker"
"$out/parcel-markdown-worker"
printf 'Retained parcel Markdown worker artifacts: %s\n' "$out"
