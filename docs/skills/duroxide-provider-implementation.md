---
title: Duroxide Provider Implementation
version: "1.0"
scope: Implementing custom storage backends for Duroxide
audience: Provider developers, LLMs assisting with provider development
---

# Duroxide Provider Implementation Skill

## Summary

This skill enables AI assistants to help implement custom storage providers for Duroxide, a durable execution runtime for Rust. Providers are storage backends that persist orchestration state, manage work queues, and ensure atomic operations.

## Key Concepts

### Provider Role
- **Providers are "dumb storage"** — they store and retrieve data exactly as instructed
- **Runtime owns all logic** — providers never interpret events or make orchestration decisions
- **ID generation is runtime's job** — providers MUST NOT generate `execution_id` or `event_id`

### Architecture
- **Two work queues**: Orchestrator queue (completions, timers) and Worker queue (activities)
- **Event history**: Append-only log per instance/execution
- **Peek-lock semantics**: Items locked during processing, deleted on ack

### Core Trait Methods

| Method | Purpose | Complexity |
|--------|---------|------------|
| `read()` | Load history for latest execution | Easy |
| `append_with_execution()` | Append events to history | Easy |
| `fetch_work_item()` | Fetch and lock from worker queue | Medium |
| `ack_work_item()` | Delete from queue, enqueue completion | Medium |
| `fetch_orchestration_item()` | Fetch turn with instance lock | Complex |
| `ack_orchestration_item()` | Atomic commit of turn | Complex |

### Critical Requirements

1. **Atomicity**: `ack_orchestration_item()` must be a single transaction
2. **Lock validation**: Always check lock is still valid before commit
3. **Instance-level locking**: All messages for one instance processed together
4. **Ordering**: Enqueue worker items BEFORE cancelling (for same-turn schedule+cancel)
5. **Error classification**: Use `ProviderError::retryable()` vs `permanent()`

### Activity Cancellation

Implemented via **lock stealing**:
1. Runtime calls `ack_orchestration_item()` with `cancelled_activities` list
2. Provider deletes those entries from worker queue (same transaction)
3. Worker's next lock renewal fails → triggers cancellation

## Implementation Guidance

When implementing a provider:

1. **Start with history** — `read()` and `append_with_execution()` are simplest
2. **Add worker queue** — `enqueue_for_worker()`, `fetch_work_item()`, `ack_work_item()`
3. **Add orchestrator queue** — Similar pattern but with instance-level locking
4. **Handle advanced features** — Lock renewal, cancellation, ProviderAdmin

### Common Pitfalls

- ❌ Generating IDs in the provider (runtime's job)
- ❌ Non-atomic ack operations (use transactions)
- ❌ Creating instances in `enqueue_orchestrator_work()` (only in ack)
- ❌ DELETE before INSERT for cancelled activities (wrong order)
- ❌ Silently succeeding on lock renewal failure (must return error)

## References

- **Implementation Guide**: [docs/provider-implementation-guide.md](../provider-implementation-guide.md)
- **Testing Guide**: [docs/provider-testing-guide.md](../provider-testing-guide.md)
- **Reference Implementations**:
  - SQLite (bundled): `src/providers/sqlite.rs`
  - PostgreSQL: [duroxide-pg](https://github.com/microsoft/duroxide-pg)
- **Provider Trait**: `src/providers/mod.rs`
- **Validation Tests**: `tests/sqlite_provider_validations.rs`

## Usage

When asked to implement a Duroxide provider:

1. Review the implementation guide for detailed method semantics
2. Use SQLite or PostgreSQL implementations as reference
3. Follow the "simplest path" — history → worker queue → orchestrator queue
4. Run validation tests to verify correctness
5. Consider implementing `ProviderAdmin` for production use
