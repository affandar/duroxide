//! Worker (activity) dispatcher implementation for Runtime
//!
//! This module contains the worker dispatcher logic that:
//! - Spawns concurrent activity workers
//! - Fetches and executes activity work items
//! - Handles activity completion and failure atomically
//! - Supports cooperative activity cancellation
//!
//! # Architecture
//!
//! The worker dispatcher spawns N concurrent worker tasks, each of which:
//! 1. Fetches activity work items from the provider
//! 2. Checks if the orchestration is still running (cancellation support)
//! 3. Spawns an activity manager task for lock renewal and cancellation detection
//! 4. Executes the activity handler
//! 5. Handles completion, failure, or cancellation outcomes
//!
//! # Cancellation Flow
//!
//! When an orchestration reaches a terminal state while an activity is running:
//! 1. The activity manager detects the terminal state during lock renewal
//! 2. It signals the activity's cancellation token
//! 3. The worker dispatcher waits up to the grace period for the activity to complete
//! 4. If the activity doesn't complete, it's aborted and the work item is dropped

use crate::providers::{ExecutionState, WorkItem};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use super::super::{Runtime, registry};

// ============================================================================
// Types
// ============================================================================

/// Outcome of activity execution, used for metrics and ack handling.
#[derive(Debug, Clone, Copy)]
enum ActivityOutcome {
    /// Activity completed successfully
    Success,
    /// Activity failed with an application error
    AppError,
    /// Activity failed due to configuration error (e.g., unregistered)
    ConfigError,
    /// Orchestration was cancelled, activity result dropped
    Cancelled,
}

/// Context for processing a single activity work item.
///
/// Groups together all the data needed to execute an activity,
/// reducing parameter count in helper functions.
struct ActivityWorkContext {
    /// The orchestration instance ID
    instance: String,
    /// The execution ID within the instance
    execution_id: u64,
    /// The activity's unique ID within the execution
    activity_id: u64,
    /// The activity handler name
    activity_name: String,
    /// Serialized input for the activity
    input: String,
    /// Lock token for the work item
    lock_token: String,
    /// Number of times this message has been fetched
    attempt_count: u32,
    /// Serialized work item (for poison message reporting)
    item_serialized: String,
    /// Worker ID for logging
    worker_id: String,
}

// ============================================================================
// Runtime Implementation
// ============================================================================

impl Runtime {
    /// Start the worker dispatcher with N concurrent workers for executing activities.
    ///
    /// Each worker runs in a loop, fetching and processing activity work items.
    /// Workers share the same activity registry and shutdown flag.
    pub(in crate::runtime) fn start_work_dispatcher(
        self: Arc<Self>,
        activities: Arc<registry::ActivityRegistry>,
    ) -> JoinHandle<()> {
        let concurrency = self.options.worker_concurrency;
        let shutdown = self.shutdown_flag.clone();

        tokio::spawn(async move {
            let mut worker_handles = Vec::with_capacity(concurrency);

            for worker_idx in 0..concurrency {
                let rt = Arc::clone(&self);
                let activities = Arc::clone(&activities);
                let shutdown = Arc::clone(&shutdown);
                let worker_id = format!("work-{worker_idx}-{}", rt.runtime_id);

                let handle = tokio::spawn(async move {
                    loop {
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }

                        let min_interval = rt.options.dispatcher_min_poll_interval;
                        let start_time = std::time::Instant::now();

                        let work_found = process_next_work_item(&rt, &activities, &shutdown, &worker_id).await;

                        // Enforce minimum polling interval to prevent hot loops
                        if !work_found {
                            enforce_min_poll_interval(start_time, min_interval, &shutdown).await;
                        }
                    }
                });
                worker_handles.push(handle);
            }

            for handle in worker_handles {
                let _ = handle.await;
            }
        })
    }
}

/// Process the next available work item from the queue.
/// Returns `true` if work was found and processed, `false` otherwise.
async fn process_next_work_item(
    rt: &Arc<Runtime>,
    activities: &Arc<registry::ActivityRegistry>,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
    worker_id: &str,
) -> bool {
    let fetch_result = rt
        .history_store
        .fetch_work_item(rt.options.worker_lock_timeout, rt.options.dispatcher_long_poll_timeout)
        .await;

    match fetch_result {
        Ok(Some((item, token, attempt_count, execution_state))) => {
            let item_serialized = serde_json::to_string(&item).unwrap_or_default();

            match item {
                WorkItem::ActivityExecute {
                    instance,
                    execution_id,
                    id,
                    name,
                    input,
                } => {
                    let ctx = ActivityWorkContext {
                        instance,
                        execution_id,
                        activity_id: id,
                        activity_name: name,
                        input,
                        lock_token: token,
                        attempt_count,
                        item_serialized,
                        worker_id: worker_id.to_string(),
                    };

                    // Skip activities for terminal/missing orchestrations
                    if should_skip_activity(&execution_state) {
                        skip_terminal_activity(rt, &ctx, &execution_state).await;
                    } else if ctx.attempt_count > rt.options.max_attempts {
                        // Handle poison messages
                        handle_poison_message(rt, &ctx).await;
                    } else {
                        // Execute activity with cancellation support
                        execute_activity(rt, activities, shutdown, ctx).await;
                    }
                }
                other => {
                    error!(?other, "unexpected WorkItem in Worker dispatcher; state corruption");
                    panic!("unexpected WorkItem in Worker dispatcher");
                }
            }
            true
        }
        Ok(None) => false,
        Err(e) => {
            warn!("Error fetching work item: {:?}", e);
            false
        }
    }
}

/// Enforce minimum polling interval to prevent hot loops.
async fn enforce_min_poll_interval(
    start_time: std::time::Instant,
    min_interval: Duration,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
) {
    let elapsed = start_time.elapsed();
    if elapsed < min_interval {
        let sleep_duration = min_interval - elapsed;
        if !shutdown.load(Ordering::Relaxed) {
            tokio::time::sleep(sleep_duration).await;
        }
    } else {
        tokio::task::yield_now().await;
    }
}

// ============================================================================
// Activity Processing
// ============================================================================

/// Check if an activity should be skipped based on orchestration state.
fn should_skip_activity(execution_state: &ExecutionState) -> bool {
    matches!(
        execution_state,
        ExecutionState::Terminal { .. } | ExecutionState::Missing
    )
}

/// Skip an activity because its orchestration is terminal or missing.
async fn skip_terminal_activity(rt: &Arc<Runtime>, ctx: &ActivityWorkContext, execution_state: &ExecutionState) {
    let (reason, status) = match execution_state {
        ExecutionState::Terminal { status } => ("terminal", Some(status.as_str())),
        ExecutionState::Missing => ("missing", None),
        ExecutionState::Running => unreachable!(),
    };

    tracing::debug!(
        target: "duroxide::runtime",
        instance = %ctx.instance,
        execution_id = %ctx.execution_id,
        activity_name = %ctx.activity_name,
        activity_id = %ctx.activity_id,
        status = ?status,
        "Skipping activity because orchestration is {}", reason
    );

    if let Err(e) = rt.history_store.ack_work_item(&ctx.lock_token, None).await {
        tracing::warn!(
            target: "duroxide::runtime",
            instance = %ctx.instance,
            activity_id = %ctx.activity_id,
            error = %e,
            "Failed to ack skipped activity work item ({})", reason
        );
    }
}

/// Handle a poison message (activity fetched too many times).
async fn handle_poison_message(rt: &Arc<Runtime>, ctx: &ActivityWorkContext) {
    warn!(
        instance = %ctx.instance,
        activity_name = %ctx.activity_name,
        activity_id = ctx.activity_id,
        attempt_count = ctx.attempt_count,
        max_attempts = rt.options.max_attempts,
        "Activity message exceeded max attempts, marking as poison"
    );

    let error = crate::ErrorDetails::Poison {
        attempt_count: ctx.attempt_count,
        max_attempts: rt.options.max_attempts,
        message_type: crate::PoisonMessageType::Activity {
            instance: ctx.instance.clone(),
            execution_id: ctx.execution_id,
            activity_name: ctx.activity_name.clone(),
            activity_id: ctx.activity_id,
        },
        message: ctx.item_serialized.clone(),
    };

    let _ = rt
        .history_store
        .ack_work_item(
            &ctx.lock_token,
            Some(WorkItem::ActivityFailed {
                instance: ctx.instance.clone(),
                execution_id: ctx.execution_id,
                id: ctx.activity_id,
                details: error,
            }),
        )
        .await;

    rt.record_activity_poison();
}

// ============================================================================
// Activity Execution
// ============================================================================

/// Execute an activity with full cancellation support.
async fn execute_activity(
    rt: &Arc<Runtime>,
    activities: &Arc<registry::ActivityRegistry>,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
    ctx: ActivityWorkContext,
) {
    let cancellation_token = CancellationToken::new();

    let manager_handle = spawn_activity_manager(
        Arc::clone(&rt.history_store),
        ctx.lock_token.clone(),
        rt.options.worker_lock_timeout,
        rt.options.worker_lock_renewal_buffer,
        rt.options.activity_cancellation_grace_period,
        Arc::clone(shutdown),
        cancellation_token.clone(),
    );

    let activity_ctx = build_activity_context(rt, &ctx, cancellation_token.clone()).await;

    tracing::debug!(
        target: "duroxide::runtime",
        instance_id = %ctx.instance,
        execution_id = %ctx.execution_id,
        activity_name = %ctx.activity_name,
        activity_id = %ctx.activity_id,
        worker_id = %ctx.worker_id,
        "Activity started"
    );

    let start_time = std::time::Instant::now();

    let (ack_result, outcome) = match activities.resolve_handler(&ctx.activity_name) {
        Some((_version, handler)) => {
            run_activity_with_cancellation(
                rt,
                &ctx,
                handler,
                activity_ctx,
                cancellation_token,
                manager_handle,
                start_time,
            )
            .await
        }
        None => {
            manager_handle.abort();
            handle_unregistered_activity(rt, &ctx, start_time).await
        }
    };

    handle_activity_outcome(rt, &ctx, ack_result, outcome).await;
}

/// Build the ActivityContext with orchestration metadata.
async fn build_activity_context(
    rt: &Arc<Runtime>,
    ctx: &ActivityWorkContext,
    cancellation_token: CancellationToken,
) -> crate::ActivityContext {
    let descriptor = rt.get_orchestration_descriptor(&ctx.instance).await;
    let (orch_name, orch_version) = descriptor
        .map(|d| (d.name, d.version))
        .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));

    crate::ActivityContext::new_with_cancellation(
        ctx.instance.clone(),
        ctx.execution_id,
        orch_name,
        orch_version,
        ctx.activity_name.clone(),
        ctx.activity_id,
        ctx.worker_id.clone(),
        cancellation_token,
    )
}

/// Run an activity with cancellation support using `tokio::select!`.
async fn run_activity_with_cancellation(
    rt: &Arc<Runtime>,
    ctx: &ActivityWorkContext,
    handler: Arc<dyn crate::runtime::ActivityHandler>,
    activity_ctx: crate::ActivityContext,
    cancellation_token: CancellationToken,
    manager_handle: JoinHandle<()>,
    start_time: std::time::Instant,
) -> (Result<(), crate::providers::ProviderError>, ActivityOutcome) {
    let input = ctx.input.clone();
    let mut activity_handle = tokio::spawn(async move { handler.invoke(activity_ctx, input).await });

    tokio::select! {
        joined = &mut activity_handle => {
            manager_handle.abort();
            // Handle normal activity completion (success, error, or panic)
            match joined {
                Ok(Ok(result)) => handle_activity_success(rt, ctx, result, start_time).await,
                Ok(Err(error)) => handle_activity_error(rt, ctx, error, start_time).await,
                Err(join_error) => handle_activity_error(rt, ctx, join_error.to_string(), start_time).await,
            }
        }
        _ = cancellation_token.cancelled() => {
            manager_handle.abort();
            // Handle cancellation: wait grace period, then drop result
            let grace = rt.options.activity_cancellation_grace_period;

            tracing::info!(
                target: "duroxide::runtime",
                instance = %ctx.instance,
                execution_id = %ctx.execution_id,
                activity_name = %ctx.activity_name,
                activity_id = %ctx.activity_id,
                worker_id = %ctx.worker_id,
                grace_ms = %grace.as_millis(),
                "Orchestration terminated, waiting for activity grace period"
            );

            // Wait for activity to finish within grace period, or abort it
            let finished_in_time = tokio::time::timeout(grace, &mut activity_handle).await.is_ok();
            if !finished_in_time {
                tracing::debug!(
                    target: "duroxide::runtime",
                    instance_id = %ctx.instance,
                    activity_name = %ctx.activity_name,
                    activity_id = %ctx.activity_id,
                    "Activity did not finish within grace period; aborting"
                );
                activity_handle.abort();
                let _ = activity_handle.await;
            }

            // Record metrics and ack (drop result since orchestration is terminal)
            let duration_seconds = start_time.elapsed().as_secs_f64();
            rt.record_activity_execution(&ctx.activity_name, "cancelled", duration_seconds, 0);

            let result = rt.history_store.ack_work_item(&ctx.lock_token, None).await;
            if let Err(e) = &result {
                tracing::warn!(
                    target: "duroxide::runtime",
                    instance = %ctx.instance,
                    activity_id = %ctx.activity_id,
                    error = %e,
                    "Failed to ack cancelled activity work item"
                );
            }
            (result, ActivityOutcome::Cancelled)
        }
    }
}

// ============================================================================
// Activity Completion Handlers
// ============================================================================

/// Handle successful activity completion.
async fn handle_activity_success(
    rt: &Arc<Runtime>,
    ctx: &ActivityWorkContext,
    result: String,
    start_time: std::time::Instant,
) -> (Result<(), crate::providers::ProviderError>, ActivityOutcome) {
    let duration_ms = start_time.elapsed().as_millis() as u64;
    let duration_seconds = duration_ms as f64 / 1000.0;

    tracing::debug!(
        target: "duroxide::runtime",
        instance_id = %ctx.instance,
        execution_id = %ctx.execution_id,
        activity_name = %ctx.activity_name,
        activity_id = %ctx.activity_id,
        worker_id = %ctx.worker_id,
        outcome = "success",
        duration_ms = %duration_ms,
        result_size = %result.len(),
        "Activity completed"
    );

    rt.record_activity_execution(&ctx.activity_name, "success", duration_seconds, 0);

    let ack_result = rt
        .history_store
        .ack_work_item(
            &ctx.lock_token,
            Some(WorkItem::ActivityCompleted {
                instance: ctx.instance.clone(),
                execution_id: ctx.execution_id,
                id: ctx.activity_id,
                result,
            }),
        )
        .await;

    (ack_result, ActivityOutcome::Success)
}

/// Handle activity application error.
async fn handle_activity_error(
    rt: &Arc<Runtime>,
    ctx: &ActivityWorkContext,
    error: String,
    start_time: std::time::Instant,
) -> (Result<(), crate::providers::ProviderError>, ActivityOutcome) {
    let duration_ms = start_time.elapsed().as_millis() as u64;
    let duration_seconds = duration_ms as f64 / 1000.0;

    tracing::warn!(
        target: "duroxide::runtime",
        instance_id = %ctx.instance,
        execution_id = %ctx.execution_id,
        activity_name = %ctx.activity_name,
        activity_id = %ctx.activity_id,
        worker_id = %ctx.worker_id,
        outcome = "app_error",
        duration_ms = %duration_ms,
        error = %error,
        "Activity failed (application error)"
    );

    rt.record_activity_execution(&ctx.activity_name, "app_error", duration_seconds, 0);

    let ack_result = rt
        .history_store
        .ack_work_item(
            &ctx.lock_token,
            Some(WorkItem::ActivityFailed {
                instance: ctx.instance.clone(),
                execution_id: ctx.execution_id,
                id: ctx.activity_id,
                details: crate::ErrorDetails::Application {
                    kind: crate::AppErrorKind::ActivityFailed,
                    message: error,
                    retryable: false,
                },
            }),
        )
        .await;

    (ack_result, ActivityOutcome::AppError)
}

/// Handle unregistered activity (configuration error).
async fn handle_unregistered_activity(
    rt: &Arc<Runtime>,
    ctx: &ActivityWorkContext,
    start_time: std::time::Instant,
) -> (Result<(), crate::providers::ProviderError>, ActivityOutcome) {
    let duration_ms = start_time.elapsed().as_millis() as u64;
    let duration_seconds = duration_ms as f64 / 1000.0;

    tracing::error!(
        target: "duroxide::runtime",
        instance_id = %ctx.instance,
        execution_id = %ctx.execution_id,
        activity_name = %ctx.activity_name,
        activity_id = %ctx.activity_id,
        worker_id = %ctx.worker_id,
        outcome = "system_error",
        error_type = "unregistered",
        duration_ms = %duration_ms,
        "Activity failed (unregistered)"
    );

    rt.record_activity_execution(&ctx.activity_name, "config_error", duration_seconds, 0);

    let ack_result = rt
        .history_store
        .ack_work_item(
            &ctx.lock_token,
            Some(WorkItem::ActivityFailed {
                instance: ctx.instance.clone(),
                execution_id: ctx.execution_id,
                id: ctx.activity_id,
                details: crate::ErrorDetails::Configuration {
                    kind: crate::ConfigErrorKind::UnregisteredActivity,
                    resource: ctx.activity_name.clone(),
                    message: None,
                },
            }),
        )
        .await;

    (ack_result, ActivityOutcome::ConfigError)
}

// ============================================================================
// Outcome Handling
// ============================================================================

/// Handle the final outcome of activity execution.
async fn handle_activity_outcome(
    rt: &Arc<Runtime>,
    ctx: &ActivityWorkContext,
    ack_result: Result<(), crate::providers::ProviderError>,
    outcome: ActivityOutcome,
) {
    match ack_result {
        Ok(()) => match outcome {
            ActivityOutcome::Success => rt.record_activity_success(),
            ActivityOutcome::AppError => rt.record_activity_app_error(),
            ActivityOutcome::ConfigError => rt.record_activity_config_error(),
            ActivityOutcome::Cancelled => {}
        },
        Err(e) => {
            warn!(
                instance = %ctx.instance,
                execution_id = ctx.execution_id,
                activity_id = ctx.activity_id,
                worker_id = %ctx.worker_id,
                error = %e,
                "worker: atomic ack failed, abandoning work item"
            );
            let _ = rt
                .history_store
                .abandon_work_item(&ctx.lock_token, Some(Duration::from_millis(100)), false)
                .await;
            rt.record_activity_infra_error();
        }
    }
}

// ============================================================================
// Activity Manager (Lock Renewal + Cancellation Detection)
// ============================================================================

/// Calculate the renewal interval based on lock timeout and buffer settings.
///
/// - If timeout >= 15s: renew at `(timeout - buffer)`, minimum 1s
/// - If timeout < 15s: renew at `0.5 * timeout`, minimum 1s
fn calculate_renewal_interval(lock_timeout: Duration, buffer: Duration) -> Duration {
    if lock_timeout >= Duration::from_secs(15) {
        let buffer = buffer.min(lock_timeout);
        lock_timeout
            .checked_sub(buffer)
            .unwrap_or(Duration::from_secs(1))
            .max(Duration::from_secs(1))
    } else {
        let half = (lock_timeout.as_secs_f64() * 0.5).ceil().max(1.0);
        Duration::from_secs_f64(half)
    }
}

/// Spawn a background task to manage an in-flight activity.
///
/// Handles: lock renewal, cancellation detection, and cancellation signaling.
fn spawn_activity_manager(
    store: Arc<dyn crate::providers::Provider>,
    token: String,
    lock_timeout: Duration,
    buffer: Duration,
    grace_period: Duration,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    cancellation_token: CancellationToken,
) -> JoinHandle<()> {
    let renewal_interval = calculate_renewal_interval(lock_timeout, buffer);

    tracing::debug!(
        target: "duroxide::runtime::worker",
        lock_token = %token,
        lock_timeout_secs = %lock_timeout.as_secs(),
        renewal_interval_secs = %renewal_interval.as_secs(),
        grace_period_secs = %grace_period.as_secs(),
        "Spawning activity manager"
    );

    // Note: grace_period is passed but not used here - the grace period waiting
    // is handled by handle_activity_cancellation() after this task signals cancellation.
    // This task's only jobs are: (1) renew locks, (2) detect terminal state and signal.
    let _ = grace_period;

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(renewal_interval);
        interval.tick().await; // Skip first immediate tick

        loop {
            interval.tick().await;

            if shutdown.load(Ordering::Relaxed) {
                tracing::debug!(
                    target: "duroxide::runtime::worker",
                    lock_token = %token,
                    "Activity manager stopping due to shutdown"
                );
                break;
            }

            match store.renew_work_item_lock(&token, lock_timeout).await {
                Ok(ExecutionState::Running) => {
                    tracing::trace!(
                        target: "duroxide::runtime::worker",
                        lock_token = %token,
                        extend_secs = %lock_timeout.as_secs(),
                        "Work item lock renewed"
                    );
                }
                Ok(ExecutionState::Terminal { ref status }) => {
                    tracing::info!(
                        target: "duroxide::runtime::worker",
                        lock_token = %token,
                        status = %status,
                        "Orchestration terminated, signaling activity cancellation"
                    );
                    cancellation_token.cancel();
                    // Our job is done - the select! in run_activity_with_cancellation
                    // will wake up and handle the grace period waiting
                    break;
                }
                Ok(ExecutionState::Missing) => {
                    tracing::info!(
                        target: "duroxide::runtime::worker",
                        lock_token = %token,
                        "Orchestration missing, signaling activity cancellation"
                    );
                    cancellation_token.cancel();
                    break;
                }
                Err(e) => {
                    tracing::debug!(
                        target: "duroxide::runtime::worker",
                        lock_token = %token,
                        error = %e,
                        "Failed to renew lock (may have been acked/abandoned)"
                    );
                    break;
                }
            }
        }

        tracing::debug!(
            target: "duroxide::runtime::worker",
            lock_token = %token,
            "Activity manager stopped"
        );
    })
}
