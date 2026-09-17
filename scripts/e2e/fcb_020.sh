#!/bin/bash
# FCB-020.V production verification scenario:
# Deterministic UI reducer, sidebar panels, command routing, focus model,
# open-atlas-reader-back interaction, gesture ownership, and motion mailbox.
#
# Supported routes:
#   scripts/e2e/fcb_020.sh [cargo-test-args...]
#   scripts/e2e/fcb_020.sh --lane reducer
#   scripts/e2e/fcb_020.sh --lane interaction
#   scripts/e2e/fcb_020.sh --lane production
#   scripts/e2e/fcb_020.sh --lane all
#
# Environment:
#   FCB_020_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-020-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-020/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_020_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_020_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-020/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-020-receipts-$RUN_ID"
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

echo "[fcb-020] route: headless cargo test (fcb_020_production, open_atlas_reader_interaction, ui_reducer)"
echo "[fcb-020] lane: $LANE"
echo "[fcb-020] run id: $RUN_ID"

case "$LANE" in
    reducer)
        echo "[fcb-020] running ui_reducer tests..."
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test ui_reducer "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    interaction)
        echo "[fcb-020] running open_atlas_reader_interaction tests..."
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test open_atlas_reader_interaction "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-020] running fcb_020_production tests..."
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test fcb_020_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-020] running all FCB-020 test suites..."
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test ui_reducer "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test open_atlas_reader_interaction "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test fcb_020_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-020] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-020] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ] || [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 8 ]; then
            echo "[fcb-020] ERROR: expected at least 8 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

echo "[fcb-020] PASS (all required routes executed)"
