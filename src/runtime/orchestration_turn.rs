use crate::{
    Event,
    runtime::{OrchestrationHandler, OrchestratorMsg},
};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use tracing::{debug, warn};

/// Result of executing an orchestration turn
#[derive(Debug)]
pub enum TurnResult {
    /// Turn completed successfully, orchestration continues
    Continue,
    /// Orchestration completed with output
    Completed(String),
    /// Orchestration failed with error
    Failed(String),
    /// Orchestration requested continue-as-new
    ContinueAsNew { input: String, version: Option<String> },
    /// Orchestration was cancelled
    Cancelled(String),
}

/// Represents a single orchestration turn with clean lifecycle stages
pub struct OrchestrationTurn {
    /// Instance identifier
    instance: String,

    /// Current turn index
    turn_index: u64,
    /// Current execution ID
    execution_id: u64,
    /// History events generated during this turn
    history_delta: Vec<Event>,
    /// Actions to dispatch after persistence
    pending_actions: Vec<crate::Action>,
    /// Current history at start of turn
    baseline_history: Vec<Event>,
    /// Next event_id for new events added this turn
    next_event_id: u64,
    /// If set, indicates a nondeterminism error detected during this turn
    nondet_error: Option<String>,
}

impl OrchestrationTurn {
    /// Create a new orchestration turn
    pub fn new(instance: String, turn_index: u64, execution_id: u64, baseline_history: Vec<Event>) -> Self {
        let next_event_id = baseline_history.last().map(|e| e.event_id() + 1).unwrap_or(1);

        Self {
            instance,
            turn_index,
            execution_id,
            history_delta: Vec::new(),
            pending_actions: Vec::new(),
            baseline_history,
            next_event_id,
            nondet_error: None,
        }
    }

    /// Stage 1: Convert completion messages directly to events
    /// Returns ack tokens for atomic acknowledgment
    pub fn prep_completions(&mut self, messages: Vec<(OrchestratorMsg, String)>) -> Vec<String> {
        let mut ack_tokens = Vec::new();
        debug!(
            instance = %self.instance,
            turn_index = self.turn_index,
            message_count = messages.len(),
            "converting messages to events"
        );

        for (msg, token) in messages {
            // Check filtering conditions
            if !self.is_completion_for_current_execution(&msg) {
                if self.has_continue_as_new_in_history() {
                    warn!(instance = %self.instance, "ignoring completion from previous execution");
                } else {
                    warn!(instance = %self.instance, "completion from different execution");
                }
                ack_tokens.push(token);
                continue;
            }

            if self.is_completion_already_in_history(&msg) {
                warn!(instance = %self.instance, "ignoring duplicate completion");
                ack_tokens.push(token);
                continue;
            }

            // Drop duplicates already staged in this turn's history_delta
            let already_in_delta = match &msg {
                OrchestratorMsg::ActivityCompleted { id, .. } | OrchestratorMsg::ActivityFailed { id, .. } => {
                    self.history_delta.iter().any(|e| match e {
                        Event::ActivityCompleted { source_event_id, .. } if source_event_id == id => true,
                        Event::ActivityFailed { source_event_id, .. } if source_event_id == id => true,
                        _ => false,
                    })
                }
                OrchestratorMsg::TimerFired { id, .. } => self
                    .history_delta
                    .iter()
                    .any(|e| matches!(e, Event::TimerFired { source_event_id, .. } if source_event_id == id)),
                OrchestratorMsg::SubOrchCompleted { id, .. } | OrchestratorMsg::SubOrchFailed { id, .. } => {
                    self.history_delta.iter().any(|e| match e {
                        Event::SubOrchestrationCompleted { source_event_id, .. } if source_event_id == id => true,
                        Event::SubOrchestrationFailed { source_event_id, .. } if source_event_id == id => true,
                        _ => false,
                    })
                }
                OrchestratorMsg::ExternalByName { name, data, .. } => self
                    .history_delta
                    .iter()
                    .any(|e| matches!(e, Event::ExternalEvent { name: n, data: d, .. } if n == name && d == data)),
                OrchestratorMsg::CancelRequested { .. } => false,
            };
            if already_in_delta {
                warn!(instance = %self.instance, "dropping duplicate completion in current turn");
                ack_tokens.push(token);
                continue;
            }

            // Nondeterminism detection: ensure completion has a matching schedule and kind
            let schedule_kind = |id: &u64| -> Option<&'static str> {
                for e in self.baseline_history.iter().chain(self.history_delta.iter()) {
                    match e {
                        Event::ActivityScheduled { event_id, .. } if event_id == id => return Some("activity"),
                        Event::TimerCreated { event_id, .. } if event_id == id => return Some("timer"),
                        Event::SubOrchestrationScheduled { event_id, .. } if event_id == id => {
                            return Some("suborchestration");
                        }
                        _ => {}
                    }
                }
                None
            };
            let mut nd_err: Option<String> = None;
            match &msg {
                OrchestratorMsg::ActivityCompleted { id, .. } | OrchestratorMsg::ActivityFailed { id, .. } => {
                    match schedule_kind(id) {
                        Some("activity") => {}
                        Some(other) => {
                            nd_err = Some(format!(
                                "nondeterministic: completion kind mismatch for id={}, expected '{}', got 'activity'",
                                id, other
                            ))
                        }
                        None => {
                            nd_err = Some(format!(
                                "nondeterministic: no matching schedule for completion id={}",
                                id
                            ))
                        }
                    }
                }
                OrchestratorMsg::TimerFired { id, .. } => match schedule_kind(id) {
                    Some("timer") => {}
                    Some(other) => {
                        nd_err = Some(format!(
                            "nondeterministic: completion kind mismatch for id={}, expected '{}', got 'timer'",
                            id, other
                        ))
                    }
                    None => nd_err = Some(format!("nondeterministic: no matching schedule for timer id={}", id)),
                },
                OrchestratorMsg::SubOrchCompleted { id, .. } | OrchestratorMsg::SubOrchFailed { id, .. } => {
                    match schedule_kind(id) {
                        Some("suborchestration") => {}
                        Some(other) => {
                            nd_err = Some(format!(
                                "nondeterministic: completion kind mismatch for id={}, expected '{}', got 'suborchestration'",
                                id, other
                            ))
                        }
                        None => {
                            nd_err = Some(format!(
                                "nondeterministic: no matching schedule for sub-orchestration id={}",
                                id
                            ))
                        }
                    }
                }
                OrchestratorMsg::ExternalByName { .. } | OrchestratorMsg::CancelRequested { .. } => {}
            }
            if let Some(err) = nd_err {
                warn!(instance = %self.instance, error=%err, "detected nondeterminism in completion batch");
                self.nondet_error = Some(err);
                ack_tokens.push(token);
                continue;
            }

            // Convert message to event
            let event_opt = match msg {
                OrchestratorMsg::ActivityCompleted { id, result, .. } => {
                    Some(Event::ActivityCompleted {
                        event_id: 0, // Will be assigned
                        source_event_id: id,
                        result,
                    })
                }
                OrchestratorMsg::ActivityFailed { id, error, .. } => Some(Event::ActivityFailed {
                    event_id: 0,
                    source_event_id: id,
                    error,
                }),
                OrchestratorMsg::TimerFired { id, fire_at_ms, .. } => Some(Event::TimerFired {
                    event_id: 0,
                    source_event_id: id,
                    fire_at_ms,
                }),
                OrchestratorMsg::ExternalByName { name, data, .. } => {
                    // Only materialize ExternalEvent if a subscription exists in this execution
                    let subscribed = self
                        .baseline_history
                        .iter()
                        .any(|e| matches!(e, Event::ExternalSubscribed { name: hist_name, .. } if hist_name == &name));
                    if subscribed {
                        Some(Event::ExternalEvent {
                            event_id: 0, // Will be assigned
                            name,
                            data,
                        })
                    } else {
                        warn!(instance = %self.instance, event_name=%name, "dropping ExternalByName with no matching subscription in history");
                        None
                    }
                }
                OrchestratorMsg::SubOrchCompleted { id, result, .. } => Some(Event::SubOrchestrationCompleted {
                    event_id: 0,
                    source_event_id: id,
                    result,
                }),
                OrchestratorMsg::SubOrchFailed { id, error, .. } => Some(Event::SubOrchestrationFailed {
                    event_id: 0,
                    source_event_id: id,
                    error,
                }),
                OrchestratorMsg::CancelRequested { reason, .. } => {
                    let already_terminated = self.baseline_history.iter().any(|e| {
                        matches!(
                            e,
                            Event::OrchestrationCompleted { .. } | Event::OrchestrationFailed { .. }
                        )
                    });
                    let already_cancelled = self
                        .baseline_history
                        .iter()
                        .chain(self.history_delta.iter())
                        .any(|e| matches!(e, Event::OrchestrationCancelRequested { .. }));

                    if !already_terminated && !already_cancelled {
                        Some(Event::OrchestrationCancelRequested { event_id: 0, reason })
                    } else {
                        None
                    }
                }
            };

            if let Some(mut event) = event_opt {
                // Assign event_id and add to history_delta
                event.set_event_id(self.next_event_id);
                self.next_event_id += 1;
                self.history_delta.push(event);
            }

            ack_tokens.push(token);
        }

        debug!(
            instance = %self.instance,
            event_count = self.history_delta.len(),
            "completion events created"
        );

        ack_tokens
    }

    /// Get the current execution ID from the baseline history
    fn get_current_execution_id(&self) -> u64 {
        self.execution_id
    }

    /// Check if a completion message belongs to the current execution
    fn is_completion_for_current_execution(&self, msg: &OrchestratorMsg) -> bool {
        let current_execution_id = self.get_current_execution_id();
        match msg {
            OrchestratorMsg::ActivityCompleted { execution_id, .. } => *execution_id == current_execution_id,
            OrchestratorMsg::ActivityFailed { execution_id, .. } => *execution_id == current_execution_id,
            OrchestratorMsg::TimerFired { execution_id, .. } => *execution_id == current_execution_id,
            OrchestratorMsg::SubOrchCompleted { execution_id, .. } => *execution_id == current_execution_id,
            OrchestratorMsg::SubOrchFailed { execution_id, .. } => *execution_id == current_execution_id,
            OrchestratorMsg::ExternalByName { .. } => true, // External events don't have execution IDs
            OrchestratorMsg::CancelRequested { .. } => true, // Cancellation applies to current execution
        }
    }

    /// Check if a completion is already in the baseline history (duplicate)
    fn is_completion_already_in_history(&self, msg: &OrchestratorMsg) -> bool {
        match msg {
            OrchestratorMsg::ActivityCompleted { id, .. } => self
                .baseline_history
                .iter()
                .any(|e| matches!(e, Event::ActivityCompleted { source_event_id, .. } if *source_event_id == *id)),
            OrchestratorMsg::TimerFired { id, .. } => self
                .baseline_history
                .iter()
                .any(|e| matches!(e, Event::TimerFired { source_event_id, .. } if *source_event_id == *id)),
            OrchestratorMsg::SubOrchCompleted { id, .. } => self.baseline_history.iter().any(
                |e| matches!(e, Event::SubOrchestrationCompleted { source_event_id, .. } if *source_event_id == *id),
            ),
            OrchestratorMsg::SubOrchFailed { id, .. } => self
                .baseline_history
                .iter()
                .any(|e| matches!(e, Event::SubOrchestrationFailed { source_event_id, .. } if *source_event_id == *id)),
            OrchestratorMsg::ExternalByName { name, data, .. } => self.baseline_history.iter().any(|e| {
                matches!(e, Event::ExternalEvent { name: hist_name, data: hist_data, .. }
                        if hist_name == name && hist_data == data)
            }),
            _ => false,
        }
    }

    /// Check if this orchestration has used continue_as_new
    fn has_continue_as_new_in_history(&self) -> bool {
        self.baseline_history
            .iter()
            .any(|e| matches!(e, Event::OrchestrationContinuedAsNew { .. }))
    }

    /// Stage 2: Execute one turn of the orchestration using the replay engine
    /// This stage runs the orchestration logic and generates history deltas and actions
    pub fn execute_orchestration(&mut self, handler: Arc<dyn OrchestrationHandler>, input: String) -> TurnResult {
        debug!(
            instance = %self.instance,
            turn_index = self.turn_index,
            "executing orchestration turn"
        );
        if let Some(err) = self.nondet_error.clone() {
            return TurnResult::Failed(err);
        }

        // Build working history: baseline + completion events from this turn
        let working_history_len_before = self.baseline_history.len() + self.history_delta.len();
        let mut working_history = self.baseline_history.clone();
        working_history.extend(self.history_delta.clone());

        // Run orchestration with unified cursor model
        let execution_id = self.get_current_execution_id();
        let run_result = catch_unwind(AssertUnwindSafe(|| {
            crate::run_turn_with_status(working_history, self.turn_index, execution_id, move |ctx| {
                let h = handler.clone();
                let inp = input.clone();
                async move { h.invoke(ctx, inp).await }
            })
        }));

        let (updated_history, decisions, output_opt, nondet_flag) = match run_result {
            Ok(tuple) => tuple,
            Err(panic_payload) => {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    format!("nondeterministic: {}", s)
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    format!("nondeterministic: {}", s)
                } else {
                    "nondeterministic: orchestration panicked".to_string()
                };
                return TurnResult::Failed(msg);
            }
        };

        // If futures flagged nondeterminism (scheduling-order mismatch), fail gracefully
        if let Some(err) = nondet_flag.clone() {
            return TurnResult::Failed(err);
        }

        // If futures recorded nondeterminism, fail gracefully
        if updated_history.is_empty() {
            // Nothing to check; proceed
        }
        // Inspect nondeterminism flag
        if let Some(err) = {
            // Rebuild a context view to peek at the flag by leveraging a small helper
            // We can infer nondeterminism if no new scheduling matched and futures set the flag
            // Here, we conservatively check the last delta for no-op and rely on prep_completions filtering.
            // Since we cannot access ctx here, rely on Turn-level flag (set earlier) or re-run minimal check.
            None::<String>
        } {
            return TurnResult::Failed(err);
        }

        // Calculate NEW events added during orchestration execution
        // (beyond the completion events we already added in prep_completions)
        if updated_history.len() > working_history_len_before {
            let new_events = updated_history[working_history_len_before..].to_vec();
            self.history_delta.extend(new_events);
        }

        self.pending_actions = decisions;

        // Check for cancellation first - if cancelled, return immediately
        let full_history = {
            let mut h = self.baseline_history.clone();
            h.extend(self.history_delta.clone());
            h
        };

        if let Some(cancel_event) = full_history
            .iter()
            .find(|e| matches!(e, Event::OrchestrationCancelRequested { .. }))
        {
            if let Event::OrchestrationCancelRequested { reason, .. } = cancel_event {
                return TurnResult::Cancelled(reason.clone());
            }
        }

        // Check for continue-as-new decision FIRST (takes precedence over output)
        for decision in &self.pending_actions {
            if let crate::Action::ContinueAsNew { input, version } = decision {
                return TurnResult::ContinueAsNew {
                    input: input.clone(),
                    version: version.clone(),
                };
            }
        }

        // Determine turn result based on output and decisions
        if let Some(output) = output_opt {
            return match output {
                Ok(result) => TurnResult::Completed(result),
                Err(error) => TurnResult::Failed(error),
            };
        }

        // Cursor model handles non-determinism automatically via strict sequential consumption
        TurnResult::Continue
    }

}

impl OrchestrationTurn {

    // Getter methods for atomic execution
    pub fn history_delta(&self) -> &[Event] {
        &self.history_delta
    }

    pub fn pending_actions(&self) -> &[crate::Action] {
        &self.pending_actions
    }

    /// Check if this turn made any progress (added history)
    pub fn made_progress(&self) -> bool {
        !self.history_delta.is_empty()
    }

    /// Get the final history after this turn
    pub fn final_history(&self) -> Vec<Event> {
        let mut final_hist = self.baseline_history.clone();
        final_hist.extend(self.history_delta.clone());
        final_hist
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Event;
    use crate::runtime::OrchestratorMsg;

    #[test]
    fn test_turn_creation() {
        let turn = OrchestrationTurn::new(
            "test-instance".to_string(),
            1,
            1, // execution_id
            vec![Event::OrchestrationStarted {
                event_id: 0,
                name: "test-orch".to_string(),
                version: "1.0.0".to_string(),
                input: "test-input".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
        );

        assert_eq!(turn.instance, "test-instance");
        assert_eq!(turn.turn_index, 1);
        assert!(turn.history_delta.is_empty());
        assert!(!turn.made_progress());
    }

    #[test]
    fn test_prep_completions_creates_events() {
        // Provide matching schedule for the injected completion
        let baseline = vec![Event::ActivityScheduled {
            event_id: 1,
            name: "x".to_string(),
            input: "y".to_string(),
            execution_id: 1,
        }];
        let mut turn = OrchestrationTurn::new("test-instance".to_string(), 1, 1, baseline);

        let messages = vec![(
            OrchestratorMsg::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1,
                result: "success".to_string(),
                ack_token: Some("token1".to_string()),
            },
            "token1".to_string(),
        )];

        let ack_tokens = turn.prep_completions(messages);

        // Should have converted message to event
        assert_eq!(ack_tokens.len(), 1);
        assert_eq!(turn.history_delta.len(), 1);
        assert!(turn.made_progress());
    }
}

// Include comprehensive tests
#[path = "orchestration_turn_tests.rs"]
mod orchestration_turn_tests;
