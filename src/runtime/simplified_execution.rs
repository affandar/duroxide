use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::{Event, runtime::{Runtime, OrchestrationHandler, router::OrchestratorMsg}};
use super::orchestration_turn::{OrchestrationTurn, TurnResult};

impl Runtime {
    /// Simplified run_instance_to_completion with clear stage separation
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
        // Ensure instance not already active
        {
            let mut act = self.active_instances.lock().await;
            if !act.insert(instance.to_string()) {
                return (Vec::new(), Err("already_active".into()));
            }
        }

        // Ensure cleanup of active flag even on panic
        let _active_guard = ActiveInstanceGuard {
            rt: self.clone(),
            instance: instance.to_string(),
        };

        // Load initial history and set up execution tracking
        let mut history = self.history_store.read(instance).await;
        let current_execution_id = self.history_store.latest_execution_id(instance).await.unwrap_or(1);
        self.current_execution_ids
            .lock()
            .await
            .insert(instance.to_string(), current_execution_id);

        // Pin version from history if available
        self.setup_version_pinning(instance, orchestration_name, &history).await;

        // Resolve orchestration handler
        let handler = match self.resolve_orchestration_handler(instance, orchestration_name).await {
            Ok(h) => h,
            Err(error) => {
                // Handle unregistered orchestration
                return self.handle_unregistered_orchestration(instance, &mut history, error).await;
            }
        };

        // Register for orchestrator messages
        let mut message_rx = self.router.register(instance).await;

        // Rehydrate any pending work from history
        super::completions::rehydrate_pending(instance, &history, &self.history_store).await;

        // Extract input and parent linkage for the orchestration
        let (input, parent_link) = self.extract_orchestration_context(orchestration_name, &history);

        let mut turn_index = 0u64;

        // MAIN EXECUTION LOOP
        loop {
            debug!(
                instance = %instance,
                turn_index = turn_index,
                "starting orchestration turn"
            );

            // STAGE 1: RECEIVE AND BATCH COMPLETIONS
            let messages = self.receive_completion_batch(&mut message_rx).await;
            
            if messages.is_empty() {
                //CR TODO : test whether we actually deyhdrate or are there always some waiters registered?
                // Check if we should dehydrate due to inactivity
                if !self.has_waiters(instance).await {
                    info!(instance = %instance, "dehydrating instance due to inactivity");
                    self.router.unregister(instance).await;
                    return (history, Ok(String::new()));
                }
                
                // Brief sleep and continue if we have waiters
                tokio::time::sleep(Duration::from_millis(Self::POLLER_IDLE_SLEEP_MS)).await;
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
                    return (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Ok(output.clone()),
                        parent_link.clone(),
                    ).await;
                }
                TurnResult::Failed(error) => {
                    // Orchestration failed
                    return (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Err(error.clone()),
                        parent_link.clone(),
                    ).await;
                }
                TurnResult::ContinueAsNew { input: new_input, version } => {
                    // Handle continue-as-new
                    return (&self).handle_continue_as_new_v2(
                        instance,
                        orchestration_name,
                        &mut turn,
                        &mut history,
                        new_input,
                        version,
                    ).await;
                }
                TurnResult::Cancelled(reason) => {
                    // Handle cancellation
                    return (&self).handle_orchestration_completion(
                        instance,
                        &mut turn,
                        &mut history,
                        Err(format!("canceled: {}", reason)),
                        parent_link.clone(),
                    ).await;
                }
            }
        }
    }

    /// Receive a batch of completion messages with timeout for dehydration
    async fn receive_completion_batch(
        &self,
        message_rx: &mut mpsc::UnboundedReceiver<OrchestratorMsg>,
    ) -> Vec<(OrchestratorMsg, String)> {
        let mut messages = Vec::new();

        // Wait for first message with timeout
        let first_msg = tokio::time::timeout(
            Duration::from_millis(Self::ORCH_IDLE_DEHYDRATE_MS),
            message_rx.recv(),
        ).await;

        let first_msg = match first_msg {
            Ok(Some(msg)) => msg,
            Ok(None) => return messages, // Channel closed
            Err(_) => return messages,   // Timeout
        };

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
        messages
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
struct ActiveInstanceGuard {
    rt: Arc<Runtime>,
    instance: String,
}

impl Drop for ActiveInstanceGuard {
    fn drop(&mut self) {
        let rt = self.rt.clone();
        let instance = self.instance.clone();
        tokio::spawn(async move {
            rt.active_instances.lock().await.remove(&instance);
            rt.current_execution_ids.lock().await.remove(&instance);
        });
    }
}

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
