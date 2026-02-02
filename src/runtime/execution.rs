// Execution module uses Mutex locks - poison indicates a panic and should propagate
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::clone_on_ref_ptr)]

use std::sync::Arc;
use tracing::debug;

use super::replay_engine::{ReplayEngine, TurnResult};
use crate::{
    Event, EventKind,
    providers::{ScheduledActivityIdentifier, WorkItem},
    runtime::{OrchestrationHandler, Runtime},
};

impl Runtime {
    /// Execute a single orchestration turn atomically
    ///
    /// This method processes completion messages and executes one orchestration turn,
    /// collecting all resulting work items and history changes atomically.
    /// The handler must already be resolved and provided as a parameter.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_single_execution_atomic(
        self: Arc<Self>,
        instance: &str,
        history_mgr: &mut crate::runtime::state_helpers::HistoryManager,
        workitem_reader: &crate::runtime::state_helpers::WorkItemReader,
        execution_id: u64,
        worker_id: &str,
        handler: Arc<dyn OrchestrationHandler>,
        orchestration_version: String,
    ) -> (
        Vec<Event>,
        Vec<WorkItem>,
        Vec<WorkItem>,
        Vec<ScheduledActivityIdentifier>,
        Result<String, String>,
    ) {
        let orchestration_name = &workitem_reader.orchestration_name;
        debug!(instance, orchestration_name, "🚀 Starting atomic single execution");

        // Track all changes
        let mut worker_items = Vec::new();
        let mut orchestrator_items = Vec::new();
        let mut cancelled_activities: Vec<ScheduledActivityIdentifier> = Vec::new();

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
        // Get the persisted history length for is_replaying tracking
        let persisted_len = history_mgr.original_len();
        let mut turn = ReplayEngine::new(instance.to_string(), execution_id, current_execution_history)
            .with_persisted_history_len(persisted_len);

        // Prep completions from incoming messages
        if !messages.is_empty() {
            turn.prep_completions(messages.to_vec());
        }

        // Execute the orchestration logic
        let turn_result = turn.execute_orchestration(
            handler.clone(),
            input.clone(),
            orchestration_name.to_string(),
            orchestration_version.clone(),
            worker_id,
        );

        // Select/select2 losers: request cancellation for those activities now.
        for activity_id in turn.cancelled_activity_ids() {
            cancelled_activities.push(ScheduledActivityIdentifier {
                instance: instance.to_string(),
                execution_id,
                activity_id: *activity_id,
            });
        }

        // Select/select2 losers: collect sub-orchestration cancellations.
        // These will be added to orchestrator_items after the match block
        // (to avoid borrow conflict with enqueue_detached_from_pending closure).
        let cancelled_sub_orch_items: Vec<WorkItem> = turn
            .cancelled_sub_orchestration_ids()
            .iter()
            .map(|child_instance_id| WorkItem::CancelInstance {
                instance: crate::build_child_instance_id(instance, child_instance_id),
                reason: "parent dropped sub-orchestration future".to_string(),
            })
            .collect();

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
                            let execution_id = self.get_execution_id_for_instance(instance, Some(execution_id)).await;
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
                            fire_at_ms,
                        } => {
                            let execution_id = self.get_execution_id_for_instance(instance, Some(execution_id)).await;

                            // Enqueue TimerFired to orchestrator queue with delayed visibility
                            // Provider will use fire_at_ms for the visible_at timestamp
                            // Note: fire_at_ms is computed at scheduling time (wall-clock),
                            // ensuring timers fire at the correct absolute time regardless of history state.
                            orchestrator_items.push(WorkItem::TimerFired {
                                instance: instance.to_string(),
                                execution_id,
                                id: *scheduling_event_id,
                                fire_at_ms: *fire_at_ms,
                            });
                        }
                        crate::Action::StartSubOrchestration {
                            scheduling_event_id,
                            name,
                            version,
                            instance: sub_instance,
                            input,
                        } => {
                            let child_full = crate::build_child_instance_id(instance, sub_instance);
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
                history_mgr.append(Event::with_event_id(
                    next_id,
                    instance,
                    execution_id,
                    None,
                    EventKind::OrchestrationCompleted { output: output.clone() },
                ));
                // Note: Orchestration completion metrics are recorded via record_orchestration_completion_with_labels
                // which is called earlier with full context (name, version, duration, etc.)

                // Notify parent if this is a sub-orchestration
                if let Some((parent_instance, parent_id)) = parent_link {
                    tracing::debug!(target = "duroxide::runtime::execution", instance=%instance, parent_instance=%parent_instance, parent_id=%parent_id, "Enqueue SubOrchCompleted to parent");
                    orchestrator_items.push(WorkItem::SubOrchCompleted {
                        parent_instance: parent_instance.clone(),
                        parent_execution_id: self.get_execution_id_for_instance(&parent_instance, None).await,
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
                match &details {
                    crate::ErrorDetails::Application { .. } => self.record_orchestration_application_error(),
                    crate::ErrorDetails::Infrastructure { .. } => self.record_orchestration_infrastructure_error(),
                    crate::ErrorDetails::Configuration { .. } => self.record_orchestration_configuration_error(),
                    crate::ErrorDetails::Poison { .. } => self.record_orchestration_poison(),
                }
                history_mgr.append_failed(details.clone());

                // Notify parent if this is a sub-orchestration
                if let Some((parent_instance, parent_id)) = parent_link {
                    tracing::debug!(target = "duroxide::runtime::execution", instance=%instance, parent_instance=%parent_instance, parent_id=%parent_id, "Enqueue SubOrchFailed to parent");
                    orchestrator_items.push(WorkItem::SubOrchFailed {
                        parent_instance: parent_instance.clone(),
                        parent_execution_id: self.get_execution_id_for_instance(&parent_instance, None).await,
                        parent_id,
                        details: details.clone(),
                    });
                }

                Err(details.display_message())
            }
            TurnResult::ContinueAsNew { input, version } => {
                // Add ContinuedAsNew terminal event to history
                let next_id = history_mgr.next_event_id();
                history_mgr.append(Event::with_event_id(
                    next_id,
                    instance,
                    execution_id,
                    None,
                    EventKind::OrchestrationContinuedAsNew { input: input.clone() },
                ));

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
                    kind: crate::AppErrorKind::Cancelled { reason: reason.clone() },
                    message: String::new(),
                    retryable: false,
                };
                history_mgr.append_failed(details.clone());

                // Propagate cancellation to children.
                // Also record SubOrchestrationCancelRequested events in the *parent* history
                // for observability/debugging.
                let child_cancellations = Self::get_child_cancellation_targets(history_mgr.full_history_iter());

                for (schedule_id, child_suffix) in child_cancellations {
                    let next_id = history_mgr.next_event_id();
                    history_mgr.append(Event::with_event_id(
                        next_id,
                        instance,
                        execution_id,
                        Some(schedule_id),
                        EventKind::SubOrchestrationCancelRequested {
                            reason: "orchestration_terminal_failed".to_string(),
                        },
                    ));

                    orchestrator_items.push(WorkItem::CancelInstance {
                        instance: crate::build_child_instance_id(instance, &child_suffix),
                        reason: "parent canceled".to_string(),
                    });
                }

                // Notify parent if this is a sub-orchestration
                if let Some((parent_instance, parent_id)) = parent_link {
                    orchestrator_items.push(WorkItem::SubOrchFailed {
                        parent_instance: parent_instance.clone(),
                        parent_execution_id: self.get_execution_id_for_instance(&parent_instance, None).await,
                        parent_id,
                        details: details.clone(),
                    });
                }

                Err(details.display_message())
            }
        };

        // Now add cancelled sub-orchestration items (deferred to avoid borrow conflict)
        orchestrator_items.extend(cancelled_sub_orch_items);

        debug!(
            instance,
            "run_single_execution_atomic complete: history_delta={}, worker={}, orch={}",
            history_mgr.delta().len(),
            worker_items.len(),
            orchestrator_items.len()
        );
        (
            history_mgr.delta().to_vec(),
            worker_items,
            orchestrator_items,
            cancelled_activities,
            result,
        )
    }

    /// Get cancellation targets for child sub-orchestrations.
    ///
    /// Returns `(scheduling_event_id, child_instance_suffix)` for children that were scheduled
    /// but have not yet completed/failed.
    fn get_child_cancellation_targets<'a>(history: impl Iterator<Item = &'a Event>) -> Vec<(u64, String)> {
        // Single-pass collection of scheduled children and completed IDs
        let (scheduled_children, completed_ids): (Vec<(u64, String)>, std::collections::HashSet<u64>) = history.fold(
            (Vec::new(), std::collections::HashSet::<u64>::new()),
            |(mut scheduled, mut completed), e| {
                match &e.kind {
                    EventKind::SubOrchestrationScheduled { instance: child, .. } => {
                        scheduled.push((e.event_id(), child.clone()));
                    }
                    EventKind::SubOrchestrationCompleted { .. } | EventKind::SubOrchestrationFailed { .. } => {
                        if let Some(source_id) = e.source_event_id {
                            completed.insert(source_id);
                        }
                    }
                    _ => {}
                }
                (scheduled, completed)
            },
        );

        // Return cancellation targets for uncompleted children
        scheduled_children
            .into_iter()
            .filter(|(id, _child_suffix)| !completed_ids.contains(id))
            .collect()
    }
}
