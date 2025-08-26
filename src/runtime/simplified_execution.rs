use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::{Event, runtime::{Runtime, OrchestrationHandler, router::OrchestratorMsg}};
use super::orchestration_turn::{OrchestrationTurn, TurnResult};

/// Result of attempting to receive a batch of completion messages
enum BatchResult {
    Messages(Vec<(OrchestratorMsg, String)>),
    ChannelClosed,
    Timeout,
}

impl Runtime {
    /// Simplified run_instance_to_completion with clear stage separation
    /// 
    /// This is the new simplified execution engine (v2).
    /// 
    /// Architecture:
    /// - Outer loop: Dequeue orchestrator messages, ensure instance is active
    /// - Inner loop: Process batches of completions in deterministic turns
    /// - Four-stage turn lifecycle: prep -> execute -> persist -> ack
    pub async fn run_instance_to_completion_v2(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
    ) -> (Vec<Event>, Result<String, String>) {
        eprintln!("🚀 [V2] Starting instance execution: {} / {}", instance, orchestration_name);
        debug!(instance, orchestration_name, "🚀 Starting instance execution (v2 - simplified engine)");
        
        let start_time = std::time::Instant::now();
        // Ensure instance not already active
        {
            let mut act = self.active_instances.lock().await;
            if !act.insert(instance.to_string()) {
                return (Vec::new(), Err("already_active".into()));
            }
        }
        
        // Ensure cleanup even if task panics (copied from v1)
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
        eprintln!("🚀 [V2] Loading history for instance: {}", instance);
        let mut history = self.history_store.read(instance).await;
        eprintln!("🚀 [V2] Loaded {} history events", history.len());
        
        let current_execution_id = self.history_store.latest_execution_id(instance).await.unwrap_or(1);
        self.current_execution_ids
            .lock()
            .await
            .insert(instance.to_string(), current_execution_id);
        eprintln!("🚀 [V2] Set execution ID {} for instance {}", current_execution_id, instance);

        // Pin version from history if available
        self.setup_version_pinning(instance, orchestration_name, &history).await;

        // Resolve orchestration handler
        eprintln!("🚀 [V2] Resolving orchestration handler for: {}", orchestration_name);
        let handler = match self.resolve_orchestration_handler(instance, orchestration_name).await {
            Ok(h) => {
                eprintln!("🚀 [V2] Found orchestration handler for: {}", orchestration_name);
                h
            },
            Err(error) => {
                eprintln!("🚀 [V2] Failed to resolve orchestration handler: {}", error);
                // Handle unregistered orchestration
                return self.handle_unregistered_orchestration(instance, &mut history, error).await;
            }
        };

        // Register for orchestrator messages
        eprintln!("🚀 [V2] Registering message router for instance: {}", instance);
        let mut message_rx = self.router.register(instance).await;
        eprintln!("🚀 [V2] Registered message router successfully");

        // Rehydrate any pending work from history
        eprintln!("🚀 [V2] Rehydrating pending work from history");
        super::completions::rehydrate_pending(instance, &history, &self.history_store).await;
        eprintln!("🚀 [V2] Rehydration complete");

        // Extract input and parent linkage for the orchestration
        let (input, parent_link) = self.extract_orchestration_context(orchestration_name, &history);

        let mut turn_index = 0u64;

        // Check if this is a fresh orchestration start (has OrchestrationStarted but no progress)
        let needs_initial_execution = history.iter().any(|e| matches!(e, Event::OrchestrationStarted { .. })) 
            && !history.iter().any(|e| matches!(e, 
                Event::ActivityScheduled { .. } | 
                Event::TimerCreated { .. } | 
                Event::OrchestrationCompleted { .. } | 
                Event::OrchestrationFailed { .. }
            ));

        if needs_initial_execution {
            eprintln!("🚀 [V2] Fresh orchestration detected - running initial turn");
            
            // Execute initial turn without waiting for messages
            let mut turn = OrchestrationTurn::new(
                instance.to_string(),
                orchestration_name.to_string(),
                turn_index,
                history.clone(),
            );

            eprintln!("🔧 [V2] About to execute initial orchestration turn");
            match turn.execute_orchestration(handler.clone(), input.clone()) {
                TurnResult::Continue => {
                    // Orchestration made progress, persist and continue
                    eprintln!("🔧 [V2] Initial turn returned Continue - orchestration made progress but didn't finish");
                    if let Err(e) = turn.persist_changes(self.history_store.clone(), &self).await {
                        return self.handle_persistence_error(instance, &history, e).await;
                    }
                    history = turn.final_history();
                    turn_index += 1;
                    eprintln!("🚀 [V2] Initial turn completed, turn_index now: {}", turn_index);
                }
                TurnResult::Completed(output) => {
                    // Orchestration completed on first execution
                    eprintln!("🚀 [V2] Orchestration completed on initial turn");
                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Ok(output.clone()),
                        parent_link.clone(),
                    ).await;
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, duration_ms = duration.as_millis(),
                           "✅ Instance execution completed successfully (v2 - simplified engine)");
                    return result;
                }
                TurnResult::Failed(error) => {
                    // Orchestration failed
                    eprintln!("🚀 [V2] Orchestration failed on initial turn: {}", error);
                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Err(error.clone()),
                        parent_link.clone(),
                    ).await;
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, error = %error, duration_ms = duration.as_millis(),
                           "❌ Instance execution failed (v2 - simplified engine)");
                    return result;
                }
                TurnResult::ContinueAsNew { input: new_input, version } => {
                    // Handle continue-as-new
                    eprintln!("🚀 [V2] Continue-as-new on initial turn");
                    let result = (&self).handle_continue_as_new_v2(
                        instance,
                        orchestration_name,
                        &mut turn,
                        &mut history,
                        new_input,
                        version,
                    ).await;
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, duration_ms = duration.as_millis(),
                           "🔄 Instance execution continued-as-new (v2 - simplified engine)");
                    return result;
                }
                TurnResult::Cancelled(reason) => {
                    // Handle cancellation
                    eprintln!("🚀 [V2] Orchestration cancelled on initial turn: {}", reason);
                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Err(format!("canceled: {}", reason)),
                        parent_link.clone(),
                    ).await;
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, reason = %reason, duration_ms = duration.as_millis(),
                           "⚠️ Instance execution cancelled (v2 - simplified engine)");
                    return result;
                }
            }
        }

        // MAIN EXECUTION LOOP
        eprintln!("🚀 [V2] Entering main execution loop");
        loop {
            eprintln!("🚀 [V2] Starting turn {} for instance {}", turn_index, instance);
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
                    // Channel closed - clean exit (like v1 line 1249)
                    debug!(instance = %instance, "channel closed, dehydrating instance");
                    self.router.unregister(instance).await;
                    return (history, Ok(String::new()));
                }
                BatchResult::Timeout => {
                    // Timeout - check waiters before dehydrating (like v1 lines 1253-1262)
                    let has_waiters = self.has_waiters(instance).await;
                    if has_waiters {
                        // Keep running if there are waiters
                        debug!(instance = %instance, "timeout but has waiters, continuing");
                        tokio::time::sleep(Duration::from_millis(Self::POLLER_IDLE_SLEEP_MS)).await;
                        continue;
                    } else {
                        // No waiters - safe to dehydrate
                        debug!(instance = %instance, "timeout with no waiters, dehydrating instance");
                        self.router.unregister(instance).await;
                        return (history, Ok(String::new()));
                    }
                }
            };
            
            // Skip empty batches (shouldn't happen with new logic, but safety check)
            if messages.is_empty() {
                debug!(instance = %instance, "received empty message batch, continuing");
                continue;
            }

            // STAGE 2: EXECUTE ORCHESTRATION TURN
            let mut turn = OrchestrationTurn::new(
                instance.to_string(),
                orchestration_name.to_string(),
                turn_index,
                history.clone(),
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
                        return self.handle_persistence_error(instance, &history, e).await;
                    }

                    // Update local history
                    history = turn.final_history();

                    // Acknowledge messages after successful persistence
                    turn.acknowledge_messages(self.history_store.clone()).await;

                    // Only increment turn index if we made progress
                    if turn.made_progress() {
                        turn_index += 1;
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
                           "✅ Instance execution completed successfully (v2 - simplified engine)");
                    return result;
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
                           "❌ Instance execution failed (v2 - simplified engine)");
                    return result;
                }
                TurnResult::ContinueAsNew { input: new_input, version } => {
                    // Handle continue-as-new
                    let result = (&self).handle_continue_as_new_v2(
                        instance,
                        orchestration_name,
                        &mut turn,
                        &mut history,
                        new_input,
                        version,
                    ).await;
                    
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, duration_ms = duration.as_millis(),
                           "🔄 Instance execution continued-as-new (v2 - simplified engine)");
                    return result;
                }
                TurnResult::Cancelled(reason) => {
                    // Handle cancellation
                    let result = (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Err(format!("canceled: {}", reason)),
                        parent_link.clone(),
                    ).await;
                    
                    let duration = start_time.elapsed();
                    debug!(instance, orchestration_name, reason = %reason, duration_ms = duration.as_millis(),
                           "⚠️ Instance execution cancelled (v2 - simplified engine)");
                    return result;
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
        
        // Notify waiters
        if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
            for w in waiters {
                let _ = w.send((history.clone(), Err(error.clone())));
            }
        }

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

    /// Check if instance has result waiters
    async fn has_waiters(&self, instance: &str) -> bool {
        self.result_waiters.lock().await.contains_key(instance)
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

        // Notify waiters
        if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
            for w in waiters {
                let _ = w.send((history.clone(), result.clone()));
            }
        }

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

    /// Handle continue-as-new scenario
    async fn handle_continue_as_new_v2(
        self: &Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        turn: &mut OrchestrationTurn,
        history: &mut Vec<Event>,
        input: String,
        version: Option<String>,
    ) -> (Vec<Event>, Result<String, String>) {
        // Persist turn changes
        if let Err(e) = turn.persist_changes(self.history_store.clone(), self).await {
            return self.handle_persistence_error(instance, history, e).await;
        }

        *history = turn.final_history();

        // Acknowledge messages
        turn.acknowledge_messages(self.history_store.clone()).await;

        // Handle continue-as-new using existing logic
        self.handle_continue_as_new(instance, orchestration_name, history, input, version).await;
        
        (history.clone(), Ok(String::new()))
    }

    /// Handle persistence errors
    async fn handle_persistence_error(
        &self,
        instance: &str,
        history: &[Event],
        error: String,
    ) -> (Vec<Event>, Result<String, String>) {
        error!(instance, error=%error, "persistence failed");
        
        // Notify waiters about the error
        if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
            for w in waiters {
                let _ = w.send((history.to_vec(), Err(format!("persistence failed: {}", error))));
            }
        }

        (history.to_vec(), Err(error))
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
