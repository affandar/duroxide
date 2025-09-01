# Transactional Provider - Practical Example

## The Problem We're Solving

Currently, the most critical issue in DTF is this pattern in `dispatch.rs`:

```rust
// Current BROKEN code - ignores failures!
let _ = rt.history_store
    .enqueue_work(QueueKind::Worker, WorkItem::ActivityExecute { ... })
    .await;
```

If this fails after history is persisted, the orchestration hangs forever.

## Solution: Before and After

### Before (Current Broken Implementation)

```rust
// orchestration_turn.rs - Current implementation
impl OrchestrationTurn {
    pub async fn persist_changes(&mut self, store: Arc<dyn HistoryStore>, runtime: &Runtime) -> Result<(), String> {
        // Step 1: Append history (can succeed)
        store.append(&self.instance, self.history_delta.clone())
            .await
            .map_err(|e| format!("failed to append history: {}", e))?;
        
        // Step 2: Apply decisions (failures ignored!)
        runtime.apply_decisions(&self.instance, &full_history, self.pending_actions.clone()).await;
        //                      ^^^ This calls dispatch functions that use `let _ =`
        
        // PROBLEM: If step 2 fails, history says "ActivityScheduled" but no work item exists!
        // Result: Orchestration waits forever for an activity that will never run
        
        Ok(())  // Returns success even if dispatch failed!
    }
}
```

### After (With Transactional Provider)

```rust
// orchestration_turn.rs - Fixed with transactions
impl OrchestrationTurn {
    pub async fn persist_changes(&mut self, store: Arc<dyn TransactionalHistoryStore>, runtime: &Runtime) -> Result<(), String> {
        // Build transaction with ALL operations
        let ops = vec![
            // History changes
            TransactionOp::AppendHistory {
                instance: self.instance.clone(),
                events: self.history_delta.clone(),
            },
            // Convert decisions to work items
            TransactionOp::EnqueueWork {
                queue: QueueKind::Worker,
                items: vec![
                    WorkItem::ActivityExecute {
                        instance: self.instance.clone(),
                        execution_id: self.execution_id,
                        id: 123,
                        name: "SendEmail".into(),
                        input: "user@example.com".into(),
                    }
                ],
            },
            // Acknowledge processed messages
            TransactionOp::AckMessages {
                queue: QueueKind::Orchestrator,
                tokens: self.ack_tokens.clone(),
            },
        ];
        
        // Execute atomically - ALL succeed or ALL fail
        store.atomic_batch(ops).await?;
        
        // If we get here, EVERYTHING succeeded atomically
        // No possibility of partial failure!
        Ok(())
    }
}
```

## Real-World Scenario

Let's trace through what happens when a work queue operation fails:

### Scenario: Activity Dispatch Failure

**Orchestration code:**
```rust
async fn process_order(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    // Charge payment
    let payment = ctx.schedule_activity("ChargeCard", &order.card_info)
        .into_activity().await?;
    
    // Ship item
    let shipment = ctx.schedule_activity("ShipItem", &order.item_id)
        .into_activity().await?;
    
    Ok(format!("Order processed: payment={}, shipment={}", payment, shipment))
}
```

### Without Transactions (Current - BROKEN)

1. **Turn 1**: Schedule ChargeCard activity
   ```
   History: [OrchestrationStarted, ActivityScheduled(id=1, ChargeCard)]
   Queue: [ActivityExecute(ChargeCard)] <- ENQUEUE FAILS!
   ```
   - History append: ✅ SUCCESS
   - Queue enqueue: ❌ FAILS (network error)
   - Turn result: ✅ Returns success (failure ignored)

2. **Turn 2**: Process completions
   ```
   Waiting for: ActivityCompleted(id=1)
   Receives: Nothing (activity never ran!)
   ```
   - Orchestration waits forever
   - No error reported
   - System appears hung

### With Transactions (Proposed - SAFE)

1. **Turn 1**: Schedule ChargeCard activity
   ```rust
   let txn = Transaction {
       operations: vec![
           AppendHistory([ActivityScheduled(id=1, ChargeCard)]),
           EnqueueWork(Worker, [ActivityExecute(ChargeCard)])
       ]
   };
   store.commit(txn).await  // ATOMIC!
   ```
   - Transaction: ❌ FAILS (queue enqueue fails)
   - History: Not modified (transaction rolled back)
   - Turn result: ❌ Returns error
   - Messages: Not acknowledged (will retry)

2. **Turn 1 Retry**: Schedule ChargeCard activity (retry)
   ```
   Same transaction, retried
   ```
   - Transaction: ✅ SUCCESS (transient error resolved)
   - History: Appended atomically
   - Queue: Work item enqueued atomically
   - Messages: Acknowledged

3. **Turn 2**: Activity completes normally
   ```
   Receives: ActivityCompleted(id=1, "payment-12345")
   ```
   - Orchestration continues normally

## Implementation for FileSystem Provider

Here's a concrete implementation for the FileSystem provider:

```rust
use std::fs;
use std::path::PathBuf;
use tokio::fs::{File, rename};
use tokio::io::AsyncWriteExt;

impl TransactionalHistoryStore for FsHistoryStore {
    async fn atomic_batch(&self, ops: Vec<TransactionOp>) -> Result<BatchResult, String> {
        let txn_id = uuid::Uuid::new_v4().to_string();
        let staging_dir = self.root.join(".transactions").join(&txn_id);
        fs::create_dir_all(&staging_dir)?;
        
        // Stage all changes in temporary location
        let mut staged_files = Vec::new();
        
        for op in &ops {
            match op {
                TransactionOp::AppendHistory { instance, events } => {
                    // Read current history
                    let history_file = self.root.join("instances").join(instance).join("history.jsonl");
                    let mut current = self.read_jsonl(&history_file).await;
                    
                    // Append new events
                    current.extend(events.clone());
                    
                    // Write to staging
                    let staged_path = staging_dir.join(format!("{}-history.jsonl", instance));
                    self.write_jsonl(&staged_path, &current).await?;
                    
                    staged_files.push((staged_path, history_file));
                }
                
                TransactionOp::EnqueueWork { queue, items } => {
                    // Read current queue
                    let queue_file = self.root.join("queues").join(format!("{:?}.jsonl", queue));
                    let mut current = self.read_jsonl(&queue_file).await;
                    
                    // Append new items
                    for item in items {
                        current.push(QueueEntry {
                            id: uuid::Uuid::new_v4().to_string(),
                            item: item.clone(),
                            visible_at: Instant::now(),
                            delivery_count: 0,
                        });
                    }
                    
                    // Write to staging
                    let staged_path = staging_dir.join(format!("{:?}-queue.jsonl", queue));
                    self.write_jsonl(&staged_path, &current).await?;
                    
                    staged_files.push((staged_path, queue_file));
                }
                
                TransactionOp::AckMessages { queue, tokens } => {
                    // Read current queue
                    let queue_file = self.root.join("queues").join(format!("{:?}.jsonl", queue));
                    let mut current: Vec<QueueEntry> = self.read_jsonl(&queue_file).await;
                    
                    // Remove acknowledged messages
                    current.retain(|entry| !tokens.contains(&entry.id));
                    
                    // Write to staging
                    let staged_path = staging_dir.join(format!("{:?}-ack.jsonl", queue));
                    self.write_jsonl(&staged_path, &current).await?;
                    
                    staged_files.push((staged_path, queue_file));
                }
                
                _ => {} // Handle other operations
            }
        }
        
        // CRITICAL: Atomic commit phase
        // Use directory rename for atomicity on most filesystems
        
        // Step 1: Create backup of current state
        let backup_dir = self.root.join(".transactions").join(format!("{}-backup", txn_id));
        for (_, target) in &staged_files {
            if target.exists() {
                let backup_path = backup_dir.join(target.file_name().unwrap());
                fs::create_dir_all(backup_path.parent().unwrap())?;
                fs::copy(target, backup_path)?;
            }
        }
        
        // Step 2: Atomic rename all staged files to final locations
        // This is the commit point - either all succeed or we rollback
        let mut renamed = Vec::new();
        for (staged, target) in &staged_files {
            match rename(staged, target).await {
                Ok(_) => renamed.push(target.clone()),
                Err(e) => {
                    // ROLLBACK: Restore from backup
                    for path in renamed {
                        let backup = backup_dir.join(path.file_name().unwrap());
                        if backup.exists() {
                            let _ = rename(backup, path).await;
                        }
                    }
                    
                    // Clean up staging
                    let _ = fs::remove_dir_all(&staging_dir);
                    let _ = fs::remove_dir_all(&backup_dir);
                    
                    return Err(format!("Transaction failed during commit: {}", e));
                }
            }
        }
        
        // Success! Clean up staging and backup
        let _ = fs::remove_dir_all(&staging_dir);
        let _ = fs::remove_dir_all(&backup_dir);
        
        Ok(BatchResult {
            success: true,
            partial_results: vec![OpResult::Success; ops.len()],
        })
    }
}
```

## Summary

The transactional provider approach solves the critical reliability issues by:

1. **Atomicity**: History and queue operations succeed or fail together
2. **No Silent Failures**: Failures are propagated and handled
3. **Automatic Retry**: Failed transactions can be safely retried
4. **Simple Runtime**: No need for complex compensation logic
5. **Provider Flexibility**: Each provider can use its native transaction support

This is the single most important improvement we can make to DTF's reliability.
