use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;

use crate::providers::{HistoryStore, WorkItem};

/// In-process fallback timer service.
/// Maintains a min-ordered queue of TimerSchedule items and enqueues TimerFired when due.
pub struct TimerService {
    store: Arc<dyn HistoryStore>,
    rx: tokio::sync::mpsc::UnboundedReceiver<WorkItem>,
    // key -> (instance, execution_id, id), keyed by "inst|exec|id|fire_at_ms"
    items: HashMap<String, (String, u64, u64)>,
    keys: HashSet<String>,
    min_heap: BinaryHeap<Reverse<(u64, String)>>,
    poller_idle_ms: u64,
}

impl TimerService {
    pub fn start(
        store: Arc<dyn HistoryStore>,
        poller_idle_ms: u64,
    ) -> (
        tokio::task::JoinHandle<()>,
        tokio::sync::mpsc::UnboundedSender<WorkItem>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<WorkItem>();
        let mut svc = TimerService {
            store,
            rx,
            items: HashMap::new(),
            keys: HashSet::new(),
            min_heap: BinaryHeap::new(),
            poller_idle_ms,
        };
        let handle = tokio::spawn(async move { svc.run().await });
        (handle, tx)
    }

    async fn run(&mut self) {
        loop {
            // Drain any queued schedules
            while let Ok(item) = self.rx.try_recv() {
                self.insert_item(item);
            }

            // Fire due timers
            let now = now_ms();
            let mut due: Vec<(String, u64, u64, u64)> = Vec::new();
            while let Some(Reverse((ts, key))) = self.min_heap.peek().cloned() {
                if ts <= now {
                    let _ = self.min_heap.pop();
                    if let Some((inst, exec, id)) = self.items.remove(&key) {
                        self.keys.remove(&key);
                        due.push((inst, exec, id, ts));
                    }
                } else {
                    break;
                }
            }

            for (instance, execution_id, id, fire_at_ms) in due.drain(..) {
                let _ = self
                    .store
                    .enqueue_orchestrator_work(WorkItem::TimerFired {
                        instance,
                        execution_id,
                        id,
                        fire_at_ms,
                    })
                    .await;
            }

            // Wait for next event or schedule
            if let Some(Reverse((next_ts, _))) = self.min_heap.peek().cloned() {
                let now = now_ms();
                let dur_ms = next_ts.saturating_sub(now).max(1);
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(dur_ms)) => {},
                    maybe = self.rx.recv() => {
                        match maybe {
                            Some(item) => self.insert_item(item),
                            _ => tokio::time::sleep(std::time::Duration::from_millis(self.poller_idle_ms)).await,
                        }
                    }
                }
            } else {
                // No timers; block on next schedule
                match self.rx.recv().await {
                    Some(item) => self.insert_item(item),
                    _ => tokio::time::sleep(std::time::Duration::from_millis(self.poller_idle_ms)).await,
                }
            }
        }
    }

    fn insert_item(&mut self, item: WorkItem) {
        if let WorkItem::TimerSchedule {
            instance,
            execution_id,
            id,
            fire_at_ms,
        } = item
        {
            let key = format!("{}|{}|{}|{}", instance, execution_id, id, fire_at_ms);
            if self.keys.insert(key.clone()) {
                self.min_heap.push(Reverse((fire_at_ms, key.clone())));
                self.items.insert(key, (instance, execution_id, id));
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Event;
    use crate::providers::in_memory::InMemoryHistoryStore;
    use tokio::sync::Mutex as TokioMutex;

    #[tokio::test]
    async fn fires_due_timers_in_order() {
        // Capture enqueued orchestrator items instead of draining via dequeue/ack
        #[derive(Clone)]
        struct CaptureStore {
            inner: Arc<InMemoryHistoryStore>,
            captured: Arc<TokioMutex<Vec<WorkItem>>>,
        }
        #[async_trait::async_trait]
        impl HistoryStore for CaptureStore {
            async fn read(&self, instance: &str) -> Vec<Event> { self.inner.read(instance).await }
            async fn list_instances(&self) -> Vec<String> { self.inner.list_instances().await }

            async fn enqueue_orchestrator_work(&self, item: WorkItem) -> Result<(), String> {
                self.captured.lock().await.push(item);
                Ok(())
            }

            async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String> { self.inner.enqueue_worker_work(item).await }
            async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> { self.inner.dequeue_worker_peek_lock().await }
            async fn ack_worker(&self, token: &str) -> Result<(), String> { self.inner.ack_worker(token).await }

            async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String> { self.inner.enqueue_timer_work(item).await }
            async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)> { self.inner.dequeue_timer_peek_lock().await }
            async fn ack_timer(&self, token: &str) -> Result<(), String> { self.inner.ack_timer(token).await }

            async fn latest_execution_id(&self, instance: &str) -> Option<u64> { self.inner.latest_execution_id(instance).await }
            async fn list_executions(&self, instance: &str) -> Vec<u64> { self.inner.list_executions(instance).await }
            async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event> { self.inner.read_with_execution(instance, execution_id).await }
            async fn append_with_execution(&self, instance: &str, execution_id: u64, new_events: Vec<Event>) -> Result<(), String> { self.inner.append_with_execution(instance, execution_id, new_events).await }
            async fn create_new_execution(&self, instance: &str, orchestration: &str, version: &str, input: &str, parent_instance: Option<&str>, parent_id: Option<u64>) -> Result<u64, String> { self.inner.create_new_execution(instance, orchestration, version, input, parent_instance, parent_id).await }
            fn supports_delayed_visibility(&self) -> bool { self.inner.supports_delayed_visibility() }

            async fn fetch_orchestration_item(&self) -> Option<crate::providers::OrchestrationItem> { self.inner.fetch_orchestration_item().await }
            async fn ack_orchestration_item(&self, lock_token: &str, history_delta: Vec<Event>, worker_items: Vec<WorkItem>, timer_items: Vec<WorkItem>, orchestrator_items: Vec<WorkItem>) -> Result<(), String> { self.inner.ack_orchestration_item(lock_token, history_delta, worker_items, timer_items, orchestrator_items).await }
            async fn abandon_orchestration_item(&self, lock_token: &str, delay_ms: Option<u64>) -> Result<(), String> { self.inner.abandon_orchestration_item(lock_token, delay_ms).await }
        }

        let base = Arc::new(InMemoryHistoryStore::default());
        let captured: Arc<TokioMutex<Vec<WorkItem>>> = Arc::new(TokioMutex::new(Vec::new()));
        let store: Arc<dyn HistoryStore> = Arc::new(CaptureStore { inner: base, captured: captured.clone() });
        let (_jh, tx) = TimerService::start(store.clone(), 5);
        // schedule three timers: immediate, +10ms, +5ms
        let now = now_ms();
        let _ = tx.send(WorkItem::TimerSchedule {
            instance: "i".into(),
            execution_id: 1,
            id: 1,
            fire_at_ms: now,
        });
        let _ = tx.send(WorkItem::TimerSchedule {
            instance: "i".into(),
            execution_id: 1,
            id: 2,
            fire_at_ms: now + 10,
        });
        let _ = tx.send(WorkItem::TimerSchedule {
            instance: "i".into(),
            execution_id: 1,
            id: 3,
            fire_at_ms: now + 5,
        });

        // Poll captured TimerFired items for order
        let mut fired: Vec<u64> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while fired.len() < 3 && std::time::Instant::now() < deadline {
            let mut lock = captured.lock().await;
            if !lock.is_empty() {
                let mut remaining: Vec<WorkItem> = Vec::new();
                for wi in lock.drain(..) {
                    match wi {
                        WorkItem::TimerFired { id, .. } => fired.push(id),
                        other => remaining.push(other),
                    }
                }
                // put back non-timer items if any (none expected)
                lock.extend(remaining.into_iter());
            }
            if fired.len() < 3 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
        assert_eq!(fired, vec![1, 3, 2]);
    }
}
