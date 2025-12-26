#!/usr/bin/env bash
set -euo pipefail

cd /Users/affandar/workshop/duroxide

echo "=== Memory Footprint Comparison ==="
echo ""
echo "Provider-only (no runtime):"
echo "----------------------------"

# Tokio baseline
MODE=mt_default RUST_LOG=error ./target/release/examples/memory_optimize &
PID=$!; sleep 2
RSS=$(ps -p $PID -o rss= 2>/dev/null | tr -d ' ' || echo "0")
kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true
printf "  tokio (mt_default):     %6.2f MiB\n" "$(echo "scale=2; $RSS/1024" | bc)"

# SQLite provider only
MODE=ct_sqlite_pool5 RUST_LOG=error ./target/release/examples/memory_optimize &
PID=$!; sleep 2
RSS=$(ps -p $PID -o rss= 2>/dev/null | tr -d ' ' || echo "0")
kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true
printf "  sqlite provider:        %6.2f MiB\n" "$(echo "scale=2; $RSS/1024" | bc)"

# PG provider only
MODE=pg_only RUST_LOG=error ./target/release/pg-memory-test &
PID=$!; sleep 2
RSS=$(ps -p $PID -o rss= 2>/dev/null | tr -d ' ' || echo "0")
kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true
printf "  pg provider:            %6.2f MiB\n" "$(echo "scale=2; $RSS/1024" | bc)"

# PG-opt provider only
MODE=pg_opt_only RUST_LOG=error ./target/release/pg-memory-test &
PID=$!; sleep 2
RSS=$(ps -p $PID -o rss= 2>/dev/null | tr -d ' ' || echo "0")
kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true
printf "  pg-opt provider:        %6.2f MiB\n" "$(echo "scale=2; $RSS/1024" | bc)"

echo ""
echo "Full runtime (idle):"
echo "--------------------"

# Full runtime with SQLite
MODE=sqlite RUST_LOG=error ./target/release/pg-memory-test &
PID=$!; sleep 2
RSS=$(ps -p $PID -o rss= 2>/dev/null | tr -d ' ' || echo "0")
kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true
printf "  sqlite + runtime:       %6.2f MiB\n" "$(echo "scale=2; $RSS/1024" | bc)"

# Full runtime with PG
MODE=pg RUST_LOG=error ./target/release/pg-memory-test &
PID=$!; sleep 2
RSS=$(ps -p $PID -o rss= 2>/dev/null | tr -d ' ' || echo "0")
kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true
printf "  pg + runtime:           %6.2f MiB\n" "$(echo "scale=2; $RSS/1024" | bc)"

# Full runtime with PG-opt
MODE=pg_opt RUST_LOG=error ./target/release/pg-memory-test &
PID=$!; sleep 2
RSS=$(ps -p $PID -o rss= 2>/dev/null | tr -d ' ' || echo "0")
kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true
printf "  pg-opt + runtime:       %6.2f MiB\n" "$(echo "scale=2; $RSS/1024" | bc)"

echo ""
echo "Legend:"
echo "  tokio       = Just tokio runtime (no provider)"
echo "  sqlite      = SQLite in-memory provider (bundled)"
echo "  pg          = duroxide-pg (PostgreSQL provider)"
echo "  pg-opt      = duroxide-pg-opt (PostgreSQL w/ LISTEN/NOTIFY)"
