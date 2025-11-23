# SlateDB Provider Proposal for Duroxide

## Executive Summary

This document proposes a design for a `SlateDB` provider for Duroxide. SlateDB is an embedded database built on top of object storage (like S3), offering unlimited storage with a simple Key-Value interface.

Implementing a Duroxide provider on SlateDB offers:
- **Cloud-Native Storage**: Infinite storage scale for history and state.
- **Serverless Friendly**: No dedicated database server required.
- **Cost Efficiency**: Uses cheap object storage.

**⚠️ Critical Challenge**: SlateDB (like most KV stores) may have different transaction semantics than SQL databases. We must ensure strict atomicity for `ack_orchestration_item` to guarantee consistency.

---

## 1. Key-Value Schema Design

We will map Duroxide's relational model to a hierarchical Key-Value schema. We use prefixes to organize data types.

### 1.1 Instance Metadata
*   **Key**: `inst/{instance_id}`
*   **Value**: JSON `InstanceMetadata`
    ```json
    {
      "name": "MyOrchestration",
      "version": "v1",
      "current_execution_id": 1,
      "status": "Running",
      "created_at": 1234567890
    }
    ```

### 1.2 Execution Data
*   **Key**: `exec/{instance_id}/{execution_id}`
*   **Value**: JSON `ExecutionState`
    ```json
    {
      "status": "Running",
      "output": null,
      "started_at": ...
    }
    ```

### 1.3 Event History
We store events ordered by ID to allow efficient range scans (replay).
*   **Key**: `hist/{instance_id}/{execution_id}/{event_id_padded}`
    *   *Note*: `event_id_padded` is zero-padded (e.g., `00000001`) to ensure lexicographical order.
*   **Value**: JSON `Event`

### 1.4 Orchestrator Queue (Work Items)
We need to scan for work efficiently. We'll use a composite key for visibility.
*   **Key**: `q/orch/{visible_at_ts}/{instance_id}/{item_id}`
*   **Value**: JSON `OrchestratorQueueItem` (includes lock token, metadata)

### 1.5 Worker Queue
*   **Key**: `q/work/{item_id}`
*   **Value**: JSON `WorkerQueueItem`

### 1.6 Instance Locks (Critical)
*   **Key**: `locks/inst/{instance_id}`
*   **Value**: JSON `LockInfo`
    ```json
    {
      "token": "uuid...",
      "expires_at": 1234567890
    }
    ```

---

## 2. Implementation Strategy

### 2.1 Locking & Concurrency (`fetch_orchestration_item`)

Duroxide requires exclusive instance locking.

**Algorithm:**
1.  **Scan for Work**: Range scan `q/orch/` to find items where `visible_at <= now`.
2.  **Check Lock**: For a candidate `instance_id`, `GET locks/inst/{instance_id}`.
    *   If exists AND `expires_at > now`: Skip (locked by another).
    *   If missing OR `expires_at <= now`: Attempt to acquire.
3.  **Acquire Lock (CAS)**:
    *   Use `CAS` (Compare-And-Swap) or `PutIfAbsent` on `locks/inst/{instance_id}`.
    *   Set `expires_at = now + 30s`.
    *   If CAS fails, retry scan (race condition).
4.  **Fetch Data**: Once locked, range scan `hist/{instance_id}/{current_exec_id}/` to load history.

### 2.2 Atomic Commit (`ack_orchestration_item`)

This is the most critical operation. It must atomically:
1.  Delete processed queue items.
2.  Append new history events.
3.  Update instance/execution metadata.
4.  Enqueue new work (worker/orchestrator).
5.  Release (delete) the lock.

**Transactionality**:
*   **If SlateDB supports Atomic Batches**: We simply create a `WriteBatch` containing all Puts and Deletes and commit it.
*   **If SlateDB does NOT support cross-key atomicity**: We must implement a "Write-Ahead Log" (WAL) pattern in a single key or use a specific "Commit" record.
    *   *Assumption*: Most LSM-based KV stores support atomic batches. We will assume this capability.

### 2.3 Queue Management
*   **Enqueue**: `PUT` to `q/orch/...` or `q/work/...`.
*   **Dequeue**: Handled via scanning and locking (for orchestrator) or atomic claim (for workers).
*   **Worker Queue**: Workers also need a "peek-lock". We can use a similar locking mechanism: `locks/work/{item_id}`.

---

## 3. Detailed Method Implementation Plan

### `fetch_orchestration_item`
1.  **Iterate** `q/orch/` prefix.
2.  Filter for `visible_at <= now`.
3.  For each candidate instance, attempt **Optimistic Locking**:
    *   Read `locks/inst/{instance_id}`.
    *   If locked, skip.
    *   Write `locks/inst/{instance_id}` with `Condition::KeyNotExists` (or CAS matches old lock).
4.  If lock acquired:
    *   Read `inst/{instance_id}` metadata.
    *   Scan `hist/{instance_id}/{exec_id}/` for full history.
    *   Collect all queue items for this instance (to be deleted on ack).
    *   Return `OrchestrationItem`.

### `ack_orchestration_item`
1.  **Validate Lock**: Check if we still hold the lock (token matches, not expired).
2.  **Prepare Batch**:
    *   `DELETE` processed queue items (`q/orch/...`).
    *   `PUT` new history events (`hist/...`).
    *   `PUT` updated instance metadata (`inst/...`).
    *   `PUT` new worker items (`q/work/...`).
    *   `PUT` new orchestrator items (`q/orch/...`).
    *   `DELETE` lock key (`locks/inst/...`).
3.  **Commit Batch**: Execute the atomic write batch.

### `read_history`
1.  Scan prefix `hist/{instance_id}/{execution_id}/`.
2.  Deserialize and return events.

---

## 4. Challenges & Mitigations

| Challenge | Mitigation |
|-----------|------------|
| **Object Storage Latency** | SlateDB caches data locally. Writes might be slower than disk. Use aggressive caching for reads. |
| **Consistency** | Ensure SlateDB is configured for strong consistency (flush to object store or reliable local WAL). |
| **Lock Contention** | Randomize poll intervals slightly to reduce collisions on the same instance. |
| **Ghost Locks** | Ensure all locks have `expires_at` timestamps and are respected by all readers. |

## 5. Next Steps

1.  **Verify SlateDB Capabilities**: Confirm support for Atomic Write Batches and CAS/Conditional Writes.
2.  **Prototype**: Implement `fetch` and `ack` loop with SlateDB.
3.  **Run Test Suite**: Use `duroxide::provider_validations` to ensure correctness.
