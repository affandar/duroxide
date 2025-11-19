//! Orchestration dispatcher implementation for Runtime
//!
//! This module contains the orchestration dispatcher logic that:
//! - Spawns concurrent orchestration workers
//! - Fetches and processes orchestration items from the queue
//! - Handles orchestration execution and atomic commits

use crate::Event;
use crate::providers::{ExecutionMetadata, ProviderError, WorkItem};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::task::JoinHandle;
use tracing::{debug, error, warn};

use super::super::{HistoryManager, Runtime, WorkItemReader};

impl Runtime {
    /// Start the orchestration dispatcher with N concurrent workers
    pub(in crate::runtime) fn start_orchestration_dispatcher(self: Arc<Self>) -> JoinHandle<()> {
        // EXECUTION: spawns N concurrent orchestration workers
        // Instance-level locking in provider prevents concurrent processing of same instance
        let concurrency = self.options.orchestration_concurrency;
        let shutdown = self.shutdown_flag.clone();

        tokio::spawn(async move {
            let mut worker_handles = Vec::new();

            for worker_idx in 0..concurrency {
                let rt = self.clone();
                let shutdown = shutdown.clone();
                // Generate unique worker ID: orch-{index}-{runtime_id}
                let worker_id = format!("orch-{}-{}", worker_idx, rt.runtime_id);
                let handle = tokio::spawn(async move {
                    // debug!("Orchestration worker {} started", worker_id);
                    loop {
                        // Check shutdown flag before fetching
                        if shutdown.load(Ordering::Relaxed) {
                            // debug!("Orchestration worker {} exiting", worker_id);
                            break;
                        }

                        if let Ok(Some(item)) = rt
                            .history_store
                            .fetch_orchestration_item(rt.options.orchestrator_lock_timeout_secs)
                            .await
                        {
                            // Process orchestration item atomically
                            // Provider ensures no other worker has this instance locked
                            rt.process_orchestration_item(item, &worker_id).await;
                        } else {
                            tokio::time::sleep(std::time::Duration::from_millis(rt.options.dispatcher_idle_sleep_ms))
                                .await;
                        }
                    }
                });
                worker_handles.push(handle);
            }

            // Wait for all workers to complete
            for handle in worker_handles {
                let _ = handle.await;
            }
            // debug!("Orchestration dispatcher exited");
        })
    }

    /// Process a single orchestration item atomically
    pub(in crate::runtime) async fn process_orchestration_item(
        self: &Arc<Self>,
        item: crate::providers::OrchestrationItem,
        worker_id: &str,
    ) {
        // EXECUTION: builds deltas and commits via ack_orchestration_item
        let instance = &item.instance;
        let lock_token = &item.lock_token;

        // Extract metadata from history and work items
        let temp_history_mgr = HistoryManager::from_history(&item.history);
        let workitem_reader = WorkItemReader::from_messages(&item.messages, &temp_history_mgr, instance);

        // Bail on truly terminal histories (Completed/Failed), or ContinuedAsNew without a CAN start message
        if temp_history_mgr.is_completed
            || temp_history_mgr.is_failed
            || (temp_history_mgr.is_continued_as_new && !workitem_reader.is_continue_as_new)
        {
            warn!(instance = %instance, "Instance is terminal (completed/failed or CAN without start), acking batch without processing");
            let _ = self
                .ack_orchestration_with_changes(
                    lock_token,
                    item.execution_id,
                    vec![],
                    vec![],
                    vec![],
                    ExecutionMetadata::default(),
                )
                .await;
            return;
        }

        // Extract version before moving temp_history_mgr
        let version = temp_history_mgr.version().unwrap_or_else(|| "unknown".to_string());

        // Decide execution id and history to use for this execution
        let (execution_id_to_use, mut history_mgr) = if workitem_reader.is_continue_as_new {
            // ContinueAsNew - start with empty history for new execution
            (item.execution_id + 1, HistoryManager::from_history(&[]))
        } else {
            // Normal execution - use existing history
            (item.execution_id, temp_history_mgr)
        };

        // Log execution start with structured context
        if workitem_reader.has_orchestration_name() {
            tracing::debug!(
                target: "duroxide::runtime",
                instance_id = %instance,
                execution_id = %execution_id_to_use,
                orchestration_name = %workitem_reader.orchestration_name,
                orchestration_version = %version,
                worker_id = %worker_id,
                is_continue_as_new = %workitem_reader.is_continue_as_new,
                "Orchestration started"
            );
        } else if !workitem_reader.completion_messages.is_empty() {
            // Empty orchestration name with completion messages - just warn and skip
            tracing::warn!(instance = %item.instance, "empty effective batch - this should not happen");
        }

        // Process the execution (unified path)
        let (worker_items, orchestrator_items, execution_id_for_ack) = if workitem_reader.has_orchestration_name() {
            let (wi, oi) = self
                .handle_orchestration_atomic(instance, &mut history_mgr, &workitem_reader, execution_id_to_use, worker_id)
                .await;
            (wi, oi, execution_id_to_use)
        } else {
            // Empty effective batch
            (vec![], vec![], execution_id_to_use)
        };

        // Atomically commit all changes
        let history_delta = history_mgr.delta();

        // Compute execution metadata from history_delta (runtime responsibility)
        let metadata = Runtime::compute_execution_metadata(history_delta, &orchestrator_items, item.execution_id);

        // Log orchestration completion/failure
        if let Some(ref status) = metadata.status {
            let version = metadata.orchestration_version.as_deref().unwrap_or("unknown");
            let orch_name = metadata.orchestration_name.as_deref().unwrap_or("unknown");
            let event_count = history_mgr.full_history().len();

            match status.as_str() {
                "Completed" => {
                    tracing::debug!(
                        target: "duroxide::runtime",
                        instance_id = %instance,
                        execution_id = %execution_id_for_ack,
                        orchestration_name = %orch_name,
                        orchestration_version = %version,
                        worker_id = %worker_id,
                        history_events = %event_count,
                        "Orchestration completed"
                    );
                }
                "Failed" => {
                    // Extract error type from history_delta to determine log level
                    let error_type = history_delta
                        .iter()
                        .find_map(|event| {
                            if let Event::OrchestrationFailed { details, .. } = event {
                                Some(details.category())
                            } else {
                                None
                            }
                        })
                        .unwrap_or("unknown");

                    // Only log as ERROR for infrastructure/configuration errors
                    // Application errors are expected business logic failures
                    match error_type {
                        "infrastructure" | "configuration" => {
                            tracing::error!(
                                target: "duroxide::runtime",
                                instance_id = %instance,
                                execution_id = %execution_id_for_ack,
                                orchestration_name = %orch_name,
                                orchestration_version = %version,
                                worker_id = %worker_id,
                                history_events = %event_count,
                                error_type = %error_type,
                                error = metadata.output.as_deref().unwrap_or("unknown"),
                                "Orchestration failed"
                            );
                        }
                        "application" => {
                            tracing::warn!(
                                target: "duroxide::runtime",
                                instance_id = %instance,
                                execution_id = %execution_id_for_ack,
                                orchestration_name = %orch_name,
                                orchestration_version = %version,
                                worker_id = %worker_id,
                                history_events = %event_count,
                                error_type = %error_type,
                                error = metadata.output.as_deref().unwrap_or("unknown"),
                                "Orchestration failed (application error)"
                            );
                        }
                        _ => {
                            tracing::error!(
                                target: "duroxide::runtime",
                                instance_id = %instance,
                                execution_id = %execution_id_for_ack,
                                orchestration_name = %orch_name,
                                orchestration_version = %version,
                                worker_id = %worker_id,
                                history_events = %event_count,
                                error_type = %error_type,
                                error = metadata.output.as_deref().unwrap_or("unknown"),
                                "Orchestration failed (unknown error type)"
                            );
                        }
                    }
                }
                "ContinuedAsNew" => {
                    tracing::debug!(
                        target: "duroxide::runtime",
                        instance_id = %instance,
                        execution_id = %execution_id_for_ack,
                        orchestration_name = %orch_name,
                        orchestration_version = %version,
                        worker_id = %worker_id,
                        history_events = %event_count,
                        "Orchestration continued as new"
                    );
                }
                _ => {}
            }
        }

        // Robust ack with basic retry on any provider error
        match self
            .ack_orchestration_with_changes(
                lock_token,
                execution_id_for_ack,
                history_delta.to_vec(),
                worker_items,
                orchestrator_items,
                metadata,
            )
            .await
        {
            Ok(()) => {
                // Success - orchestration committed
            }
            Err(e) => {
                // Failed to ack - need to fail the orchestration with infrastructure error
                //
                // This error handler is reached in two scenarios:
                // 1. Non-retryable error: Permanent infrastructure issue (data corruption, invalid format, etc.)
                // 2. Retryable error that failed after all retries: Persistent infrastructure issue
                //    (database locked, network failure, etc. that persisted despite retries)
                //
                // In both cases, the infrastructure problem prevents the orchestration from progressing,
                // so we treat it as an infrastructure failure and mark the orchestration as failed.
                warn!(instance = %instance, error = %e, "Failed to ack orchestration item, failing orchestration");

                // Create infrastructure error and commit it
                let infra_error = e.to_infrastructure_error();
                self.record_orchestration_infrastructure_error();

                // Create a new history manager to append the failure event
                let mut failure_history_mgr = HistoryManager::from_history(&item.history);
                failure_history_mgr.append_failed(infra_error.clone());

                // Try to commit the failure event
                let failure_delta = failure_history_mgr.delta().to_vec();
                let failure_metadata = Runtime::compute_execution_metadata(&failure_delta, &[], item.execution_id);

                // Attempt to ack the failure (lock is still valid since we didn't abandon it yet)
                match self
                    .ack_orchestration_with_changes(
                        lock_token,
                        execution_id_for_ack,
                        failure_delta.clone(),
                        vec![],
                        vec![],
                        failure_metadata,
                    )
                    .await
                {
                    Ok(()) => {
                        // Successfully committed failure event
                        warn!(instance = %instance, "Successfully committed orchestration failure event");
                        // Abandon the lock now that failure is committed
                        drop(self.history_store.abandon_orchestration_item(lock_token, Some(50)));
                    }
                    Err(e2) => {
                        // Failed to commit failure event - abandon lock
                        warn!(instance = %instance, error = %e2, "Failed to commit failure event, abandoning lock");
                        drop(self.history_store.abandon_orchestration_item(lock_token, Some(50)));
                    }
                }
            }
        }
    }

    /// Helper methods for atomic orchestration processing
    pub(in crate::runtime) async fn handle_orchestration_atomic(
        self: &Arc<Self>,
        instance: &str,
        history_mgr: &mut HistoryManager,
        workitem_reader: &WorkItemReader,
        execution_id: u64,
        worker_id: &str,
    ) -> (Vec<WorkItem>, Vec<WorkItem>) {
        let mut worker_items = Vec::new();
        let mut orchestrator_items = Vec::new();

        // Create started event if this is a new instance
        if history_mgr.is_empty() {
            let orchestration_name = &workitem_reader.orchestration_name;
            
            // Get version policy for this orchestration
            let version_policy = self.orchestration_registry
                .policy
                .lock()
                .await
                .get(orchestration_name)
                .cloned();

            // Log registered orchestrations with their versions
            let registered_names = self.orchestration_registry.list_orchestration_names();
            let mut registered_with_versions: HashMap<String, Vec<String>> = HashMap::new();
            for name in &registered_names {
                let versions = self.orchestration_registry.list_orchestration_versions(name);
                registered_with_versions.insert(
                    name.clone(),
                    versions.iter().map(|v| v.to_string()).collect()
                );
            }

            debug!(
                target: "duroxide::runtime::dispatchers::orchestration:resolve_version",
                orchestration_name = %orchestration_name,
                workitem_version = ?workitem_reader.version,
                version_policy = ?version_policy,
                registered_count = registered_names.len(),
                registered_orchestrations = ?registered_names,
                registered_with_versions = ?registered_with_versions,
                "🔍 DIAGNOSTIC: Resolving new orchestration version - full context dump"
            );

            // Resolve version: use provided version or get from registry policy
            let resolved_version = if let Some(v) = &workitem_reader.version {
                debug!(
                    target: "duroxide::runtime::dispatchers::orchestration:resolve_version",
                    orchestration_name = %orchestration_name,
                    version = %v,
                    "Using provided version from work item"
                );
                v.to_string()
            } else if let Some(v) = self
                .orchestration_registry
                .resolve_version(orchestration_name)
                .await
            {
                debug!(
                    target: "duroxide::runtime::dispatchers::orchestration:resolve_version",
                    orchestration_name = %orchestration_name,
                    resolved_version = %v,
                    "Resolved version from registry"
                );
                v.to_string()
            } else {
                error!(
                    target: "duroxide::runtime::dispatchers::orchestration:resolve_version",
                    orchestration_name = %orchestration_name,
                    workitem_version = ?workitem_reader.version,
                    version_policy = ?version_policy,
                    registered_count = registered_names.len(),
                    registered_orchestrations = ?registered_names,
                    registered_with_versions = ?registered_with_versions,
                    "❌ Failed to resolve version for new orchestration"
                );

                // Not found in registry - fail with unregistered error
                history_mgr.append(Event::OrchestrationStarted {
                    event_id: crate::INITIAL_EVENT_ID,
                    name: workitem_reader.orchestration_name.clone(),
                    version: "0.0.0".to_string(), // Placeholder version for unregistered
                    input: workitem_reader.input.clone(),
                    parent_instance: workitem_reader.parent_instance.clone(),
                    parent_id: workitem_reader.parent_id,
                });

                history_mgr.append_failed(crate::ErrorDetails::Configuration {
                    kind: crate::ConfigErrorKind::UnregisteredOrchestration,
                    resource: workitem_reader.orchestration_name.clone(),
                    message: None,
                });
                self.record_orchestration_configuration_error();
                return (worker_items, orchestrator_items);
            };

            history_mgr.append(Event::OrchestrationStarted {
                event_id: 1, // First event always has event_id=1
                name: workitem_reader.orchestration_name.clone(),
                version: resolved_version,
                input: workitem_reader.input.clone(),
                parent_instance: workitem_reader.parent_instance.clone(),
                parent_id: workitem_reader.parent_id,
            });
        }

        // Run the atomic execution to get all changes
        let (_exec_history_delta, exec_worker_items, exec_orchestrator_items, _result) = self
            .clone()
            .run_single_execution_atomic(instance, history_mgr, workitem_reader, execution_id, worker_id)
            .await;

        // Combine all changes (history already in history_mgr via mutation)
        worker_items.extend(exec_worker_items);
        orchestrator_items.extend(exec_orchestrator_items);

        (worker_items, orchestrator_items)
    }

    /// Acknowledge an orchestration item with changes, using smart retry logic based on ProviderError
    /// Returns Ok(()) on success, or the ProviderError if all retries failed
    pub(in crate::runtime) async fn ack_orchestration_with_changes(
        &self,
        lock_token: &str,
        execution_id: u64,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) -> Result<(), ProviderError> {
        let mut attempts: u32 = 0;
        let max_attempts: u32 = 5;

        loop {
            match self
                .history_store
                .ack_orchestration_item(
                    lock_token,
                    execution_id,
                    history_delta.clone(),
                    worker_items.clone(),
                    orchestrator_items.clone(),
                    metadata.clone(),
                )
                .await
            {
                Ok(()) => {
                    return Ok(());
                }
                Err(e) => {
                    // Check if error is retryable
                    if !e.is_retryable() {
                        // Non-retryable error - don't abandon lock yet, return error
                        // Caller will try to commit failure event, then abandon lock
                        warn!(error = %e, "ack_orchestration_item failed with non-retryable error");
                        return Err(e);
                    }

                    // Retryable error - retry with backoff
                    if attempts < max_attempts {
                        let backoff_ms = 10u64.saturating_mul(1 << attempts);
                        warn!(attempts, backoff_ms, error = %e, "ack_orchestration_item failed; retrying");
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        attempts += 1;
                        continue;
                    } else {
                        warn!(attempts, error = %e, "Failed to ack_orchestration_item after max retries");
                        // Abandon the item to release lock
                        drop(self.history_store.abandon_orchestration_item(lock_token, Some(50)));
                        return Err(e);
                    }
                }
            }
        }
    }
}
