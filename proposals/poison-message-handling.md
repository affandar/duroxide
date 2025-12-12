# Poison Message Handling

## Overview

Implement dead-letter queue (DLQ) semantics for messages that repeatedly fail to process. After N abandon operations, messages are classified as "poison" and quarantined to prevent blocking the queue.

## Problem Statement

Currently, if a work item causes repeated failures (e.g., crashes the worker, causes infinite retries), it will:

1. Block the queue by cycling through lock → abandon → reappear
2. Waste compute resources on repeated failed attempts
3. Have no visibility into which messages are problematic
4. Have no mechanism to remove or inspect failed messages

## Goals

1. **Automatic quarantine**: After N abandons, move message to poison state
2. **Non-blocking**: Poisoned messages don't block normal queue processing
3. **Visibility**: Management API to enumerate poisoned messages
4. **Recovery**: Ability to remove or retry poisoned messages
5. **Proper error typing**: New error category for poison failures

---

## Detailed Design

### 1. New Error Type: Poison

Extend `ErrorDetails` with a new variant:

```rust
/// Structured error details for orchestration failures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ErrorDetails {
    Infrastructure { ... },
    Configuration { ... },
    Application { ... },
    
    /// Poison message error - message abandoned too many times.
    /// 
    /// This indicates a message that repeatedly fails to process.
    /// Could be caused by:
    /// - Malformed message data causing deserialization failures
    /// - Message triggering bugs that crash the worker
    /// - Transient infrastructure issues that have become permanent
    /// - Application code bugs triggered by specific input patterns
    Poison {
        /// Number of times the message was abandoned
        abandon_count: u32,
        /// Maximum allowed abandons before poison classification
        max_abandons: u32,
        /// Last error message if available
        last_error: Option<String>,
        /// Message type (orchestration or activity)
        message_type: PoisonMessageType,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PoisonMessageType {
    Orchestration,
    Activity { activity_name: String, activity_id: u64 },
}
```

Update `category()` and related methods:

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
            // ... existing ...
            ErrorDetails::Poison { .. } => false, // Never retryable
        }
    }
}
```

### 2. Schema Changes

Add abandon tracking and poison queue tables:

```sql
-- Modify orchestrator_queue: add abandon_count
ALTER TABLE orchestrator_queue ADD COLUMN abandon_count INTEGER DEFAULT 0;

-- Modify worker_queue: add abandon_count  
ALTER TABLE worker_queue ADD COLUMN abandon_count INTEGER DEFAULT 0;

-- New table: poisoned orchestration messages
CREATE TABLE IF NOT EXISTS poison_orchestrator_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    work_item TEXT NOT NULL,
    abandon_count INTEGER NOT NULL,
    last_error TEXT,
    poisoned_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    original_created_at TIMESTAMP,
    
    -- For enumeration
    FOREIGN KEY (instance_id) REFERENCES instances(instance_id)
);

CREATE INDEX idx_poison_orch_instance ON poison_orchestrator_queue(instance_id);
CREATE INDEX idx_poison_orch_time ON poison_orchestrator_queue(poisoned_at);

-- New table: poisoned activity messages
CREATE TABLE IF NOT EXISTS poison_worker_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    activity_id INTEGER NOT NULL,
    activity_name TEXT NOT NULL,
    work_item TEXT NOT NULL,
    abandon_count INTEGER NOT NULL,
    last_error TEXT,
    poisoned_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    original_created_at TIMESTAMP
);

CREATE INDEX idx_poison_work_instance ON poison_worker_queue(instance_id);
CREATE INDEX idx_poison_work_time ON poison_worker_queue(poisoned_at);
```

### 3. Provider Interface Extensions

Add new methods to the `Provider` trait:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    // ... existing methods ...
    
    /// Abandon an orchestration item with optional error tracking.
    /// 
    /// # Behavior
    /// 
    /// 1. If `increment_abandon_count` is true:
    ///    - Increment the message's abandon_count
    ///    - If abandon_count >= max_abandons, move to poison queue
    /// 2. Release the lock (set lock_token = NULL)
    /// 3. Apply visibility delay if provided
    /// 
    /// # Parameters
    /// 
    /// * `lock_token` - Token identifying the locked message(s)
    /// * `delay` - Optional visibility delay before message reappears
    /// * `options` - Abandon options controlling count increment and error tracking
    /// 
    /// # Returns
    /// 
    /// * `Ok(AbandonResult::Released)` - Message released back to queue
    /// * `Ok(AbandonResult::Poisoned)` - Message moved to poison queue
    /// * `Err(...)` - Provider error
    async fn abandon_orchestration_item(
        &self,
        lock_token: &str,
        delay: Option<Duration>,
        options: AbandonOptions,
    ) -> Result<AbandonResult, ProviderError>;
    
    /// Abandon a work item with optional error tracking.
    async fn abandon_work_item(
        &self,
        lock_token: &str,
        options: AbandonOptions,
    ) -> Result<AbandonResult, ProviderError>;
}

/// Options for abandon operations
#[derive(Debug, Clone, Default)]
pub struct AbandonOptions {
    /// Whether to increment the abandon count.
    /// Set to false if the message was never processed (e.g., expired in buffer).
    pub increment_count: bool,
    
    /// Whether to immediately mark as poison regardless of count.
    /// Used when dispatcher detects unrecoverable error.
    pub force_poison: bool,
    
    /// Error message to record (for debugging).
    pub error_message: Option<String>,
    
    /// Maximum abandon count before poisoning (from RuntimeOptions).
    pub max_abandon_count: u32,
}

/// Result of an abandon operation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbandonResult {
    /// Message released back to queue for retry
    Released,
    /// Message moved to poison queue (exceeded max abandons or forced)
    Poisoned { abandon_count: u32 },
}
```

### 4. Configuration

Add poison threshold to `RuntimeOptions`:

```rust
pub struct RuntimeOptions {
    // ... existing fields ...
    
    /// Maximum times a message can be abandoned before being poisoned.
    /// 
    /// After this many abandon operations (with increment_count=true),
    /// the message is moved to the poison queue and the orchestration/activity
    /// is marked as failed with a Poison error.
    /// 
    /// Default: 5
    pub max_abandon_count: u32,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            // ... existing ...
            max_abandon_count: 5,
        }
    }
}
```

### 5. Dispatcher Integration

#### Orchestration Dispatcher

```rust
impl Runtime {
    async fn process_orchestration_item(
        self: &Arc<Self>,
        item: OrchestrationItem,
        worker_id: &str,
    ) {
        // ... existing processing logic ...
        
        // On failure that requires abandon:
        match self.ack_orchestration_with_changes(...).await {
            Ok(()) => { /* success */ }
            Err(e) => {
                // Determine abandon options based on error type
                let options = AbandonOptions {
                    increment_count: true, // Message was processed, count it
                    force_poison: false,
                    error_message: Some(e.to_string()),
                    max_abandon_count: self.options.max_abandon_count,
                };
                
                match self.history_store
                    .abandon_orchestration_item(lock_token, Some(delay), options)
                    .await
                {
                    Ok(AbandonResult::Released) => {
                        // Message will be retried
                    }
                    Ok(AbandonResult::Poisoned { abandon_count }) => {
                        // Message poisoned - fail the orchestration
                        self.fail_orchestration_as_poison(
                            &item.instance,
                            item.execution_id,
                            abandon_count,
                            Some(e.to_string()),
                        ).await;
                    }
                    Err(abandon_err) => {
                        warn!("Failed to abandon: {:?}", abandon_err);
                    }
                }
            }
        }
    }
    
    /// Fail an orchestration with poison error
    async fn fail_orchestration_as_poison(
        &self,
        instance: &str,
        execution_id: u64,
        abandon_count: u32,
        last_error: Option<String>,
    ) {
        let error = ErrorDetails::Poison {
            abandon_count,
            max_abandons: self.options.max_abandon_count,
            last_error,
            message_type: PoisonMessageType::Orchestration,
        };
        
        // Create failure event and commit
        // (similar to current infrastructure error handling)
    }
}
```

#### Activity Dispatcher

```rust
impl Runtime {
    async fn process_activity_item(...) {
        // ... activity execution ...
        
        // On unrecoverable failure:
        let options = AbandonOptions {
            increment_count: true,
            force_poison: false,
            error_message: Some(error.to_string()),
            max_abandon_count: self.options.max_abandon_count,
        };
        
        match self.history_store.abandon_work_item(&token, options).await {
            Ok(AbandonResult::Released) => { /* retry */ }
            Ok(AbandonResult::Poisoned { abandon_count }) => {
                // Return poison error as activity failure
                let _ = self.history_store.ack_work_item(
                    &token,
                    WorkItem::ActivityFailed {
                        instance: instance.clone(),
                        execution_id,
                        id: activity_id,
                        details: ErrorDetails::Poison {
                            abandon_count,
                            max_abandons: self.options.max_abandon_count,
                            last_error: Some(error),
                            message_type: PoisonMessageType::Activity {
                                activity_name: name.clone(),
                                activity_id,
                            },
                        },
                    },
                ).await;
            }
            Err(_) => { /* log */ }
        }
    }
}
```

#### Buffer Expiry (No Count Increment)

For the fetcher-executor model, messages that expire in the buffer without being processed:

```rust
// Message expired in buffer before executor picked it up
let options = AbandonOptions {
    increment_count: false, // Never processed, don't count
    force_poison: false,
    error_message: None,
    max_abandon_count: self.options.max_abandon_count,
};
self.history_store.abandon_work_item(&token, options).await;
```

### 6. Management API Extensions

Add to `ManagementProvider`:

```rust
#[async_trait]
pub trait ManagementProvider: Send + Sync {
    // ... existing methods ...
    
    // ===== Poison Message Management =====
    
    /// List poisoned orchestration messages.
    /// 
    /// Returns summary of all poisoned orchestration messages,
    /// optionally filtered by instance.
    async fn list_poison_orchestrations(
        &self,
        instance_filter: Option<&str>,
    ) -> Result<Vec<PoisonOrchestrationSummary>, ProviderError> {
        Ok(Vec::new())
    }
    
    /// List poisoned activity messages.
    async fn list_poison_activities(
        &self,
        instance_filter: Option<&str>,
    ) -> Result<Vec<PoisonActivitySummary>, ProviderError> {
        Ok(Vec::new())
    }
    
    /// Get details of a specific poisoned orchestration message.
    async fn get_poison_orchestration(
        &self,
        poison_id: i64,
    ) -> Result<Option<PoisonOrchestrationDetail>, ProviderError> {
        Ok(None)
    }
    
    /// Get details of a specific poisoned activity message.
    async fn get_poison_activity(
        &self,
        poison_id: i64,
    ) -> Result<Option<PoisonActivityDetail>, ProviderError> {
        Ok(None)
    }
    
    /// Remove a poisoned orchestration message (acknowledge and discard).
    /// 
    /// The orchestration should already be marked as failed.
    /// This removes the message from the poison queue.
    async fn remove_poison_orchestration(
        &self,
        poison_id: i64,
    ) -> Result<bool, ProviderError> {
        Ok(false)
    }
    
    /// Remove a poisoned activity message.
    async fn remove_poison_activity(
        &self,
        poison_id: i64,
    ) -> Result<bool, ProviderError> {
        Ok(false)
    }
    
    /// Retry a poisoned orchestration message.
    /// 
    /// Moves the message back to the active queue with reset abandon count.
    /// Use with caution - the message may poison again.
    async fn retry_poison_orchestration(
        &self,
        poison_id: i64,
    ) -> Result<bool, ProviderError> {
        Ok(false)
    }
    
    /// Retry a poisoned activity message.
    async fn retry_poison_activity(
        &self,
        poison_id: i64,
    ) -> Result<bool, ProviderError> {
        Ok(false)
    }
}

/// Summary of a poisoned orchestration message
#[derive(Debug, Clone)]
pub struct PoisonOrchestrationSummary {
    pub poison_id: i64,
    pub instance_id: String,
    pub abandon_count: u32,
    pub last_error: Option<String>,
    pub poisoned_at: chrono::DateTime<chrono::Utc>,
}

/// Summary of a poisoned activity message  
#[derive(Debug, Clone)]
pub struct PoisonActivitySummary {
    pub poison_id: i64,
    pub instance_id: String,
    pub execution_id: u64,
    pub activity_name: String,
    pub activity_id: u64,
    pub abandon_count: u32,
    pub last_error: Option<String>,
    pub poisoned_at: chrono::DateTime<chrono::Utc>,
}

/// Full details of a poisoned orchestration message
#[derive(Debug, Clone)]
pub struct PoisonOrchestrationDetail {
    pub summary: PoisonOrchestrationSummary,
    pub work_item: WorkItem, // The actual message
    pub original_created_at: chrono::DateTime<chrono::Utc>,
}

/// Full details of a poisoned activity message
#[derive(Debug, Clone)]
pub struct PoisonActivityDetail {
    pub summary: PoisonActivitySummary,
    pub work_item: WorkItem,
    pub input: String,
    pub original_created_at: chrono::DateTime<chrono::Utc>,
}
```

### 7. Queue Stats Extension

Update `QueueStats` to include poison counts:

```rust
#[derive(Debug, Clone, Default)]
pub struct QueueStats {
    pub orchestrator_queue: usize,
    pub worker_queue: usize,
    
    // NEW: Poison queue counts
    pub poison_orchestrator_count: usize,
    pub poison_worker_count: usize,
}
```

---

## Data Flow

```
                                Normal Processing Path
┌─────────────────────────────────────────────────────────────────────────────┐
│                                                                              │
│   Queue ──fetch──► Dispatcher ──process──► Success ──ack──► Complete       │
│     │                   │                                                    │
│     │                   │ failure                                            │
│     │                   ▼                                                    │
│     │              ┌─────────┐                                               │
│     │              │ Abandon │                                               │
│     │              └────┬────┘                                               │
│     │                   │                                                    │
│     │         ┌─────────┴─────────┐                                          │
│     │         │                   │                                          │
│     │         ▼                   ▼                                          │
│     │  count < max?         count >= max?                                    │
│     │         │                   │                                          │
│     │         ▼                   ▼                                          │
│     │  ┌──────────┐       ┌─────────────┐                                    │
│     └──│ Re-queue │       │ Poison Queue│──► Orchestration Failed (Poison)  │
│        │ (retry)  │       │ (quarantine)│    or Activity Failed (Poison)    │
│        └──────────┘       └─────────────┘                                    │
│                                  │                                           │
│                                  ▼                                           │
│                           Management API                                     │
│                           - list_poison_*                                    │
│                           - remove_poison_*                                  │
│                           - retry_poison_*                                   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Metrics

Add new metrics for observability:

```rust
// Counter: messages poisoned
duroxide_messages_poisoned_total{type="orchestration|activity"}

// Gauge: current poison queue depth  
duroxide_poison_queue_depth{type="orchestration|activity"}

// Counter: poison messages removed (via management API)
duroxide_poison_messages_removed_total{type="orchestration|activity"}

// Counter: poison messages retried (via management API)
duroxide_poison_messages_retried_total{type="orchestration|activity"}
```

---

## Implementation Phases

### Phase 1: Core Infrastructure
1. Add `abandon_count` column to existing queue tables (schema migration)
2. Create poison queue tables
3. Add `ErrorDetails::Poison` variant
4. Update `AbandonOptions` and `AbandonResult` types

### Phase 2: Provider Implementation
1. Implement abandon with count tracking in SQLite provider
2. Implement poison queue insertion logic
3. Update `QueueStats` to include poison counts

### Phase 3: Dispatcher Integration
1. Update orchestration dispatcher to use new abandon API
2. Update activity dispatcher to use new abandon API
3. Implement `fail_orchestration_as_poison` helper
4. Add metrics instrumentation

### Phase 4: Management API
1. Implement `list_poison_*` methods
2. Implement `get_poison_*` methods
3. Implement `remove_poison_*` methods
4. Implement `retry_poison_*` methods

### Phase 5: Testing & Documentation
1. Unit tests for abandon count tracking
2. Integration tests for poison flow
3. Management API tests
4. Update documentation

---

## Testing

1. **Unit tests**: Abandon count tracking, threshold behavior
2. **Integration tests**: End-to-end poison flow
3. **Property tests**: Poison after exactly N abandons
4. **Management API tests**: List, remove, retry operations
5. **Edge cases**:
   - Message poisoned while other messages for same instance are processing
   - Retry poisoned message that poisons again
   - Concurrent abandon operations on same message

---

## Success Criteria

| Metric | Target |
|--------|--------|
| Messages poisoned after N abandons | 100% accuracy |
| Poison messages don't block queue | Zero impact on throughput |
| Management API enumerate all poison | Complete coverage |
| No regression in happy path | Zero performance impact |

---

## Open Questions

1. **Should we support per-orchestration/activity poison thresholds?** Current design uses a global `max_abandon_count`. Some workflows might want different thresholds.

2. **Automatic cleanup of old poison messages?** Should we add TTL-based cleanup for poison queues, or require explicit removal via management API?

3. **Alerting integration?** Should poisoning trigger alerts/webhooks in addition to metrics?
