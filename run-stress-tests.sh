#!/bin/bash
# Run Duroxide stress tests
#
# This script runs the parallel orchestrations stress test suite
# which tests multiple providers and concurrency configurations.
#
# Usage:
#   ./run-stress-tests.sh [DURATION] [--track]
#
# Examples:
#   ./run-stress-tests.sh              # Run for 30 seconds (default)
#   ./run-stress-tests.sh 60           # Run for 60 seconds
#   ./run-stress-tests.sh 5            # Quick 5 second test
#   ./run-stress-tests.sh 60 --track   # Run for 60 seconds and track results

set -e

TRACK_RESULTS=false
DURATION=""

# Parse arguments
for arg in "$@"; do
    if [ "$arg" == "--track" ]; then
        TRACK_RESULTS=true
    elif [[ "$arg" =~ ^[0-9]+$ ]]; then
        DURATION="$arg"
    fi
done

echo "=========================================="
echo "Duroxide Stress Test Suite"
echo "=========================================="
echo ""

if [ "$TRACK_RESULTS" = true ]; then
    echo "Running tests with result tracking..."
    echo ""
    # Run with tracking (pass duration if specified)
    if [ -n "$DURATION" ]; then
        ./stress-tests/track-results.sh "$DURATION"
    else
        ./stress-tests/track-results.sh
    fi
else
    # Run the stress tests in release mode for accurate performance metrics
    if [ -n "$DURATION" ]; then
        cargo run --release --package duroxide-stress-tests --bin parallel_orchestrations "$DURATION"
    else
        cargo run --release --package duroxide-stress-tests --bin parallel_orchestrations
    fi
fi

echo ""
echo "=========================================="
echo "Stress tests completed!"
echo "=========================================="

