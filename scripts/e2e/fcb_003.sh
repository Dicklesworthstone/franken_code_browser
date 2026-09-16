#!/bin/bash
# FCB-003.V production verification scenario: First-party macOS object/ABI
# ownership kernel in the separate `franken_macos` repository.
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_003.sh [cargo-test-args...]
#
# Environment:
#   FCB_003_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-003-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-003/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_003_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_003_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-003/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-003] route: upstream extension ledger + headless cargo test (fcb/fcb_003_production)"
echo "[fcb-003] run id: $RUN_ID"

# 1. Upstream extension ledger verification
echo "[fcb-003] checking upstream extension ledger..."
EXTRA_REPOS=()
if [ -d "/Users/jemanuel/projects/franken_macos" ]; then
    EXTRA_REPOS+=(--repo "franken_macos=/Users/jemanuel/projects/franken_macos")
fi
if [ -d "/Users/jemanuel/projects/franken_manim" ]; then
    EXTRA_REPOS+=(--repo "franken_manim=/Users/jemanuel/projects/franken_manim")
fi

set +e
python3 scripts/extension_ledger.py \
    --ledger scripts/upstream_extension_ledger.json \
    "${EXTRA_REPOS[@]}"
LEDGER_EXIT=$?
set -e

if [ "$LEDGER_EXIT" -ne 0 ] && [ "$LEDGER_EXIT" -ne 3 ]; then
    echo "[fcb-003] extension ledger verification rejected with exit code $LEDGER_EXIT"
    exit "$LEDGER_EXIT"
fi

# 2. Headless production verification suite
echo "[fcb-003] running fcb_003_production test suite..."
cargo test --manifest-path crates/fcb/Cargo.toml \
    --test fcb_003_production "$@"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-003-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-003] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-003] PASS (all required routes executed)"
