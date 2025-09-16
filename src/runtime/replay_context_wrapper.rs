use crate::OrchestrationContext;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::replay::ReplayDurableFuture;
use super::event_ids::ReplayHistoryEvent;

/// A wrapper around OrchestrationContext that intercepts schedule_* calls for replay
pub(crate) struct ReplayOrchestrationContext {
    inner: OrchestrationContext,
    open_futures: Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    state: Arc<Mutex<ReplayState>>, // replay-only state
}

#[derive(Debug, Clone)]
struct ReplayState {
    // Global stream of wrapped events for this turn
    events: Vec<ReplayHistoryEvent>,
    // Cursor pointing to the next not-yet-consumed scheduled event in global order
    schedule_cursor: usize,
    // Replay-local next id counter based on event index (history length + processed)
    next_replay_id: u64,
}

impl ReplayState {
    fn new(events: Vec<ReplayHistoryEvent>) -> Self {
        let next_replay_id = events.len() as u64 + 1;
        Self {
            events,
            schedule_cursor: 0,
            next_replay_id,
        }
    }

    fn alloc_replay_id(&mut self) -> u64 {
        let id = self.next_replay_id;
        self.next_replay_id += 1;
        id
    }
}

#[allow(dead_code)]
impl ReplayOrchestrationContext {
    pub(crate) fn new(
        inner: OrchestrationContext,
        open_futures: Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
        events_this_turn: Vec<ReplayHistoryEvent>,
    ) -> Self {
        Self { inner, open_futures, state: Arc::new(Mutex::new(ReplayState::new(events_this_turn))) }
    }

    pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> ReplayDurableFuture {
        let _ = (name.into(), input.into());
        self.schedule_next_or_new()
    }

    pub fn schedule_timer(&self, delay_ms: u64) -> ReplayDurableFuture {
        let _ = delay_ms;
        self.schedule_next_or_new()
    }

    #[allow(dead_code)]
    pub fn schedule_wait(&self, name: impl Into<String>) -> ReplayDurableFuture {
        let _ = name.into();
        self.schedule_next_or_new()
    }

    #[allow(dead_code)]
    pub fn schedule_sub_orchestration(&self, name: impl Into<String>, input: impl Into<String>) -> ReplayDurableFuture {
        let _ = (name.into(), input.into());
        self.schedule_next_or_new()
    }

    // Delegate other methods to inner context
    #[allow(dead_code)]
    pub fn inner(&self) -> &OrchestrationContext {
        &self.inner
    }

    pub fn take_actions(&self) -> Vec<crate::Action> {
        self.inner.take_actions()
    }

    // Replay-only: called by core after processing an event
    pub fn bump_event_index(&self) {
        let mut st = self.state.lock().unwrap();
        st.next_replay_id += 1;
    }

    // Helper: allocate next id by consuming next schedule event's event_id if available; else replay id
    fn schedule_next_or_new(&self) -> ReplayDurableFuture {
        let mut st = self.state.lock().unwrap();

        // Find next scheduled event in global order from the cursor
        let mut chosen_id: u64 = st.alloc_replay_id();
        for idx in st.schedule_cursor..st.events.len() {
            let e = &st.events[idx].event;
            let is_schedule = matches!(
                e,
                crate::Event::ActivityScheduled { .. }
                    | crate::Event::TimerCreated { .. }
                    | crate::Event::ExternalSubscribed { .. }
                    | crate::Event::SubOrchestrationScheduled { .. }
            );
            if is_schedule {
                chosen_id = st.events[idx].event_id;
                st.schedule_cursor = idx + 1; // consume
                break;
            }
        }
        drop(st);

        // Create and register replay future under chosen_id
        let replay_future = ReplayDurableFuture {
            ready: Arc::new(Mutex::new(false)),
            completion: Arc::new(Mutex::new(None)),
            should_emit_decision: Arc::new(Mutex::new(true)),
        };
        let mut futures = self.open_futures.lock().unwrap();
        futures.insert(chosen_id, replay_future.clone());
        replay_future
    }
}

// Implement other methods that might be needed
impl Clone for ReplayOrchestrationContext {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            open_futures: self.open_futures.clone(),
            state: self.state.clone(),
        }
    }
}
