use crate::OrchestrationContext;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::replay::ReplayDurableFuture;

/// TODO : CR : IMPORTANT: When merging into mainline code, all of this file should just go away and merge with existing code
/// in OrchestrationContext and other files that use it.
///
/// Extension trait for OrchestrationContext to add replay-aware scheduling methods
#[allow(dead_code)]
pub trait ReplaySchedulingExt {
    fn schedule_activity_replay(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture;

    fn schedule_timer_replay(
        &self,
        delay_ms: u64,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture;

    fn schedule_wait_replay(
        &self,
        name: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture;

    fn schedule_sub_orchestration_replay(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture;
}

impl ReplaySchedulingExt for OrchestrationContext {
    fn schedule_activity_replay(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        let name: String = name.into();
        let input: String = input.into();
        let mut inner = self.inner.lock().unwrap();
        // Try to adopt an existing scheduled activity id that matches and isn't claimed yet
        let adopted_id = inner
            .history
            .iter()
            .find_map(|e| match e {
                crate::Event::ActivityScheduled { id, name: n, input: inp, execution_id: _ } if n == &name && inp == &input && !inner.claimed_activity_ids.contains(id) => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| inner.next_id());
        inner.claimed_activity_ids.insert(adopted_id);
        drop(inner);

        // Create ReplayDurableFuture
        let replay_future = ReplayDurableFuture {
            ready: Arc::new(Mutex::new(false)),
            completion: Arc::new(Mutex::new(None)),
            should_emit_decision: Arc::new(Mutex::new(true)), // Will be set to false later if in history
        };

        // Insert into the HashMap
        let mut futures = open_futures.lock().unwrap();
        futures.insert(adopted_id, replay_future.clone());

        replay_future
    }

    fn schedule_timer_replay(
        &self,
        _delay_ms: u64,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        let mut inner = self.inner.lock().unwrap();
        // Adopt first unclaimed TimerCreated id if any, else allocate
        let adopted_id = inner
            .history
            .iter()
            .find_map(|e| match e {
                crate::Event::TimerCreated { id, .. } if !inner.claimed_timer_ids.contains(id) => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| inner.next_id());
        inner.claimed_timer_ids.insert(adopted_id);
        drop(inner);

        // Create ReplayDurableFuture
        let replay_future = ReplayDurableFuture {
            ready: Arc::new(Mutex::new(false)),
            completion: Arc::new(Mutex::new(None)),
            should_emit_decision: Arc::new(Mutex::new(true)),
        };

        // Insert into the HashMap
        let mut futures = open_futures.lock().unwrap();
        futures.insert(adopted_id, replay_future.clone());

        replay_future
    }

    fn schedule_wait_replay(
        &self,
        name: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        let name: String = name.into();
        let mut inner = self.inner.lock().unwrap();
        // Adopt existing subscription id for this name if present and unclaimed, else allocate
        let adopted_id = inner
            .history
            .iter()
            .find_map(|e| match e {
                crate::Event::ExternalSubscribed { id, name: n } if n == &name && !inner.claimed_external_ids.contains(id) => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| inner.next_id());
        inner.claimed_external_ids.insert(adopted_id);
        drop(inner);

        // Create ReplayDurableFuture
        let replay_future = ReplayDurableFuture {
            ready: Arc::new(Mutex::new(false)),
            completion: Arc::new(Mutex::new(None)),
            should_emit_decision: Arc::new(Mutex::new(true)),
        };

        // Insert into the HashMap
        let mut futures = open_futures.lock().unwrap();
        futures.insert(adopted_id, replay_future.clone());

        replay_future
    }

    fn schedule_sub_orchestration_replay(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        let name: String = name.into();
        let input: String = input.into();
        let mut inner = self.inner.lock().unwrap();
        // Adopt existing record or allocate new id
        let adopted = inner.history.iter().find_map(|e| match e {
            crate::Event::SubOrchestrationScheduled { id, name: n, input: inp, instance: _inst, execution_id: _ } if n == &name && inp == &input => Some(*id),
            _ => None,
        });
        let adopted_id = adopted.unwrap_or_else(|| inner.next_id());
        drop(inner);

        // Create ReplayDurableFuture
        let replay_future = ReplayDurableFuture {
            ready: Arc::new(Mutex::new(false)),
            completion: Arc::new(Mutex::new(None)),
            should_emit_decision: Arc::new(Mutex::new(true)),
        };

        // Insert into the HashMap
        let mut futures = open_futures.lock().unwrap();
        futures.insert(adopted_id, replay_future.clone());

        replay_future
    }
}
