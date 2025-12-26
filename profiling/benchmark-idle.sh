#!/usr/bin/env bash
set -euo pipefail

# Comprehensive idle runtime benchmark
# Measures CPU and RSS over 1 minute for various configurations

cd /Users/affandar/workshop/duroxide

WARMUP=5
SAMPLE_SECS=60
INTERVAL=1

echo "Building release binary..."
cargo build -p pg-memory-test --release 2>&1 | grep -E "Compiling|Finished" || true

BIN="./target/release/pg-memory-test"

sample_process() {
    local pid=$1
    local duration=$2
    local interval=$3
    
    local cpu_sum=0
    local cpu_min=999
    local cpu_max=0
    local rss_min=999999999
    local rss_max=0
    local n=0
    
    for ((i=0; i<duration; i+=interval)); do
        if ! kill -0 "$pid" 2>/dev/null; then
            break
        fi
        
        # Get CPU and RSS
        local stats
        stats=$(ps -p "$pid" -o pcpu=,rss= 2>/dev/null | tr -s ' ') || break
        local cpu=$(echo "$stats" | awk '{print $1}')
        local rss=$(echo "$stats" | awk '{print $2}')
        
        if [[ -n "$cpu" && -n "$rss" ]]; then
            cpu_sum=$(echo "$cpu_sum + $cpu" | bc)
            if (( $(echo "$cpu < $cpu_min" | bc -l) )); then cpu_min=$cpu; fi
            if (( $(echo "$cpu > $cpu_max" | bc -l) )); then cpu_max=$cpu; fi
            if (( rss < rss_min )); then rss_min=$rss; fi
            if (( rss > rss_max )); then rss_max=$rss; fi
            ((n++))
        fi
        
        sleep "$interval"
    done
    
    if (( n > 0 )); then
        local cpu_avg=$(echo "scale=2; $cpu_sum / $n" | bc)
        local rss_avg_mib=$(echo "scale=2; ($rss_min + $rss_max) / 2 / 1024" | bc)
        local rss_max_mib=$(echo "scale=2; $rss_max / 1024" | bc)
        echo "$cpu_avg $cpu_min $cpu_max $rss_max_mib $n"
    else
        echo "0 0 0 0 0"
    fi
}

run_test() {
    local label=$1
    local mode=$2
    local tokio_threads=$3
    local orch=$4
    local worker=$5
    local pool=$6
    
    # Start process
    MODE=$mode \
    TOKIO_THREADS=$tokio_threads \
    ORCH_CONCURRENCY=$orch \
    WORKER_CONCURRENCY=$worker \
    DUROXIDE_PG_POOL_MAX=$pool \
    IDLE_SECONDS=999999 \
    RUST_LOG=error \
    $BIN 2>/dev/null &
    
    local pid=$!
    
    # Wait for startup
    sleep $WARMUP
    
    if ! kill -0 "$pid" 2>/dev/null; then
        printf "%-40s FAILED TO START\n" "$label"
        return
    fi
    
    # Sample for duration
    local result
    result=$(sample_process "$pid" "$SAMPLE_SECS" "$INTERVAL")
    
    # Kill process
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    
    local cpu_avg=$(echo "$result" | awk '{print $1}')
    local cpu_min=$(echo "$result" | awk '{print $2}')
    local cpu_max=$(echo "$result" | awk '{print $3}')
    local rss_mib=$(echo "$result" | awk '{print $4}')
    local samples=$(echo "$result" | awk '{print $5}')
    
    printf "%-40s CPU: %5.1f%% (min=%4.1f max=%4.1f)  RSS: %5.1f MiB  [n=%d]\n" \
        "$label" "$cpu_avg" "$cpu_min" "$cpu_max" "$rss_mib" "$samples"
}

echo ""
echo "=== Idle Runtime Benchmark (${SAMPLE_SECS}s sampling, ${WARMUP}s warmup) ==="
echo ""
echo "Configuration: tokio_threads / orch_concurrency / worker_concurrency / pg_pool"
echo ""

# Test matrix
# Format: label, mode, tokio_threads, orch, worker, pool

echo "--- SQLite Provider ---"
run_test "sqlite: ct/1x2/pool=na" sqlite 0 1 2 1
run_test "sqlite: ct/2x2/pool=na" sqlite 0 2 2 1
run_test "sqlite: mt2/1x2/pool=na" sqlite 2 1 2 1
run_test "sqlite: mt2/2x2/pool=na" sqlite 2 2 2 1

echo ""
echo "--- PG Provider (pool sizes) ---"
run_test "pg: ct/1x2/pool=1" pg 0 1 2 1
run_test "pg: ct/1x2/pool=5" pg 0 1 2 5
run_test "pg: ct/1x2/pool=10" pg 0 1 2 10
run_test "pg: ct/2x2/pool=5" pg 0 2 2 5

echo ""
echo "--- PG-Opt Provider (pool sizes) ---"
run_test "pg_opt: ct/1x2/pool=1" pg_opt 0 1 2 1
run_test "pg_opt: ct/1x2/pool=5" pg_opt 0 1 2 5
run_test "pg_opt: ct/1x2/pool=10" pg_opt 0 1 2 10
run_test "pg_opt: ct/2x2/pool=5" pg_opt 0 2 2 5

echo ""
echo "--- Comparison: Smallest config (ct/1x2) ---"
run_test "sqlite: ct/1x2" sqlite 0 1 2 1
run_test "pg: ct/1x2/pool=1" pg 0 1 2 1
run_test "pg_opt: ct/1x2/pool=1" pg_opt 0 1 2 1

echo ""
echo "Legend:"
echo "  ct = current_thread (single-threaded tokio)"
echo "  mt2 = multi_thread with 2 workers"
echo "  1x2 = 1 orchestration worker, 2 activity workers"
echo "  pool = PostgreSQL connection pool size"
