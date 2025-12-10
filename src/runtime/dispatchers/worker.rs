//! Worker (activity) dispatcher implementation for Runtime
//!
//! This module contains the worker dispatcher logic that:
//! - Spawns concurrent activity workers
//! - Fetches and executes activity work items
//! - Handles activity completion and failure atomically

use crate::providers::WorkItem;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::task::JoinHandle;
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
                        let long_poll_timeout = rt.options.dispatcher_long_poll_timeout;
                        let start_time = std::time::Instant::now();
                        let mut work_found = false;

                        // Determine poll timeout based on provider capabilities
                        let poll_timeout = if rt.history_store.supports_long_polling() {
                            Some(long_poll_timeout)
                        } else {
                            None
                        };

                        let fetch_result = rt
                            .history_store
                            .fetch_work_item(rt.options.worker_lock_timeout, poll_timeout)
                            .await;

                        match fetch_result {
                            Ok(Some((item, token))) => {
                                work_found = true;
                                match item {
                                WorkItem::ActivityExecute {
                                    instance,
                                    execution_id,
                                    id,
                                    name,
                                    input,
                                } => {
                                    // Spawn lock renewal task for this activity
                                    let renewal_handle = spawn_lock_renewal_task(
                                        Arc::clone(&rt.history_store),
                                        token.clone(),
                                        rt.options.worker_lock_timeout,
                                        rt.options.worker_lock_renewal_buffer,
                                        Arc::clone(&shutdown),
                                    );

                                    let descriptor = rt.get_orchestration_descriptor(&instance).await;
                                    let (orch_name, orch_version) = if let Some(desc) = descriptor {
                                        (desc.name, desc.version)
                                    } else {
                                        ("unknown".to_string(), "unknown".to_string())
                                    };
                                    let activity_ctx = crate::ActivityContext::new(
                                        instance.clone(),
                                        execution_id,
                                        orch_name,
                                        orch_version,
                                        name.clone(),
                                        id,
                                        worker_id.clone(),
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
                                    }

                                    let (ack_result, outcome) = if let Some((_version, handler)) =
                                        activities.resolve_handler(&name)
                                    {
                                        match handler.invoke(activity_ctx, input).await {
                                            Ok(result) => {
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

                                                // Record activity success metrics
                                                rt.record_activity_execution(&name, "success", duration_seconds, 0);

                                                (
                                                    rt.history_store
                                                        .ack_work_item(
                                                            &token,
                                                            WorkItem::ActivityCompleted {
                                                                instance: instance.clone(),
                                                                execution_id,
                                                                id,
                                                                result,
                                                            },
                                                        )
                                                        .await,
                                                    ActivityOutcome::Success,
                                                )
                                            }
                                            Err(error) => {
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

                                                // Record activity app error metrics
                                                rt.record_activity_execution(&name, "app_error", duration_seconds, 0);

                                                (
                                                    rt.history_store
                                                        .ack_work_item(
                                                            &token,
                                                            WorkItem::ActivityFailed {
                                                                instance: instance.clone(),
                                                                execution_id,
                                                                id,
                                                                details: crate::ErrorDetails::Application {
                                                                    kind: crate::AppErrorKind::ActivityFailed,
                                                                    message: error,
                                                                    retryable: false,
                                                                },
                                                            },
                                                        )
                                                        .await,
                                                    ActivityOutcome::AppError,
                                                )
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
                                                    WorkItem::ActivityFailed {
                                                        instance: instance.clone(),
                                                        execution_id,
                                                        id,
                                                        details: crate::ErrorDetails::Configuration {
                                                            kind: crate::ConfigErrorKind::UnregisteredActivity,
                                                            resource: name.clone(),
                                                            message: None,
                                                        },
                                                    },
                                                )
                                                .await,
                                            ActivityOutcome::ConfigError,
                                        )
                                    };

                                    // Stop lock renewal task now that activity is complete
                                    renewal_handle.abort();

                                    match ack_result {
                                        Ok(()) => match outcome {
                                            ActivityOutcome::Success => rt.record_activity_success(),
                                            ActivityOutcome::AppError => rt.record_activity_app_error(),
                                            ActivityOutcome::ConfigError => rt.record_activity_config_error(),
                                        },
                                        Err(e) => {
                                            warn!(
                                                instance = %instance,
                                                execution_id,
                                                id,
                                                worker_id = %worker_id,
                                                error = %e,
                                                "worker: atomic ack failed"
                                            );
                                            rt.record_activity_infra_error();
                                        }
                                    }
                                }
                                other => {
                                    error!(?other, "unexpected WorkItem in Worker dispatcher; state corruption");
                                    panic!("unexpected WorkItem in Worker dispatcher");
                                }
                            }
                        },
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

/// Spawn a background task to renew the lock for an in-flight activity.
///
/// # Parameters
/// - `store`: Provider to call renew_work_item_lock on
/// - `token`: Lock token to renew
/// - `lock_timeout_secs`: Original lock timeout value
/// - `buffer_secs`: Buffer time before expiration (only used if timeout >= 15s)
/// - `shutdown`: Shared shutdown flag
///
/// # Behavior
/// - Calculates renewal interval based on timeout and buffer
/// - Renews lock periodically until activity completes or error occurs
/// - Stops automatically on shutdown
/// - Logs renewal attempts and failures
///
/// # Returns
/// JoinHandle that can be aborted when activity completes
fn spawn_lock_renewal_task(
    store: Arc<dyn crate::providers::Provider>,
    token: String,
    lock_timeout: Duration,
    buffer: Duration,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    let renewal_interval = calculate_renewal_interval(lock_timeout, buffer);

    tracing::debug!(
        target: "duroxide::runtime::worker",
        lock_token = %token,
        lock_timeout_secs = %lock_timeout.as_secs(),
        buffer_secs = %buffer.as_secs(),
        renewal_interval_secs = %renewal_interval.as_secs(),
        "Spawning lock renewal task"
    );

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(renewal_interval);
        interval.tick().await; // Skip first immediate tick

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::debug!(
                            target: "duroxide::runtime::worker",
                            lock_token = %token,
                            "Lock renewal task stopping due to shutdown"
                        );
                        break;
                    }

                    tracing::trace!(
                        target: "duroxide::runtime::worker",
                        lock_token = %token,
                        extend_secs = %lock_timeout.as_secs(),
                        "Renewing work item lock"
                    );

                    match store.renew_work_item_lock(&token, lock_timeout).await {
                        Ok(()) => {
                            tracing::trace!(
                                target: "duroxide::runtime::worker",
                                lock_token = %token,
                                "Lock renewed successfully"
                            );
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
            "Lock renewal task stopped"
        );
    })
}
