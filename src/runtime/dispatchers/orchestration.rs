//! Orchestration dispatcher implementation for Runtime
//!
//! This module contains the orchestration dispatcher logic that:
//! - Spawns concurrent orchestration workers
//! - Fetches and processes orchestration items from the queue
//! - Handles orchestration execution and atomic commits
//! - Renews locks during long-running orchestration turns

use crate::providers::{ExecutionMetadata, ProviderError, WorkItem};
use crate::{Event, EventKind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::warn;

use super::super::{HistoryManager, Runtime, WorkItemReader};

/// Calculate the renewal interval based on lock timeout and buffer settings.
///
/// # Logic
/// - If timeout ≥ 15s: renew at (timeout - buffer)
/// - If timeout < 15s: renew at 0.5 × timeout (buffer ignored)
fn calculate_renewal_interval(lock_timeout: Duration, buffer: Duration) -> Duration {
    if lock_timeout >= Duration::from_secs(15) {
        let buffer = buffer.min(lock_timeout);
        let interval = lock_timeout
            .checked_sub(buffer)
            .unwrap_or_else(|| Duration::from_secs(1));
        interval.max(Duration::from_secs(1))
    } else {
        let half = (lock_timeout.as_secs_f64() * 0.5).ceil().max(1.0);
        Duration::from_secs_f64(half)
    }
}

/// Spawn a background task to renew the lock for an in-flight orchestration.
fn spawn_orchestration_lock_renewal_task(
    store: Arc<dyn crate::providers::Provider>,
    token: String,
    lock_timeout: Duration,
    buffer: Duration,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    let renewal_interval = calculate_renewal_interval(lock_timeout, buffer);

    tracing::debug!(
        target: "duroxide::runtime::dispatchers::orchestration",
        lock_token = %token,
        lock_timeout_secs = %lock_timeout.as_secs(),
        buffer_secs = %buffer.as_secs(),
        renewal_interval_secs = %renewal_interval.as_secs(),
        "Spawning orchestration lock renewal task"
    );

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(renewal_interval);
        interval.tick().await; // Skip first immediate tick

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::debug!(
                            target: "duroxide::runtime::dispatchers::orchestration",
                            lock_token = %token,
                            "Lock renewal task stopping due to shutdown"
                        );
                        break;
                    }

                    match store.renew_orchestration_item_lock(&token, lock_timeout).await {
                        Ok(()) => {
                            tracing::trace!(
                                target: "duroxide::runtime::dispatchers::orchestration",
                                lock_token = %token,
                                extend_secs = %lock_timeout.as_secs(),
                                "Orchestration lock renewed"
                            );
                        }
                        Err(e) => {
                            tracing::debug!(
                                target: "duroxide::runtime::dispatchers::orchestration",
                                lock_token = %token,
                                error = %e,
                                "Failed to renew orchestration lock (may have been acked/abandoned)"
                            );
                            // Stop renewal - lock is gone or expired
                            break;
                        }
                    }
                }
            }
        }

        tracing::debug!(
            target: "duroxide::runtime::dispatchers::orchestration",
            lock_token = %token,
            "Orchestration lock renewal task stopped"
        );
    })
}

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
                let rt = Arc::clone(&self);
                let shutdown = Arc::clone(&shutdown);
                // Generate unique worker ID: orch-{index}-{runtime_id}
                let worker_id = format!("orch-{worker_idx}-{}", rt.runtime_id);
                let handle = tokio::spawn(async move {
                    // debug!("Orchestration worker {} started", worker_id);
                    loop {
                        // Check shutdown flag before fetching
                        if shutdown.load(Ordering::Relaxed) {
                            // debug!("Orchestration worker {} exiting", worker_id);
                            break;
                        }

                        let min_interval = rt.options.dispatcher_min_poll_interval;
                        let poll_timeout = rt.options.dispatcher_long_poll_timeout;
                        let start_time = std::time::Instant::now();
                        let mut work_found = false;

                        match rt
                            .history_store
                            .fetch_orchestration_item(rt.options.orchestrator_lock_timeout, poll_timeout)
                            .await
                        {
                            Ok(Some((item, lock_token, attempt_count))) => {
                                // Spawn lock renewal task for this orchestration
                                let renewal_handle = spawn_orchestration_lock_renewal_task(
                                    Arc::clone(&rt.history_store),
                                    lock_token.clone(),
                                    rt.options.orchestrator_lock_timeout,
                                    rt.options.orchestrator_lock_renewal_buffer,
                                    Arc::clone(&shutdown),
                                );

                                // TEST HOOK: Inject delay after spawning renewal task
                                // This simulates slow processing to test lock renewal
                                #[cfg(feature = "test-hooks")]
                                if let Some(delay) =
                                    crate::runtime::test_hooks::get_orch_processing_delay(&item.instance)
                                {
                                    tracing::debug!(
                                        instance = %item.instance,
                                        delay_ms = delay.as_millis(),
                                        "Test hook: injecting orchestration processing delay"
                                    );
                                    tokio::time::sleep(delay).await;
                                }

                                // Process orchestration item atomically
                                // Provider ensures no other worker has this instance locked
                                rt.process_orchestration_item(item, &lock_token, attempt_count, &worker_id)
                                    .await;

                                // Stop lock renewal task now that orchestration turn is complete
                                renewal_handle.abort();

                                work_found = true;
                            }
                            Ok(None) => {
                                // No work available
                            }
                            Err(e) => {
                                warn!("Error fetching orchestration item: {:?}", e);
                                // Backoff on error
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                continue;
                            }
                        }

                        // Enforce minimum polling interval to prevent hot loops
                        if !work_found {
                            let elapsed = start_time.elapsed();
                            if elapsed < min_interval {
                                let sleep_duration = min_interval - elapsed;
                                if !shutdown.load(Ordering::Relaxed) {
                                    tokio::time::sleep(sleep_duration).await;
                                }
                            } else {
                                // Waited long enough (e.g. long poll timeout expired), yield to prevent starvation
                                tokio::task::yield_now().await;
                            }
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
        lock_token: &str,
        attempt_count: u32,
        worker_id: &str,
    ) {
        // EXECUTION: builds deltas and commits via ack_orchestration_item
        let instance = &item.instance;

        // Check for poison message - message has been fetched too many times
        if attempt_count > self.options.max_attempts {
            warn!(
                instance = %instance,
                attempt_count = attempt_count,
                max_attempts = self.options.max_attempts,
                "Orchestration message exceeded max attempts, marking as poison"
            );

            self.fail_orchestration_as_poison(&item, lock_token, attempt_count)
                .await;
            return;
        }

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

        // Track start time for duration metrics
        let start_time = std::time::Instant::now();
        let mut turn_count = 0u64; // Track turns for this execution

        // Log execution start with structured context and record metrics
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

            // Record orchestration start metric
            // Note: We can't currently distinguish client-initiated vs suborchestration-initiated
            // since we don't have parent context information available here.
            let initiated_by = if workitem_reader.is_continue_as_new {
                "continueAsNew"
            } else {
                "client"
            };
            self.record_orchestration_start(&workitem_reader.orchestration_name, &version, initiated_by);

            // Increment active orchestrations gauge ONLY for brand new orchestrations
            // Do NOT increment for continue-as-new - the orchestration was already active!
            if history_mgr.full_history().is_empty() && !workitem_reader.is_continue_as_new {
                self.increment_active_orchestrations();
            }
        } else if !workitem_reader.completion_messages.is_empty() {
            // Empty orchestration name with completion messages - just warn and skip
            tracing::warn!(instance = %item.instance, "empty effective batch - this should not happen");
        }

        // Process the execution (unified path)
        let (worker_items, orchestrator_items, execution_id_for_ack) = if workitem_reader.has_orchestration_name() {
            let (wi, oi) = self
                .handle_orchestration_atomic(
                    instance,
                    &mut history_mgr,
                    &workitem_reader,
                    execution_id_to_use,
                    worker_id,
                )
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

        // Calculate metrics
        let duration_seconds = start_time.elapsed().as_secs_f64();
        turn_count += 1; // At least one turn was processed

        // Log orchestration completion/failure and record metrics
        if let Some(ref status) = metadata.status {
            // Use actual orchestration name from work item, not metadata (which may be "unknown")
            let orch_name = if workitem_reader.has_orchestration_name() {
                workitem_reader.orchestration_name.as_str()
            } else {
                "unknown"
            };
            let version = metadata.orchestration_version.as_deref().unwrap_or(&version);
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

                    // Record completion metrics with labels
                    self.record_orchestration_completion_with_labels(
                        orch_name,
                        version,
                        "success",
                        duration_seconds,
                        turn_count,
                        event_count as u64,
                    );

                    // Decrement active orchestrations (truly completed)
                    self.decrement_active_orchestrations();
                }
                "Failed" => {
                    // Extract error type from history_delta to determine log level
                    let (error_type, error_category) = history_delta
                        .iter()
                        .find_map(|event| {
                            if let EventKind::OrchestrationFailed { details } = &event.kind {
                                let category = details.category();
                                let error_type = match category {
                                    "infrastructure" => "infrastructure_error",
                                    "configuration" => "config_error",
                                    "application" => "app_error",
                                    _ => "unknown",
                                };
                                Some((error_type, category))
                            } else {
                                None
                            }
                        })
                        .unwrap_or(("unknown", "unknown"));

                    // Only log as ERROR for infrastructure/configuration errors
                    // Application errors are expected business logic failures
                    match error_category {
                        "infrastructure" | "configuration" => {
                            tracing::error!(
                                target: "duroxide::runtime",
                                instance_id = %instance,
                                execution_id = %execution_id_for_ack,
                                orchestration_name = %orch_name,
                                orchestration_version = %version,
                                worker_id = %worker_id,
                                history_events = %event_count,
                                error_type = %error_category,
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
                                error_type = %error_category,
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
                                error_type = %error_category,
                                error = metadata.output.as_deref().unwrap_or("unknown"),
                                "Orchestration failed (unknown error type)"
                            );
                        }
                    }

                    // Record failure metrics with labels
                    self.record_orchestration_failure_with_labels(orch_name, version, error_type, error_category);

                    // Also record completion with failed status for duration tracking
                    self.record_orchestration_completion_with_labels(
                        orch_name,
                        version,
                        "failed",
                        duration_seconds,
                        turn_count,
                        event_count as u64,
                    );

                    // Decrement active orchestrations (terminal failure)
                    self.decrement_active_orchestrations();
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

                    // Record continue-as-new metric
                    self.record_continue_as_new(orch_name, execution_id_for_ack);

                    // Record completion with continue-as-new status
                    self.record_orchestration_completion_with_labels(
                        orch_name,
                        version,
                        "continuedAsNew",
                        duration_seconds,
                        turn_count,
                        event_count as u64,
                    );

                    // NOTE: Do NOT decrement active_orchestrations on continue-as-new!
                    // The orchestration is still active, just starting a new execution.
                    // The increment for the new execution will happen when the ContinueAsNew
                    // message is processed (above in the is_continue_as_new branch).
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
                        // Successfully committed failure event (ack also releases the lock)
                        warn!(instance = %instance, "Successfully committed orchestration failure event");
                    }
                    Err(e2) => {
                        // Failed to commit failure event - abandon lock
                        warn!(instance = %instance, error = %e2, "Failed to commit failure event, abandoning lock");
                        drop(self.history_store.abandon_orchestration_item(
                            lock_token,
                            Some(std::time::Duration::from_millis(50)),
                            false,
                        ));
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

        // Resolve handler once - use provided version or resolve from registry policy
        let resolved_handler = if let Some(v_str) = &workitem_reader.version {
            // Use exact version if provided
            if let Ok(v) = semver::Version::parse(v_str) {
                self.orchestration_registry
                    .resolve_handler_exact(&workitem_reader.orchestration_name, &v)
                    .map(|h| (v, h))
            } else {
                None
            }
        } else {
            // Resolve using policy (returns both version and handler)
            self.orchestration_registry
                .resolve_handler(&workitem_reader.orchestration_name)
        };

        let (resolved_version, handler) = match resolved_handler {
            Some((v, h)) => (v, h),
            None => {
                // Not found in registry - fail with unregistered error
                if history_mgr.is_empty() {
                    history_mgr.append(Event::with_event_id(
                        crate::INITIAL_EVENT_ID,
                        instance,
                        execution_id,
                        None,
                        EventKind::OrchestrationStarted {
                            name: workitem_reader.orchestration_name.clone(),
                            version: "0.0.0".to_string(), // Placeholder version for unregistered
                            input: workitem_reader.input.clone(),
                            parent_instance: workitem_reader.parent_instance.clone(),
                            parent_id: workitem_reader.parent_id,
                        },
                    ));

                    history_mgr.append_failed(crate::ErrorDetails::Configuration {
                        kind: crate::ConfigErrorKind::UnregisteredOrchestration,
                        resource: workitem_reader.orchestration_name.clone(),
                        message: None,
                    });
                    self.record_orchestration_configuration_error();
                }
                return (worker_items, orchestrator_items);
            }
        };

        // Create started event if this is a new instance
        if history_mgr.is_empty() {
            history_mgr.append(Event::with_event_id(
                1, // First event always has event_id=1
                instance,
                execution_id,
                None,
                EventKind::OrchestrationStarted {
                    name: workitem_reader.orchestration_name.clone(),
                    version: resolved_version.to_string(),
                    input: workitem_reader.input.clone(),
                    parent_instance: workitem_reader.parent_instance.clone(),
                    parent_id: workitem_reader.parent_id,
                },
            ));
        }

        // Run the atomic execution to get all changes, passing the resolved handler
        let (_exec_history_delta, exec_worker_items, exec_orchestrator_items, _result) = Arc::clone(self)
            .run_single_execution_atomic(instance, history_mgr, workitem_reader, execution_id, worker_id, handler)
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
                        drop(self.history_store.abandon_orchestration_item(
                            lock_token,
                            Some(std::time::Duration::from_millis(50)),
                            false,
                        ));
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Fail an orchestration as a poison message.
    ///
    /// This is called when the attempt_count exceeds max_attempts, indicating the message
    /// repeatedly fails to process (crash/abandon cycle).
    async fn fail_orchestration_as_poison(
        self: &Arc<Self>,
        item: &crate::providers::OrchestrationItem,
        lock_token: &str,
        attempt_count: u32,
    ) {
        let error = crate::ErrorDetails::Poison {
            attempt_count,
            max_attempts: self.options.max_attempts,
            message_type: crate::PoisonMessageType::Orchestration {
                instance: item.instance.clone(),
                execution_id: item.execution_id,
            },
            message: serde_json::to_string(&item.messages).unwrap_or_default(),
        };

        // Create failure event and commit
        let mut history_mgr = HistoryManager::from_history(&item.history);

        // Track parent info for sub-orchestration failure notification
        let mut parent_link: Option<(String, u64)> = None;

        // If history is empty, we need to create an OrchestrationStarted event first
        if history_mgr.is_empty() {
            // Try to extract orchestration name from work items
            let (orchestration_name, input, parent_instance, parent_id) = item
                .messages
                .iter()
                .find_map(|msg| match msg {
                    WorkItem::StartOrchestration {
                        orchestration,
                        input,
                        parent_instance,
                        parent_id,
                        ..
                    } => Some((
                        orchestration.clone(),
                        input.clone(),
                        parent_instance.clone(),
                        *parent_id,
                    )),
                    WorkItem::ContinueAsNew {
                        orchestration, input, ..
                    } => Some((orchestration.clone(), input.clone(), None, None)),
                    _ => None,
                })
                .unwrap_or_else(|| (item.orchestration_name.clone(), String::new(), None, None));

            // Save parent link for notification
            if let (Some(pi), Some(pid)) = (&parent_instance, parent_id) {
                parent_link = Some((pi.clone(), pid));
            }

            history_mgr.append(Event::with_event_id(
                crate::INITIAL_EVENT_ID,
                &item.instance,
                item.execution_id,
                None,
                EventKind::OrchestrationStarted {
                    name: orchestration_name,
                    version: item.version.clone(),
                    input,
                    parent_instance,
                    parent_id,
                },
            ));
        } else {
            // Check existing history for parent link
            for event in &item.history {
                if let EventKind::OrchestrationStarted {
                    parent_instance: Some(pi),
                    parent_id: Some(pid),
                    ..
                } = &event.kind
                {
                    parent_link = Some((pi.clone(), *pid));
                    break;
                }
            }
        }

        history_mgr.append_failed(error.clone());

        let metadata = ExecutionMetadata {
            status: Some("Failed".to_string()),
            output: Some(error.display_message()),
            orchestration_name: Some(item.orchestration_name.clone()),
            orchestration_version: Some(item.version.clone()),
        };

        // If this is a sub-orchestration, notify parent of failure
        let orchestrator_items = if let Some((parent_instance, parent_id)) = parent_link {
            tracing::debug!(
                target = "duroxide::runtime::execution",
                instance = %item.instance,
                parent_instance = %parent_instance,
                parent_id = %parent_id,
                "Enqueue SubOrchFailed to parent (poison)"
            );
            let parent_execution_id = self.get_execution_id_for_instance(&parent_instance, None).await;
            vec![WorkItem::SubOrchFailed {
                parent_instance,
                parent_execution_id,
                parent_id,
                details: error.clone(),
            }]
        } else {
            vec![]
        };

        let _ = self
            .ack_orchestration_with_changes(
                lock_token,
                item.execution_id,
                history_mgr.delta().to_vec(),
                vec![],
                orchestrator_items,
                metadata,
            )
            .await;

        // Record metrics for poison detection
        self.record_orchestration_poison();
    }
}
