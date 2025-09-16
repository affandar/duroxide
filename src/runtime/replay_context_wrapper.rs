use crate::OrchestrationContext;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::replay::ReplayDurableFuture;
use super::event_ids::ReplayHistoryEvent;

/// A wrapper around OrchestrationContext that intercepts schedule_* calls for replay
pub(crate) struct ReplayOrchestrationContext {
    inner: OrchestrationContext,
    open_futures: Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    pub(crate) state: Arc<Mutex<ReplayState>>, // replay-only state
    decisions: Arc<Mutex<Vec<crate::Action>>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplayState {
    // Global stream of wrapped events for this turn
    pub(crate) events: Vec<ReplayHistoryEvent>,
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
        Self {
            inner,
            open_futures,
            state: Arc::new(Mutex::new(ReplayState::new(events_this_turn))),
            decisions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> ReplayDurableFuture {
        let name: String = name.into();
        let input: String = input.into();
        let (fut, id) = self.schedule_next_or_new();
        self.decisions
            .lock()
            .unwrap()
            .push(crate::Action::CallActivity { id, name, input });
        fut
    }

    pub fn schedule_timer(&self, delay_ms: u64) -> ReplayDurableFuture {
        let (fut, id) = self.schedule_next_or_new();
        self.decisions
            .lock()
            .unwrap()
            .push(crate::Action::CreateTimer { id, delay_ms });
        fut
    }

    #[allow(dead_code)]
    pub fn schedule_wait(&self, name: impl Into<String>) -> ReplayDurableFuture {
        let name: String = name.into();
        let (fut, id) = self.schedule_next_or_new();
        self.decisions
            .lock()
            .unwrap()
            .push(crate::Action::WaitExternal { id, name });
        fut
    }

    #[allow(dead_code)]
    pub fn schedule_sub_orchestration(&self, name: impl Into<String>, input: impl Into<String>) -> ReplayDurableFuture {
        let name: String = name.into();
        let input: String = input.into();
        let (fut, id) = self.schedule_next_or_new();
        // Use portable placeholder; runtime will prefix with parent instance
        let child_instance = format!("sub::{id}");
        self.decisions.lock().unwrap().push(crate::Action::StartSubOrchestration {
            id,
            name,
            version: None,
            instance: child_instance,
            input,
        });
        fut
    }

    // Delegate other methods to inner context
    #[allow(dead_code)]
    pub fn inner(&self) -> &OrchestrationContext {
        &self.inner
    }

    pub fn take_actions(&self) -> Vec<crate::Action> {
        let mut out = self.inner.take_actions();
        let mut mine = self.decisions.lock().unwrap();
        out.extend(mine.drain(..));
        out
    }

    // Replay-only: called by core after processing an event
    pub fn bump_event_index(&self) {
        let mut st = self.state.lock().unwrap();
        st.next_replay_id += 1;
    }

    // Helper: allocate next id by consuming next schedule event's event_id if available; else replay id
    fn schedule_next_or_new(&self) -> (ReplayDurableFuture, u64) {
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
        (replay_future, chosen_id)
    }

    // ===== Decision recording overrides =====
    pub fn continue_as_new(&self, input: impl Into<String>) {
        let input: String = input.into();
        self.decisions
            .lock()
            .unwrap()
            .push(crate::Action::ContinueAsNew { input, version: None });
    }

    pub fn continue_as_new_versioned(&self, version: impl Into<String>, input: impl Into<String>) {
        let input: String = input.into();
        let version: String = version.into();
        self.decisions.lock().unwrap().push(crate::Action::ContinueAsNew {
            input,
            version: Some(version),
        });
    }

    pub fn schedule_orchestration(
        &self,
        name: impl Into<String>,
        instance: impl Into<String>,
        input: impl Into<String>,
    ) {
        let name: String = name.into();
        let instance: String = instance.into();
        let input: String = input.into();
        // Allocate a correlation id for chained orchestration
        let mut st = self.state.lock().unwrap();
        let id = st.alloc_replay_id();
        drop(st);
        self.decisions.lock().unwrap().push(crate::Action::StartOrchestrationDetached {
            id,
            name,
            version: None,
            instance,
            input,
        });
    }

    pub fn schedule_orchestration_versioned(
        &self,
        name: impl Into<String>,
        version: Option<String>,
        instance: impl Into<String>,
        input: impl Into<String>,
    ) {
        let name: String = name.into();
        let instance: String = instance.into();
        let input: String = input.into();
        let mut st = self.state.lock().unwrap();
        let id = st.alloc_replay_id();
        drop(st);
        self.decisions.lock().unwrap().push(crate::Action::StartOrchestrationDetached {
            id,
            name,
            version,
            instance,
            input,
        });
    }
}

// Implement other methods that might be needed
impl Clone for ReplayOrchestrationContext {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            open_futures: self.open_futures.clone(),
            state: self.state.clone(),
            decisions: self.decisions.clone(),
        }
    }
}
