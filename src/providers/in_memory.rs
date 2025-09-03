use std::collections::HashMap;
use tokio::sync::Mutex;

use super::{HistoryStore, WorkItem};
use crate::Event;

const CAP: usize = 1024;

#[derive(Default)]
pub struct InMemoryHistoryStore {
    // Multi-execution: instance -> executions (execution_id starts at 1)
    inner: Mutex<HashMap<String, Vec<Vec<Event>>>>,
    orchestrator_q: Mutex<Vec<WorkItem>>, // simple FIFO
    worker_q: Mutex<Vec<WorkItem>>,       // simple FIFO
    timer_q: Mutex<Vec<WorkItem>>,        // simple FIFO
    // Peek-lock state per-queue: token -> item(s). Items here are invisible until ack/abandon.
    // Orchestrator stores Vec<WorkItem> for batch operations
    invisible_orchestrator: Mutex<HashMap<String, Vec<WorkItem>>>,
    invisible_worker: Mutex<HashMap<String, WorkItem>>,
    invisible_timer: Mutex<HashMap<String, WorkItem>>,
}

#[async_trait::async_trait]
impl HistoryStore for InMemoryHistoryStore {
    async fn read(&self, instance: &str) -> Vec<Event> {
        let g = self.inner.lock().await;
        match g.get(instance) {
            Some(execs) => execs.last().cloned().unwrap_or_default(),
            None => Vec::new(),
        }
    }
    async fn append(&self, instance: &str, new_events: Vec<Event>) -> Result<(), String> {
        self.append_with_execution(
            instance,
            self.latest_execution_id(instance).await.unwrap_or(1),
            new_events,
        )
        .await
    }
    async fn reset(&self) {
        self.inner.lock().await.clear();
    }
    async fn list_instances(&self) -> Vec<String> {
        self.inner.lock().await.keys().cloned().collect()
    }
    async fn dump_all_pretty(&self) -> String {
        let g = self.inner.lock().await;
        let mut out = String::new();
        for (inst, events) in g.iter() {
            out.push_str(&format!("instance={inst}\n"));
            for e in events {
                out.push_str(&format!("  {e:#?}\n"));
            }
        }
        out
    }

    async fn create_instance(&self, instance: &str) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        if g.contains_key(instance) {
            return Err(format!("instance already exists: {instance}"));
        }
        g.insert(instance.to_string(), vec![Vec::new()]);
        Ok(())
    }

    async fn remove_instance(&self, instance: &str) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        if g.remove(instance).is_none() {
            return Err(format!("instance not found: {instance}"));
        }
        Ok(())
    }

    // ===== Orchestrator Queue Methods =====
    
    async fn enqueue_orchestrator_work(&self, item: WorkItem) -> Result<(), String> {
        let mut q = self.orchestrator_q.lock().await;
        if !q.contains(&item) {
            q.push(item);
        }
        Ok(())
    }

    async fn dequeue_orchestrator_peek_lock(&self) -> Option<(Vec<WorkItem>, String)> {
        let mut q = self.orchestrator_q.lock().await;
        if q.is_empty() {
            return None;
        }
        
        // Group items by instance
        let mut instance_map: HashMap<String, Vec<(usize, WorkItem)>> = HashMap::new();
        for (idx, item) in q.iter().enumerate() {
            let instance = match item {
                WorkItem::StartOrchestration { instance, .. }
                | WorkItem::ActivityCompleted { instance, .. }
                | WorkItem::ActivityFailed { instance, .. }
                | WorkItem::TimerFired { instance, .. }
                | WorkItem::ExternalRaised { instance, .. }
                | WorkItem::CancelInstance { instance, .. }
                | WorkItem::ContinueAsNew { instance, .. } => instance.clone(),
                WorkItem::SubOrchCompleted { parent_instance, .. }
                | WorkItem::SubOrchFailed { parent_instance, .. } => parent_instance.clone(),
                _ => continue,
            };
            instance_map.entry(instance).or_default().push((idx, item.clone()));
        }
        
        // Take the first instance
        if let Some((instance, items)) = instance_map.into_iter().next() {
            // Remove items from queue in reverse order to maintain indices
            let mut indices: Vec<usize> = items.iter().map(|(idx, _)| *idx).collect();
            indices.sort_by(|a, b| b.cmp(a));
            for idx in indices {
                q.remove(idx);
            }
            
            let work_items: Vec<WorkItem> = items.into_iter().map(|(_, item)| item).collect();
            let token = format!(
                "o:{}:{}:{}",
                instance,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_nanos(),
                work_items.len()
            );
            
            // Store the batch in invisible map
            self.invisible_orchestrator
                .lock()
                .await
                .insert(token.clone(), work_items.clone());
                
            Some((work_items, token))
        } else {
            None
        }
    }
    
    async fn ack_orchestrator(&self, token: &str) -> Result<(), String> {
        self.invisible_orchestrator.lock().await.remove(token);
        Ok(())
    }
    
    async fn abandon_orchestrator(&self, token: &str) -> Result<(), String> {
        if let Some(items) = self.invisible_orchestrator.lock().await.remove(token) {
            let mut q = self.orchestrator_q.lock().await;
            // Insert at front to maintain FIFO semantics
            for item in items.into_iter().rev() {
                q.insert(0, item);
            }
        }
        Ok(())
    }
    
    // ===== Worker Queue Methods =====
    
    async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String> {
        let mut q = self.worker_q.lock().await;
        if !q.contains(&item) {
            q.push(item);
        }
        Ok(())
    }

    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> {
        let mut q = self.worker_q.lock().await;
        if q.is_empty() {
            return None;
        }
        let item = q.remove(0);
        let token = format!(
            "w:{}:{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos(),
            q.len()
        );
        
        self.invisible_worker.lock().await.insert(token.clone(), item.clone());
        Some((item, token))
    }
    
    async fn ack_worker(&self, token: &str) -> Result<(), String> {
        self.invisible_worker.lock().await.remove(token);
        Ok(())
    }
    
    async fn abandon_worker(&self, token: &str) -> Result<(), String> {
        if let Some(item) = self.invisible_worker.lock().await.remove(token) {
            let mut q = self.worker_q.lock().await;
            q.insert(0, item);
        }
        Ok(())
    }
    
    // ===== Timer Queue Methods =====
    
    async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String> {
        let mut q = self.timer_q.lock().await;
        if !q.contains(&item) {
            q.push(item);
        }
        Ok(())
    }
    
    async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)> {
        let mut q = self.timer_q.lock().await;
        if q.is_empty() {
            return None;
        }
        let item = q.remove(0);
        let token = format!(
            "t:{}:{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos(),
            q.len()
        );
        
        self.invisible_timer.lock().await.insert(token.clone(), item.clone());
        Some((item, token))
    }
    
    async fn ack_timer(&self, token: &str) -> Result<(), String> {
        self.invisible_timer.lock().await.remove(token);
        Ok(())
    }
    
    async fn abandon_timer(&self, token: &str) -> Result<(), String> {
        if let Some(item) = self.invisible_timer.lock().await.remove(token) {
            let mut q = self.timer_q.lock().await;
            q.insert(0, item);
        }
        Ok(())
    }

    // metadata APIs removed

    async fn latest_execution_id(&self, instance: &str) -> Option<u64> {
        let g = self.inner.lock().await;
        g.get(instance).map(|v| v.len() as u64)
    }

    async fn list_executions(&self, instance: &str) -> Vec<u64> {
        let g = self.inner.lock().await;
        match g.get(instance) {
            Some(v) if !v.is_empty() => (1..=v.len() as u64).collect(),
            _ => Vec::new(),
        }
    }

    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event> {
        let g = self.inner.lock().await;
        match g.get(instance) {
            Some(execs) => execs
                .get((execution_id.saturating_sub(1)) as usize)
                .cloned()
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    async fn append_with_execution(
        &self,
        instance: &str,
        execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        let execs = g
            .get_mut(instance)
            .ok_or_else(|| format!("instance not found: {instance}"))?;
        let idx = (execution_id.saturating_sub(1)) as usize;
        if idx >= execs.len() {
            return Err(format!("execution not found: {}#{}", instance, execution_id));
        }
        let cur = &mut execs[idx];
        if cur.len() + new_events.len() > CAP {
            return Err(format!(
                "history cap exceeded (cap={}, have={}, append={})",
                CAP,
                cur.len(),
                new_events.len()
            ));
        }
        // Idempotent append for completion-like events by (kind,id)
        use std::collections::HashSet;
        let mut seen: HashSet<(u64, &'static str)> = HashSet::new();
        for e in cur.iter() {
            match e {
                Event::ActivityCompleted { id, .. } => {
                    seen.insert((*id, "ac"));
                }
                Event::ActivityFailed { id, .. } => {
                    seen.insert((*id, "af"));
                }
                Event::TimerFired { id, .. } => {
                    seen.insert((*id, "tf"));
                }
                Event::ExternalEvent { id, .. } => {
                    seen.insert((*id, "xe"));
                }
                Event::SubOrchestrationCompleted { id, .. } => {
                    seen.insert((*id, "sc"));
                }
                Event::SubOrchestrationFailed { id, .. } => {
                    seen.insert((*id, "sf"));
                }
                Event::OrchestrationCompleted { .. } => {
                    seen.insert((0, "oc"));
                }
                Event::OrchestrationFailed { .. } => {
                    seen.insert((0, "of"));
                }
                _ => {}
            }
        }
        for e in new_events.into_iter() {
            let dup = match &e {
                Event::ActivityCompleted { id, .. } => seen.contains(&(*id, "ac")),
                Event::ActivityFailed { id, .. } => seen.contains(&(*id, "af")),
                Event::TimerFired { id, .. } => seen.contains(&(*id, "tf")),
                Event::ExternalEvent { id, .. } => seen.contains(&(*id, "xe")),
                Event::SubOrchestrationCompleted { id, .. } => seen.contains(&(*id, "sc")),
                Event::SubOrchestrationFailed { id, .. } => seen.contains(&(*id, "sf")),
                Event::OrchestrationCompleted { .. } => seen.contains(&(0, "oc")),
                Event::OrchestrationFailed { .. } => seen.contains(&(0, "of")),
                _ => false,
            };
            if !dup {
                cur.push(e);
            }
        }
        Ok(())
    }

    async fn create_new_execution(
        &self,
        instance: &str,
        orchestration: &str,
        version: &str,
        input: &str,
        parent_instance: Option<&str>,
        parent_id: Option<u64>,
    ) -> Result<u64, String> {
        let mut g = self.inner.lock().await;
        let execs = g
            .get_mut(instance)
            .ok_or_else(|| format!("instance not found: {instance}"))?;
        execs.push(vec![Event::OrchestrationStarted {
            name: orchestration.to_string(),
            version: version.to_string(),
            input: input.to_string(),
            parent_instance: parent_instance.map(|s| s.to_string()),
            parent_id,
        }]);
        Ok(execs.len() as u64)
    }

    // ===== New Atomic Orchestration Methods =====

    async fn fetch_orchestration_item(&self) -> Option<super::OrchestrationItem> {
        // First dequeue a batch of orchestrator messages
        let (messages, lock_token) = self.dequeue_orchestrator_peek_lock().await?;
        
        // Extract instance from the first message
        let instance = match messages.first()? {
            WorkItem::StartOrchestration { instance, .. }
            | WorkItem::ActivityCompleted { instance, .. }
            | WorkItem::ActivityFailed { instance, .. }
            | WorkItem::TimerFired { instance, .. }
            | WorkItem::ExternalRaised { instance, .. }
            | WorkItem::CancelInstance { instance, .. }
            | WorkItem::ContinueAsNew { instance, .. } => instance.clone(),
            WorkItem::SubOrchCompleted { parent_instance, .. }
            | WorkItem::SubOrchFailed { parent_instance, .. } => parent_instance.clone(),
            _ => return None,
        };
        
        // Read the history for this instance
        let history = self.read(&instance).await;
        
        // If this is a new instance (StartOrchestration), create it
        if history.is_empty() && messages.iter().any(|m| matches!(m, WorkItem::StartOrchestration { .. })) {
            let _ = self.create_instance(&instance).await;
        }
        
        // Extract orchestration metadata from history
        let (orchestration_name, version, execution_id) = if let Some(event) = history.iter().find(|e| {
            matches!(e, Event::OrchestrationStarted { .. })
        }) {
            match event {
                Event::OrchestrationStarted { name, version, .. } => {
                    (name.clone(), version.clone(), self.latest_execution_id(&instance).await.unwrap_or(1))
                }
                _ => return None,
            }
        } else {
            // New instance - extract from StartOrchestration message
            if let Some(WorkItem::StartOrchestration { orchestration, version, .. }) = messages.iter().find(|m| {
                matches!(m, WorkItem::StartOrchestration { .. })
            }) {
                (orchestration.clone(), version.clone().unwrap_or_else(|| "1.0.0".to_string()), 1)
            } else {
                return None;
            }
        };
        
        Some(super::OrchestrationItem {
            instance,
            orchestration_name,
            execution_id,
            version,
            history,
            messages,
            lock_token,
        })
    }
    
    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        timer_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
    ) -> Result<(), String> {
        // Get the instance from the invisible orchestrator map
        let instance = {
            let invisible = self.invisible_orchestrator.lock().await;
            let batch = invisible.get(lock_token)
                .ok_or_else(|| "Lock token not found".to_string())?;
            
            match batch.first() {
                Some(item) => match item {
                    WorkItem::StartOrchestration { instance, .. }
                    | WorkItem::ActivityCompleted { instance, .. }
                    | WorkItem::ActivityFailed { instance, .. }
                    | WorkItem::TimerFired { instance, .. }
                    | WorkItem::ExternalRaised { instance, .. }
                    | WorkItem::CancelInstance { instance, .. }
                    | WorkItem::ContinueAsNew { instance, .. } => instance.clone(),
                    WorkItem::SubOrchCompleted { parent_instance, .. }
                    | WorkItem::SubOrchFailed { parent_instance, .. } => parent_instance.clone(),
                    _ => return Err("Cannot determine instance from work item".to_string()),
                },
                None => return Err("Empty batch in lock".to_string()),
            }
        };
        
        // Perform all operations with best-effort atomicity
        let mut errors = Vec::new();
        
        // 1. Append history delta
        if !history_delta.is_empty() {
            if let Err(e) = self.append(&instance, history_delta).await {
                errors.push(format!("Failed to append history: {}", e));
            }
        }
        
        // 2. Enqueue worker items
        {
            let mut q = self.worker_q.lock().await;
            for item in worker_items {
                q.push(item);
            }
        }
        
        // 3. Enqueue timer items
        {
            let mut q = self.timer_q.lock().await;
            for item in timer_items {
                q.push(item);
            }
        }
        
        // 4. Enqueue orchestrator items
        {
            let mut q = self.orchestrator_q.lock().await;
            for item in orchestrator_items {
                q.push(item);
            }
        }
        
        // 5. Acknowledge the batch (release the lock)
        {
            let mut invisible = self.invisible_orchestrator.lock().await;
            invisible.remove(lock_token);
        }
        
        // Return error if any operation failed
        if !errors.is_empty() {
            Err(errors.join("; "))
        } else {
            Ok(())
        }
    }
    
    async fn abandon_orchestration_item(
        &self,
        lock_token: &str,
        delay_ms: Option<u64>,
    ) -> Result<(), String> {
        if delay_ms.is_some() {
            tracing::warn!("visibility delay not yet implemented for in_memory provider");
        }
        
        // Simply abandon the orchestrator batch
        self.abandon_orchestrator(lock_token).await
    }
}

// No provider wrapper; runtime owns in-memory queues and workers. This module exposes
// only an in-memory HistoryStore for durability during tests.
