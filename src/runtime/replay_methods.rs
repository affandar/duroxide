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
        _name: impl Into<String>,
        _input: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        // Generate monotonically increasing ID instead of claiming from history
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id();
        drop(inner);

        // Create ReplayDurableFuture
        let replay_future = ReplayDurableFuture {
            ready: Arc::new(Mutex::new(false)),
            completion: Arc::new(Mutex::new(None)),
            should_emit_decision: Arc::new(Mutex::new(true)), // Will be set to false later if in history
        };

        // Insert into the HashMap
        let mut futures = open_futures.lock().unwrap();
        futures.insert(id, replay_future.clone());

        replay_future
    }

    fn schedule_timer_replay(
        &self,
        _delay_ms: u64,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        // Generate monotonically increasing ID instead of claiming from history
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id();
        drop(inner);

        // Create ReplayDurableFuture
        let replay_future = ReplayDurableFuture {
            ready: Arc::new(Mutex::new(false)),
            completion: Arc::new(Mutex::new(None)),
            should_emit_decision: Arc::new(Mutex::new(true)),
        };

        // Insert into the HashMap
        let mut futures = open_futures.lock().unwrap();
        futures.insert(id, replay_future.clone());

        replay_future
    }

    fn schedule_wait_replay(
        &self,
        _name: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        // Generate monotonically increasing ID instead of claiming from history
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id();
        drop(inner);

        // Create ReplayDurableFuture
        let replay_future = ReplayDurableFuture {
            ready: Arc::new(Mutex::new(false)),
            completion: Arc::new(Mutex::new(None)),
            should_emit_decision: Arc::new(Mutex::new(true)),
        };

        // Insert into the HashMap
        let mut futures = open_futures.lock().unwrap();
        futures.insert(id, replay_future.clone());

        replay_future
    }

    fn schedule_sub_orchestration_replay(
        &self,
        _name: impl Into<String>,
        _input: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        // Generate monotonically increasing ID instead of claiming from history
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id();
        drop(inner);

        // Create ReplayDurableFuture
        let replay_future = ReplayDurableFuture {
            ready: Arc::new(Mutex::new(false)),
            completion: Arc::new(Mutex::new(None)),
            should_emit_decision: Arc::new(Mutex::new(true)),
        };

        // Insert into the HashMap
        let mut futures = open_futures.lock().unwrap();
        futures.insert(id, replay_future.clone());

        replay_future
    }
}
