# Provider Transactionality Proposal

## Overview

This proposal introduces transactional capabilities at the provider level to address the critical persistence failures identified in the orchestration runtime. By moving transactionality into the storage layer, we can guarantee atomicity and consistency without complex compensation logic in the runtime.

## Core Transactional API

### 1. Transaction Context

```rust
/// Transaction handle for atomic operations
pub struct Transaction {
    id: String,
    operations: Vec<TransactionOp>,
    isolation_level: IsolationLevel,
}

pub enum IsolationLevel {
    ReadCommitted,    // Default - see committed data from other transactions
    RepeatableRead,   // Consistent view throughout transaction
    Serializable,     // Full isolation
}

pub enum TransactionOp {
    AppendHistory { 
        instance: String, 
        events: Vec<Event> 
    },
    EnqueueWork { 
        queue: QueueKind, 
        items: Vec<WorkItem> 
    },
    AckMessages { 
        queue: QueueKind, 
        tokens: Vec<String> 
    },
    ConditionalAppend {
        instance: String,
        condition: AppendCondition,
        events: Vec<Event>,
    },
    CreateInstance {
        instance: String,
        if_not_exists: bool,
    },
}

pub enum AppendCondition {
    HistoryLength(usize),           // Append only if history has exactly N events
    LastEventMatches(Event),        // Append only if last event matches
    NoTerminalEvent,                // Append only if no terminal event exists
    ExecutionIdEquals(u64),         // Append only if execution ID matches
}
```

### 2. Enhanced Provider Trait

```rust
#[async_trait::async_trait]
pub trait TransactionalHistoryStore: HistoryStore {
    /// Begin a new transaction
    async fn begin_transaction(&self) -> Result<Transaction, String>;
    
    /// Execute all operations atomically
    async fn commit(&self, txn: Transaction) -> Result<CommitResult, String>;
    
    /// Rollback a transaction
    async fn rollback(&self, txn: Transaction) -> Result<(), String>;
    
    /// Execute a batch of operations atomically without explicit transaction
    async fn atomic_batch(&self, ops: Vec<TransactionOp>) -> Result<BatchResult, String> {
        let txn = self.begin_transaction().await?;
        let mut txn = txn;
        txn.operations = ops;
        self.commit(txn).await
    }
}

pub struct CommitResult {
    pub success: bool,
    pub operations_applied: usize,
    pub conflict_reason: Option<String>,
}

pub struct BatchResult {
    pub success: bool,
    pub partial_results: Vec<OpResult>,
}

pub enum OpResult {
    Success,
    Failed(String),
    Skipped(String),  // For conditional operations
}
```

## Use Cases

### 1. Atomic Turn Persistence

**Current Problem**: History append succeeds but dispatch fails, causing hangs.

**Solution with Transactions**:
```rust
impl OrchestrationTurn {
    pub async fn persist_changes_transactional(
        &mut self,
        store: Arc<dyn TransactionalHistoryStore>,
        runtime: &Arc<Runtime>,
    ) -> Result<(), String> {
        let mut txn = store.begin_transaction().await?;
        
        // Add history append
        txn.operations.push(TransactionOp::AppendHistory {
            instance: self.instance.clone(),
            events: self.history_delta.clone(),
        });
        
        // Add all work items from decisions
        let work_items = self.convert_decisions_to_work_items(&self.pending_actions)?;
        for (queue, items) in work_items {
            txn.operations.push(TransactionOp::EnqueueWork {
                queue,
                items,
            });
        }
        
        // Add message acknowledgments
        txn.operations.push(TransactionOp::AckMessages {
            queue: QueueKind::Orchestrator,
            tokens: self.ack_tokens.clone(),
        });
        
        // Commit atomically - all succeed or all fail
        match store.commit(txn).await {
            Ok(result) if result.success => Ok(()),
            Ok(result) => Err(format!("Transaction failed: {:?}", result.conflict_reason)),
            Err(e) => Err(format!("Transaction error: {}", e)),
        }
    }
}
```

### 2. Conditional Operations

**Current Problem**: Race conditions when multiple executions exist (ContinueAsNew).

**Solution with Conditions**:
```rust
// Append terminal event only if no terminal event exists
let txn = store.begin_transaction().await?;
txn.operations.push(TransactionOp::ConditionalAppend {
    instance: instance.clone(),
    condition: AppendCondition::NoTerminalEvent,
    events: vec![Event::OrchestrationCompleted { output }],
});

// Notify parent only if append succeeded
txn.operations.push(TransactionOp::EnqueueWork {
    queue: QueueKind::Orchestrator,
    items: vec![WorkItem::SubOrchCompleted { ... }],
});

store.commit(txn).await?;
```

### 3. Multi-Instance Transactions

**Current Problem**: Sub-orchestration start requires operations on both parent and child.

**Solution with Multi-Instance Transactions**:
```rust
pub async fn start_sub_orchestration_transactional(
    store: Arc<dyn TransactionalHistoryStore>,
    parent_instance: &str,
    child_instance: &str,
    config: SubOrchConfig,
) -> Result<(), String> {
    let txn = store.begin_transaction().await?;
    
    // Create child instance
    txn.operations.push(TransactionOp::CreateInstance {
        instance: child_instance.to_string(),
        if_not_exists: true,
    });
    
    // Append start event to child
    txn.operations.push(TransactionOp::AppendHistory {
        instance: child_instance.to_string(),
        events: vec![Event::OrchestrationStarted { ... }],
    });
    
    // Update parent history
    txn.operations.push(TransactionOp::AppendHistory {
        instance: parent_instance.to_string(),
        events: vec![Event::SubOrchestrationScheduled { ... }],
    });
    
    // Enqueue work item
    txn.operations.push(TransactionOp::EnqueueWork {
        queue: QueueKind::Orchestrator,
        items: vec![WorkItem::StartOrchestration { ... }],
    });
    
    store.commit(txn).await?;
}
```

## Advanced Features

### 1. Optimistic Concurrency Control

```rust
pub struct OptimisticTransaction {
    base: Transaction,
    read_versions: HashMap<String, u64>,  // instance -> version
}

impl TransactionalHistoryStore {
    /// Read with version tracking for optimistic concurrency
    async fn read_versioned(&self, instance: &str) -> (Vec<Event>, u64);
    
    /// Commit with version validation
    async fn commit_optimistic(&self, txn: OptimisticTransaction) -> Result<CommitResult, String> {
        // Validate all read versions match current versions
        for (instance, expected_version) in &txn.read_versions {
            let (_, current_version) = self.read_versioned(instance).await;
            if current_version != *expected_version {
                return Ok(CommitResult {
                    success: false,
                    operations_applied: 0,
                    conflict_reason: Some(format!("Version conflict on instance {}", instance)),
                });
            }
        }
        self.commit(txn.base).await
    }
}
```

### 2. Saga Support

```rust
pub struct SagaTransaction {
    forward_ops: Vec<TransactionOp>,
    compensation_ops: Vec<TransactionOp>,
    checkpoint_after: Option<usize>,  // Create savepoint after N operations
}

impl TransactionalHistoryStore {
    async fn execute_saga(&self, saga: SagaTransaction) -> Result<SagaResult, String> {
        let mut completed_ops = 0;
        
        // Try forward path
        for (i, op) in saga.forward_ops.iter().enumerate() {
            if let Err(e) = self.atomic_batch(vec![op.clone()]).await {
                // Forward path failed, run compensations
                for j in (0..i).rev() {
                    if let Some(comp) = saga.compensation_ops.get(j) {
                        let _ = self.atomic_batch(vec![comp.clone()]).await;
                    }
                }
                return Err(format!("Saga failed at step {}: {}", i, e));
            }
            completed_ops += 1;
            
            // Create checkpoint if requested
            if Some(i) == saga.checkpoint_after {
                self.create_savepoint(&saga).await?;
            }
        }
        
        Ok(SagaResult { completed_ops })
    }
}
```

### 3. Distributed Transaction Coordination

```rust
/// Two-phase commit coordinator for cross-provider transactions
pub struct DistributedTransaction {
    id: String,
    participants: Vec<Arc<dyn TransactionalHistoryStore>>,
    operations: HashMap<usize, Vec<TransactionOp>>,  // participant index -> ops
}

pub async fn two_phase_commit(dtxn: DistributedTransaction) -> Result<(), String> {
    // Phase 1: Prepare
    let mut prepare_handles = vec![];
    for (idx, participant) in dtxn.participants.iter().enumerate() {
        let ops = dtxn.operations.get(&idx).cloned().unwrap_or_default();
        let participant = participant.clone();
        let handle = tokio::spawn(async move {
            participant.prepare_transaction(ops).await
        });
        prepare_handles.push(handle);
    }
    
    // Collect prepare results
    let mut all_prepared = true;
    for handle in prepare_handles {
        if handle.await??.is_err() {
            all_prepared = false;
            break;
        }
    }
    
    // Phase 2: Commit or Abort
    if all_prepared {
        for participant in &dtxn.participants {
            participant.commit_prepared(&dtxn.id).await?;
        }
        Ok(())
    } else {
        for participant in &dtxn.participants {
            let _ = participant.abort_prepared(&dtxn.id).await;
        }
        Err("Distributed transaction aborted".into())
    }
}
```

## Implementation Strategy

### Phase 1: Local Transactions (Immediate)
- Implement `atomic_batch` for single-provider operations
- Focus on FileSystem and InMemory providers
- Solve the critical dispatch failure issue

### Phase 2: Conditional Operations (Q2)
- Add conditional append support
- Implement optimistic concurrency control
- Enable safe ContinueAsNew handling

### Phase 3: Distributed Transactions (Q3)
- Add two-phase commit protocol
- Support for cloud providers (Azure, AWS)
- Cross-region transaction support

## Provider-Specific Implementation Notes

### FileSystem Provider
```rust
impl TransactionalHistoryStore for FsHistoryStore {
    async fn commit(&self, txn: Transaction) -> Result<CommitResult, String> {
        // Use file locking for atomicity
        let lock_file = format!("{}/txn-{}.lock", self.root, txn.id);
        let _lock = FileLock::acquire(&lock_file).await?;
        
        // Write all changes to temporary files
        let temp_files = self.write_to_temp(txn.operations).await?;
        
        // Atomically rename all temp files
        for (temp, final_path) in temp_files {
            fs::rename(temp, final_path).await?;
        }
        
        Ok(CommitResult { success: true, operations_applied: txn.operations.len(), conflict_reason: None })
    }
}
```

### Cloud Providers (Azure Storage, DynamoDB)
```rust
impl TransactionalHistoryStore for AzureTableStore {
    async fn commit(&self, txn: Transaction) -> Result<CommitResult, String> {
        // Use Azure Storage batch transactions
        let batch = TableBatch::new();
        
        for op in txn.operations {
            match op {
                TransactionOp::AppendHistory { instance, events } => {
                    // Convert to entity group transaction
                    let entities = events_to_entities(instance, events);
                    batch.insert_entities(entities);
                }
                TransactionOp::EnqueueWork { queue, items } => {
                    // Use Azure Queue Storage batch
                    let messages = items_to_messages(queue, items);
                    batch.enqueue_messages(messages);
                }
                _ => {}
            }
        }
        
        // Execute as single batch transaction (up to 100 operations)
        self.table_client.execute_batch(batch).await
            .map(|_| CommitResult { success: true, operations_applied: txn.operations.len(), conflict_reason: None })
            .map_err(|e| e.to_string())
    }
}
```

## Benefits

1. **Atomicity**: All-or-nothing semantics eliminate partial failures
2. **Consistency**: No more hung orchestrations from dispatch failures
3. **Isolation**: Transactions prevent race conditions
4. **Durability**: Provider-level guarantees for persistence
5. **Simplicity**: Runtime code becomes much simpler without compensation logic
6. **Performance**: Batch operations reduce round trips
7. **Reliability**: Provider-native transactions are battle-tested

## Migration Path

1. **Add optional trait**: `TransactionalHistoryStore` extends `HistoryStore`
2. **Gradual adoption**: Runtime checks for transactional support, falls back to current behavior
3. **Provider upgrade**: Update providers one at a time
4. **Full migration**: Once all providers support transactions, remove old code paths

```rust
// Runtime adaptation
if let Some(txn_store) = store.as_any().downcast_ref::<dyn TransactionalHistoryStore>() {
    // Use transactional path
    turn.persist_changes_transactional(txn_store, runtime).await
} else {
    // Fall back to current implementation
    turn.persist_changes(store, runtime).await
}
```

## Conclusion

Adding transactionality at the provider level solves the fundamental reliability issues in the orchestration runtime. By guaranteeing atomic operations across history and work queues, we eliminate entire classes of bugs related to partial failures and race conditions. The proposed API is extensible, allowing for advanced features like distributed transactions while maintaining backward compatibility.
