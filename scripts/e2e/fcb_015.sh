#!/bin/bash
# FCB-015.V production verification scenario: Camera math, pointer anchoring,
# inverse-projection oracle, deep-zoom drift, interruptible navigation history,
# and deterministic interaction replay.
#
# Supported routes:
#   scripts/e2e/fcb_015.sh [cargo-test-args...]
#   scripts/e2e/fcb_015.sh --lane camera
#   scripts/e2e/fcb_015.sh --lane navigation
#   scripts/e2e/fcb_015.sh --lane all
#
# Environment:
#   FCB_015_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-015-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-015/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_015_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_015_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-015/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-015-receipts-$RUN_ID"
mkdir -p "$RECEIPTS_DIR"
export FCB_RECEIPTS_DIR="$RECEIPTS_DIR"

LANE="all"
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --lane)
            LANE="$2"
            shift 2
            ;;
        *)
            EXTRA_ARGS+=("$1")
            shift
            ;;
    esac
done

echo "[fcb-015] route: headless cargo test (fcb-map)"
echo "[fcb-015] lane: $LANE"
echo "[fcb-015] run id: $RUN_ID"

if [ "$LANE" = "camera" ] || [ "$LANE" = "all" ]; then
    echo "[fcb-015] running camera_transforms..."
    cargo test --manifest-path crates/fcb-map/Cargo.toml \
        --test camera_transforms "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
fi

if [ "$LANE" = "navigation" ] || [ "$LANE" = "all" ]; then
    echo "[fcb-015] running navigation_history..."
    cargo test --manifest-path crates/fcb-map/Cargo.toml \
        --test navigation_history "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
fi

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-015] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$RECEIPT_COUNT" -eq 0 ]; then
        echo "[fcb-015] ERROR: no receipts generated" >&2
        exit 1
    fi
fi

echo "[fcb-015] PASS (all required routes executed)"
