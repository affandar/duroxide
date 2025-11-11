# Dockerfile for profiling Duroxide with flamegraph
FROM rust:latest

# Install dependencies for profiling
RUN apt-get update && apt-get install -y \
    linux-perf \
    git \
    && rm -rf /var/lib/apt/lists/*

# Install flamegraph
RUN cargo install flamegraph

# Set working directory
WORKDIR /workspace

# Copy the entire project
COPY . .

# Build the stress test in release mode with debug symbols
ENV CARGO_PROFILE_RELEASE_DEBUG=true
RUN cd sqlite-stress && cargo build --release --bin sqlite-stress

# Default command: run profiling
CMD ["bash", "-c", "cd sqlite-stress && cargo flamegraph --bin sqlite-stress -- --duration 30"]

