#!/bin/sh
# Production source ownership and typed reader activation, not real Rust ABI
# execution or physical-Mac qualification. Keep outputs for failure diagnosis.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-captured-search.XXXXXX")
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchCapture.swift" \
    "$src/tests/AtlasSearchCaptureTests.swift" -o "$out/capture-tests"
"$out/capture-tests"
python3 - "$src" "$out/ReaderVocabulary.swift" <<'PY'
from pathlib import Path
import sys
src = Path(sys.argv[1])
reader = (src / 'AtlasReader.swift').read_text()
search = (src / 'AtlasReaderSearch.swift').read_text()
coordinator = (src / 'AtlasReaderCoordinator.swift').read_text()
# Preserve production transport/value/target logic unchanged. The activation
# test injects typed reader results; decoder/coordinator suites remain separate.
parts = [reader[:reader.index('enum AtlasReaderRequest:')],
         reader[reader.index('struct AtlasReaderIdentity:'):reader.index('struct AtlasReaderInfo:')],
         coordinator[coordinator.index('struct AtlasReaderTransport:'):coordinator.index('/// This reference')],
         search[:search.index('    static func decode(')] + '\n}\n']
Path(sys.argv[2]).write_text('\n'.join(parts))
PY
swiftc -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$out/ReaderVocabulary.swift" \
    "$src/AtlasCapturedReader.swift" "$src/tests/AtlasCapturedReaderTests.swift" \
    -o "$out/reader-tests"
"$out/reader-tests"
printf 'Retained test artifacts: %s\n' "$out"
