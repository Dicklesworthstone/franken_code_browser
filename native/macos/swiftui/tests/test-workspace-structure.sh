#!/bin/sh
# Run through strict RCH at an exact reviewed revision. Scripted native receipts
# qualify wire admission, ownership and model transitions, not Rust extraction,
# live discovery, real source hashing, macOS rendering or hardware behavior.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-workspace-structure.XXXXXX")
# Reuse the production source/grant value and model verbatim. The excluded
# presentation factory selects real optional bridges; these fixtures inject
# transports into the same model and retained-reader coordinator instead.
python3 - "$src" "$out" <<'PY'
import pathlib
import sys
src, out = map(pathlib.Path, sys.argv[1:])
stream = (src / "AtlasWorkspaceStreamModel.swift").read_text()
start = stream.index("struct AtlasWorkspaceStreamSourceIdentity:")
end = stream.index("/// The project owner can revoke", start)
(out / "Source.swift").write_text("import Foundation\n" + stream[start:end])
model = (src / "AtlasWorkspaceStructureModel.swift").read_text()
start = model.index("@MainActor final class AtlasWorkspaceStructurePresentation:")
end = model.index("/// A copied result reader", start)
(out / "Model.swift").write_text(model[:start] + model[end:])
PY
cc -std=c11 -Wall -Wextra -Werror -I"$src/../../../crates/fcb-bridge/include" \
    -c "$src/tests/AtlasWorkspaceStructureFixtures.c" -o "$out/structure-fixture.o"
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
set -- "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasSearch.swift" "$src/AtlasSearchExpression.swift" "$src/AtlasSearchCapture.swift" \
    "$src/AtlasSearchCoordinator.swift" "$src/AtlasProjectIO.swift" \
    "$src/AtlasIndexedSearch.swift" "$src/AtlasIndexedSearchWire.swift" "$src/AtlasIndexedSource.swift" \
    "$src/AtlasReader.swift" "$src/AtlasReaderSearch.swift" "$src/AtlasReaderComparison.swift" \
    "$src/AtlasReaderOutline.swift" "$src/AtlasReaderReferences.swift" "$src/AtlasReaderTextSelection.swift" \
    "$src/AtlasReaderDocument.swift" "$src/AtlasDocumentDisplay.swift" \
    "$src/AtlasReaderCoordinator.swift" "$src/AtlasCapturedReader.swift" "$src/AtlasSuppliedReader.swift" \
    "$src/AtlasNavigationHistory.swift" "$src/AtlasSource.swift" "$src/AtlasProjectCatalog.swift" \
    "$src/AtlasDocumentPreviewModel.swift" "$src/AtlasPagedReaderModel.swift" \
    "$src/AtlasWorkspaceStructure.swift" "$src/AtlasWorkspaceStructureBridge.swift"
swiftc "$@" "$out/Source.swift" "$out/Model.swift" \
    "$src/tests/AtlasWorkspaceStructureFixture.swift" "$src/tests/AtlasWorkspaceStructureTests.swift" \
    "$out/structure-fixture.o" -o "$out/model-tests"
"$out/model-tests"
swiftc "$@" "$src/tests/AtlasWorkspaceStructureBridgeTests.swift" \
    "$out/structure-fixture.o" -o "$out/bridge-tests"
"$out/bridge-tests"
printf 'Retained test artifacts: %s\n' "$out"

