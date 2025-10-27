#!/bin/bash
# Run Duroxide stress tests
#
# This script runs the parallel orchestrations stress test suite
# which tests multiple providers and concurrency configurations.

set -e

echo "=========================================="
echo "Duroxide Stress Test Suite"
echo "=========================================="
echo ""

# Run the stress tests in release mode for accurate performance metrics
cargo run --release --package duroxide-stress-tests --bin parallel_orchestrations

echo ""
echo "=========================================="
echo "Stress tests completed!"
echo "=========================================="

