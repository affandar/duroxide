use crate::OrchestrationContext;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::replay::ReplayDurableFuture;

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
        let name_str = name.into();
        let input_str = input.into();

        // Call the original method
        let future = self.schedule_activity(name_str.clone(), input_str.clone());

        // Extract the ID from the future - we need to peek at its internals
        // The DurableFuture contains a Kind enum with the ID
        if let crate::futures::Kind::Activity { id, .. } = &future.0 {

            // Create ReplayDurableFuture
            let replay_future = ReplayDurableFuture {
                ready: Arc::new(Mutex::new(false)),
                completion: Arc::new(Mutex::new(None)),
                should_emit_decision: Arc::new(Mutex::new(true)), // Will be set to false later if in history
            };

            // Insert into the HashMap
            let mut futures = open_futures.lock().unwrap();
            futures.insert(*id, replay_future.clone());

            replay_future
        } else {
            panic!("Unexpected future kind returned from schedule_activity");
        }
    }

    fn schedule_timer_replay(
        &self,
        delay_ms: u64,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        let future = self.schedule_timer(delay_ms);

        if let crate::futures::Kind::Timer { id, .. } = &future.0 {

            // Create ReplayDurableFuture
            let replay_future = ReplayDurableFuture {
                ready: Arc::new(Mutex::new(false)),
                completion: Arc::new(Mutex::new(None)),
                should_emit_decision: Arc::new(Mutex::new(true)),
            };

            // Insert into the HashMap
            let mut futures = open_futures.lock().unwrap();
            futures.insert(*id, replay_future.clone());

            replay_future
        } else {
            panic!("Unexpected future kind returned from schedule_timer");
        }
    }

    fn schedule_wait_replay(
        &self,
        name: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        let name_str = name.into();
        let future = self.schedule_wait(name_str.clone());

        if let crate::futures::Kind::External { id, .. } = &future.0 {

            // Create ReplayDurableFuture
            let replay_future = ReplayDurableFuture {
                ready: Arc::new(Mutex::new(false)),
                completion: Arc::new(Mutex::new(None)),
                should_emit_decision: Arc::new(Mutex::new(true)),
            };

            // Insert into the HashMap
            let mut futures = open_futures.lock().unwrap();
            futures.insert(*id, replay_future.clone());

            replay_future
        } else {
            panic!("Unexpected future kind returned from schedule_wait");
        }
    }

    fn schedule_sub_orchestration_replay(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        open_futures: &Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> ReplayDurableFuture {
        let name_str = name.into();
        let input_str = input.into();
        let future = self.schedule_sub_orchestration(name_str.clone(), input_str.clone());

        if let crate::futures::Kind::SubOrch { id, .. } = &future.0
        {

            // Create ReplayDurableFuture
            let replay_future = ReplayDurableFuture {
                ready: Arc::new(Mutex::new(false)),
                completion: Arc::new(Mutex::new(None)),
                should_emit_decision: Arc::new(Mutex::new(true)),
            };

            // Insert into the HashMap
            let mut futures = open_futures.lock().unwrap();
            futures.insert(*id, replay_future.clone());

            replay_future
        } else {
            panic!("Unexpected future kind returned from schedule_sub_orchestration");
        }
    }
}
