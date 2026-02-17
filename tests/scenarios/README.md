# Scenario Tests

This directory contains regression tests that model specific scenarios found in actual production usage of duroxide.

These tests validate complex orchestration patterns, long-running workflows, and edge cases discovered during real-world deployments. They serve as both regression tests and documentation of how duroxide handles complex orchestration patterns in practice.

## Test Scenarios

### Continue-As-New Chains
Tests for long-running orchestration chains that use `continue_as_new` to restart themselves:
- Simple counter chains
- Chains with activity execution
- Concurrent chains running in parallel

### Instance Actor Pattern
Tests modeling the "instance actor" pattern where an orchestration manages the lifecycle of a single resource instance:
- Health check cycles
- Multiple activities per iteration
- Continue-as-new with state preservation
- Concurrent instance actors

### Single-Thread Runtime Mode
Tests validating duroxide behavior when running in tokio's `current_thread` runtime mode, which is the execution model used by embedded Rust async code in single-threaded hosts (e.g., database extensions, embedded systems, WASM):
- Basic orchestration lifecycle
- Sequential activity execution
- Timer handling
- Continue-as-new chains
- Concurrent orchestrations on single thread
- Single concurrency configuration (1x1)
- **Activity cancellation (1x1 concurrency)**:
  - Cooperative activity: Activity checks `ctx.cancelled()` and exits gracefully when orchestration is cancelled
  - Runaway activity: Activity ignores cancellation token, gets forcibly aborted after grace period

## Purpose

These tests ensure that patterns discovered and validated in production environments continue to work correctly as the codebase evolves. They are particularly important for validating:
- Version resolution across continue-as-new boundaries
- State preservation during orchestration restarts
- Concurrent execution correctness
- Complex activity workflows
- Single-threaded execution model compatibility

### Session Affinity (Durable-Copilot-SDK Pattern)
Tests modeling the durable-copilot-sdk architecture: stateful conversation workloads where
in-memory `CopilotSession` objects must persist across sequential activity invocations on the
same worker. The test faithfully ports the SDK's `SessionManager`, `durableTurnOrchestration`,
and `runAgentTurn` activity to exercise the full session lifecycle:
- **Scaled-out multi-conversation**: 2 worker runtimes, 4 concurrent conversations (2 simple + 2 with wait/timer/CAN), per-worker `SessionManager` isolation, session affinity verification, zero cache-miss re-creation
