#!/bin/bash
# FCB-033.V production verification scenario:
# FrankenMarkdown-owned code fence flow and large/wide table layout,
# exact code copy without whole-fence clones, row virtualization with pinned semantic headers,
# wide table horizontal scrolling, late wide cell expansion, and upstream API closure.
#
# Supported routes:
#   scripts/e2e/fcb_033.sh [cargo-test-args...]
#   scripts/e2e/fcb_033.sh --lane consumer
#   scripts/e2e/fcb_033.sh --lane production
#   scripts/e2e/fcb_033.sh --lane all
#
# Environment:
#   FCB_033_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-033-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-033/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_033_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_033_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-033/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-033-receipts-$RUN_ID"
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

echo "[fcb-033] route: headless cargo test (fcb_033_production, code_table_flow_consumer)"
echo "[fcb-033] lane: $LANE"
echo "[fcb-033] run id: $RUN_ID"

case "$LANE" in
    consumer)
        echo "[fcb-033] running code_table_flow_consumer tests..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test code_table_flow_consumer "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-033] running fcb_033_production tests..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test fcb_033_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-033] running all FCB-033 test suites..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test code_table_flow_consumer "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test fcb_033_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-033] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-033] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ] || [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 10 ]; then
            echo "[fcb-033] ERROR: expected at least 10 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

echo "[fcb-033] PASS (all required routes executed)"
