#!/bin/sh
# Filesystem fixtures for the independent strict-RCH verifier. No native Mac,
# Rust checkpoint ABI, or power-loss durability qualification is claimed here.
set -eu
src=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=$(mktemp -d "${TMPDIR:-/tmp}/fcb-desk-recovery-store.XXXXXX")
set --
if [ "$(uname -s)" = Linux ]; then set -- -static-stdlib; fi
swiftc "$@" -swift-version 6 -warnings-as-errors \
    "$src/AtlasReadingDesk.swift" "$src/AtlasDeskRecovery.swift" \
    "$src/tests/AtlasDeskRecoveryStoreTests.swift" -o "$out/desk-recovery-store-tests"
"$out/desk-recovery-store-tests"
printf 'Retained test artifacts: %s\n' "$out"
