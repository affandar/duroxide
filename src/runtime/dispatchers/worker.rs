//! Worker (activity) dispatcher implementation for Runtime
//!
//! This module contains the worker dispatcher logic that:
//! - Spawns concurrent activity workers
//! - Fetches and executes activity work items
//! - Handles activity completion and failure atomically
//! - Supports cooperative activity cancellation

use crate::providers::{ExecutionState, WorkItem};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use super::super::{Runtime, registry};

impl Runtime {
    /// Start the worker dispatcher with N concurrent workers for executing activities
    pub(in crate::runtime) fn start_work_dispatcher(
        self: Arc<Self>,
        activities: Arc<registry::ActivityRegistry>,
    ) -> JoinHandle<()> {
        // EXECUTION: spawns N concurrent worker dispatchers
        // Activities are independent work units that can run in parallel
        let concurrency = self.options.worker_concurrency;
        let shutdown = self.shutdown_flag.clone();

        tokio::spawn(async move {
            let mut worker_handles = Vec::new();

            for worker_idx in 0..concurrency {
                let rt = Arc::clone(&self);
                let activities = Arc::clone(&activities);
                let shutdown = Arc::clone(&shutdown);
                // Generate unique worker ID: work-{index}-{runtime_id}
                let worker_id = format!("work-{worker_idx}-{}", rt.runtime_id);
                let handle = tokio::spawn(async move {
                    // debug!("Worker dispatcher {} started", worker_id);
                    loop {
                        // Check shutdown flag before fetching
                        if shutdown.load(Ordering::Relaxed) {
                            // debug!("Worker dispatcher {} exiting", worker_id);
                            break;
                        }

                        let min_interval = rt.options.dispatcher_min_poll_interval;
                        let poll_timeout = rt.options.dispatcher_long_poll_timeout;
                        let start_time = std::time::Instant::now();
                        let mut work_found = false;

                        let fetch_result = rt
                            .history_store
                            .fetch_work_item(rt.options.worker_lock_timeout, poll_timeout)
                            .await;

                        match fetch_result {
                            Ok(Some((item, token, attempt_count, execution_state))) => {
                                work_found = true;

                                // Serialize item before destructuring for potential poison message
                                let item_serialized = serde_json::to_string(&item).unwrap_or_default();

                                match item {
                                    WorkItem::ActivityExecute {
                                        instance,
                                        execution_id,
                                        id,
                                        name,
                                        input,
                                    } => {
                                        // Skip activities whose orchestration is already terminal/missing
                                        match execution_state {
                                            ExecutionState::Terminal { ref status } => {
                                                tracing::debug!(
                                                    target: "duroxide::runtime",
                                                    instance = %instance,
                                                    execution_id = %execution_id,
                                                    activity_name = %name,
                                                    activity_id = %id,
                                                    status = %status,
                                                    "Skipping activity because orchestration is terminal"
                                                );

                                                if let Err(e) = rt.history_store.ack_work_item(&token, None).await {
                                                    tracing::warn!(
                                                        target: "duroxide::runtime",
                                                        instance = %instance,
                                                        activity_id = %id,
                                                        error = %e,
                                                        "Failed to ack skipped activity work item (terminal)"
                                                    );
                                                }
                                                continue;
                                            }
                                            ExecutionState::Missing => {
                                                tracing::debug!(
                                                    target: "duroxide::runtime",
                                                    instance = %instance,
                                                    execution_id = %execution_id,
                                                    activity_name = %name,
                                                    activity_id = %id,
                                                    "Skipping activity because orchestration is missing"
                                                );

                                                if let Err(e) = rt.history_store.ack_work_item(&token, None).await {
                                                    tracing::warn!(
                                                        target: "duroxide::runtime",
                                                        instance = %instance,
                                                        activity_id = %id,
                                                        error = %e,
                                                        "Failed to ack skipped activity work item (missing)"
                                                    );
                                                }
                                                continue;
                                            }
                                            ExecutionState::Running => {
                                                // Proceed
                                            }
                                        }
                                        // Check for poison message - message has been fetched too many times
                                        if attempt_count > rt.options.max_attempts {
                                            warn!(
                                                instance = %instance,
                                                activity_name = %name,
                                                activity_id = id,
                                                attempt_count = attempt_count,
                                                max_attempts = rt.options.max_attempts,
                                                "Activity message exceeded max attempts, marking as poison"
                                            );

                                            let error = crate::ErrorDetails::Poison {
                                                attempt_count,
                                                max_attempts: rt.options.max_attempts,
                                                message_type: crate::PoisonMessageType::Activity {
                                                    instance: instance.clone(),
                                                    execution_id,
                                                    activity_name: name.clone(),
                                                    activity_id: id,
                                                },
                                                message: item_serialized,
                                            };

                                            // Ack with failure
                                            let _ = rt
                                                .history_store
                                                .ack_work_item(
                                                    &token,
                                                    Some(WorkItem::ActivityFailed {
                                                        instance,
                                                        execution_id,
                                                        id,
                                                        details: error,
                                                    }),
                                                )
                                                .await;

                                            rt.record_activity_poison();
                                            continue;
                                        }
                                        // Create cancellation token for this activity
                                        let cancellation_token = CancellationToken::new();

                                        // Spawn activity manager (lock renewal + cancellation detection)
                                        let manager_handle = spawn_activity_manager(
                                            Arc::clone(&rt.history_store),
                                            token.clone(),
                                            rt.options.worker_lock_timeout,
                                            rt.options.worker_lock_renewal_buffer,
                                            rt.options.activity_cancellation_grace_period,
                                            Arc::clone(&shutdown),
                                            cancellation_token.clone(),
                                        );

                                        let descriptor = rt.get_orchestration_descriptor(&instance).await;
                                        let (orch_name, orch_version) = if let Some(desc) = descriptor {
                                            (desc.name, desc.version)
                                        } else {
                                            ("unknown".to_string(), "unknown".to_string())
                                        };
                                        let activity_ctx = crate::ActivityContext::new_with_cancellation(
                                            instance.clone(),
                                            execution_id,
                                            orch_name,
                                            orch_version,
                                            name.clone(),
                                            id,
                                            worker_id.clone(),
                                            cancellation_token.clone(),
                                        );

                                        // Log activity start with all correlation fields
                                        tracing::debug!(
                                            target: "duroxide::runtime",
                                            instance_id = %instance,
                                            execution_id = %execution_id,
                                            activity_name = %name,
                                            activity_id = %id,
                                            worker_id = %worker_id,
                                            "Activity started"
                                        );
                                        let start_time = std::time::Instant::now();

                                        // Execute activity and atomically ack with completion
                                        enum ActivityOutcome {
                                            Success,
                                            AppError,
                                            ConfigError,
                                            Cancelled, // Orchestration was cancelled, result dropped
                                        }

                                        let (ack_result, outcome) = if let Some((_version, handler)) =
                                            activities.resolve_handler(&name)
                                        {
                                            let mut activity_handle = tokio::spawn(async move {
                                                handler.invoke(activity_ctx, input).await
                                            });

                                            tokio::select! {
                                                joined = &mut activity_handle => {
                                                    manager_handle.abort();
                                                    match joined {
                                                        Ok(Ok(result)) => {
                                                            let duration_ms = start_time.elapsed().as_millis() as u64;
                                                            let duration_seconds = duration_ms as f64 / 1000.0;

                                                            tracing::debug!(
                                                                target: "duroxide::runtime",
                                                                instance_id = %instance,
                                                                execution_id = %execution_id,
                                                                activity_name = %name,
                                                                activity_id = %id,
                                                                worker_id = %worker_id,
                                                                outcome = "success",
                                                                duration_ms = %duration_ms,
                                                                result_size = %result.len(),
                                                                "Activity completed"
                                                            );

                                                            rt.record_activity_execution(&name, "success", duration_seconds, 0);

                                                            (
                                                                rt.history_store
                                                                    .ack_work_item(
                                                                        &token,
                                                                        Some(WorkItem::ActivityCompleted {
                                                                            instance: instance.clone(),
                                                                            execution_id,
                                                                            id,
                                                                            result,
                                                                        }),
                                                                    )
                                                                    .await,
                                                                ActivityOutcome::Success,
                                                            )
                                                        }
                                                        Ok(Err(error)) => {
                                                            let duration_ms = start_time.elapsed().as_millis() as u64;
                                                            let duration_seconds = duration_ms as f64 / 1000.0;

                                                            tracing::warn!(
                                                                target: "duroxide::runtime",
                                                                instance_id = %instance,
                                                                execution_id = %execution_id,
                                                                activity_name = %name,
                                                                activity_id = %id,
                                                                worker_id = %worker_id,
                                                                outcome = "app_error",
                                                                duration_ms = %duration_ms,
                                                                error = %error,
                                                                "Activity failed (application error)"
                                                            );

                                                            rt.record_activity_execution(&name, "app_error", duration_seconds, 0);

                                                            (
                                                                rt.history_store
                                                                    .ack_work_item(
                                                                        &token,
                                                                        Some(WorkItem::ActivityFailed {
                                                                            instance: instance.clone(),
                                                                            execution_id,
                                                                            id,
                                                                            details: crate::ErrorDetails::Application {
                                                                                kind: crate::AppErrorKind::ActivityFailed,
                                                                                message: error,
                                                                                retryable: false,
                                                                            },
                                                                        }),
                                                                    )
                                                                    .await,
                                                                ActivityOutcome::AppError,
                                                            )
                                                        }
                                                        Err(join_error) => {
                                                            let duration_ms = start_time.elapsed().as_millis() as u64;
                                                            let duration_seconds = duration_ms as f64 / 1000.0;

                                                            tracing::warn!(
                                                                target: "duroxide::runtime",
                                                                instance_id = %instance,
                                                                execution_id = %execution_id,
                                                                activity_name = %name,
                                                                activity_id = %id,
                                                                worker_id = %worker_id,
                                                                outcome = "app_error",
                                                                duration_ms = %duration_ms,
                                                                error = %join_error,
                                                                "Activity panicked"
                                                            );

                                                            rt.record_activity_execution(&name, "app_error", duration_seconds, 0);

                                                            (
                                                                rt.history_store
                                                                    .ack_work_item(
                                                                        &token,
                                                                        Some(WorkItem::ActivityFailed {
                                                                            instance: instance.clone(),
                                                                            execution_id,
                                                                            id,
                                                                            details: crate::ErrorDetails::Application {
                                                                                kind: crate::AppErrorKind::ActivityFailed,
                                                                                message: join_error.to_string(),
                                                                                retryable: false,
                                                                            },
                                                                        }),
                                                                    )
                                                                    .await,
                                                                ActivityOutcome::AppError,
                                                            )
                                                        }
                                                    }
                                                }
                                                _ = cancellation_token.cancelled() => {
                                                    manager_handle.abort();
                                                    tracing::info!(
                                                        target: "duroxide::runtime",
                                                        instance = %instance,
                                                        execution_id = %execution_id,
                                                        activity_name = %name,
                                                        activity_id = %id,
                                                        worker_id = %worker_id,
                                                        "Orchestration terminated, waiting for activity grace period"
                                                    );

                                                    let grace = rt.options.activity_cancellation_grace_period;
                                                    match tokio::time::timeout(grace, &mut activity_handle).await {
                                                        Ok(Ok(result)) => {
                                                            let ack_res = rt.history_store.ack_work_item(&token, None).await;
                                                            if let Err(e) = &ack_res {
                                                                tracing::warn!(
                                                                    target: "duroxide::runtime",
                                                                    instance = %instance,
                                                                    activity_id = %id,
                                                                    error = %e,
                                                                    "Failed to ack cancelled activity work item"
                                                                );
                                                            }

                                                            (
                                                                ack_res,
                                                                ActivityOutcome::Cancelled,
                                                            )
                                                        }
                                                        Ok(Err(error)) => {
                                                            let duration_ms = start_time.elapsed().as_millis() as u64;
                                                            let duration_seconds = duration_ms as f64 / 1000.0;

                                                            tracing::info!(
                                                                target: "duroxide::runtime",
                                                                instance_id = %instance,
                                                                execution_id = %execution_id,
                                                                activity_name = %name,
                                                                activity_id = %id,
                                                                worker_id = %worker_id,
                                                                duration_ms = %duration_ms,
                                                                error = %error,
                                                                "Activity failed after cancellation; dropping result"
                                                            );

                                                            rt.record_activity_execution(&name, "cancelled", duration_seconds, 0);

                                                            let ack_res = rt.history_store.ack_work_item(&token, None).await;
                                                            if let Err(e) = &ack_res {
                                                                tracing::warn!(
                                                                    target: "duroxide::runtime",
                                                                    instance = %instance,
                                                                    activity_id = %id,
                                                                    error = %e,
                                                                    "Failed to ack cancelled activity work item (failed)"
                                                                );
                                                            }

                                                            (
                                                                ack_res,
                                                                ActivityOutcome::Cancelled,
                                                            )
                                                        }
                                                        Err(_timeout) => {
                                                            tracing::warn!(
                                                                target: "duroxide::runtime",
                                                                instance_id = %instance,
                                                                execution_id = %execution_id,
                                                                activity_name = %name,
                                                                activity_id = %id,
                                                                worker_id = %worker_id,
                                                                grace_ms = %grace.as_millis(),
                                                                "Activity did not finish within cancellation grace period; aborting"
                                                            );

                                                            activity_handle.abort();
                                                            let _ = activity_handle.await;

                                                            let ack_res = rt.history_store.ack_work_item(&token, None).await;
                                                            if let Err(e) = &ack_res {
                                                                tracing::warn!(
                                                                    target: "duroxide::runtime",
                                                                    instance = %instance,
                                                                    activity_id = %id,
                                                                    error = %e,
                                                                    "Failed to ack cancelled activity work item (aborted)"
                                                                );
                                                            }

                                                            (
                                                                ack_res,
                                                                ActivityOutcome::Cancelled,
                                                            )
                                                        }
                                                    }
                                                }
                                            }
                                        } else {
                                            let duration_ms = start_time.elapsed().as_millis() as u64;
                                            let duration_seconds = duration_ms as f64 / 1000.0;
                                            tracing::error!(
                                                target: "duroxide::runtime",
                                                instance_id = %instance,
                                                execution_id = %execution_id,
                                                activity_name = %name,
                                                activity_id = %id,
                                                worker_id = %worker_id,
                                                outcome = "system_error",
                                                error_type = "unregistered",
                                                duration_ms = %duration_ms,
                                                "Activity failed (unregistered)"
                                            );

                                            // Record activity config error metrics
                                            rt.record_activity_execution(&name, "config_error", duration_seconds, 0);

                                            (
                                                rt.history_store
                                                    .ack_work_item(
                                                        &token,
                                                        Some(WorkItem::ActivityFailed {
                                                            instance: instance.clone(),
                                                            execution_id,
                                                            id,
                                                            details: crate::ErrorDetails::Configuration {
                                                                kind: crate::ConfigErrorKind::UnregisteredActivity,
                                                                resource: name.clone(),
                                                                message: None,
                                                            },
                                                        }),
                                                    )
                                                    .await,
                                                ActivityOutcome::ConfigError,
                                            )
                                        };

                                        // Stop activity manager now that activity is complete
                                        manager_handle.abort();

                                        match ack_result {
                                            Ok(()) => match outcome {
                                                ActivityOutcome::Success => rt.record_activity_success(),
                                                ActivityOutcome::AppError => rt.record_activity_app_error(),
                                                ActivityOutcome::ConfigError => rt.record_activity_config_error(),
                                                ActivityOutcome::Cancelled => {
                                                    // Activity completed but result was dropped due to cancellation
                                                    // No additional recording needed - already recorded above
                                                }
                                            },
                                            Err(e) => {
                                                warn!(
                                                    instance = %instance,
                                                    execution_id,
                                                    id,
                                                    worker_id = %worker_id,
                                                    error = %e,
                                                    "worker: atomic ack failed, abandoning work item"
                                                );
                                                // Abandon work item so it can be retried
                                                let _ = rt
                                                    .history_store
                                                    .abandon_work_item(&token, Some(Duration::from_millis(100)), false)
                                                    .await;
                                                rt.record_activity_infra_error();
                                            }
                                        }
                                    }
                                    other => {
                                        error!(?other, "unexpected WorkItem in Worker dispatcher; state corruption");
                                        panic!("unexpected WorkItem in Worker dispatcher");
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Error fetching work item: {:?}", e);
                                // Backoff on error
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                continue;
                            }
                            Ok(None) => {
                                // No work available
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
                                // Waited long enough, yield to prevent starvation
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

            // debug!("Work dispatcher exited");
        })
    }
}

/// Calculate the renewal interval based on lock timeout and buffer settings.
///
/// # Logic
/// - If timeout ≥ 15s: renew at (timeout - buffer)
/// - If timeout < 15s: renew at 0.5 × timeout (buffer ignored)
///
/// # Returns
/// Renewal interval as a [`Duration`]
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

/// Spawn a background task to manage an in-flight activity.
///
/// This task handles:
/// 1. Periodic lock renewal to keep the activity work item locked
/// 2. Detection of orchestration cancellation via ExecutionState
/// 3. Cooperative cancellation signaling via CancellationToken
///
/// # Parameters
/// - `store`: Provider to call renew_work_item_lock on
/// - `token`: Lock token to renew
/// - `lock_timeout`: Original lock timeout value
/// - `buffer`: Buffer time before expiration (only used if timeout >= 15s)
/// - `grace_period`: Time to wait for activity to respond to cancellation before giving up
/// - `shutdown`: Shared shutdown flag
/// - `cancellation_token`: Token to signal cancellation to the activity
///
/// # Behavior
/// - Calculates renewal interval based on timeout and buffer
/// - Renews lock periodically until activity completes or error occurs
/// - When ExecutionState is Terminal or Missing, triggers cancellation_token
/// - Stops automatically on shutdown
/// - Logs renewal attempts and failures
///
/// # Returns
/// JoinHandle that can be aborted when activity completes
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
        buffer_secs = %buffer.as_secs(),
        renewal_interval_secs = %renewal_interval.as_secs(),
        grace_period_secs = %grace_period.as_secs(),
        "Spawning activity manager"
    );

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(renewal_interval);
        interval.tick().await; // Skip first immediate tick
        let mut cancellation_signaled = false;
        let mut grace_deadline: Option<tokio::time::Instant> = None;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::debug!(
                            target: "duroxide::runtime::worker",
                            lock_token = %token,
                            "Activity manager stopping due to shutdown"
                        );
                        break;
                    }

                    // Check if grace period has expired
                    if let Some(deadline) = grace_deadline {
                        if tokio::time::Instant::now() >= deadline {
                            tracing::warn!(
                                target: "duroxide::runtime::worker",
                                lock_token = %token,
                                grace_period_secs = %grace_period.as_secs(),
                                "Activity did not respond to cancellation within grace period"
                            );
                            // Activity didn't respond to cancellation - stop managing
                            // The activity task will be aborted when manager_handle.abort() is called
                            break;
                        }
                    }

                    match store.renew_work_item_lock(&token, lock_timeout).await {
                        Ok(execution_state) => {
                            match execution_state {
                                ExecutionState::Running => {
                                    tracing::trace!(
                                        target: "duroxide::runtime::worker",
                                        lock_token = %token,
                                        extend_secs = %lock_timeout.as_secs(),
                                        "Work item lock renewed"
                                    );
                                }
                                ExecutionState::Terminal { ref status } => {
                                    if !cancellation_signaled {
                                        tracing::info!(
                                            target: "duroxide::runtime::worker",
                                            lock_token = %token,
                                            status = %status,
                                            "Orchestration terminated, signaling activity cancellation"
                                        );
                                        cancellation_token.cancel();
                                        cancellation_signaled = true;
                                        grace_deadline = Some(tokio::time::Instant::now() + grace_period);
                                    }
                                    // Continue renewing lock during grace period
                                    // so the activity can complete gracefully
                                }
                                ExecutionState::Missing => {
                                    if !cancellation_signaled {
                                        tracing::info!(
                                            target: "duroxide::runtime::worker",
                                            lock_token = %token,
                                            "Orchestration missing, signaling activity cancellation"
                                        );
                                        cancellation_token.cancel();
                                        cancellation_signaled = true;
                                        grace_deadline = Some(tokio::time::Instant::now() + grace_period);
                                    }
                                    // Continue renewing lock during grace period
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                target: "duroxide::runtime::worker",
                                lock_token = %token,
                                error = %e,
                                "Failed to renew lock (may have been acked/abandoned)"
                            );
                            // Stop renewal - lock is gone or expired
                            break;
                        }
                    }
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
