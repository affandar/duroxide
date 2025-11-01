use crate::{Event, providers::WorkItem, runtime::OrchestrationHandler};
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
    /// Orchestration failed with error details
    Failed(crate::ErrorDetails),
    /// Orchestration requested continue-as-new
    ContinueAsNew { input: String, version: Option<String> },
    /// Orchestration was cancelled
    Cancelled(String),
}

/// Replays history and executes one deterministic orchestration evaluation
pub struct ReplayEngine {
    /// Instance identifier
    pub(crate) instance: String,

    /// Current execution ID
    pub(crate) execution_id: u64,
    /// History events generated during this run
    pub(crate) history_delta: Vec<Event>,
    /// Actions to dispatch after persistence
    pub(crate) pending_actions: Vec<crate::Action>,
    /// Current history at start of run
    pub(crate) baseline_history: Vec<Event>,
    /// Next event_id for new events added this run
    pub(crate) next_event_id: u64,
    /// Unified error collector for system-level errors that abort the turn
    pub(crate) abort_error: Option<crate::ErrorDetails>,
}

impl ReplayEngine {
    /// Create a new replay engine for an instance/execution
    pub fn new(instance: String, execution_id: u64, baseline_history: Vec<Event>) -> Self {
        let next_event_id = baseline_history.last().map(|e| e.event_id() + 1).unwrap_or(1);

        Self {
            instance,
            execution_id,
            history_delta: Vec::new(),
            pending_actions: Vec::new(),
            baseline_history,
            next_event_id,
            abort_error: None,
        }
    }

    /// Stage 1: Convert completion messages directly to events
    pub fn prep_completions(&mut self, messages: Vec<WorkItem>) {
        debug!(
            instance = %self.instance,
            message_count = messages.len(),
            "converting messages to events"
        );

        for msg in messages {
            // Check filtering conditions
            if !self.is_completion_for_current_execution(&msg) {
                if self.has_continue_as_new_in_history() {
                    warn!(instance = %self.instance, "ignoring completion from previous execution");
                } else {
                    warn!(instance = %self.instance, "completion from different execution");
                }
                continue;
            }

            if self.is_completion_already_in_history(&msg) {
                warn!(instance = %self.instance, "ignoring duplicate completion");
                continue;
            }

            // Drop duplicates already staged in this run's history_delta
            let already_in_delta = match &msg {
                WorkItem::ActivityCompleted { id, .. } | WorkItem::ActivityFailed { id, .. } => {
                    self.history_delta.iter().any(|e| match e {
                        Event::ActivityCompleted { source_event_id, .. } if source_event_id == id => true,
                        Event::ActivityFailed { source_event_id, .. } if source_event_id == id => true,
                        _ => false,
                    })
                }
                WorkItem::TimerFired { id, .. } => self
                    .history_delta
                    .iter()
                    .any(|e| matches!(e, Event::TimerFired { source_event_id, .. } if source_event_id == id)),
                WorkItem::SubOrchCompleted { parent_id, .. } | WorkItem::SubOrchFailed { parent_id, .. } => {
                    self.history_delta.iter().any(|e| match e {
                        Event::SubOrchestrationCompleted { source_event_id, .. } if source_event_id == parent_id => {
                            true
                        }
                        Event::SubOrchestrationFailed { source_event_id, .. } if source_event_id == parent_id => true,
                        _ => false,
                    })
                }
                WorkItem::ExternalRaised { name, data, .. } => self
                    .history_delta
                    .iter()
                    .any(|e| matches!(e, Event::ExternalEvent { name: n, data: d, .. } if n == name && d == data)),
                WorkItem::CancelInstance { .. } => false,
                _ => false, // Non-completion work items
            };
            if already_in_delta {
                warn!(instance = %self.instance, "dropping duplicate completion in current run");
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
            let mut nd_err: Option<crate::ErrorDetails> = None;
            match &msg {
                WorkItem::ActivityCompleted { id, .. } | WorkItem::ActivityFailed { id, .. } => {
                    match schedule_kind(id) {
                        Some("activity") => {}
                        Some(other) => {
                            nd_err = Some(crate::ErrorDetails::Configuration {
                                kind: crate::ConfigErrorKind::Nondeterminism,
                                resource: String::new(),
                                message: Some(format!(
                                    "completion kind mismatch for id={id}, expected '{other}', got 'activity'"
                                )),
                            })
                        }
                        None => {
                            nd_err = Some(crate::ErrorDetails::Configuration {
                                kind: crate::ConfigErrorKind::Nondeterminism,
                                resource: String::new(),
                                message: Some(format!("no matching schedule for completion id={id}")),
                            })
                        }
                    }
                }
                WorkItem::TimerFired { id, .. } => match schedule_kind(id) {
                    Some("timer") => {}
                    Some(other) => {
                        nd_err = Some(crate::ErrorDetails::Configuration {
                            kind: crate::ConfigErrorKind::Nondeterminism,
                            resource: String::new(),
                            message: Some(format!(
                                "completion kind mismatch for id={id}, expected '{other}', got 'timer'"
                            )),
                        })
                    }
                    None => {
                        nd_err = Some(crate::ErrorDetails::Configuration {
                            kind: crate::ConfigErrorKind::Nondeterminism,
                            resource: String::new(),
                            message: Some(format!("no matching schedule for timer id={id}")),
                        })
                    }
                },
                WorkItem::SubOrchCompleted { parent_id, .. } | WorkItem::SubOrchFailed { parent_id, .. } => {
                    match schedule_kind(parent_id) {
                        Some("suborchestration") => {}
                        Some(other) => {
                            nd_err = Some(crate::ErrorDetails::Configuration {
                                kind: crate::ConfigErrorKind::Nondeterminism,
                                resource: String::new(),
                                message: Some(format!(
                                    "completion kind mismatch for id={parent_id}, expected '{other}', got 'suborchestration'"
                                )),
                            })
                        }
                        None => {
                            nd_err = Some(crate::ErrorDetails::Configuration {
                                kind: crate::ConfigErrorKind::Nondeterminism,
                                resource: String::new(),
                                message: Some(format!(
                                    "no matching schedule for sub-orchestration id={parent_id}"
                                )),
                            })
                        }
                    }
                }
                WorkItem::ExternalRaised { .. } | WorkItem::CancelInstance { .. } => {}
                _ => {} // Non-completion work items
            }
            if let Some(err) = nd_err {
                warn!(instance = %self.instance, error = %err.display_message(), "detected nondeterminism in completion batch");
                self.abort_error = Some(err);
                continue;
            }

            // Convert message to event
            let event_opt = match msg {
                WorkItem::ActivityCompleted { id, result, .. } => {
                    Some(Event::ActivityCompleted {
                        event_id: 0, // Will be assigned
                        source_event_id: id,
                        result,
                    })
                }
                WorkItem::ActivityFailed { id, details, .. } => {
                    // Always create event in history for audit trail
                    let event = Event::ActivityFailed {
                        event_id: 0,
                        source_event_id: id,
                        details: details.clone(),
                    };
                    
                    // Check if system error (abort turn)
                    match &details {
                        crate::ErrorDetails::Configuration { .. } | crate::ErrorDetails::Infrastructure { .. } => {
                            warn!(instance = %self.instance, id, ?details, "System error aborts turn");
                            if self.abort_error.is_none() {
                                self.abort_error = Some(details);
                            }
                        }
                        crate::ErrorDetails::Application { .. } => {
                            // Normal flow
                        }
                    }
                    
                    Some(event)
                }
                WorkItem::TimerFired { id, fire_at_ms, .. } => Some(Event::TimerFired {
                    event_id: 0,
                    source_event_id: id,
                    fire_at_ms,
                }),
                WorkItem::ExternalRaised { name, data, .. } => {
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
                WorkItem::SubOrchCompleted { parent_id, result, .. } => Some(Event::SubOrchestrationCompleted {
                    event_id: 0,
                    source_event_id: parent_id,
                    result,
                }),
                WorkItem::SubOrchFailed { parent_id, details, .. } => {
                    // Always create event in history for audit trail
                    let event = Event::SubOrchestrationFailed {
                        event_id: 0,
                        source_event_id: parent_id,
                        details: details.clone(),
                    };
                    
                    // Check if system error (abort parent turn)
                    match &details {
                        crate::ErrorDetails::Configuration { .. } | crate::ErrorDetails::Infrastructure { .. } => {
                            warn!(instance = %self.instance, parent_id, ?details, "Child system error aborts parent");
                            if self.abort_error.is_none() {
                                self.abort_error = Some(details);
                            }
                        }
                        crate::ErrorDetails::Application { .. } => {
                            // Normal flow
                        }
                    }
                    
                    Some(event)
                }
                WorkItem::CancelInstance { reason, .. } => {
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
                _ => None, // Non-completion work items
            };

            if let Some(mut event) = event_opt {
                // Assign event_id and add to history_delta
                event.set_event_id(self.next_event_id);
                self.next_event_id += 1;
                self.history_delta.push(event);
            }
        }

        debug!(
            instance = %self.instance,
            event_count = self.history_delta.len(),
            "completion events created"
        );
    }

    /// Get the current execution ID from the baseline history
    fn get_current_execution_id(&self) -> u64 {
        self.execution_id
    }

    /// Check if a completion message belongs to the current execution
    fn is_completion_for_current_execution(&self, msg: &WorkItem) -> bool {
        let current_execution_id = self.get_current_execution_id();
        match msg {
            WorkItem::ActivityCompleted { execution_id, .. } => *execution_id == current_execution_id,
            WorkItem::ActivityFailed { execution_id, .. } => *execution_id == current_execution_id,
            WorkItem::TimerFired { execution_id, .. } => *execution_id == current_execution_id,
            WorkItem::SubOrchCompleted {
                parent_execution_id, ..
            } => *parent_execution_id == current_execution_id,
            WorkItem::SubOrchFailed {
                parent_execution_id, ..
            } => *parent_execution_id == current_execution_id,
            WorkItem::ExternalRaised { .. } => true, // External events don't have execution IDs
            WorkItem::CancelInstance { .. } => true, // Cancellation applies to current execution
            _ => false,                              // Non-completion work items (shouldn't reach here)
        }
    }

    /// Check if a completion is already in the baseline history (duplicate)
    fn is_completion_already_in_history(&self, msg: &WorkItem) -> bool {
        match msg {
            WorkItem::ActivityCompleted { id, .. } => self
                .baseline_history
                .iter()
                .any(|e| matches!(e, Event::ActivityCompleted { source_event_id, .. } if *source_event_id == *id)),
            WorkItem::TimerFired { id, .. } => self
                .baseline_history
                .iter()
                .any(|e| matches!(e, Event::TimerFired { source_event_id, .. } if *source_event_id == *id)),
            WorkItem::SubOrchCompleted { parent_id, .. } => self.baseline_history.iter().any(
                |e| matches!(e, Event::SubOrchestrationCompleted { source_event_id, .. } if *source_event_id == *parent_id),
            ),
            WorkItem::SubOrchFailed { parent_id, .. } => self
                .baseline_history
                .iter()
                .any(|e| matches!(e, Event::SubOrchestrationFailed { source_event_id, .. } if *source_event_id == *parent_id)),
            WorkItem::ExternalRaised { name, data, .. } => self.baseline_history.iter().any(|e| {
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
            "executing orchestration turn"
        );
        // Check abort_error FIRST - before running user code
        if let Some(err) = self.abort_error.clone() {
            return TurnResult::Failed(err);
        }

        // Build working history: baseline + completion events from this run
        let working_history_len_before = self.baseline_history.len() + self.history_delta.len();
        let mut working_history = self.baseline_history.clone();
        working_history.extend(self.history_delta.clone());

        // Run orchestration with unified cursor model
        let execution_id = self.get_current_execution_id();
        let run_result = catch_unwind(AssertUnwindSafe(|| {
            crate::run_turn_with_status(working_history, execution_id, move |ctx| {
                let h = handler.clone();
                let inp = input.clone();
                async move { h.invoke(ctx, inp).await }
            })
        }));

        let (updated_history, decisions, output_opt, nondet_flag) = match run_result {
            Ok(tuple) => tuple,
            Err(panic_payload) => {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "orchestration panicked".to_string()
                };
                return TurnResult::Failed(crate::ErrorDetails::Configuration {
                    kind: crate::ConfigErrorKind::Nondeterminism,
                    resource: String::new(),
                    message: Some(msg),
                });
            }
        };

        // If futures flagged nondeterminism (scheduling-order mismatch), fail gracefully
        if let Some(err) = nondet_flag.clone() {
            return TurnResult::Failed(crate::ErrorDetails::Configuration {
                kind: crate::ConfigErrorKind::Nondeterminism,
                resource: String::new(),
                message: Some(err),
            });
        }

        // If futures recorded nondeterminism, fail gracefully
        if updated_history.is_empty() {
            // Nothing to check; proceed
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

        if let Some(Event::OrchestrationCancelRequested { reason, .. }) = full_history
            .iter()
            .find(|e| matches!(e, Event::OrchestrationCancelRequested { .. }))
        {
            return TurnResult::Cancelled(reason.clone());
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

        // Determine result based on output and decisions
        if let Some(output) = output_opt {
            return match output {
                Ok(result) => TurnResult::Completed(result),
                Err(error) => TurnResult::Failed(crate::ErrorDetails::Application {
                    kind: crate::AppErrorKind::OrchestrationFailed,
                    message: error,
                    retryable: false,
                }),
            };
        }

        // Cursor model handles non-determinism automatically via strict sequential consumption
        TurnResult::Continue
    }
}

impl ReplayEngine {
    // Getter methods for atomic execution
    pub fn history_delta(&self) -> &[Event] {
        &self.history_delta
    }

    pub fn pending_actions(&self) -> &[crate::Action] {
        &self.pending_actions
    }

    /// Check if this run made any progress (added history)
    pub fn made_progress(&self) -> bool {
        !self.history_delta.is_empty()
    }

    /// Get the final history after this run
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

    #[test]
    fn test_engine_creation() {
        let engine = ReplayEngine::new(
            "test-instance".to_string(),
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

        assert_eq!(engine.instance, "test-instance");
        assert!(engine.history_delta.is_empty());
        assert!(!engine.made_progress());
    }
}

// Include comprehensive tests
#[path = "replay_engine_tests.rs"]
mod replay_engine_tests;
