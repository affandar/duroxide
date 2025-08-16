use std::collections::HashMap;
use tokio::sync::Mutex;

use crate::Event;
use super::HistoryStore;

const CAP: usize = 1024;

#[derive(Default)]
pub struct InMemoryHistoryStore { inner: Mutex<HashMap<String, Vec<Event>>> }

#[async_trait::async_trait]
impl HistoryStore for InMemoryHistoryStore {
    async fn read(&self, instance: &str) -> Vec<Event> {
        self.inner.lock().await.get(instance).cloned().unwrap_or_default()
    }
    async fn append(&self, instance: &str, new_events: Vec<Event>) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        let ent = match g.get_mut(instance) {
            Some(v) => v,
            None => return Err(format!("instance not found: {instance}")),
        };
        if ent.len() + new_events.len() > CAP {
            return Err(format!("history cap exceeded (cap={}, have={}, append={})", CAP, ent.len(), new_events.len()));
        }
        ent.extend(new_events);
        Ok(())
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
        g.insert(instance.to_string(), Vec::new());
        Ok(())
    }

    async fn remove_instance(&self, instance: &str) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        if g.remove(instance).is_none() { return Err(format!("instance not found: {instance}")); }
        Ok(())
    }
}

// No provider wrapper; runtime owns in-memory queues and workers. This module exposes
// only an in-memory HistoryStore for durability during tests.


