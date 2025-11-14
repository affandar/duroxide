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

            for _worker_idx in 0..concurrency {
                let rt = self.clone();
                let activities = activities.clone();
                let shutdown = shutdown.clone();
                // Generate unique worker ID with GUID suffix
                let worker_id = format!("work-{}", Self::generate_worker_suffix());
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
