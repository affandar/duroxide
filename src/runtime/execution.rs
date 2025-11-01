use std::sync::Arc;
use tracing::debug;

use super::replay_engine::{ReplayEngine, TurnResult};
use crate::{
    Event,
    providers::WorkItem,
    runtime::{OrchestrationHandler, Runtime},
};

impl Runtime {
    /// Execute a single orchestration turn atomically
    ///
    /// This method processes completion messages and executes one orchestration turn,
    /// collecting all resulting work items and history changes atomically.
    /// It handles version pinning, handler resolution, and deterministic completion processing.
    /// Resolve the orchestration handler using version from history
    async fn resolve_orchestration_handler(
        &self,
        orchestration_name: &str,
        history_mgr: &crate::runtime::state_helpers::HistoryManager,
    ) -> Result<Arc<dyn OrchestrationHandler>, crate::ErrorDetails> {
        // Get version from history (includes delta for new instances)
        let version_from_history = history_mgr.version();

        let handler_opt = if let Some(ref version_str) = version_from_history {
            // Use exact version from history
            if let Ok(v) = semver::Version::parse(version_str) {
                self.orchestration_registry
                    .resolve_handler_exact(orchestration_name, &v)
            } else {
                None
            }
        } else {
            // No version in history - shouldn't happen at this point
            None
        };

        handler_opt.ok_or_else(|| {
            if let Some(v) = version_from_history {
                crate::ErrorDetails::Configuration {
                    kind: crate::ConfigErrorKind::MissingVersion {
                        requested_version: v.clone(),
                    },
                    resource: orchestration_name.to_string(),
                    message: None,
                }
            } else {
                crate::ErrorDetails::Configuration {
                    kind: crate::ConfigErrorKind::UnregisteredOrchestration,
                    resource: orchestration_name.to_string(),
                    message: None,
                }
            }
        })
    }

    /// Execute a single orchestration turn atomically
    pub async fn run_single_execution_atomic(
        self: Arc<Self>,
        instance: &str,
        history_mgr: &mut crate::runtime::state_helpers::HistoryManager,
        workitem_reader: &crate::runtime::state_helpers::WorkItemReader,
        execution_id: u64,
    ) -> (Vec<Event>, Vec<WorkItem>, Vec<WorkItem>, Result<String, String>) {
        let orchestration_name = &workitem_reader.orchestration_name;
        debug!(instance, orchestration_name, "🚀 Starting atomic single execution");

        // Track all changes
        let mut worker_items = Vec::new();
        let mut orchestrator_items = Vec::new();

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
                        execution_id: crate::INITIAL_EXECUTION_ID,
                    });
                }
            }
        };

        // History must have OrchestrationStarted at this point (either from existing history or newly created in delta)
        debug_assert!(!history_mgr.is_empty(), "history_mgr should never be empty here");

        // Resolve orchestration handler using version from history
        let handler = match self
            .resolve_orchestration_handler(orchestration_name, history_mgr)
            .await
        {
            Ok(h) => h,
            Err(error) => {
                // Handle unregistered orchestration
                history_mgr.append_failed(error.clone());
                return (
                    history_mgr.delta().to_vec(),
                    worker_items,
                    orchestrator_items,
                    Err(error.display_message()),
                );
            }
        };

        // Extract input and parent linkage from history manager
        // (works for both existing history and newly appended OrchestrationStarted in delta)
        let (input, parent_link) = history_mgr.extract_context();

        if let Some((ref pinst, pid)) = parent_link {
            tracing::debug!(target = "duroxide::runtime::execution", instance=%instance, parent_instance=%pinst, parent_id=%pid, "Detected parent link for orchestration");
        } else {
            tracing::debug!(target = "duroxide::runtime::execution", instance=%instance, "No parent link for orchestration");
        }

        // Execute orchestration turn
        let messages = &workitem_reader.completion_messages;

        debug!(
            instance = %instance,
            message_count = messages.len(),
            "starting orchestration turn atomically"
        );
        // Use full history (always contains only current execution)
        let current_execution_history = history_mgr.full_history();
        let mut turn = ReplayEngine::new(instance.to_string(), execution_id, current_execution_history);

        // Prep completions from incoming messages
        if !messages.is_empty() {
            turn.prep_completions(messages.to_vec());
        }

        // Execute the orchestration logic
        let turn_result = turn.execute_orchestration(handler.clone(), input.clone());

        // Collect history delta from turn
        history_mgr.extend(turn.history_delta().to_vec());

        // Nondeterminism detection is handled by ReplayEngine::execute_orchestration

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

                            // Record TimerCreated in history for deterministic replay
                            // The turn has already added this via schedule_timer(), but we need to ensure
                            // the event_id matches the scheduling_event_id
                            // Actually, the turn already adds it, so we don't need to add it again here

                            // Enqueue TimerFired to orchestrator queue with delayed visibility
                            // Provider will use fire_at_ms for the visible_at timestamp
                            orchestrator_items.push(WorkItem::TimerFired {
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
                            let child_full = format!("{instance}::{sub_instance}");
                            orchestrator_items.push(WorkItem::StartOrchestration {
                                instance: child_full,
                                orchestration: name.clone(),
                                input: input.clone(),
                                version: version.clone(),
                                parent_instance: Some(instance.to_string()),
                                parent_id: Some(*scheduling_event_id),
                                execution_id: crate::INITIAL_EXECUTION_ID,
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
                                execution_id: crate::INITIAL_EXECUTION_ID,
                            });
                        }
                        _ => {} // Other actions don't generate work items
                    }
                }

                Ok(String::new())
            }
            TurnResult::Completed(output) => {
                // Honor detached orchestration starts at terminal state
                enqueue_detached_from_pending(turn.pending_actions());

                // Add completion event with next event_id
                let next_id = history_mgr.next_event_id();
                history_mgr.append(Event::OrchestrationCompleted {
                    event_id: next_id,
                    output: output.clone(),
                });

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
            TurnResult::Failed(details) => {
                // Honor detached orchestration starts at terminal state
                enqueue_detached_from_pending(turn.pending_actions());

                // Add failure event
                history_mgr.append_failed(details.clone());

                // Notify parent if this is a sub-orchestration
                if let Some((parent_instance, parent_id)) = parent_link {
                    tracing::debug!(target = "duroxide::runtime::execution", instance=%instance, parent_instance=%parent_instance, parent_id=%parent_id, "Enqueue SubOrchFailed to parent");
                    orchestrator_items.push(WorkItem::SubOrchFailed {
                        parent_instance: parent_instance.clone(),
                        parent_execution_id: self.get_execution_id_for_instance(&parent_instance).await,
                        parent_id,
                        details: details.clone(),
                    });
                }

                Err(details.display_message())
            }
            TurnResult::ContinueAsNew { input, version } => {
                // Add ContinuedAsNew terminal event to history
                let next_id = history_mgr.next_event_id();
                history_mgr.append(Event::OrchestrationContinuedAsNew {
                    event_id: next_id,
                    input: input.clone(),
                });

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
                let details = crate::ErrorDetails::Application {
                    kind: crate::AppErrorKind::Cancelled {
                        reason: reason.clone(),
                    },
                    message: String::new(),
                    retryable: false,
                };
                history_mgr.append_failed(details.clone());

                // Propagate cancellation to children
                let full_history = history_mgr.full_history();
                let cancel_work_items = self.get_child_cancellation_work_items(instance, &full_history).await;
                orchestrator_items.extend(cancel_work_items);

                // Notify parent if this is a sub-orchestration
                if let Some((parent_instance, parent_id)) = parent_link {
                    orchestrator_items.push(WorkItem::SubOrchFailed {
                        parent_instance: parent_instance.clone(),
                        parent_execution_id: self.get_execution_id_for_instance(&parent_instance).await,
                        parent_id,
                        details: details.clone(),
                    });
                }

                Err(details.display_message())
            }
        };

        debug!(
            instance,
            "run_single_execution_atomic complete: history_delta={}, worker={}, orch={}",
            history_mgr.delta().len(),
            worker_items.len(),
            orchestrator_items.len()
        );
        (history_mgr.delta().to_vec(), worker_items, orchestrator_items, result)
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
                let child_full = format!("{instance}::{child_suffix}");
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
