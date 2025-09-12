use crate::OrchestrationContext;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::replay::ReplayDurableFuture;
use super::replay_methods::ReplaySchedulingExt;

/// A wrapper around OrchestrationContext that intercepts schedule_* calls for replay
pub(crate) struct ReplayOrchestrationContext {
    inner: OrchestrationContext,
    open_futures: Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
}

#[allow(dead_code)]
impl ReplayOrchestrationContext {
    pub(crate) fn new(
        inner: OrchestrationContext,
        open_futures: Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    ) -> Self {
        Self { inner, open_futures }
    }

    pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> ReplayDurableFuture {
        self.inner.schedule_activity_replay(name, input, &self.open_futures)
    }

    pub fn schedule_timer(&self, delay_ms: u64) -> ReplayDurableFuture {
        self.inner.schedule_timer_replay(delay_ms, &self.open_futures)
    }

    #[allow(dead_code)]
    pub fn schedule_wait(&self, name: impl Into<String>) -> ReplayDurableFuture {
        self.inner.schedule_wait_replay(name, &self.open_futures)
    }

    #[allow(dead_code)]
    pub fn schedule_sub_orchestration(&self, name: impl Into<String>, input: impl Into<String>) -> ReplayDurableFuture {
        self.inner
            .schedule_sub_orchestration_replay(name, input, &self.open_futures)
    }

    // Delegate other methods to inner context
    #[allow(dead_code)]
    pub fn inner(&self) -> &OrchestrationContext {
        &self.inner
    }

    pub fn take_actions(&self) -> Vec<crate::Action> {
        self.inner.take_actions()
    }
}

// Implement other methods that might be needed
impl Clone for ReplayOrchestrationContext {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            open_futures: self.open_futures.clone(),
        }
    }
}
