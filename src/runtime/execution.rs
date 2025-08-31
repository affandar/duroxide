use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::{Event, runtime::{Runtime, OrchestrationHandler, router::OrchestratorMsg}};
use super::orchestration_turn::{OrchestrationTurn, TurnResult};



/// Result of attempting to receive a batch of completion messages
enum BatchResult {
    Messages(Vec<(OrchestratorMsg, String)>),
    ChannelClosed,
    Timeout,
}

impl Runtime {
    /// Execute an orchestration instance to completion
    /// 
    /// Architecture:
    /// - Outer loop: Dequeue orchestrator messages, ensure instance is active
    /// - Inner loop: Process batches of completions in deterministic turns
    /// - Four-stage turn lifecycle: prep -> execute -> persist -> ack
    /// - Deterministic completion processing with robust error handling
    pub async fn run_instance_to_completion(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
    ) -> (Vec<Event>, Result<String, String>) {
        debug!(instance, orchestration_name, "Starting instance execution");
        

        // Ensure instance not already active
        {
            let mut act = self.active_instances.lock().await;
            if !act.insert(instance.to_string()) {
                debug!(instance, "instance already active, returning early");
                return (Vec::new(), Err("already_active".into()));
            }
        }


        // Run the single execution - continue-as-new will be handled via orchestrator queue
        self.clone().run_single_execution(instance, orchestration_name).await
    }

    /// Execute a single orchestration execution
    async fn run_single_execution(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
    ) -> (Vec<Event>, Result<String, String>) {
        debug!(instance, orchestration_name, "🚀 Starting single execution");
        let start_time = std::time::Instant::now();
        
        // Ensure cleanup even if task panics
        struct ActiveGuard {
            rt: Arc<Runtime>,
            inst: String,
        }
        impl Drop for ActiveGuard {
            fn drop(&mut self) {
                let rt = self.rt.clone();
                let inst = self.inst.clone();
                let _ = tokio::spawn(async move {
                    rt.active_instances.lock().await.remove(&inst);
                    rt.current_execution_ids.lock().await.remove(&inst);
                });
            }
        }
        let _active_guard = ActiveGuard {
            rt: self.clone(),
            inst: instance.to_string(),
        };



        // Load initial history and set up execution tracking
        let full_instance_history = self.history_store.read(instance).await;
        
        // Keep full history for operations that need cross-execution data
        let mut history = full_instance_history.clone();

        
        let current_execution_id = self.history_store.latest_execution_id(instance).await.unwrap_or(1);
        self.current_execution_ids
            .lock()
            .await
            .insert(instance.to_string(), current_execution_id);


        // Pin version from history if available
        self.setup_version_pinning(instance, orchestration_name, &history).await;

        // Resolve orchestration handler

        let handler = match self.resolve_orchestration_handler(instance, orchestration_name).await {
            Ok(h) => {

                h
            },
            Err(error) => {

                // Handle unregistered orchestration
                let (hist, result) = self.handle_unregistered_orchestration(instance, &mut history, error).await;
                return (hist, result);
            }
        };

        // Register for orchestrator messages

        let mut message_rx = self.router.register(instance).await;


        // Rehydrate any pending work from history

        // Instance dehydration/rehydration handled by router


        // Extract input and parent linkage for the orchestration
        let (input, parent_link) = self.extract_orchestration_context(orchestration_name, &history);

        let mut turn_index = 0u64;
        let mut consecutive_no_progress_turns = 0;
        const MAX_NO_PROGRESS_TURNS: u32 = 3;

        // Check if this is a fresh orchestration start (has OrchestrationStarted but no progress)
        // Use current execution history to properly handle continue-as-new scenarios
        let current_execution_history = Self::extract_current_execution_history(&history);
        let needs_initial_execution = current_execution_history.iter().any(|e| matches!(e, Event::OrchestrationStarted { .. })) 
            && !current_execution_history.iter().any(|e| matches!(e, 
                Event::ActivityScheduled { .. } | 
                Event::TimerCreated { .. } | 
                Event::OrchestrationCompleted { .. } | 
                Event::OrchestrationFailed { .. }
            ));

        if needs_initial_execution {

            
            // Execute initial turn without waiting for messages
            let mut turn = OrchestrationTurn::new(
                instance.to_string(),
                orchestration_name.to_string(),
                turn_index,
                current_execution_history.clone(),
            );


            match turn.execute_orchestration(handler.clone(), input.clone()) {
                TurnResult::Continue => {
                    // Orchestration made progress, persist and continue

                    if let Err(e) = turn.persist_changes(self.history_store.clone(), &self).await {
                        let (hist, result) = self.handle_persistence_error(instance, &history, e).await;
                        return (hist, result);
                    }
                    history = turn.final_history();
                    turn_index += 1;

                }
                TurnResult::Completed(output) => {
                    // Orchestration completed on first execution

                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Ok(output.clone()),
                        parent_link.clone(),
                    ).await;
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, duration_ms = duration.as_millis(),
                           "✅ Instance execution completed successfully");
                    return (result.0, result.1);
                }
                TurnResult::Failed(error) => {
                    // Orchestration failed

                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Err(error.clone()),
                        parent_link.clone(),
                    ).await;
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, error = %error, duration_ms = duration.as_millis(),
                           "❌ Instance execution failed");
                    return (result.0, result.1);
                }
                TurnResult::ContinueAsNew { input: new_input, version } => {
                    // Handle continue-as-new
                    match (&self).handle_continue_as_new(
                        instance,
                        orchestration_name,
                        &mut turn,
                        &mut history,
                        new_input,
                        version,
                    ).await {
                        Ok(()) => {
                            let duration = start_time.elapsed();
                            debug!(instance, orchestration_name, duration_ms = duration.as_millis(),
                                   "🔄 Instance execution continued-as-new");
                            // Continue-as-new is now handled via orchestrator queue
                            // Return success with empty result as the new execution will provide the final result
                            return (history, Ok(String::new()));
                        }
                        Err(e) => {
                            return (history, Err(e));
                        }
                    }
                }
                TurnResult::Cancelled(reason) => {
                    // Handle cancellation with propagation to child orchestrations
                    
                    // Propagate cancellation to sub-orchestrations
                    self.propagate_cancellation_to_children(instance, &history).await;

                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Err(format!("canceled: {}", reason)),
                        parent_link.clone(),
                    ).await;
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, reason = %reason, duration_ms = duration.as_millis(),
                           "⚠️ Instance execution cancelled");
                    return (result.0, result.1);
                }
            }
        }

        // MAIN EXECUTION LOOP

        let mut loop_iterations = 0;
        loop {
            loop_iterations += 1;

            debug!(instance, loop_iterations, "main execution loop iteration");

            
            // Emergency brake for debugging
            if loop_iterations > 10 {



                // Let's continue a few more to see what happens
            }
            
            if loop_iterations > 15 {
                let (hist, result) = self.handle_persistence_error(instance, &history, "emergency exit - infinite loop in main execution".to_string()).await;
                return (hist, result);
            }
            
            // Check for infinite loop prevention
            if consecutive_no_progress_turns >= MAX_NO_PROGRESS_TURNS {
                let error = format!(
                    "execution stalled: {} consecutive turns with no progress in main loop - likely no message delivery or infinite loop", 
                    consecutive_no_progress_turns
                );
                let (hist, result) = self.handle_persistence_error(instance, &history, error).await;
                return (hist, result);
            }
            

            debug!(
                instance = %instance,
                turn_index = turn_index,
                "starting orchestration turn"
            );

            // STAGE 1: RECEIVE AND BATCH COMPLETIONS
            let batch_result = self.receive_completion_batch(&mut message_rx).await;
            
            let messages = match batch_result {
                BatchResult::Messages(msgs) => msgs,
                BatchResult::ChannelClosed => {
                    // Channel closed - clean exit
                    debug!(instance = %instance, "channel closed, dehydrating instance");
                    self.router.unregister(instance).await;
                    return (history, Ok(String::new()));
                }
                BatchResult::Timeout => {
                    // Timeout - check waiters before dehydrating
                    let has_waiters = self.has_waiters(instance).await;
                    if has_waiters {
                        // Keep running if there are waiters
                        debug!(instance = %instance, "timeout but has waiters, continuing");

                        tokio::time::sleep(Duration::from_millis(Self::POLLER_IDLE_SLEEP_MS)).await;
                        
                        // Don't increment no-progress counter on timeout - we're just waiting for messages
                        // Only increment when we execute a turn that makes no progress
                        
                        continue;
                    } else {
                        // No waiters - safe to dehydrate
                        debug!(instance = %instance, "timeout with no waiters, dehydrating instance");

                        self.router.unregister(instance).await;
                        return (history, Ok(String::new()));
                    }
                }
            };

            // Skip empty message batches - no turn to execute without completions
            if messages.is_empty() {

                debug!(instance = %instance, "received empty message batch, waiting for completions");
                // Don't increment no-progress counter for empty messages - we're just waiting
                continue;
            }


            // STAGE 2: EXECUTE ORCHESTRATION TURN
            let current_execution_history = Self::extract_current_execution_history(&history);
            let mut turn = OrchestrationTurn::new(
                instance.to_string(),
                orchestration_name.to_string(),
                turn_index,
                current_execution_history,
            );

            // Prep completions from incoming messages
            turn.prep_completions(messages);

            // Execute the orchestration logic
            let turn_result = turn.execute_orchestration(handler.clone(), input.clone());

            // STAGE 3: HANDLE TURN RESULT
            match turn_result {
                TurnResult::Continue => {
                    // Standard turn - persist changes and continue
                    if let Err(e) = turn.persist_changes(self.history_store.clone(), &self).await {
                        // CR TODO : abandon the messages here as well
                        let (hist, result) = self.handle_persistence_error(instance, &history, e).await;
                        return (hist, result);
                    }

                    // Update local history
                    history = turn.final_history();

                    // Acknowledge messages after successful persistence
                    turn.acknowledge_messages(self.history_store.clone()).await;

                    // Check for progress and prevent infinite loops
                    if turn.made_progress() {
                        turn_index += 1;
                        consecutive_no_progress_turns = 0;

                    } else {
                        consecutive_no_progress_turns += 1;

                        
                        if consecutive_no_progress_turns >= MAX_NO_PROGRESS_TURNS {
                            let error = format!(
                                "execution stalled: {} consecutive turns with no progress - likely infinite loop or unconsumed completions", 
                                consecutive_no_progress_turns
                            );

                            
                            // Return with the current history and error
                            let (hist, result) = self.handle_persistence_error(instance, &history, error).await;
                            return (hist, result);
                        }
                    }
                }
                TurnResult::Completed(output) => {
                    // Orchestration completed successfully
                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Ok(output.clone()),
                        parent_link.clone(),
                    ).await;
                    
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, duration_ms = duration.as_millis(),
                           "✅ Instance execution completed successfully");
                    return (result.0, result.1);
                }
                TurnResult::Failed(error) => {
                    // Orchestration failed
                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Err(error.clone()),
                        parent_link.clone(),
                    ).await;
                    
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, error = %error, duration_ms = duration.as_millis(),
                           "❌ Instance execution failed");
                    return (result.0, result.1);
                }
                TurnResult::ContinueAsNew { input: new_input, version } => {
                    // Handle continue-as-new
                    match (&self).handle_continue_as_new(
                        instance,
                        orchestration_name,
                        &mut turn,
                        &mut history,
                        new_input,
                        version,
                    ).await {
                        Ok(()) => {
                            let duration = start_time.elapsed();
                            debug!(instance, orchestration_name, duration_ms = duration.as_millis(),
                                   "🔄 Instance execution continued-as-new");
                            // Continue-as-new is now handled via orchestrator queue
                            // Return success with empty result as the new execution will provide the final result
                            return (history, Ok(String::new()));
                        }
                        Err(e) => {
                            return (history, Err(e));
                        }
                    }
                }
                TurnResult::Cancelled(reason) => {
                    // Handle cancellation with propagation to child orchestrations
                    
                    // Propagate cancellation to sub-orchestrations
                    self.propagate_cancellation_to_children(instance, &history).await;
                    
                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Err(format!("canceled: {}", reason)),
                        parent_link.clone(),
                    ).await;
                    
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, reason = %reason, duration_ms = duration.as_millis(),
                           "⚠️ Instance execution cancelled");
                    return (result.0, result.1);
                }
            }
        }
    }



    /// Receive a batch of completion messages with timeout for dehydration
    async fn receive_completion_batch(
        &self,
        message_rx: &mut mpsc::UnboundedReceiver<OrchestratorMsg>,
    ) -> BatchResult {
        // Wait for first message with timeout
        let first_msg = tokio::time::timeout(
            Duration::from_millis(Self::ORCH_IDLE_DEHYDRATE_MS),
            message_rx.recv(),
        ).await;

        let first_msg = match first_msg {
            Ok(Some(msg)) => msg,
            Ok(None) => {
                debug!("message channel closed");
                return BatchResult::ChannelClosed;
            }
            Err(_) => {
                debug!("message receive timeout");
                return BatchResult::Timeout;
            }
        };

        let mut messages = Vec::new();

        // Extract ack token from first message
        if let Some(token) = extract_ack_token(&first_msg) {
            messages.push((first_msg, token));
        }

        // Collect additional messages up to batch limit
        for _ in 0..Self::COMPLETION_BATCH_LIMIT {
            match message_rx.try_recv() {
                Ok(msg) => {
                    if let Some(token) = extract_ack_token(&msg) {
                        messages.push((msg, token));
                    }
                }
                Err(_) => break,
            }
        }

        debug!(message_count = messages.len(), "received completion batch");
        BatchResult::Messages(messages)
    }

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
            self.orchestration_registry.get(orchestration_name)
        };

        handler_opt.ok_or_else(|| {
            if let Some(v) = pinned_version {
                format!("canceled: missing version {}@{}", orchestration_name, v)
            } else {
                format!("unregistered:{}", orchestration_name)
            }
        })
    }

    /// Handle unregistered orchestration by failing gracefully
    async fn handle_unregistered_orchestration(
        &self,
        instance: &str,
        history: &mut Vec<Event>,
        error: String,
    ) -> (Vec<Event>, Result<String, String>) {
        // Append failure event
        if let Err(e) = self.history_store.append(instance, vec![Event::OrchestrationFailed { error: error.clone() }]).await {
            error!(instance, error=%e, "failed to append OrchestrationFailed for unknown orchestration");
            panic!("history append failed: {e}");
        }

        history.push(Event::OrchestrationFailed { error: error.clone() });
        
        // Waiters removed - using polling approach instead

        (history.clone(), Err(error))
    }

    /// Extract orchestration input and parent linkage from history
    fn extract_orchestration_context(&self, orchestration_name: &str, history: &[Event]) -> (String, Option<(String, u64)>) {
        let mut input = String::new();
        let mut parent_link = None;

        for e in history.iter().rev() {
            if let Event::OrchestrationStarted {
                name: n,
                input: inp,
                parent_instance,
                parent_id,
                ..
            } = e {
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

    /// Check if instance has result waiters (always false now - using polling approach)
    async fn has_waiters(&self, _instance: &str) -> bool {
        false
    }

    /// Handle orchestration completion (success or failure)
    async fn handle_orchestration_completion(
        self: &Arc<Self>,
        instance: &str,
        turn: &mut OrchestrationTurn,
        history: &mut Vec<Event>,
        result: Result<String, String>,
        parent_link: Option<(String, u64)>,
    ) -> (Vec<Event>, Result<String, String>) {
        // Persist any remaining changes from the turn
        if let Err(e) = turn.persist_changes(self.history_store.clone(), self).await {
            return self.handle_persistence_error(instance, history, e).await;
        }

        // Update local history
        *history = turn.final_history();

        // Append terminal event
        let terminal_event = match &result {
            Ok(output) => Event::OrchestrationCompleted { output: output.clone() },
            Err(error) => Event::OrchestrationFailed { error: error.clone() },
        };

        if let Err(e) = self.history_store.append(instance, vec![terminal_event.clone()]).await {
            return self.handle_persistence_error(instance, history, format!("failed to append terminal event: {}", e)).await;
        }

        history.push(terminal_event);

        // Acknowledge messages
        turn.acknowledge_messages(self.history_store.clone()).await;

        // Waiters removed - using polling approach instead

        // Notify parent if this is a sub-orchestration
        if let Some((parent_instance, parent_id)) = parent_link {
            let work_item = match &result {
                Ok(output) => crate::providers::WorkItem::SubOrchCompleted {
                    parent_instance: parent_instance.clone(),
                    parent_execution_id: self.get_execution_id_for_instance(&parent_instance).await,
                    parent_id,
                    result: output.clone(),
                },
                Err(error) => crate::providers::WorkItem::SubOrchFailed {
                    parent_instance: parent_instance.clone(),
                    parent_execution_id: self.get_execution_id_for_instance(&parent_instance).await,
                    parent_id,
                    error: error.clone(),
                },
            };

            let _ = self.history_store.enqueue_work(crate::providers::QueueKind::Orchestrator, work_item).await;
        }

        (history.clone(), result)
    }

    /// Handle continue-as-new scenario - persist events and enqueue new execution
    async fn handle_continue_as_new(
        self: &Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        turn: &mut OrchestrationTurn,
        history: &mut Vec<Event>,
        input: String,
        version: Option<String>,
    ) -> Result<(), String> {
        // Persist turn changes first
        if let Err(e) = turn.persist_changes(self.history_store.clone(), self).await {
            return Err(format!("failed to persist turn changes: {e}"));
        }

        *history = turn.final_history();

        // Acknowledge messages first
        turn.acknowledge_messages(self.history_store.clone()).await;

        // First, append the ContinuedAsNew event to close the current execution
        if let Err(e) = self.history_store.append(instance, vec![
            Event::OrchestrationContinuedAsNew { input: input.clone() },
        ]).await {
            error!(instance, error = %e, "failed to persist continue-as-new event");
            return Err(format!("continue-as-new persistence failed: {e}"));
        }
        
        // Read the final history of the current execution before creating the new one
        // This includes the ContinuedAsNew event we just appended
        let _final_history = self.history_store.read(instance).await;
        
        // If no version specified, clear any pinned version to allow default resolution
        // Otherwise use the specified version
        let version_str = if let Some(v) = version {
            v
        } else {
            // Clear pinned version to allow default resolution in the new execution
            self.pinned_versions.lock().await.remove(instance);
            "0.0.0".to_string()
        };
        if let Err(e) = self.history_store.create_new_execution(
            instance,
            orchestration_name,
            &version_str,
            &input,
            None,  // No parent for continue-as-new
            None,  // No parent ID
        ).await {
            error!(instance, error = %e, "failed to create new execution for continue-as-new");
            return Err(format!("continue-as-new new execution failed: {e}"));
        }

        // Enqueue a ContinueAsNew work item to the orchestrator queue
        // This will be picked up by the orchestration dispatcher and start a new execution
        // Note: We don't remove from active_instances here - the ActiveGuard will handle that
        // when this execution completes. The dispatcher will need to handle the case where
        // the instance is still marked as active.
        if let Err(e) = self.history_store.enqueue_work(
            crate::providers::QueueKind::Orchestrator,
            crate::providers::WorkItem::ContinueAsNew {
                instance: instance.to_string(),
                orchestration: orchestration_name.to_string(),
                input: input.clone(),
            },
        ).await {
            error!(instance, error = %e, "failed to enqueue continue-as-new work item");
            return Err(format!("continue-as-new enqueue failed: {e}"));
        }
        
        // Waiters removed - using polling approach instead
        // The polling approach will detect the ContinuedAsNew event and handle appropriately

        Ok(())
    }

    /// Handle persistence errors
    async fn handle_persistence_error(
        &self,
        instance: &str,
        history: &[Event],
        error: String,
    ) -> (Vec<Event>, Result<String, String>) {
        error!(instance, error=%error, "persistence failed");
        
        // Waiters removed - using polling approach instead

        (history.to_vec(), Err(error))
    }

    /// Propagate cancellation to child sub-orchestrations
    async fn propagate_cancellation_to_children(&self, instance: &str, history: &[Event]) {
        use crate::providers::{QueueKind, WorkItem};
        
        // Find all scheduled sub-orchestrations
        let scheduled_children: Vec<(u64, String)> = history
            .iter()
            .filter_map(|e| match e {
                Event::SubOrchestrationScheduled { id, instance: child, .. } => {
                    Some((*id, child.clone()))
                }
                _ => None,
            })
            .collect();

        // Find all completed sub-orchestrations 
        let completed_ids: std::collections::HashSet<u64> = history
            .iter()
            .filter_map(|e| match e {
                Event::SubOrchestrationCompleted { id, .. } 
                | Event::SubOrchestrationFailed { id, .. } => Some(*id),
                _ => None,
            })
            .collect();

        // Cancel uncompleted children
        for (id, child_suffix) in scheduled_children {
            if !completed_ids.contains(&id) {
                let child_full = format!("{}::{}", instance, child_suffix);
                let _ = self
                    .history_store
                    .enqueue_work(
                        QueueKind::Orchestrator,
                        WorkItem::CancelInstance {
                            instance: child_full,
                            reason: "parent canceled".into(),
                        },
                    )
                    .await;
            }
        }
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
