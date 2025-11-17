//! Worker (activity) dispatcher implementation for Runtime
//!
//! This module contains the worker dispatcher logic that:
//! - Spawns concurrent activity workers
//! - Fetches and executes activity work items
//! - Handles activity completion and failure atomically

use crate::providers::WorkItem;
use std::sync::Arc;
use std::sync::atomic::Ordering;
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
                let rt = self.clone();
                let activities = activities.clone();
                let shutdown = shutdown.clone();
                // Generate unique worker ID: work-{index}-{runtime_id}
                let worker_id = format!("work-{}-{}", worker_idx, rt.runtime_id);
                let handle = tokio::spawn(async move {
                    // debug!("Worker dispatcher {} started", worker_id);
                    loop {
                        // Check shutdown flag before fetching
                        if shutdown.load(Ordering::Relaxed) {
                            // debug!("Worker dispatcher {} exiting", worker_id);
                            break;
                        }

                        if let Some((item, token)) = rt
                            .history_store
                            .fetch_work_item(rt.options.worker_lock_timeout_secs)
                            .await
                        {
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
                                        rt.history_store.clone(),
                                        token.clone(),
                                        rt.options.worker_lock_timeout_secs,
                                        rt.options.worker_lock_renewal_buffer_secs,
                                        shutdown.clone(),
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

                                    let (ack_result, outcome) = if let Some(handler) = activities.get(&name) {
                                        match handler.invoke(activity_ctx, input).await {
                                            Ok(result) => {
                                                let duration_ms = start_time.elapsed().as_millis() as u64;
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

            // debug!("Work dispatcher exited");
        })
    }
}

/// Calculate the renewal interval based on lock timeout and buffer settings.
///
/// # Logic
/// - If timeout >= 15s: renew at (timeout - buffer_secs)
/// - If timeout < 15s: renew at 0.5 * timeout (buffer ignored)
///
/// # Returns
/// Renewal interval in seconds
fn calculate_renewal_interval(lock_timeout_secs: u64, buffer_secs: u64) -> u64 {
    if lock_timeout_secs >= 15 {
        lock_timeout_secs.saturating_sub(buffer_secs).max(1)
    } else {
        ((lock_timeout_secs as f64) * 0.5).ceil() as u64
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
    lock_timeout_secs: u64,
    buffer_secs: u64,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    let renewal_interval_secs = calculate_renewal_interval(lock_timeout_secs, buffer_secs);

    tracing::debug!(
        target: "duroxide::runtime::worker",
        lock_token = %token,
        lock_timeout_secs = %lock_timeout_secs,
        buffer_secs = %buffer_secs,
        renewal_interval_secs = %renewal_interval_secs,
        "Spawning lock renewal task"
    );

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(renewal_interval_secs));
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
                        extend_secs = %lock_timeout_secs,
                        "Renewing work item lock"
                    );

                    match store.renew_work_item_lock(&token, lock_timeout_secs).await {
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
