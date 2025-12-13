# Poison Message Handling

## Overview

Implement poison message detection for work items that repeatedly fail to process. After N fetch attempts, messages are classified as "poison" and the orchestration/activity is failed immediately to prevent blocking the queue.

## Problem Statement

Currently, if a work item causes repeated failures (e.g., crashes the worker, causes infinite retries), it will:

1. Block the queue by cycling through lock → crash/abandon → lock expires → refetch
2. Waste compute resources on repeated failed attempts
3. Have no visibility into which messages are problematic
4. Never terminate - just keeps retrying forever

## Goals

1. **Automatic detection**: After N fetch attempts, detect poison messages
2. **Non-blocking**: Poison messages don't block normal queue processing
3. **Immediate failure**: Fail orchestration/activity with clear error
4. **Visibility**: Error details include the poisoned message for debugging
5. **Simple implementation**: Minimal provider changes, logic in runtime

---

## Design

### Core Concept

The provider tracks how many times a message has been fetched (attempt count). The runtime checks this count and fails immediately if it exceeds the threshold.

```
Fetch #1 → attempt_count=1 → process → crash/abandon → lock expires
Fetch #2 → attempt_count=2 → process → crash/abandon → lock expires
...
Fetch #N → attempt_count=N+1 → runtime sees count > max → FAIL IMMEDIATELY
                                                          (don't process)
```

### Why Count on Fetch?

Counting attempts on fetch (not abandon) handles all failure modes:

| Failure Mode | Lock Status | Count Incremented? |
|--------------|-------------|-------------------|
| Explicit abandon | Runtime calls abandon | ✅ Yes (on next fetch) |
| Process crash | Lock expires | ✅ Yes (on next fetch) |
| OOM / SIGKILL | Lock expires | ✅ Yes (on next fetch) |
| Infinite loop | Lock expires | ✅ Yes (on next fetch) |

The abandon API remains unchanged.

---

## Detailed Design

### 1. Schema Changes

Add attempt count tracking to queue tables:

```sql
-- Modify orchestrator_queue: add attempt_count
ALTER TABLE orchestrator_queue ADD COLUMN attempt_count INTEGER DEFAULT 0;

-- Modify worker_queue: add attempt_count
ALTER TABLE worker_queue ADD COLUMN attempt_count INTEGER DEFAULT 0;
```

### 2. Provider Changes

#### fetch_orchestration_item

Return `lock_token` and `attempt_count` as separate tuple elements (consistent with `fetch_work_item`):

```rust
/// Fetch an orchestration item for processing.
/// 
/// # Returns
/// 
/// * `Ok(Some((item, lock_token, attempt_count)))` - Orchestration item with lock and attempt count
/// * `Ok(None)` - No work available
/// * `Err(...)` - Provider error
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
) -> Result<Option<(OrchestrationItem, String, u32)>, ProviderError>;
```

Implementation:

```rust
async fn fetch_orchestration_item(...) -> Result<Option<(OrchestrationItem, String, u32)>, ProviderError> {
    // Atomically: lock messages AND increment attempt_count
    UPDATE orchestrator_queue 
    SET lock_token = ?,
        locked_until = ?,
        attempt_count = attempt_count + 1
    WHERE instance_id = ? AND visible_at <= now();
    
    // Return (item, lock_token, attempt_count)
    Ok(Some((
        OrchestrationItem { /* ... */ },
        lock_token,
        row.attempt_count,
    )))
}
```

#### OrchestrationItem

Remove `lock_token` field (now returned separately):

```rust
#[derive(Debug, Clone)]
pub struct OrchestrationItem {
    pub instance: String,
    pub orchestration_name: String,
    pub execution_id: u64,
    pub version: String,
    pub history: Vec<Event>,
    pub messages: Vec<WorkItem>,
    // lock_token removed - now returned as tuple element
}
```

#### fetch_work_item

Return attempt count as third tuple element:

```rust
/// Fetch a work item for processing.
/// 
/// # Returns
/// 
/// * `Ok(Some((item, lock_token, attempt_count)))` - Work item with lock and attempt count
/// * `Ok(None)` - No work available
/// * `Err(...)` - Provider error
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

Implementation:

```rust
async fn fetch_work_item(...) -> Result<Option<(WorkItem, String, u32)>, ProviderError> {
    // Atomically: lock message AND increment attempt_count
    UPDATE worker_queue
    SET lock_token = ?,
        locked_until = ?,
        attempt_count = attempt_count + 1
    WHERE lock_token IS NULL AND id = (
        SELECT id FROM worker_queue 
        WHERE lock_token IS NULL 
        ORDER BY id 
        LIMIT 1
    );
    
    // Return (item, token, attempt_count)
    Ok(Some((work_item, lock_token, row.attempt_count)))
}
```

### 3. New Error Type

Add `Poison` variant to `ErrorDetails`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ErrorDetails {
    Infrastructure { ... },
    Configuration { ... },
    Application { ... },
    
    /// Poison message error - message exceeded max fetch attempts.
    /// 
    /// This indicates a message that repeatedly fails to process.
    /// Could be caused by:
    /// - Malformed message data causing deserialization failures
    /// - Message triggering bugs that crash the worker
    /// - Transient infrastructure issues that became permanent
    /// - Application code bugs triggered by specific input patterns
    Poison {
        /// Number of times the message was fetched
        attempt_count: u32,
        /// Maximum allowed attempts
        max_attempts: u32,
        /// Message type and identity
        message_type: PoisonMessageType,
        /// The poisoned message content (serialized JSON for debugging)
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PoisonMessageType {
    /// Orchestration work item batch
    Orchestration {
        instance: String,
        execution_id: u64,
    },
    /// Activity execution
    Activity {
        instance: String,
        execution_id: u64,
        activity_name: String,
        activity_id: u64,
    },
}
```

Update `ErrorDetails` methods:

```rust
impl ErrorDetails {
    pub fn category(&self) -> &'static str {
        match self {
            ErrorDetails::Infrastructure { .. } => "infrastructure",
            ErrorDetails::Configuration { .. } => "configuration",
            ErrorDetails::Application { .. } => "application",
            ErrorDetails::Poison { .. } => "poison",
        }
    }
    
    pub fn is_retryable(&self) -> bool {
        match self {
            ErrorDetails::Infrastructure { retryable, .. } => *retryable,
            ErrorDetails::Application { retryable, .. } => *retryable,
            ErrorDetails::Configuration { .. } => false,
            ErrorDetails::Poison { .. } => false, // Never retryable
        }
    }
    
    pub fn display_message(&self) -> String {
        match self {
            // ... existing ...
            ErrorDetails::Poison { attempt_count, max_attempts, message_type, .. } => {
                match message_type {
                    PoisonMessageType::Orchestration { instance, .. } => {
                        format!("poison: orchestration {} exceeded {} attempts (max {})", 
                            instance, attempt_count, max_attempts)
                    }
                    PoisonMessageType::Activity { activity_name, activity_id, .. } => {
                        format!("poison: activity {}#{} exceeded {} attempts (max {})",
                            activity_name, activity_id, attempt_count, max_attempts)
                    }
                }
            }
        }
    }
}
```

### 4. Configuration

Add to `RuntimeOptions`:

```rust
pub struct RuntimeOptions {
    // ... existing fields ...
    
    /// Maximum fetch attempts before a message is considered poison.
    /// 
    /// After this many fetch attempts, the runtime will immediately fail
    /// the orchestration/activity with a Poison error instead of processing.
    /// 
    /// Default: 5
    pub max_attempts: u32,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            // ... existing ...
            max_attempts: 5,
        }
    }
}
```

### 5. Runtime Integration

#### Orchestration Dispatcher

```rust
impl Runtime {
    async fn process_orchestration_item(
        self: &Arc<Self>,
        item: OrchestrationItem,
        lock_token: &str,
        attempt_count: u32,
        worker_id: &str,
    ) {
        // Check poison threshold FIRST, before any processing
        if attempt_count > self.options.max_attempts {
            warn!(
                instance = %item.instance,
                attempt_count = attempt_count,
                max_attempts = self.options.max_attempts,
                "Orchestration message exceeded max attempts, marking as poison"
            );
            
            self.fail_orchestration_as_poison(&item, lock_token, attempt_count).await;
            return;
        }
        
        // Normal processing continues...
    }
    
    async fn fail_orchestration_as_poison(
        &self,
        item: &OrchestrationItem,
        lock_token: &str,
        attempt_count: u32,
    ) {
        let error = ErrorDetails::Poison {
            attempt_count,
            max_attempts: self.options.max_attempts,
            message_type: PoisonMessageType::Orchestration {
                instance: item.instance.clone(),
                execution_id: item.execution_id,
            },
            message: serde_json::to_string(&item.messages).unwrap_or_default(),
        };
        
        // Create failure event and commit
        let mut history_mgr = HistoryManager::from_history(&item.history);
        history_mgr.append_failed(error.clone());
        
        let metadata = ExecutionMetadata {
            status: Some("Failed".to_string()),
            output: Some(error.display_message()),
            ..Default::default()
        };
        
        let _ = self.ack_orchestration_with_changes(
            lock_token,
            item.execution_id,
            history_mgr.delta().to_vec(),
            vec![],
            vec![],
            metadata,
        ).await;
        
        // Record metrics
        self.record_orchestration_poison();
    }
}
```

#### Activity Dispatcher

```rust
impl Runtime {
    async fn process_activity_item(
        &self,
        item: WorkItem,
        lock_token: &str,
        attempt_count: u32,
        activities: &ActivityRegistry,
        worker_id: &str,
    ) {
        // Extract activity info for error reporting
        let (instance, execution_id, activity_id, activity_name, input) = match &item {
            WorkItem::ActivityExecute { instance, execution_id, id, name, input } => {
                (instance.clone(), *execution_id, *id, name.clone(), input.clone())
            }
            _ => panic!("Expected ActivityExecute"),
        };
        
        // Check poison threshold FIRST
        if attempt_count > self.options.max_attempts {
            warn!(
                instance = %instance,
                activity_name = %activity_name,
                activity_id = activity_id,
                attempt_count = attempt_count,
                max_attempts = self.options.max_attempts,
                "Activity message exceeded max attempts, marking as poison"
            );
            
            let error = ErrorDetails::Poison {
                attempt_count,
                max_attempts: self.options.max_attempts,
                message_type: PoisonMessageType::Activity {
                    instance: instance.clone(),
                    execution_id,
                    activity_name: activity_name.clone(),
                    activity_id,
                },
                message: serde_json::to_string(&item).unwrap_or_default(),
            };
            
            // Ack with failure
            let _ = self.history_store.ack_work_item(
                lock_token,
                WorkItem::ActivityFailed {
                    instance,
                    execution_id,
                    id: activity_id,
                    details: error,
                },
            ).await;
            
            self.record_activity_poison();
            return;
        }
        
        // Normal processing continues...
    }
}
```

---

## Data Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           Poison Detection Flow                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Provider                         Runtime                                   │
│   ────────                         ───────                                   │
│                                                                              │
│   fetch_*_item()                                                             │
│       │                                                                      │
│       ├─► Increment attempt_count                                            │
│       │                                                                      │
│       └─► Return item + attempt_count                                        │
│                    │                                                         │
│                    ▼                                                         │
│              ┌─────────────────┐                                             │
│              │ attempt_count   │                                             │
│              │ > max_attempts? │                                             │
│              └────────┬────────┘                                             │
│                       │                                                      │
│           ┌───────────┴───────────┐                                          │
│           │                       │                                          │
│           ▼ NO                    ▼ YES                                      │
│    ┌──────────────┐       ┌───────────────────┐                              │
│    │   Process    │       │ Fail immediately  │                              │
│    │   normally   │       │ with Poison error │                              │
│    └──────────────┘       └───────────────────┘                              │
│           │                       │                                          │
│           ▼                       ▼                                          │
│    success / abandon        Orchestration Failed                             │
│           │                 or Activity Failed                               │
│           │                 (with poison message                             │
│           ▼                  in error details)                               │
│    (if abandon, next                                                         │
│     fetch increments                                                         │
│     attempt_count)                                                           │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Metrics

Add observability for poison detection:

```rust
// Counter: messages detected as poison
duroxide_poison_detected_total{type="orchestration|activity"}

// Can filter failed orchestrations by error category
duroxide_orchestration_failures_total{category="poison"}
duroxide_activity_failures_total{category="poison"}
```

---

## Implementation Phases

### Phase 1: Schema & Provider
1. Add `attempt_count` column to queue tables (schema migration)
2. Update `OrchestrationItem` struct with `attempt_count` field
3. Update `fetch_work_item` return type to include `attempt_count`
4. Implement increment-on-fetch in provider

### Phase 2: Error Types
1. Add `ErrorDetails::Poison` variant
2. Add `PoisonMessageType` enum
3. Update `category()`, `is_retryable()`, `display_message()`

### Phase 3: Runtime Integration
1. Add `max_attempts` to `RuntimeOptions`
2. Add poison check to orchestration dispatcher
3. Add poison check to activity dispatcher
4. Add metrics instrumentation

### Phase 4: Testing
1. Unit tests for attempt count tracking
2. Integration tests for poison detection flow
3. Tests for poison error serialization

---

## Testing Scenarios

1. **Orchestration poison**: Message that crashes dispatcher repeatedly
2. **Activity poison**: Activity that crashes worker repeatedly
3. **Mixed workload**: Poison message doesn't block healthy messages
4. **Threshold boundary**: Exactly N attempts before poison
5. **Error details**: Verify message content in error

---

## Success Criteria

| Metric | Target |
|--------|--------|
| Poison detected after N+1 attempts | 100% accuracy |
| Poison messages don't block queue | Zero impact on throughput |
| Error includes message content | Full serialized message |
| Abandon API unchanged | No breaking changes |

---

## Comparison: Before vs After

| Aspect | Before | After |
|--------|--------|-------|
| Crashing message | Retries forever | Fails after N attempts |
| Queue blocking | Permanent | Self-healing |
| Visibility | None | Clear error with message dump |
| Provider complexity | N/A | Simple counter increment |
| Abandon API | Unchanged | Unchanged |
| New tables | N/A | None needed |
