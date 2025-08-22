use std::collections::HashMap;
use tokio::sync::Mutex;

use crate::Event;
use super::{HistoryStore, WorkItem, QueueKind};

const CAP: usize = 1024;

#[derive(Default)]
pub struct InMemoryHistoryStore {
    // Multi-execution: instance -> executions (execution_id starts at 1)
    inner: Mutex<HashMap<String, Vec<Vec<Event>>>>,
    orchestrator_q: Mutex<Vec<WorkItem>>, // simple FIFO
    worker_q: Mutex<Vec<WorkItem>>, // simple FIFO
    timer_q: Mutex<Vec<WorkItem>>, // simple FIFO
    // Peek-lock state per-queue: token -> item. Items here are invisible until ack/abandon.
    invisible_orchestrator: Mutex<HashMap<String, WorkItem>>,
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
        self.append_with_execution(instance, self.latest_execution_id(instance).await.unwrap_or(1), new_events).await
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
            for e in events { out.push_str(&format!("  {e:#?}\n")); }
        }
        out
    }

    async fn create_instance(&self, instance: &str) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        if g.contains_key(instance) { return Err(format!("instance already exists: {instance}")); }
        g.insert(instance.to_string(), vec![Vec::new()]);
        Ok(())
    }

    async fn remove_instance(&self, instance: &str) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        if g.remove(instance).is_none() { return Err(format!("instance not found: {instance}")); }
        Ok(())
    }

    async fn enqueue_work(&self, kind: QueueKind, item: WorkItem) -> Result<(), String> {
        match kind {
            QueueKind::Orchestrator => {
                let mut q = self.orchestrator_q.lock().await;
                if !q.contains(&item) { q.push(item); }
            }
            QueueKind::Worker => {
                let mut q = self.worker_q.lock().await;
                if !q.contains(&item) { q.push(item); }
            }
            QueueKind::Timer => {
                let mut q = self.timer_q.lock().await;
                if !q.contains(&item) { q.push(item); }
            }
        }
        Ok(())
    }

    // dequeue_work removed; runtime uses peek-lock only

    async fn dequeue_peek_lock(&self, kind: QueueKind) -> Option<(WorkItem, String)> {
        let (item_opt, token) = match kind {
            QueueKind::Orchestrator => {
                let mut q = self.orchestrator_q.lock().await;
                if q.is_empty() { return None; }
                let item = q.remove(0);
                let token = format!("o:{}:{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos(), q.len());
                (Some(item), token)
            }
            QueueKind::Worker => {
                let mut q = self.worker_q.lock().await;
                if q.is_empty() { return None; }
                let item = q.remove(0);
                let token = format!("w:{}:{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos(), q.len());
                (Some(item), token)
            }
            QueueKind::Timer => {
                let mut q = self.timer_q.lock().await;
                if q.is_empty() { return None; }
                let item = q.remove(0);
                let token = format!("t:{}:{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos(), q.len());
                (Some(item), token)
            }
        };
        let item = item_opt?;
        match kind {
            QueueKind::Orchestrator => { self.invisible_orchestrator.lock().await.insert(token.clone(), item.clone()); }
            QueueKind::Worker => { self.invisible_worker.lock().await.insert(token.clone(), item.clone()); }
            QueueKind::Timer => { self.invisible_timer.lock().await.insert(token.clone(), item.clone()); }
        }
        Some((item, token))
    }

    async fn ack(&self, kind: QueueKind, token: &str) -> Result<(), String> {
        match kind {
            QueueKind::Orchestrator => { self.invisible_orchestrator.lock().await.remove(token); }
            QueueKind::Worker => { self.invisible_worker.lock().await.remove(token); }
            QueueKind::Timer => { self.invisible_timer.lock().await.remove(token); }
        }
        Ok(())
    }

    async fn abandon(&self, kind: QueueKind, token: &str) -> Result<(), String> {
        let item_opt = match kind {
            QueueKind::Orchestrator => self.invisible_orchestrator.lock().await.remove(token),
            QueueKind::Worker => self.invisible_worker.lock().await.remove(token),
            QueueKind::Timer => self.invisible_timer.lock().await.remove(token),
        };
        if let Some(item) = item_opt {
            let q = match kind {
                QueueKind::Orchestrator => &self.orchestrator_q,
                QueueKind::Worker => &self.worker_q,
                QueueKind::Timer => &self.timer_q,
            };
            let mut qg = q.lock().await;
            qg.insert(0, item);
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
        match g.get(instance) { Some(v) if !v.is_empty() => (1..=v.len() as u64).collect(), _ => Vec::new() }
    }

    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event> {
        let g = self.inner.lock().await;
        match g.get(instance) {
            Some(execs) => execs.get((execution_id.saturating_sub(1)) as usize).cloned().unwrap_or_default(),
            None => Vec::new(),
        }
    }

    async fn append_with_execution(&self, instance: &str, execution_id: u64, new_events: Vec<Event>) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        let execs = g.get_mut(instance).ok_or_else(|| format!("instance not found: {instance}"))?;
        let idx = (execution_id.saturating_sub(1)) as usize;
        if idx >= execs.len() { return Err(format!("execution not found: {}#{}", instance, execution_id)); }
        let cur = &mut execs[idx];
        if cur.len() + new_events.len() > CAP {
            return Err(format!("history cap exceeded (cap={}, have={}, append={})", CAP, cur.len(), new_events.len()));
        }
        // Idempotent append for completion-like events by (kind,id)
        use std::collections::HashSet;
        let mut seen: HashSet<(u64, &'static str)> = HashSet::new();
    for e in cur.iter() {
            match e {
                Event::ActivityCompleted { id, .. } => { seen.insert((*id, "ac")); }
                Event::ActivityFailed { id, .. } => { seen.insert((*id, "af")); }
                Event::TimerFired { id, .. } => { seen.insert((*id, "tf")); }
                Event::ExternalEvent { id, .. } => { seen.insert((*id, "xe")); }
                Event::SubOrchestrationCompleted { id, .. } => { seen.insert((*id, "sc")); }
                Event::SubOrchestrationFailed { id, .. } => { seen.insert((*id, "sf")); }
        Event::OrchestrationCompleted { .. } => { seen.insert((0, "oc")); }
        Event::OrchestrationFailed { .. } => { seen.insert((0, "of")); }
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
            if !dup { cur.push(e); }
        }
        Ok(())
    }

    async fn reset_for_continue_as_new(&self, instance: &str, orchestration: &str, version: &str, input: &str, parent_instance: Option<&str>, parent_id: Option<u64>) -> Result<u64, String> {
        let mut g = self.inner.lock().await;
        let execs = g.get_mut(instance).ok_or_else(|| format!("instance not found: {instance}"))?;
        execs.push(vec![Event::OrchestrationStarted { name: orchestration.to_string(), version: version.to_string(), input: input.to_string(), parent_instance: parent_instance.map(|s| s.to_string()), parent_id }]);
        Ok(execs.len() as u64)
    }
}

// No provider wrapper; runtime owns in-memory queues and workers. This module exposes
// only an in-memory HistoryStore for durability during tests.


