use std::sync::Arc;
use tracing::debug;

use super::orchestration_turn::{OrchestrationTurn, TurnResult};
use crate::{
    Event,
    providers::WorkItem,
    runtime::{OrchestrationHandler, Runtime, OrchestratorMsg},
};

impl Runtime {
    /// Execute an orchestration instance to completion
    ///
    /// Architecture:
    /// - Outer loop: Dequeue orchestrator messages, ensure instance is active
    /// - Inner loop: Process batches of completions in deterministic turns
    /// - Four-stage turn lifecycle: prep -> execute -> persist -> ack
    /// - Deterministic completion processing with robust error handling

    /// Set up version pinning from history
    async fn setup_version_pinning(&self, instance: &str, orchestration_name: &str, history: &[Event]) {
        if let Some(version_str) = history.iter().rev().find_map(|e| match e {
            Event::OrchestrationStarted { name: n, version, .. } if n == orchestration_name => Some(version.clone()),
            _ => None,
        }) {
            if version_str != "0.0.0" {
                if let Ok(v) = semver::Version::parse(&version_str) {
                    self.pinned_versions.lock().await.insert(instance.to_string(), v);
                }
            }
        }
    }

    /// Resolve the orchestration handler, preferring pinned version
    async fn resolve_orchestration_handler(
        &self,
        instance: &str,
        orchestration_name: &str,
    ) -> Result<Arc<dyn OrchestrationHandler>, String> {
        let pinned_version = self.pinned_versions.lock().await.get(instance).cloned();

        let handler_opt = if let Some(v) = pinned_version.clone() {
            self.orchestration_registry.resolve_exact(orchestration_name, &v)
        } else {
            self.orchestration_registry
                .resolve_for_start(orchestration_name)
                .await
                .map(|(_v, h)| h)
        };

        handler_opt.ok_or_else(|| {
            if let Some(v) = pinned_version {
                format!("canceled: missing version {}@{}", orchestration_name, v)
            } else {
                format!("unregistered:{}", orchestration_name)
            }
        })
    }

    /// Extract orchestration input and parent linkage from history
    fn extract_orchestration_context(
        &self,
        orchestration_name: &str,
        history: &[Event],
    ) -> (String, Option<(String, u64)>) {
        let mut input = String::new();
        let mut parent_link = None;

        for e in history.iter().rev() {
            if let Event::OrchestrationStarted {
                name: n,
                input: inp,
                parent_instance,
                parent_id,
                ..
            } = e
            {
                if n == orchestration_name {
                    input = inp.clone();
                    if let (Some(pinst), Some(pid)) = (parent_instance.clone(), *parent_id) {
                        parent_link = Some((pinst, pid));
                    }
                    break;
                }
            }
        }

        (input, parent_link)
    }


    /// Extract current execution history (from most recent OrchestrationStarted)
    /// This filters out events from previous executions in continue-as-new scenarios
    fn extract_current_execution_history(full_history: &[Event]) -> Vec<Event> {
        let current_execution_start = full_history
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, e)| match e {
                Event::OrchestrationStarted { .. } => Some(i),
                _ => None,
            })
            .unwrap_or(0);

        full_history[current_execution_start..].to_vec()
    }
    /// Execute a single orchestration execution collecting all changes atomically
    pub async fn run_single_execution_atomic(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        initial_history: Vec<Event>,
        execution_id: u64,
        completion_messages: Vec<OrchestratorMsg>,
    ) -> (
        Vec<Event>,
        Vec<WorkItem>,
        Vec<WorkItem>,
        Vec<WorkItem>,
        Result<String, String>,
    ) {
        debug!(instance, orchestration_name, "🚀 Starting atomic single execution");

        // Track all changes
        let mut history_delta = Vec::new();
        let mut worker_items = Vec::new();
        let mut timer_items = Vec::new();
        let mut orchestrator_items = Vec::new();

        // Helper: compute the next event id based on existing and delta history
        let calc_next_event_id = |existing: &Vec<Event>, delta: &Vec<Event>| -> u64 {
            existing
                .iter()
                .chain(delta.iter())
                .map(|e| e.event_id())
                .max()
                .unwrap_or(0)
                + 1
        };

        // Helper: only honor detached starts at terminal; ignore all other pending actions
        let mut enqueue_detached_from_pending = |pending: &[_]| {
            for action in pending {
                if let crate::Action::StartOrchestrationDetached {
                    name,
                    version,
                    instance: sub_instance,
                    input,
                    ..
                } = action
                {
                    orchestrator_items.push(WorkItem::StartOrchestration {
                        instance: sub_instance.clone(),
                        orchestration: name.clone(),
                        input: input.clone(),
                        version: version.clone(),
                        parent_instance: None,
                        parent_id: None,
                    });
                }
            }
        };

        // Use provided history directly
        let history = initial_history.clone();

        // Pin version from history if available
        self.setup_version_pinning(instance, orchestration_name, &history).await;

        // Resolve orchestration handler
        let handler = match self.resolve_orchestration_handler(instance, orchestration_name).await {
            Ok(h) => h,
            Err(error) => {
                // Handle unregistered orchestration
                let max_existing = history.iter().map(|e| e.event_id()).max().unwrap_or(0);
                let next_id = max_existing + 1;
                let terminal_event = Event::OrchestrationFailed {
                    event_id: next_id,
                    error: error.clone(),
                };
                history_delta.push(terminal_event);
                return (history_delta, worker_items, timer_items, orchestrator_items, Err(error));
            }
        };

        // Extract input and parent linkage for the orchestration
        let (input, parent_link) = self.extract_orchestration_context(orchestration_name, &history);
        if let Some((ref pinst, pid)) = parent_link {
            tracing::debug!(target = "duroxide::runtime::execution", instance=%instance, parent_instance=%pinst, parent_id=%pid, "Detected parent link for orchestration");
        } else {
            tracing::debug!(target = "duroxide::runtime::execution", instance=%instance, "No parent link for orchestration");
        }

        // Process completion messages in a single turn
        let messages: Vec<(OrchestratorMsg, String)> = completion_messages
            .into_iter()
            .filter_map(|msg| extract_ack_token(&msg).map(|token| (msg, token)))
            .collect();

        debug!(
            instance = %instance,
            message_count = messages.len(),
            "starting orchestration turn atomically"
        );

        // Execute orchestration turn
        let current_execution_history = Self::extract_current_execution_history(&history);
        let mut turn = OrchestrationTurn::new(
            instance.to_string(),
            0, // turn_index
            execution_id,
            current_execution_history,
        );

        // Prep completions from incoming messages
        if !messages.is_empty() {
            turn.prep_completions(messages.clone());
        }

        // Execute the orchestration logic
        let turn_result = turn.execute_orchestration(handler.clone(), input.clone());

        // Collect history delta from turn
        history_delta.extend(turn.history_delta().to_vec());

        // Check for nondeterminism is now handled by the turn execution itself
        // The turn will return TurnResult::Failed with a specific error message

        // Handle turn result and collect work items
        let result = match turn_result {
            TurnResult::Continue => {
                // Collect work items from pending actions
                for action in turn.pending_actions() {
                    match action {
                        crate::Action::CallActivity {
                            scheduling_event_id,
                            name,
                            input,
                        } => {
                            let execution_id = self.get_execution_id_for_instance(instance).await;
                            worker_items.push(WorkItem::ActivityExecute {
                                instance: instance.to_string(),
                                execution_id,
                                id: *scheduling_event_id,
                                name: name.clone(),
                                input: input.clone(),
                            });
                        }
                        crate::Action::CreateTimer {
                            scheduling_event_id,
                            delay_ms,
                        } => {
                            let execution_id = self.get_execution_id_for_instance(instance).await;
                            let fire_at_ms = Self::calculate_timer_fire_time(&turn.final_history(), *delay_ms);
                            timer_items.push(WorkItem::TimerSchedule {
                                instance: instance.to_string(),
                                execution_id,
                                id: *scheduling_event_id,
                                fire_at_ms,
                            });
                        }
                        crate::Action::StartSubOrchestration {
                            scheduling_event_id,
                            name,
                            version,
                            instance: sub_instance,
                            input,
                        } => {
                            // Construct the full child instance name with parent prefix
                            let child_full = format!("{}::{}", instance, sub_instance);
                            orchestrator_items.push(WorkItem::StartOrchestration {
                                instance: child_full,
                                orchestration: name.clone(),
                                input: input.clone(),
                                version: version.clone(),
                                parent_instance: Some(instance.to_string()),
                                parent_id: Some(*scheduling_event_id),
                            });
                        }
                        crate::Action::StartOrchestrationDetached {
                            scheduling_event_id: _,
                            name,
                            version,
                            instance: sub_instance,
                            input,
                        } => {
                            orchestrator_items.push(WorkItem::StartOrchestration {
                                instance: sub_instance.clone(),
                                orchestration: name.clone(),
                                input: input.clone(),
                                version: version.clone(),
                                parent_instance: None,
                                parent_id: None,
                            });
                        }
                        _ => {} // Other actions don't generate work items
                    }
                }

                Ok(String::new())
            }
            TurnResult::Completed(output) => {
                // At terminal, only detached orchestration actions should be honored; other pending actions are ignored.
                enqueue_detached_from_pending(turn.pending_actions());

                // Add completion event with next event_id
                let next_id = calc_next_event_id(&history, &history_delta);
                let terminal_event = Event::OrchestrationCompleted {
                    event_id: next_id,
                    output: output.clone(),
                };
                history_delta.push(terminal_event);

                // Notify parent if this is a sub-orchestration
                if let Some((parent_instance, parent_id)) = parent_link {
                    tracing::debug!(target = "duroxide::runtime::execution", instance=%instance, parent_instance=%parent_instance, parent_id=%parent_id, "Enqueue SubOrchCompleted to parent");
                    orchestrator_items.push(WorkItem::SubOrchCompleted {
                        parent_instance: parent_instance.clone(),
                        parent_execution_id: self.get_execution_id_for_instance(&parent_instance).await,
                        parent_id,
                        result: output.clone(),
                    });
                }

                Ok(output)
            }
            TurnResult::Failed(error) => {
                // At terminal, only detached orchestration actions should be honored; other pending actions are ignored.
                enqueue_detached_from_pending(turn.pending_actions());

                // Add failure event
                let next_id = calc_next_event_id(&history, &history_delta);
                let terminal_event = Event::OrchestrationFailed {
                    event_id: next_id,
                    error: error.clone(),
                };
                history_delta.push(terminal_event);

                // Notify parent if this is a sub-orchestration
                if let Some((parent_instance, parent_id)) = parent_link {
                    tracing::debug!(target = "duroxide::runtime::execution", instance=%instance, parent_instance=%parent_instance, parent_id=%parent_id, "Enqueue SubOrchFailed to parent");
                    orchestrator_items.push(WorkItem::SubOrchFailed {
                        parent_instance: parent_instance.clone(),
                        parent_execution_id: self.get_execution_id_for_instance(&parent_instance).await,
                        parent_id,
                        error: error.clone(),
                    });
                }

                Err(error)
            }
            TurnResult::ContinueAsNew { input, version } => {
                // Add ContinuedAsNew terminal event to history
                let next_id = calc_next_event_id(&history, &history_delta);
                let terminal_event = Event::OrchestrationContinuedAsNew {
                    event_id: next_id,
                    input: input.clone(),
                };
                history_delta.push(terminal_event);

                // Enqueue continue as new work item
                orchestrator_items.push(WorkItem::ContinueAsNew {
                    instance: instance.to_string(),
                    orchestration: orchestration_name.to_string(),
                    input: input.clone(),
                    version: version.clone(),
                });

                Ok("continued as new".to_string())
            }
            TurnResult::Cancelled(reason) => {
                // Cancellation is currently recorded as a failure with a specific message.
                let error = format!("canceled: {}", reason);
                let next_id = calc_next_event_id(&history, &history_delta);
                let terminal_event = Event::OrchestrationFailed {
                    event_id: next_id,
                    error: error.clone(),
                };
                history_delta.push(terminal_event);

                // Propagate cancellation to children
                let full_history = {
                    let mut h = history.clone();
                    h.extend(history_delta.iter().cloned());
                    h
                };
                let cancel_work_items = self.get_child_cancellation_work_items(instance, &full_history).await;
                orchestrator_items.extend(cancel_work_items);

                // Notify parent if this is a sub-orchestration
                if let Some((parent_instance, parent_id)) = parent_link {
                    orchestrator_items.push(WorkItem::SubOrchFailed {
                        parent_instance: parent_instance.clone(),
                        parent_execution_id: self.get_execution_id_for_instance(&parent_instance).await,
                        parent_id,
                        error: error.clone(),
                    });
                }

                Err(error)
            }
        };

        debug!(
            instance,
            "run_single_execution_atomic complete: history_delta={}, worker={}, timer={}, orch={}",
            history_delta.len(),
            worker_items.len(),
            timer_items.len(),
            orchestrator_items.len()
        );
        (history_delta, worker_items, timer_items, orchestrator_items, result)
    }

    /// Get work items to cancel child sub-orchestrations
    async fn get_child_cancellation_work_items(&self, instance: &str, history: &[Event]) -> Vec<WorkItem> {
        // Find all scheduled sub-orchestrations
        let scheduled_children: Vec<(u64, String)> = history
            .iter()
            .filter_map(|e| match e {
                Event::SubOrchestrationScheduled {
                    event_id,
                    instance: child,
                    ..
                } => Some((*event_id, child.clone())),
                _ => None,
            })
            .collect();

        // Find all completed sub-orchestrations (by source_event_id)
        let completed_ids: std::collections::HashSet<u64> = history
            .iter()
            .filter_map(|e| match e {
                Event::SubOrchestrationCompleted { source_event_id, .. }
                | Event::SubOrchestrationFailed { source_event_id, .. } => Some(*source_event_id),
                _ => None,
            })
            .collect();

        // Create cancel work items for uncompleted children
        let mut cancel_items = Vec::new();
        for (id, child_suffix) in scheduled_children {
            if !completed_ids.contains(&id) {
                let child_full = format!("{}::{}", instance, child_suffix);
                cancel_items.push(WorkItem::CancelInstance {
                    instance: child_full,
                    reason: "parent canceled".to_string(),
                });
            }
        }
        cancel_items
    }

    /// Calculate timer fire time based on history
    fn calculate_timer_fire_time(history: &[Event], delay_ms: u64) -> u64 {
        // Find the current logical time from the most recent timer event
        let current_time = history
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::TimerFired { fire_at_ms, .. } => Some(*fire_at_ms),
                _ => None,
            })
            .unwrap_or_else(|| {
                // Fallback to system time
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            });

        current_time.saturating_add(delay_ms)
    }
}

/// RAII guard to ensure active instance cleanup

/// Extract ack token from an orchestrator message
fn extract_ack_token(msg: &OrchestratorMsg) -> Option<String> {
    match msg {
        OrchestratorMsg::ActivityCompleted { ack_token, .. } => ack_token.clone(),
        OrchestratorMsg::ActivityFailed { ack_token, .. } => ack_token.clone(),
        OrchestratorMsg::TimerFired { ack_token, .. } => ack_token.clone(),
        OrchestratorMsg::ExternalByName { ack_token, .. } => ack_token.clone(),
        OrchestratorMsg::SubOrchCompleted { ack_token, .. } => ack_token.clone(),
        OrchestratorMsg::SubOrchFailed { ack_token, .. } => ack_token.clone(),
        OrchestratorMsg::CancelRequested { ack_token, .. } => ack_token.clone(),
    }
}
