use std::collections::HashMap;
use tokio::sync::Mutex;

use crate::Event;
use super::{HistoryStore, WorkItem};

const CAP: usize = 1024;

#[derive(Default)]
pub struct InMemoryHistoryStore {
    // Multi-execution: instance -> executions (execution_id starts at 1)
    inner: Mutex<HashMap<String, Vec<Vec<Event>>>>,
    work_q: Mutex<Vec<WorkItem>>, // simple FIFO
    meta: Mutex<HashMap<String, String>>, // instance -> orchestration name
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

    async fn enqueue_work(&self, item: WorkItem) -> Result<(), String> {
        self.work_q.lock().await.push(item);
        Ok(())
    }

    async fn dequeue_work(&self) -> Option<WorkItem> {
        let mut q = self.work_q.lock().await;
        if q.is_empty() { return None; }
        Some(q.remove(0))
    }

    async fn set_instance_orchestration(&self, instance: &str, orchestration: &str) -> Result<(), String> {
        self.meta.lock().await.insert(instance.to_string(), orchestration.to_string());
        Ok(())
    }

    async fn get_instance_orchestration(&self, instance: &str) -> Option<String> {
        self.meta.lock().await.get(instance).cloned()
    }

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
        cur.extend(new_events);
        Ok(())
    }

    async fn reset_for_continue_as_new(&self, instance: &str, _orchestration: &str, input: &str) -> Result<u64, String> {
        let mut g = self.inner.lock().await;
        let execs = g.get_mut(instance).ok_or_else(|| format!("instance not found: {instance}"))?;
        execs.push(vec![Event::OrchestrationStarted { name: self.meta.lock().await.get(instance).cloned().unwrap_or_default(), input: input.to_string() }]);
        Ok(execs.len() as u64)
    }
}

// No provider wrapper; runtime owns in-memory queues and workers. This module exposes
// only an in-memory HistoryStore for durability during tests.


