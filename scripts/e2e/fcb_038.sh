#!/bin/bash
# FCB-038.V production verification scenario:
# Shared upstream typography and FCB actual-scale theme/gallery qualification.
# Bundled vs system route identities, 1x/2x fractional offsets, punctuation,
# bidi/emoji, selection overlays, and native readable text inspection plus semantic copy.
#
# Supported routes:
#   scripts/e2e/fcb_038.sh [cargo-test-args...]
#   scripts/e2e/fcb_038.sh --lane gallery
#   scripts/e2e/fcb_038.sh --lane production
#   scripts/e2e/fcb_038.sh --lane all
#
# Environment:
#   FCB_038_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-038-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-038/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_038_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_038_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-038/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-038-receipts-$RUN_ID"
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

echo "[fcb-038] route: headless cargo test (fcb_038_production, typography_gallery_test)"
echo "[fcb-038] lane: $LANE"
echo "[fcb-038] run id: $RUN_ID"

case "$LANE" in
    gallery)
        echo "[fcb-038] running typography_gallery_test..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test typography_gallery_test "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-038] running fcb_038_production tests..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test fcb_038_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-038] running all FCB-038 test suites..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test typography_gallery_test "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test fcb_038_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-038] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-038] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ] || [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 10 ]; then
            echo "[fcb-038] ERROR: expected at least 10 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

echo "[fcb-038] PASS (all required routes executed)"
