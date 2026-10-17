#!/bin/sh
# Host ownership tests only. Execute through the repository's strict remote
# verification lane; this is not native UI, Rust ABI or Metal qualification.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-pinned-readers.XXXXXX")
set --
# Use the shipped static Observation runtime on Linux, as in the reader suites.
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasPinnedReaders.swift" "$src/tests/AtlasPinnedReadersTests.swift" \
    -o "$out/pinned-reader-tests"
"$out/pinned-reader-tests"
printf 'Retained test artifacts: %s\n' "$out"
