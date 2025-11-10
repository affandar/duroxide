#!/bin/bash
# Run Duroxide stress tests
#
# This script runs the parallel orchestrations stress test suite
# which tests multiple providers and concurrency configurations.
#
# Usage:
#   ./run-stress-tests.sh [DURATION] [--track|--track-cloud]
#
# Examples:
#   ./run-stress-tests.sh              # Run for 30 seconds (default)
#   ./run-stress-tests.sh 60           # Run for 60 seconds
#   ./run-stress-tests.sh 5            # Quick 5 second test
#   ./run-stress-tests.sh 60 --track   # Run for 60 seconds and track results
#   ./run-stress-tests.sh --track-cloud   # Track results to the cloud log

set -e

TRACK_MODE=""
DURATION=""

# Parse arguments
for arg in "$@"; do
    case "$arg" in
        --track)
            if [ "$TRACK_MODE" = "cloud" ]; then
                echo "Error: --track cannot be combined with --track-cloud" >&2
                exit 1
            fi
            TRACK_MODE="local"
            ;;
        --track-cloud)
            if [ "$TRACK_MODE" = "local" ]; then
                echo "Error: --track-cloud cannot be combined with --track" >&2
                exit 1
            fi
            TRACK_MODE="cloud"
            ;;
        *)
            if [[ "$arg" =~ ^[0-9]+$ ]]; then
                DURATION="$arg"
            else
                echo "Warning: Unrecognized argument '$arg' will be ignored" >&2
            fi
            ;;
    esac
done

echo "=========================================="
echo "Duroxide Stress Test Suite"
echo "=========================================="
echo ""

if [ -n "$TRACK_MODE" ]; then
    if [ "$TRACK_MODE" = "cloud" ]; then
        echo "Running tests with cloud result tracking..."
    else
        echo "Running tests with result tracking..."
    fi
    echo ""
    # Run with tracking (pass duration if specified)
    if [ -n "$DURATION" ]; then
        if [ "$TRACK_MODE" = "cloud" ]; then
            ./sqlite-stress/track-results.sh "$DURATION" --cloud
        else
            ./sqlite-stress/track-results.sh "$DURATION"
        fi
    else
        if [ "$TRACK_MODE" = "cloud" ]; then
            ./sqlite-stress/track-results.sh --cloud
        else
            ./sqlite-stress/track-results.sh
        fi
    fi
else
    # Run the stress tests in release mode for accurate performance metrics
    if [ -n "$DURATION" ]; then
        cargo run --release --package duroxide-sqlite-stress --bin sqlite-stress "$DURATION"
    else
        cargo run --release --package duroxide-sqlite-stress --bin sqlite-stress
    fi
fi

echo ""
echo "=========================================="
echo "Stress tests completed!"
echo "=========================================="

