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

// Worker dispatcher uses Mutex locks - poison indicates a panic and should propagate
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::clone_on_ref_ptr)]
//! 5. Handles completion, failure, or cancellation outcomes
//!
//! # Cancellation Flow
//!
//! When an orchestration reaches a terminal state while an activity is running:
//! 1. The activity manager detects the terminal state during lock renewal
//! 2. It signals the activity's cancellation token
//! 3. The worker dispatcher waits up to the grace period for the activity to complete
//! 4. If the activity doesn't complete, it's aborted and the work item is dropped

use crate::providers::WorkItem;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Mutex;
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
    /// Session ID if this activity was scheduled on a session
    session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionLeaseKey {
    instance_id: String,
    session_id: String,
    worker_id: String,
}

struct SessionLeaseEntry {
    active_count: usize,
    session_lost_token: CancellationToken,
    renewal_handle: JoinHandle<()>,
    idle_release_handle: Option<JoinHandle<()>>,
}

#[derive(Clone, Default)]
struct SessionRenewalRegistry {
    inner: Arc<Mutex<HashMap<SessionLeaseKey, SessionLeaseEntry>>>,
}


impl SessionRenewalRegistry {
    fn new() -> Self {
        Self::default()
    }

    async fn owned_session_count(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// Release all owned session locks immediately (graceful shutdown).
    async fn release_all(&self, store: Arc<dyn crate::providers::Provider>) {
        let entries: Vec<(SessionLeaseKey, SessionLeaseEntry)> = {
            let mut map = self.inner.lock().await;
            map.drain().collect()
        };
        for (key, entry) in entries {
            entry.renewal_handle.abort();
            if let Some(idle_handle) = entry.idle_release_handle {
                idle_handle.abort();
            }
            let _ = store
                .release_session_lock(&key.instance_id, &key.session_id, &key.worker_id)
                .await;
        }
    }

    async fn acquire(
        &self,
        store: Arc<dyn crate::providers::Provider>,
        key: SessionLeaseKey,
        session_lock_duration: Duration,
        shutdown: Arc<std::sync::atomic::AtomicBool>,
    ) -> CancellationToken {
        let mut map = self.inner.lock().await;
        if let Some(existing) = map.get_mut(&key) {
            existing.active_count += 1;
            if let Some(idle_handle) = existing.idle_release_handle.take() {
                idle_handle.abort();
            }
            return existing.session_lost_token.clone();
        }

        let session_lost_token = CancellationToken::new();
        let renewal_handle = spawn_session_renewal(
            store,
            key.instance_id.clone(),
            key.session_id.clone(),
            key.worker_id.clone(),
            session_lock_duration,
            shutdown,
            session_lost_token.clone(),
        );

        map.insert(
            key,
            SessionLeaseEntry {
                active_count: 1,
                session_lost_token: session_lost_token.clone(),
                renewal_handle,
                idle_release_handle: None,
            },
        );

        session_lost_token
    }

    async fn release(
        &self,
        store: Arc<dyn crate::providers::Provider>,
        key: &SessionLeaseKey,
        session_idle_timeout: Option<Duration>,
    ) {
        let mut map = self.inner.lock().await;
        if let Some(existing) = map.get_mut(key) {
            if existing.active_count > 1 {
                existing.active_count -= 1;
                return;
            }

            existing.active_count = 0;

            if let Some(timeout) = session_idle_timeout {
                if existing.idle_release_handle.is_none() {
                    let registry = self.clone();
                    let key_clone = key.clone();
                    let idle_handle = tokio::spawn(async move {
                        tokio::time::sleep(timeout).await;
                        registry.try_release_after_idle(store, key_clone).await;
                    });
                    existing.idle_release_handle = Some(idle_handle);
                }
                return;
            }

            // No idle timeout configured: keep the session lease alive indefinitely.
            return;
        }
    }

    async fn try_release_after_idle(&self, store: Arc<dyn crate::providers::Provider>, key: SessionLeaseKey) {
        let maybe_entry = {
            let mut map = self.inner.lock().await;
            let should_release = map.get(&key).map(|entry| entry.active_count == 0).unwrap_or(false);

            if should_release { map.remove(&key) } else { None }
        };

        if let Some(entry) = maybe_entry {
            entry.renewal_handle.abort();

            let _ = store
                .release_session_lock(&key.instance_id, &key.session_id, &key.worker_id)
                .await;
        }
    }
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
        let session_renewals = SessionRenewalRegistry::new();

        tokio::spawn(async move {
            let mut worker_handles = Vec::with_capacity(concurrency);

            for worker_idx in 0..concurrency {
                let rt = Arc::clone(&self);
                let activities = Arc::clone(&activities);
                let shutdown = Arc::clone(&shutdown);
                let session_renewals = session_renewals.clone();
                let worker_id = format!("work-{worker_idx}-{}", rt.runtime_id);

                let handle = tokio::spawn(async move {
                    let mut consecutive_retryable_errors: u32 = 0;

                    loop {
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }

                        let min_interval = rt.options.dispatcher_min_poll_interval;
                        let start_time = std::time::Instant::now();

                        let work_found =
                            match process_next_work_item(&rt, &activities, &shutdown, &session_renewals, &worker_id)
                                .await
                            {
                                Ok(found) => {
                                    consecutive_retryable_errors = 0;
                                    found
                                }
                                Err(e) if e.is_retryable() => {
                                    // Exponential backoff for retryable errors (database locks, etc.)
                                    consecutive_retryable_errors += 1;
                                    let backoff_ms = (100 * 2_u64.pow(consecutive_retryable_errors)).min(3000);
                                    warn!(
                                        "Error fetching work item (retryable, attempt {}): {:?}, backing off {}ms",
                                        consecutive_retryable_errors, e, backoff_ms
                                    );
                                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                                    continue;
                                }
                                Err(e) => {
                                    // Permanent errors - log and continue with normal polling
                                    warn!("Error fetching work item (permanent): {:?}", e);
                                    consecutive_retryable_errors = 0;
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                    continue;
                                }
                            };

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

            // Graceful shutdown: release all session locks immediately so other
            // workers can claim them without waiting for lock expiry.
            session_renewals.release_all(Arc::clone(&self.history_store)).await;
        })
    }
}

/// Process the next available work item from the queue.
///
/// Returns:
/// - `Ok(true)` if work was found and processed
/// - `Ok(false)` if no work was available
/// - `Err(e)` if fetch failed (caller handles backoff)
async fn process_next_work_item(
    rt: &Arc<Runtime>,
    activities: &Arc<registry::ActivityRegistry>,
    shutdown: &Arc<std::sync::atomic::AtomicBool>,
    session_renewals: &SessionRenewalRegistry,
    worker_id: &str,
) -> Result<bool, crate::providers::ProviderError> {
    let (item, token, attempt_count) = {
        let use_sessions = rt.history_store.supports_sessions();
        let at_session_capacity = use_sessions
            && rt.options.max_sessions_per_worker > 0
            && session_renewals.owned_session_count().await >= rt.options.max_sessions_per_worker;
        let result = if use_sessions && !at_session_capacity {
            rt.history_store
                .fetch_session_work_item(
                    rt.options.worker_lock_timeout,
                    rt.options.dispatcher_long_poll_timeout,
                    worker_id,
                    rt.options.session_lock_duration,
                )
                .await?
        } else {
            rt.history_store
                .fetch_work_item(rt.options.worker_lock_timeout, rt.options.dispatcher_long_poll_timeout)
                .await?
        };
        match result {
            Some(r) => r,
            None => return Ok(false),
        }
    };

    let item_serialized = serde_json::to_string(&item).unwrap_or_default();

    match item {
        WorkItem::ActivityExecute {
            instance,
            execution_id,
            id,
            name,
            input,
            session_id,
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
                session_id,
            };

            // Note: We no longer check execution state at fetch time.
            // Cancellation is detected during lock renewal (lock stealing).
            if ctx.attempt_count > rt.options.max_attempts {
                // Handle poison messages
                handle_poison_message(rt, &ctx).await;
            } else {
                // Execute activity with cancellation support
                execute_activity(rt, activities, shutdown, session_renewals, ctx).await;
            }
        }
        other => {
            error!(?other, "unexpected WorkItem in Worker dispatcher; state corruption");
            panic!("unexpected WorkItem in Worker dispatcher");
        }
    }

    Ok(true)
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
    session_renewals: &SessionRenewalRegistry,
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

    // Acquire shared per-session renewal lease (single renewal task per session key).
    // Also bridge session-lock-loss to activity cancellation.
    let (session_lease_key, session_lock_loss_bridge) = if let Some(ref sid) = ctx.session_id {
        let key = SessionLeaseKey {
            instance_id: ctx.instance.clone(),
            session_id: sid.clone(),
            worker_id: ctx.worker_id.clone(),
        };

        let session_lost_token = session_renewals
            .acquire(
                Arc::clone(&rt.history_store),
                key.clone(),
                rt.options.session_lock_duration,
                Arc::clone(shutdown),
            )
            .await;

        let activity_cancel = cancellation_token.clone();
        let bridge = tokio::spawn(async move {
            session_lost_token.cancelled().await;
            activity_cancel.cancel();
        });

        (Some(key), Some(bridge))
    } else {
        (None, None)
    };

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
            if let Some(h) = session_lock_loss_bridge {
                h.abort();
            }
            if let Some(key) = session_lease_key.as_ref() {
                session_renewals
                    .release(Arc::clone(&rt.history_store), key, rt.options.session_idle_timeout)
                    .await;
            }
            abandon_unregistered_activity(rt, &ctx).await;
            // Early return after abandonment - no ack_result needed since we abandoned
            return;
        }
    };

    if let Some(h) = session_lock_loss_bridge {
        h.abort();
    }
    if let Some(key) = session_lease_key.as_ref() {
        session_renewals
            .release(Arc::clone(&rt.history_store), key, rt.options.session_idle_timeout)
            .await;
    }

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

    let mut activity_ctx = crate::ActivityContext::new_with_cancellation(
        ctx.instance.clone(),
        ctx.execution_id,
        orch_name,
        orch_version,
        ctx.activity_name.clone(),
        ctx.activity_id,
        ctx.worker_id.clone(),
        cancellation_token,
        Arc::clone(&rt.history_store),
    );
    activity_ctx.set_session_id(ctx.session_id.clone());
    activity_ctx
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

/// Abandon unregistered activity with exponential backoff for rolling deployment support.
///
/// The poison message handling will eventually fail the activity if genuinely missing.
async fn abandon_unregistered_activity(rt: &Arc<Runtime>, ctx: &ActivityWorkContext) {
    let backoff = rt.options.unregistered_backoff.delay(ctx.attempt_count);
    let remaining_attempts = rt.options.max_attempts.saturating_sub(ctx.attempt_count);

    tracing::warn!(
        target: "duroxide::runtime",
        instance = %ctx.instance,
        execution_id = %ctx.execution_id,
        activity_name = %ctx.activity_name,
        activity_id = %ctx.activity_id,
        worker_id = %ctx.worker_id,
        attempt_count = %ctx.attempt_count,
        max_attempts = %rt.options.max_attempts,
        remaining_attempts = %remaining_attempts,
        backoff_secs = %backoff.as_secs_f32(),
        "Activity not registered, abandoning with {:.1}s backoff (will poison in {} more attempts)",
        backoff.as_secs_f32(),
        remaining_attempts
    );

    // Abandon with delay - poison handling will eventually terminate if genuinely missing
    let _ = rt
        .history_store
        .abandon_work_item(&ctx.lock_token, Some(backoff), false)
        .await;
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

/// Spawn a background task to renew a session lock while an activity is executing.
///
/// Renews the session lock every `duration / 2`. If renewal fails (session closed or
/// claimed by another worker), this task cancels `session_lost_token` so all activities
/// bound to this session lease are cooperatively cancelled.
/// TODO: Optimization — piggyback session renewals on activity lock renewal ticks
/// to reduce timer/task overhead under high session fan-in.
fn spawn_session_renewal(
    store: Arc<dyn crate::providers::Provider>,
    instance_id: String,
    session_id: String,
    worker_id: String,
    session_lock_duration: Duration,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    session_lost_token: CancellationToken,
) -> JoinHandle<()> {
    let renewal_interval = Duration::from_millis((session_lock_duration.as_millis() / 2).max(1000) as u64);

    tracing::debug!(
        target: "duroxide::runtime::worker",
        instance_id = %instance_id,
        session_id = %session_id,
        worker_id = %worker_id,
        renewal_interval_secs = %renewal_interval.as_secs(),
        "Spawning session renewal task"
    );

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(renewal_interval);
        interval.tick().await; // Skip first immediate tick

        loop {
            interval.tick().await;

            if shutdown.load(Ordering::Relaxed) {
                tracing::debug!(
                    target: "duroxide::runtime::worker",
                    session_id = %session_id,
                    "Session renewal stopping due to shutdown"
                );
                break;
            }

            match store
                .renew_session_lock(&instance_id, &session_id, &worker_id, session_lock_duration)
                .await
            {
                Ok(()) => {
                    tracing::trace!(
                        target: "duroxide::runtime::worker",
                        session_id = %session_id,
                        "Session lock renewed"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        target: "duroxide::runtime::worker",
                        session_id = %session_id,
                        error = %e,
                        "Session lock renewal failed; canceling activities bound to this session"
                    );
                    session_lost_token.cancel();
                    break;
                }
            }
        }
    })
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
                Ok(()) => {
                    // Lock renewed successfully - orchestration is still running
                    tracing::trace!(
                        target: "duroxide::runtime::worker",
                        lock_token = %token,
                        extend_secs = %lock_timeout.as_secs(),
                        "Work item lock renewed"
                    );
                }
                Err(e) => {
                    // Lock renewal failed - activity was cancelled (lock stolen) or lock expired
                    tracing::info!(
                        target: "duroxide::runtime::worker",
                        lock_token = %token,
                        error = %e,
                        "Lock renewal failed, signaling activity cancellation (lock was stolen or expired)"
                    );
                    cancellation_token.cancel();
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
