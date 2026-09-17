#!/bin/bash
# FCB-078.V production verification scenario: Isolated headless source/search/map
# external library consumers (see examples/fcb-headless-consumer).
#
# Supported routes:
#   scripts/e2e/fcb_078.sh                  (runs all headless consumer test suites)
#   scripts/e2e/fcb_078.sh --lane consumer  (runs headless consumer lib unit tests)
#   scripts/e2e/fcb_078.sh --lane startup   (runs no-native/no-storage startup proof)
#   scripts/e2e/fcb_078.sh --lane audit     (runs dependency closure audit)
#   scripts/e2e/fcb_078.sh [cargo-test-args...]
#
# Environment:
#   FCB_078_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-078-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-078/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_078_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_078_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-078/$RUN_ID"
mkdir -p "$ARTIFACT_DIR/receipts"
export FCB_RECEIPTS_DIR="$REPO_ROOT/$ARTIFACT_DIR/receipts"

LANE="all"
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

echo "[fcb-078] route: headless cargo test (examples/fcb-headless-consumer)"
echo "[fcb-078] lane: $LANE"
echo "[fcb-078] run id: $RUN_ID"

if [ "$LANE" = "consumer" ]; then
    echo "[fcb-078] executing headless consumer lib tests..."
    cargo test --manifest-path examples/fcb-headless-consumer/Cargo.toml \
        --lib "${CARGO_ARGS[@]+"${CARGO_ARGS[@]}"}"
elif [ "$LANE" = "startup" ]; then
    echo "[fcb-078] executing no_native_startup_proof integration tests..."
    cargo test --manifest-path examples/fcb-headless-consumer/Cargo.toml \
        --test no_native_startup_proof "${CARGO_ARGS[@]+"${CARGO_ARGS[@]}"}"
elif [ "$LANE" = "audit" ]; then
    echo "[fcb-078] executing dependency_closure_audit integration tests..."
    cargo test --manifest-path examples/fcb-headless-consumer/Cargo.toml \
        --test dependency_closure_audit "${CARGO_ARGS[@]+"${CARGO_ARGS[@]}"}"
elif [ ${#CARGO_ARGS[@]} -gt 0 ]; then
    echo "[fcb-078] executing headless consumer tests with custom args: ${CARGO_ARGS[*]}..."
    cargo test --manifest-path examples/fcb-headless-consumer/Cargo.toml "${CARGO_ARGS[@]}"
else
    echo "[fcb-078] executing all headless consumer test suites..."
    cargo test --manifest-path examples/fcb-headless-consumer/Cargo.toml
fi

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-078-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
fi

RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" 2>/dev/null | wc -l | tr -d ' ')
echo "[fcb-078] receipts archived: $ARTIFACT_DIR/receipts ($RECEIPT_COUNT receipts retained)"

if [ "$RECEIPT_COUNT" -eq 0 ]; then
    echo "[fcb-078] ERROR: No receipts were retained in $ARTIFACT_DIR/receipts" >&2
    exit 1
fi

echo "[fcb-078] PASS (isolated headless external consumer qualification verified)"
