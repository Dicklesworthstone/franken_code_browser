#!/bin/bash
# FCB-059 Hostile corpus and regression minimizer scenario runner
# (HOSTILE.source and HOSTILE.document regression lanes).
#
# Supported routes (documented invocation):
#   scripts/e2e/fcb_059.sh                  (runs all hostile regression lanes)
#   scripts/e2e/fcb_059.sh --lane source    (runs HOSTILE.source lane)
#   scripts/e2e/fcb_059.sh --lane document  (runs HOSTILE.document lane)
#   scripts/e2e/fcb_059.sh [cargo-test-args...]
#
# Environment:
#   FCB_059_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-059-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-059/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_059_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_059_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-059/$RUN_ID"
mkdir -p "$ARTIFACT_DIR/receipts"
export FCB_RECEIPTS_DIR="$REPO_ROOT/$ARTIFACT_DIR/receipts"

LANE=""
CARGO_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --lane)
            LANE="$2"
            shift 2
            ;;
        --lane=*)
            LANE="${1#*=}"
            shift 1
            ;;
        *)
            CARGO_ARGS+=("$1")
            shift 1
            ;;
    esac
done

echo "[fcb-059] route: headless cargo test (fcb-conformance)"
echo "[fcb-059] run id: $RUN_ID"

if [ "$LANE" = "source" ]; then
    echo "[fcb-059] executing HOSTILE.source lane..."
    cargo test --manifest-path crates/fcb-conformance/Cargo.toml \
        --test hostile_source_lane "${CARGO_ARGS[@]}"
elif [ "$LANE" = "document" ]; then
    echo "[fcb-059] executing HOSTILE.document lane..."
    cargo test --manifest-path crates/fcb-conformance/Cargo.toml \
        --test hostile_document_lane "${CARGO_ARGS[@]}"
elif [ ${#CARGO_ARGS[@]} -gt 0 ]; then
    echo "[fcb-059] executing fcb-conformance tests with custom args: ${CARGO_ARGS[*]}..."
    cargo test --manifest-path crates/fcb-conformance/Cargo.toml "${CARGO_ARGS[@]}"
else
    echo "[fcb-059] executing all hostile conformance lanes (source + document)..."
    cargo test --manifest-path crates/fcb-conformance/Cargo.toml \
        --test hostile_source_lane \
        --test hostile_document_lane
fi

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-059-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-059] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-059] PASS (HOSTILE conformance regression lanes verified)"
