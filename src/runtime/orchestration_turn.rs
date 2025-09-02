use super::completion_map::{CompletionKind, CompletionMap};
use crate::providers::HistoryStore;
use crate::{
    Event,
    runtime::{OrchestrationHandler, router::OrchestratorMsg},
};
use std::sync::Arc;
use tracing::{debug, error, warn};

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
    /// Orchestration name for handler resolution
    #[allow(dead_code)]
    orchestration_name: String,
    /// Current turn index
    turn_index: u64,
    /// Incoming completion messages mapped and ordered
    completion_map: CompletionMap,
    /// Acknowledgment token for batch-ack after persistence
    ack_token: Option<String>,
    /// History events generated during this turn
    history_delta: Vec<Event>,
    /// Actions to dispatch after persistence
    pending_actions: Vec<crate::Action>,
    /// Current history at start of turn
    baseline_history: Vec<Event>,
}

impl OrchestrationTurn {
    /// Create a new orchestration turn
    pub fn new(instance: String, orchestration_name: String, turn_index: u64, baseline_history: Vec<Event>) -> Self {
        Self {
            instance,
            orchestration_name,
            turn_index,
            completion_map: CompletionMap::new(),
            ack_token: None,
            history_delta: Vec::new(),
            pending_actions: Vec::new(),
            baseline_history,
        }
    }

    /// Stage 1: Prepare completion map from incoming orchestrator messages
    /// This stage accumulates all completions and their ack tokens without side effects
    pub fn prep_completions(&mut self, messages: Vec<(OrchestratorMsg, String)>) {
        debug!(
            instance = %self.instance,
            turn_index = self.turn_index,
            message_count = messages.len(),
            "prepping completion map"
        );

        for (msg, token) in messages {
            let ack_token = match &msg {
                OrchestratorMsg::ExternalByName { name, data, .. } => {
                    // Handle external events specially since they need history lookup

                    self.completion_map.add_external_completion(
                        name.clone(),
                        data.clone(),
                        &self.baseline_history,
                        Some(token),
                    )
                }
                OrchestratorMsg::CancelRequested { reason, .. } => {
                    // Handle cancellation immediately - add to history delta, don't use completion map

                    // Check if orchestration is already terminated
                    // Since baseline_history is now filtered to current execution only,
                    // we can simply check for terminal events
                    let already_terminated = self.baseline_history.iter().any(|e| {
                        matches!(
                            e,
                            Event::OrchestrationCompleted { .. } | Event::OrchestrationFailed { .. }
                        )
                    });

                    // Check for duplicate cancellation (idempotent)
                    let already_cancelled = self
                        .baseline_history
                        .iter()
                        .chain(self.history_delta.iter())
                        .any(|e| matches!(e, Event::OrchestrationCancelRequested { .. }));

                    // Only add cancellation if not already terminated and not already cancelled
                    if !already_terminated && !already_cancelled {
                        self.history_delta
                            .push(Event::OrchestrationCancelRequested { reason: reason.clone() });
                    }

                    Some(token)
                }
                _ => {
                    // All other completions use standard processing
                    self.completion_map.add_completion(msg)
                }
            };

            // Store the ack token for later batching (only store the first one since they're all the same)
            if self.ack_token.is_none() {
                self.ack_token = ack_token;
            }
        }

        debug!(
            instance = %self.instance,
            completion_count = self.completion_map.ordered.len(),
            has_ack_token = self.ack_token.is_some(),
            "completion map prepared"
        );
    }

    /// Stage 2: Execute one turn of the orchestration using the replay engine
    /// This stage runs the orchestration logic and generates history deltas and actions
    pub fn execute_orchestration(&mut self, handler: Arc<dyn OrchestrationHandler>, input: String) -> TurnResult {
        debug!(
            instance = %self.instance,
            turn_index = self.turn_index,
            "executing orchestration turn"
        );

        // Set up the modified history with completion map context
        let mut working_history = self.baseline_history.clone();

        // Add any completion events from the completion map in order
        // This allows the orchestration to see completions during replay
        self.apply_completions_to_history(&mut working_history);

        // Use completion-aware replay execution
        // NOTE: We copy the completion map here because:
        // 1. The orchestration execution needs exclusive access to modify completion state
        // 2. We need to pass it to async futures that may outlive this scope
        // 3. Rust's borrow checker doesn't allow Arc<Mutex<&mut T>> to work cleanly
        // 4. The copy cost is minimal compared to the determinism benefits
        let completion_map_arc = std::sync::Arc::new(std::sync::Mutex::new(self.completion_map.clone()));

        let (updated_history, decisions, _logs, output_opt, _claims) =
            super::completion_aware_futures::run_turn_with_completion_map(
                working_history,
                self.turn_index,
                completion_map_arc.clone(),
                move |ctx| {
                    let h = handler.clone();
                    let inp = input.clone();
                    async move { h.invoke(ctx, inp).await }
                },
            );

        // Sync back any consumption state from the orchestration execution
        // Futures mark items as consumed during polling
        {
            let executed_map = completion_map_arc.lock().unwrap();
            // Update consumption state in our completion map
            for (i, entry) in self.completion_map.ordered.iter_mut().enumerate() {
                if let Some(executed_entry) = executed_map.ordered.get(i) {
                    entry.consumed = executed_entry.consumed;
                }
            }
            // Remove consumed entries from by_id map
            self.completion_map
                .by_id
                .retain(|key, _| executed_map.by_id.contains_key(key));
        }

        // Cleanup consumed entries
        self.completion_map.cleanup_consumed();

        // Calculate the delta from baseline
        if updated_history.len() > self.baseline_history.len() {
            self.history_delta = updated_history[self.baseline_history.len()..].to_vec();
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
            if let Event::OrchestrationCancelRequested { reason } = cancel_event {
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

        // Check for cancellation in the completion map or history
        if self.completion_map.by_id.contains_key(&(CompletionKind::Cancel, 0)) {
            if let Some(completion) = self.completion_map.get_ready_completion(CompletionKind::Cancel, 0) {
                if let crate::runtime::completion_map::CompletionValue::CancelReason(reason) = completion.data {
                    return TurnResult::Cancelled(reason);
                }
            }
        }

        // Check if there are unconsumed completions that indicate non-determinism
        if self.completion_map.has_unconsumed() {
            let unconsumed = self.completion_map.get_unconsumed();

            // Generate more specific error message based on context
            let error = self.generate_nondeterminism_error(&unconsumed);

            warn!(instance = %self.instance, error = %error, "detected non-determinism");
            return TurnResult::Failed(error);
        }

        TurnResult::Continue
    }

    /// Generate a specific non-determinism error message based on the context
    fn generate_nondeterminism_error(
        &self,
        unconsumed: &[(crate::runtime::completion_map::CompletionKind, u64)],
    ) -> String {
        // Check if this looks like a completion kind mismatch
        // Look at the history to see what kinds of futures were scheduled
        let mut expected_kinds = std::collections::HashMap::new();

        for event in &self.baseline_history {
            match event {
                crate::Event::ActivityScheduled { id, .. } => {
                    expected_kinds.insert(*id, crate::runtime::completion_map::CompletionKind::Activity);
                }
                crate::Event::TimerCreated { id, .. } => {
                    expected_kinds.insert(*id, crate::runtime::completion_map::CompletionKind::Timer);
                }
                crate::Event::ExternalSubscribed { id, .. } => {
                    expected_kinds.insert(*id, crate::runtime::completion_map::CompletionKind::External);
                }
                crate::Event::SubOrchestrationScheduled { id, .. } => {
                    expected_kinds.insert(*id, crate::runtime::completion_map::CompletionKind::SubOrchestration);
                }
                _ => {}
            }
        }

        // Check for kind mismatches in unconsumed completions
        for (completion_kind, completion_id) in unconsumed {
            if let Some(expected_kind) = expected_kinds.get(completion_id) {
                if expected_kind != completion_kind {
                    return format!(
                        "nondeterministic: completion kind mismatch for id={}, expected '{}', got '{}'",
                        completion_id,
                        kind_to_string(*expected_kind),
                        kind_to_string(*completion_kind)
                    );
                }
            } else {
                // Unexpected completion ID - no corresponding schedule found
                return format!(
                    "nondeterministic: unexpected completion id={} of kind '{}' (no matching schedule)",
                    completion_id,
                    kind_to_string(*completion_kind)
                );
            }
        }

        // If no specific mismatch found, provide the generic message
        format!("nondeterministic: unconsumed completions: {:?}", unconsumed)
    }
}

/// Convert completion kind to string for error messages
fn kind_to_string(kind: crate::runtime::completion_map::CompletionKind) -> &'static str {
    match kind {
        crate::runtime::completion_map::CompletionKind::Activity => "activity",
        crate::runtime::completion_map::CompletionKind::Timer => "timer",
        crate::runtime::completion_map::CompletionKind::External => "external",
        crate::runtime::completion_map::CompletionKind::SubOrchestration => "suborchestration",
        crate::runtime::completion_map::CompletionKind::Cancel => "cancel",
    }
}

impl OrchestrationTurn {
    /// Stage 3: Persist all state changes atomically
    /// This stage writes all history deltas and dispatches actions
    pub async fn persist_changes(
        &mut self,
        history_store: Arc<dyn HistoryStore>,
        runtime: &Arc<crate::runtime::Runtime>,
    ) -> Result<(), String> {
        debug!(
            instance = %self.instance,
            turn_index = self.turn_index,
            delta_count = self.history_delta.len(),
            action_count = self.pending_actions.len(),
            "persisting turn changes"
        );

        // Persist history delta if any
        if !self.history_delta.is_empty() {
            history_store
                .append(&self.instance, self.history_delta.clone())
                .await
                .map_err(|e| format!("failed to append history: {}", e))?;

            debug!(
                instance = %self.instance,
                events_appended = self.history_delta.len(),
                "history delta persisted"
            );
        }

        // Apply decisions (dispatch actions) now that history is persisted
        let full_history = {
            let mut h = self.baseline_history.clone();
            h.extend(self.history_delta.clone());
            h
        };

        runtime
            .apply_decisions(&self.instance, &full_history, self.pending_actions.clone())
            .await;

        debug!(
            instance = %self.instance,
            actions_applied = self.pending_actions.len(),
            "actions dispatched"
        );

        Ok(())
    }

    /// Stage 4: Acknowledge all processed messages in batch
    /// This stage only runs after successful persistence
    pub async fn acknowledge_messages(&mut self, history_store: Arc<dyn HistoryStore>) {
        if self.ack_token.is_none() {
            return;
        }
        
        // Check if we should abandon due to nondeterminism
        // Only trigger if the orchestration hasn't already completed successfully
        // We need to check the current history from the store since events might have been added there
        let current_history = history_store.read(&self.instance).await;
        let has_completed = current_history.iter().any(|e| {
            matches!(
                e,
                Event::OrchestrationCompleted { .. }
                    | Event::OrchestrationFailed { .. }
                    | Event::OrchestrationContinuedAsNew { .. }
            )
        });
        
        if self.completion_map.has_unconsumed() && !has_completed {
            if let Some(token) = &self.ack_token {
                debug!(
                    instance = %self.instance,
                    "terminating orchestration due to unconsumed completions (nondeterminism)"
                );
                
                // Add failure event to history delta
                self.history_delta.push(Event::OrchestrationFailed {
                    error: "nondeterministic: unconsumed completions detected".to_string(),
                });
                
                // Persist the failure event
                if let Err(e) = history_store.append(&self.instance, vec![Event::OrchestrationFailed {
                    error: "nondeterministic: unconsumed completions detected".to_string(),
                }]).await {
                    error!(
                        instance = %self.instance,
                        error = %e,
                        "failed to persist nondeterminism failure"
                    );
                }
                
                // Acknowledge the batch since we've handled it (by failing)
                if let Err(e) = history_store.ack_orchestrator(token).await {
                    error!(
                        instance = %self.instance,
                        error = %e,
                        "failed to ack batch after nondeterminism failure"
                    );
                }
            }
            self.ack_token = None;
            return;
        }

        debug!(
            instance = %self.instance,
            "acknowledging processed messages"
        );

        // Acknowledge the batch - all messages share the same token
        if let Some(token) = &self.ack_token {
            if let Err(e) = history_store.ack_orchestrator(token).await {
                error!(
                    instance = %self.instance,
                    error = %e,
                    "batch acknowledgment failed - messages will timeout and be redelivered"
                );
                // Don't fall back to individual acks - let the provider handle redelivery
                // This prevents partial acknowledgment scenarios that could cause inconsistency
            } else {
                debug!(
                    instance = %self.instance,
                    "batch acknowledgment successful"
                );
            }
        }

        self.ack_token = None;
    }

    /// Helper: Apply completion map entries to history for orchestration execution
    fn apply_completions_to_history(&mut self, _history: &mut Vec<Event>) {
        // We don't actually modify history here - the completion map will be consulted
        // by the modified future polling logic. This method is kept for future use
        // if we need to inject completion events directly into history.

        // The completion map will be made available to futures through a different mechanism
        // to maintain the existing API contract with the orchestration context.
    }

    /// Get a reference to the completion map for future polling
    pub fn completion_map(&self) -> &CompletionMap {
        &self.completion_map
    }

    /// Get a mutable reference to the completion map for future polling
    pub fn completion_map_mut(&mut self) -> &mut CompletionMap {
        &mut self.completion_map
    }

    /// Check if this turn made any progress (added history or has completions)
    pub fn made_progress(&self) -> bool {
        !self.history_delta.is_empty() || !self.completion_map.ordered.is_empty()
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
    use crate::runtime::router::OrchestratorMsg;

    #[test]
    fn test_turn_lifecycle() {
        let mut turn = OrchestrationTurn::new(
            "test-instance".to_string(),
            "test-orchestration".to_string(),
            1,
            vec![Event::OrchestrationStarted {
                name: "test-orchestration".to_string(),
                version: "1.0.0".to_string(),
                input: "test-input".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
        );

        // Stage 1: Prep completions
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

        turn.prep_completions(messages);
        assert_eq!(turn.ack_token.is_some(), true);
        assert!(turn.completion_map.is_next_ready(CompletionKind::Activity, 1));

        // Other stages would require actual runtime integration to test properly
    }
}

// Include comprehensive tests
#[path = "orchestration_turn_tests.rs"]
mod orchestration_turn_tests;
