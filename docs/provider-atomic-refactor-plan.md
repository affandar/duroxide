# Provider Atomic Refactor Plan

## Overview

The goal is to merge the HistoryStore read & append functions into the `dequeue_orchestrator_peek_lock()` and `ack_orchestrator()` functions. This involves introducing three new methods that subsume the existing ones.

## New Atomic Provider Methods

### 1. `fetch_orchestration_item()`

This method should:
- Fetch a batch of orchestrator queue messages along with the history for the instance
- Return the instance ID, orchestration name, execution ID, and version
- The provider must "lock" this instance, preventing further batches until it's updated or abandoned
- If it's a new instance, messages will contain a `StartOrchestration` message, and history will be empty

### 2. `ack_orchestration_item()`

Using the lock token from `fetch_orchestration_item()`, this method will:
- Atomically commit updated history and a set of messages to enqueue into the worker queue, timer queue, and back to the orchestrator queue (e.g., for sub-orchestration completions)
- **Release the instance lock** (acknowledge the batch)
- All history writes for an instance will be done via this update

### 3. `abandon_orchestration_item()`

This method will:
- Abandon the batch of orchestration work
- Set its visibility so the instance does not wake up until some time in the future
- **Release the instance lock** (abandon the batch)

## Key Changes

- With these new methods, `enqueue_worker_work` and `enqueue_timer_work` will no longer be needed externally (they become internal only)
- The `in_memory` and `fs` providers should provide best-effort atomicity guarantees
- Focus on getting the interface right and hooking up the engine
- No sloppy work, remove dead code, add specific unit tests for the APIs being added

## Implementation Approach

1. Add the new `OrchestrationItem` struct and trait methods to the `HistoryStore` trait
2. Implement these methods in both `fs` and `in_memory` providers with best-effort atomicity
3. Update the runtime to use these new atomic methods instead of the old individual operations
4. Create comprehensive unit tests for the new atomic API
5. Update existing tests to use the new atomic API

## Core Insight

Instead of having separate `read()`, `append()`, `enqueue_*()`, and `ack()` operations, we have atomic operations that handle fetching, acknowledging, and abandoning orchestration items as single transactions. The `ack_orchestration_item()` method both commits changes AND releases the lock, making it clear that it's the acknowledgment operation.

## Benefits

- **Atomicity**: All operations on an orchestration instance are atomic
- **Consistency**: No partial updates or inconsistent states
- **Performance**: Fewer round trips to the provider
- **Simplicity**: Cleaner API with fewer methods to implement
- **Reliability**: Better error handling and recovery
