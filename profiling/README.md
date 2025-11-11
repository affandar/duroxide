# Performance Profiling

Profile Duroxide runtime performance using Docker + flamegraph.

## Quick Start

```bash
# Run profiling (30 seconds)
docker build -t duroxide-profiler -f profiling/Dockerfile .
docker run --rm --privileged \
    -v "$(pwd):/workspace" \
    -v "$(pwd)/profiling/results:/workspace/profiling/results" \
    duroxide-profiler \
    bash -c "cd sqlite-stress && cargo flamegraph --output /workspace/profiling/results/profile_$(date +%Y%m%d_%H%M%S).svg --bin sqlite-stress -- --duration 30"

# View results
open profiling/results/*.svg
```

## Files

- `Dockerfile` - Docker image with Rust + perf + flamegraph
- `docker-compose.yml` - Simplified orchestration (optional)
- `RESULTS.md` - Latest performance optimization results
- `results/` - Generated flamegraph SVGs (gitignored)

## For LLMs

See `ai_prompts/duroxide-profile.md` for detailed profiling instructions and analysis guidance.

