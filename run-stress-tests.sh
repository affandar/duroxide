#!/bin/bash
# Run Duroxide stress tests
#
# This script runs the parallel orchestrations stress test suite
# which tests multiple providers and concurrency configurations.
#
# Usage:
#   ./run-stress-tests.sh          # Run tests without tracking
#   ./run-stress-tests.sh --track   # Run tests and track results in stress-test-results.md

set -e

TRACK_RESULTS=false

# Parse arguments (support --track)
for arg in "$@"; do
    if [ "$arg" == "--track" ]; then
        TRACK_RESULTS=true
    fi
done

echo "=========================================="
echo "Duroxide Stress Test Suite"
echo "=========================================="
echo ""

if [ "$TRACK_RESULTS" = true ]; then
    echo "Running tests with result tracking..."
    echo ""
    # Run with tracking
    ./stress-tests/track-results.sh
else
    # Run the stress tests in release mode for accurate performance metrics
    cargo run --release --package duroxide-stress-tests --bin parallel_orchestrations
fi

echo ""
echo "=========================================="
echo "Stress tests completed!"
echo "=========================================="

