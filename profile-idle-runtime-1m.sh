#!/usr/bin/env bash
set -euo pipefail

# Measures CPU and memory footprint of an *idle* duroxide runtime for ~1 minute.
# macOS-compatible (uses `ps`).

PROFILE="release"          # debug|release
WARMUP_SECONDS=5
SAMPLE_SECONDS=60
INTERVAL_SECONDS=1

usage() {
  cat <<EOF
Usage:
  $0 [--debug|--release] [--warmup N] [--seconds N] [--interval N]

Defaults:
  --release
  --warmup  ${WARMUP_SECONDS}
  --seconds ${SAMPLE_SECONDS}
  --interval ${INTERVAL_SECONDS}

Notes:
  - Requires: cargo, and building with --features sqlite
  - Samples: pcpu (percent CPU), rss (KB), vsz (KB), thcount (threads)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug)
      PROFILE="debug"; shift ;;
    --release)
      PROFILE="release"; shift ;;
    --warmup)
      WARMUP_SECONDS="$2"; shift 2 ;;
    --seconds)
      SAMPLE_SECONDS="$2"; shift 2 ;;
    --interval)
      INTERVAL_SECONDS="$2"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      echo "Unknown arg: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found on PATH" >&2
  exit 1
fi

echo "Building example (profile=${PROFILE})…" >&2
if [[ "${PROFILE}" == "release" ]]; then
  cargo build --example idle_runtime_footprint --features sqlite --release >/dev/null
else
  cargo build --example idle_runtime_footprint --features sqlite >/dev/null
fi

BIN="target/${PROFILE}/examples/idle_runtime_footprint"
if [[ ! -x "${BIN}" ]]; then
  echo "Expected binary not found/executable: ${BIN}" >&2
  exit 1
fi

# Keep the runtime alive long enough for warmup + sampling + a small tail.
TOTAL_SECONDS=$((WARMUP_SECONDS + SAMPLE_SECONDS + 5))

TMP_FILE="$(mktemp -t duroxide-idle-XXXXXX.tsv)"

# Start the runtime in the background.
DUROXIDE_IDLE_SECONDS="${TOTAL_SECONDS}" \
  RUST_LOG=warn \
  "${BIN}" >/dev/null 2>&1 &
PID=$!

if ! kill -0 "${PID}" >/dev/null 2>&1; then
  echo "Failed to start runtime process" >&2
  exit 1
fi

echo "Sampling PID=${PID} (warmup=${WARMUP_SECONDS}s, sample=${SAMPLE_SECONDS}s, interval=${INTERVAL_SECONDS}s)…" >&2
sleep "${WARMUP_SECONDS}"

# Header
printf "ts\tpid\tetime\tpcpu\trss_kb\tvsz_kb\n" >"${TMP_FILE}"

for ((i=0; i<"${SAMPLE_SECONDS}"; i+="${INTERVAL_SECONDS}")); do
  if ! kill -0 "${PID}" >/dev/null 2>&1; then
    echo "Process exited early during sampling." >&2
    break
  fi

  TS="$(date +%s)"

  # macOS ps fields:
  #   etime: elapsed time
  #   pcpu: percent CPU
  #   rss/vsz: KB
  # Note: thread-count keywords vary by platform and are not available on all macOS builds.
  LINE="$(ps -p "${PID}" -o pid=,etime=,pcpu=,rss=,vsz= | awk 'NR==1{print $1"\t"$2"\t"$3"\t"$4"\t"$5}')"
  if [[ -z "${LINE}" ]]; then
    echo "ps returned no data (process ended?)" >&2
    break
  fi

  printf "%s\t%s\n" "${TS}" "${LINE}" >>"${TMP_FILE}"

  sleep "${INTERVAL_SECONDS}"
done

# Wait for the process to exit (or kill if it lingers unexpectedly).
wait "${PID}" 2>/dev/null || true

# Summarize
# Columns (ts pid etime pcpu rss_kb vsz_kb)
awk -F'\t' '
  NR==1 { next }
  {
    pcpu=$4+0
    rss=$5+0
    vsz=$6+0

    n++
    cpu_sum += pcpu

    if (n==1 || pcpu > cpu_max) cpu_max = pcpu
    if (n==1 || pcpu < cpu_min) cpu_min = pcpu

    if (n==1 || rss > rss_max) rss_max = rss
    if (n==1 || rss < rss_min) rss_min = rss

    if (n==1 || vsz > vsz_max) vsz_max = vsz
    if (n==1 || vsz < vsz_min) vsz_min = vsz

  }
  END {
    if (n==0) {
      print "No samples collected." > "/dev/stderr"
      exit 1
    }

    cpu_avg = cpu_sum / n

    printf "\nIdle runtime footprint (n=%d samples)\n", n
    printf "CPU (pcpu): avg=%.3f%%  min=%.3f%%  max=%.3f%%\n", cpu_avg, cpu_min, cpu_max
    printf "RSS:        min=%.2f MiB  max=%.2f MiB\n", rss_min/1024.0, rss_max/1024.0
    printf "VSZ:        min=%.2f MiB  max=%.2f MiB\n", vsz_min/1024.0, vsz_max/1024.0
  }
' "${TMP_FILE}"

printf "\nRaw samples: %s\n" "${TMP_FILE}" >&2
