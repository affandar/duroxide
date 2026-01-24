// Replay engine uses Mutex locks - poison indicates a panic and should propagate
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::clone_on_ref_ptr)]

use crate::{Action, Event, EventKind, providers::WorkItem, runtime::OrchestrationHandler};
use crate::{CompletionResult, OrchestrationContext};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use tracing::{debug, warn};

/// Result of executing an orchestration turn
#[derive(Debug)]
pub enum TurnResult {
    /// Turn completed successfully, orchestration continues
    Continue,
    /// Orchestration completed with output
    Completed(String),
    /// Orchestration failed with error details
    Failed(crate::ErrorDetails),
    /// Orchestration requested continue-as-new
    ContinueAsNew { input: String, version: Option<String> },
    /// Orchestration was cancelled
    Cancelled(String),
}

/// Replays history and executes one deterministic orchestration evaluation
pub struct ReplayEngine {
    /// Instance identifier
    pub(crate) instance: String,

    /// Current execution ID
    pub(crate) execution_id: u64,
    /// History events generated during this run
    pub(crate) history_delta: Vec<Event>,
    /// Actions to dispatch after persistence
    pub(crate) pending_actions: Vec<crate::Action>,

    /// ActivityScheduled event_ids for activity losers of select/select2.
    /// These should be cancelled via provider lock stealing.
    pub(crate) cancelled_activity_ids: Vec<u64>,

    /// Sub-orchestration instance IDs for sub-orch losers of select/select2.
    /// These should be cancelled via CancelInstance work items.
    pub(crate) cancelled_sub_orchestration_ids: Vec<String>,

    /// Current history at start of run
    pub(crate) baseline_history: Vec<Event>,
    /// Next event_id for new events added this run
    pub(crate) next_event_id: u64,
    /// Unified error collector for system-level errors that abort the turn
    pub(crate) abort_error: Option<crate::ErrorDetails>,

    /// Number of events in baseline_history that were actually persisted (from DB).
    /// Events beyond this index in baseline_history are NEW this turn (not replay).
    /// Used to correctly track is_replaying state.
    persisted_history_len: usize,
}

impl ReplayEngine {
    /// Create a new replay engine for an instance/execution
    pub fn new(instance: String, execution_id: u64, baseline_history: Vec<Event>) -> Self {
        let next_event_id = baseline_history.last().map(|e| e.event_id() + 1).unwrap_or(1);
        let persisted_len = baseline_history.len(); // Default: assume all are persisted

        Self {
            instance,
            execution_id,
            history_delta: Vec::new(),
            pending_actions: Vec::new(),
            cancelled_activity_ids: Vec::new(),
            cancelled_sub_orchestration_ids: Vec::new(),
            baseline_history,
            next_event_id,
            abort_error: None,
            persisted_history_len: persisted_len,
        }
    }

    /// Set the number of events in baseline_history that were actually persisted.
    ///
    /// This is used to correctly track the `is_replaying` state.
    /// Events at indices `0..persisted_len` in the working history are replays;
    /// events at indices `persisted_len..` are new this turn.
    ///
    /// This should be set to `history_mgr.original_len()` - the count of events
    /// that came from the database, NOT including newly created events like
    /// OrchestrationStarted on the first turn.
    pub fn with_persisted_history_len(mut self, len: usize) -> Self {
        self.persisted_history_len = len;
        self
    }

    /// Stage 1: Convert completion messages directly to events
    ///
    /// The conversion from WorkItem to Event generates history_delta
    /// which is then processed by the execution path.
    pub fn prep_completions(&mut self, messages: Vec<WorkItem>) {
        debug!(
            instance = %self.instance,
            message_count = messages.len(),
            "converting messages to events"
        );

        for msg in messages {
            // Check filtering conditions
            if !self.is_completion_for_current_execution(&msg) {
                if self.has_continue_as_new_in_history() {
                    warn!(instance = %self.instance, "ignoring completion from previous execution");
                } else {
                    warn!(instance = %self.instance, "completion from different execution");
                }
                continue;
            }

            if self.is_completion_already_in_history(&msg) {
                warn!(instance = %self.instance, "ignoring duplicate completion");
                continue;
            }

            // Drop duplicates already staged in this run's history_delta
            let already_in_delta = match &msg {
                WorkItem::ActivityCompleted { id, .. } | WorkItem::ActivityFailed { id, .. } => {
                    self.history_delta.iter().any(|e| {
                        e.source_event_id == Some(*id)
                            && matches!(
                                &e.kind,
                                EventKind::ActivityCompleted { .. } | EventKind::ActivityFailed { .. }
                            )
                    })
                }
                WorkItem::TimerFired { id, .. } => self
                    .history_delta
                    .iter()
                    .any(|e| e.source_event_id == Some(*id) && matches!(&e.kind, EventKind::TimerFired { .. })),
                WorkItem::SubOrchCompleted { parent_id, .. } | WorkItem::SubOrchFailed { parent_id, .. } => {
                    self.history_delta.iter().any(|e| {
                        e.source_event_id == Some(*parent_id)
                            && matches!(
                                &e.kind,
                                EventKind::SubOrchestrationCompleted { .. } | EventKind::SubOrchestrationFailed { .. }
                            )
                    })
                }
                WorkItem::ExternalRaised { name, data, .. } => self.history_delta.iter().any(
                    |e| matches!(&e.kind, EventKind::ExternalEvent { name: n, data: d } if n == name && d == data),
                ),
                WorkItem::CancelInstance { .. } => false,
                _ => false, // Non-completion work items
            };
            if already_in_delta {
                warn!(instance = %self.instance, "dropping duplicate completion in current run");
                continue;
            }

            // Nondeterminism detection: ensure completion has a matching schedule and kind
            let schedule_kind = |id: &u64| -> Option<&'static str> {
                for e in self.baseline_history.iter().chain(self.history_delta.iter()) {
                    if e.event_id != *id {
                        continue;
                    }
                    match &e.kind {
                        EventKind::ActivityScheduled { .. } => return Some("activity"),
                        EventKind::TimerCreated { .. } => return Some("timer"),
                        EventKind::SubOrchestrationScheduled { .. } => return Some("suborchestration"),
                        _ => {}
                    }
                }
                None
            };
            let mut nd_err: Option<crate::ErrorDetails> = None;
            match &msg {
                WorkItem::ActivityCompleted { id, .. } | WorkItem::ActivityFailed { id, .. } => {
                    match schedule_kind(id) {
                        Some("activity") => {}
                        Some(other) => {
                            nd_err = Some(crate::ErrorDetails::Configuration {
                                kind: crate::ConfigErrorKind::Nondeterminism,
                                resource: String::new(),
                                message: Some(format!(
                                    "completion kind mismatch for id={id}, expected '{other}', got 'activity'"
                                )),
                            })
                        }
                        None => {
                            nd_err = Some(crate::ErrorDetails::Configuration {
                                kind: crate::ConfigErrorKind::Nondeterminism,
                                resource: String::new(),
                                message: Some(format!("no matching schedule for completion id={id}")),
                            })
                        }
                    }
                }
                WorkItem::TimerFired { id, .. } => match schedule_kind(id) {
                    Some("timer") => {}
                    Some(other) => {
                        nd_err = Some(crate::ErrorDetails::Configuration {
                            kind: crate::ConfigErrorKind::Nondeterminism,
                            resource: String::new(),
                            message: Some(format!(
                                "completion kind mismatch for id={id}, expected '{other}', got 'timer'"
                            )),
                        })
                    }
                    None => {
                        nd_err = Some(crate::ErrorDetails::Configuration {
                            kind: crate::ConfigErrorKind::Nondeterminism,
                            resource: String::new(),
                            message: Some(format!("no matching schedule for timer id={id}")),
                        })
                    }
                },
                WorkItem::SubOrchCompleted { parent_id, .. } | WorkItem::SubOrchFailed { parent_id, .. } => {
                    match schedule_kind(parent_id) {
                        Some("suborchestration") => {}
                        Some(other) => {
                            nd_err = Some(crate::ErrorDetails::Configuration {
                                kind: crate::ConfigErrorKind::Nondeterminism,
                                resource: String::new(),
                                message: Some(format!(
                                    "completion kind mismatch for id={parent_id}, expected '{other}', got 'suborchestration'"
                                )),
                            })
                        }
                        None => {
                            nd_err = Some(crate::ErrorDetails::Configuration {
                                kind: crate::ConfigErrorKind::Nondeterminism,
                                resource: String::new(),
                                message: Some(format!("no matching schedule for sub-orchestration id={parent_id}")),
                            })
                        }
                    }
                }
                WorkItem::ExternalRaised { .. } | WorkItem::CancelInstance { .. } => {}
                _ => {} // Non-completion work items
            }
            if let Some(err) = nd_err {
                warn!(instance = %self.instance, error = %err.display_message(), "detected nondeterminism in completion batch");
                self.abort_error = Some(err);
                continue;
            }

            // Convert message to event
            let event_opt = match msg {
                WorkItem::ActivityCompleted { id, result, .. } => Some(Event::new(
                    &self.instance,
                    self.execution_id,
                    Some(id),
                    EventKind::ActivityCompleted { result },
                )),
                WorkItem::ActivityFailed { id, details, .. } => {
                    // Always create event in history for audit trail
                    let event = Event::new(
                        &self.instance,
                        self.execution_id,
                        Some(id),
                        EventKind::ActivityFailed {
                            details: details.clone(),
                        },
                    );

                    // Check if system error (abort turn)
                    match &details {
                        crate::ErrorDetails::Configuration { .. }
                        | crate::ErrorDetails::Infrastructure { .. }
                        | crate::ErrorDetails::Poison { .. } => {
                            warn!(
                                instance = %self.instance,
                                activity_id = id,
                                error = %details.display_message(),
                                "System error aborts turn"
                            );
                            if self.abort_error.is_none() {
                                self.abort_error = Some(details.clone());
                            }
                        }
                        crate::ErrorDetails::Application { .. } => {
                            // Normal flow
                        }
                    }

                    Some(event)
                }
                WorkItem::TimerFired { id, fire_at_ms, .. } => Some(Event::new(
                    &self.instance,
                    self.execution_id,
                    Some(id),
                    EventKind::TimerFired { fire_at_ms },
                )),
                WorkItem::ExternalRaised { name, data, .. } => {
                    // Only materialize ExternalEvent if a subscription exists in this execution
                    let subscribed = self.baseline_history.iter().any(
                        |e| matches!(&e.kind, EventKind::ExternalSubscribed { name: hist_name } if hist_name == &name),
                    );
                    if subscribed {
                        Some(Event::new(
                            &self.instance,
                            self.execution_id,
                            None, // ExternalEvent doesn't have source_event_id
                            EventKind::ExternalEvent { name, data },
                        ))
                    } else {
                        warn!(instance = %self.instance, event_name=%name, "dropping ExternalByName with no matching subscription in history");
                        None
                    }
                }
                WorkItem::SubOrchCompleted { parent_id, result, .. } => Some(Event::new(
                    &self.instance,
                    self.execution_id,
                    Some(parent_id),
                    EventKind::SubOrchestrationCompleted { result },
                )),
                WorkItem::SubOrchFailed { parent_id, details, .. } => {
                    // Always create event in history for audit trail
                    let event = Event::new(
                        &self.instance,
                        self.execution_id,
                        Some(parent_id),
                        EventKind::SubOrchestrationFailed {
                            details: details.clone(),
                        },
                    );

                    // Check if system error (abort parent turn)
                    match &details {
                        crate::ErrorDetails::Configuration { .. }
                        | crate::ErrorDetails::Infrastructure { .. }
                        | crate::ErrorDetails::Poison { .. } => {
                            warn!(instance = %self.instance, parent_id, ?details, "Child system error aborts parent");
                            if self.abort_error.is_none() {
                                self.abort_error = Some(details);
                            }
                        }
                        crate::ErrorDetails::Application { .. } => {
                            // Normal flow
                        }
                    }

                    Some(event)
                }
                WorkItem::CancelInstance { reason, .. } => {
                    let already_terminated = self.baseline_history.iter().any(|e| {
                        matches!(
                            &e.kind,
                            EventKind::OrchestrationCompleted { .. } | EventKind::OrchestrationFailed { .. }
                        )
                    });
                    let already_cancelled = self
                        .baseline_history
                        .iter()
                        .chain(self.history_delta.iter())
                        .any(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. }));

                    if !already_terminated && !already_cancelled {
                        Some(Event::new(
                            &self.instance,
                            self.execution_id,
                            None,
                            EventKind::OrchestrationCancelRequested { reason },
                        ))
                    } else {
                        None
                    }
                }
                _ => None, // Non-completion work items
            };

            if let Some(mut event) = event_opt {
                // Assign event_id and add to history_delta
                event.set_event_id(self.next_event_id);
                self.next_event_id += 1;
                self.history_delta.push(event);
            }
        }

        debug!(
            instance = %self.instance,
            event_count = self.history_delta.len(),
            "completion events created"
        );
    }

    /// Get the current execution ID from the baseline history
    fn get_current_execution_id(&self) -> u64 {
        self.execution_id
    }

    /// Check if a completion message belongs to the current execution
    fn is_completion_for_current_execution(&self, msg: &WorkItem) -> bool {
        let current_execution_id = self.get_current_execution_id();
        match msg {
            WorkItem::ActivityCompleted { execution_id, .. } => *execution_id == current_execution_id,
            WorkItem::ActivityFailed { execution_id, .. } => *execution_id == current_execution_id,
            WorkItem::TimerFired { execution_id, .. } => *execution_id == current_execution_id,
            WorkItem::SubOrchCompleted {
                parent_execution_id, ..
            } => *parent_execution_id == current_execution_id,
            WorkItem::SubOrchFailed {
                parent_execution_id, ..
            } => *parent_execution_id == current_execution_id,
            WorkItem::ExternalRaised { .. } => true, // External events don't have execution IDs
            WorkItem::CancelInstance { .. } => true, // Cancellation applies to current execution
            _ => false,                              // Non-completion work items (shouldn't reach here)
        }
    }

    /// Check if a completion is already in the baseline history (duplicate)
    fn is_completion_already_in_history(&self, msg: &WorkItem) -> bool {
        match msg {
            WorkItem::ActivityCompleted { id, .. } => self
                .baseline_history
                .iter()
                .any(|e| e.source_event_id == Some(*id) && matches!(&e.kind, EventKind::ActivityCompleted { .. })),
            WorkItem::TimerFired { id, .. } => self
                .baseline_history
                .iter()
                .any(|e| e.source_event_id == Some(*id) && matches!(&e.kind, EventKind::TimerFired { .. })),
            WorkItem::SubOrchCompleted { parent_id, .. } => self.baseline_history.iter().any(|e| {
                e.source_event_id == Some(*parent_id) && matches!(&e.kind, EventKind::SubOrchestrationCompleted { .. })
            }),
            WorkItem::SubOrchFailed { parent_id, .. } => self.baseline_history.iter().any(|e| {
                e.source_event_id == Some(*parent_id) && matches!(&e.kind, EventKind::SubOrchestrationFailed { .. })
            }),
            WorkItem::ExternalRaised { name, data, .. } => self.baseline_history.iter().any(|e| {
                matches!(&e.kind, EventKind::ExternalEvent { name: hist_name, data: hist_data }
                        if hist_name == name && hist_data == data)
            }),
            _ => false,
        }
    }

    /// Check if this orchestration has used continue_as_new
    fn has_continue_as_new_in_history(&self) -> bool {
        self.baseline_history
            .iter()
            .any(|e| matches!(&e.kind, EventKind::OrchestrationContinuedAsNew { .. }))
    }

    /// Stage 2: Execute one turn of the orchestration using the replay engine
    /// This stage runs the orchestration logic and generates history deltas and actions
    ///
    /// This implementation uses the commands-vs-history model:
    /// 1. Processes history events in order, matching emitted actions
    /// 2. Delivers completions to the results map
    /// 3. Returns new actions beyond history as pending_actions
    pub fn execute_orchestration(
        &mut self,
        handler: Arc<dyn OrchestrationHandler>,
        input: String,
        orchestration_name: String,
        orchestration_version: String,
        worker_id: &str,
    ) -> TurnResult {
        debug!(
            instance = %self.instance,
            "executing orchestration turn"
        );
        // Check abort_error FIRST - before running user code
        if let Some(err) = self.abort_error.clone() {
            return TurnResult::Failed(err);
        }

        // Build working history: baseline + completion events from this run
        let mut working_history = self.baseline_history.clone();
        working_history.extend_from_slice(&self.history_delta);

        // Track the boundary between replay and new execution.
        // persisted_history_len = events from DB (true replay)
        // Events beyond this are new this turn (not replay)
        let replay_boundary = self.persisted_history_len;

        // Check for terminal history - don't process
        if working_history.iter().any(|e| {
            matches!(
                e.kind,
                EventKind::OrchestrationCompleted { .. }
                    | EventKind::OrchestrationFailed { .. }
                    | EventKind::OrchestrationContinuedAsNew { .. }
            )
        }) {
            return TurnResult::Continue;
        }

        // Check for empty history
        if working_history.is_empty() {
            return TurnResult::Failed(crate::ErrorDetails::Configuration {
                kind: crate::ConfigErrorKind::Nondeterminism,
                resource: String::new(),
                message: Some("corrupted history: empty".to_string()),
            });
        }

        // Validate first event is OrchestrationStarted
        if !matches!(working_history[0].kind, EventKind::OrchestrationStarted { .. }) {
            return TurnResult::Failed(crate::ErrorDetails::Configuration {
                kind: crate::ConfigErrorKind::Nondeterminism,
                resource: String::new(),
                message: Some("corrupted history: first event must be OrchestrationStarted".to_string()),
            });
        }

        // Create context
        let ctx = OrchestrationContext::new(
            Vec::new(), // Empty history - not used by replay engine
            self.execution_id,
            self.instance.clone(),
            orchestration_name.clone(),
            orchestration_version.clone(),
            Some(worker_id.to_string()),
        );

        // Clone ctx for use in the async closure
        let ctx_for_future = ctx.clone();

        // Create the orchestration future
        let h = handler.clone();
        let inp = input.clone();
        let fut_result = catch_unwind(AssertUnwindSafe(|| {
            Box::pin(async move { h.invoke(ctx_for_future, inp).await })
        }));

        let mut fut = match fut_result {
            Ok(f) => f,
            Err(panic_payload) => {
                let msg = extract_panic_message(panic_payload);
                return TurnResult::Failed(crate::ErrorDetails::Configuration {
                    kind: crate::ConfigErrorKind::Nondeterminism,
                    resource: String::new(),
                    message: Some(msg),
                });
            }
        };

        // Track open schedules and schedule kinds for validation
        let mut open_schedules: HashSet<u64> = HashSet::new();
        let mut schedule_kinds: std::collections::HashMap<u64, ActionKind> = std::collections::HashMap::new();
        let mut emitted_actions: VecDeque<(u64, Action)> = VecDeque::new();

        let mut must_poll = true;
        let mut output_opt: Option<Result<String, String>> = None;

        // Context starts in replaying mode IF there's persisted history to replay.
        // If no persisted events (replay_boundary == 0), we're not replaying - this is fresh execution.
        if replay_boundary == 0 {
            ctx.set_is_replaying(false);
        }

        for (event_index, event) in working_history.iter().enumerate() {
            // Check if we've moved past the persisted history into new events
            // We check BEFORE polling so the orchestration sees the correct state
            if event_index >= replay_boundary {
                ctx.set_is_replaying(false);
            }

            // Poll if needed
            if must_poll {
                let poll_result = catch_unwind(AssertUnwindSafe(|| poll_once(fut.as_mut())));

                match poll_result {
                    Ok(Poll::Ready(result)) => {
                        output_opt = Some(result);
                        // Drain any remaining emitted actions
                        emitted_actions.extend(ctx.drain_emitted_actions());
                        break; // Orchestration completed
                    }
                    Ok(Poll::Pending) => {
                        // Drain emitted actions
                        emitted_actions.extend(ctx.drain_emitted_actions());
                    }
                    Err(panic_payload) => {
                        let msg = extract_panic_message(panic_payload);
                        return TurnResult::Failed(crate::ErrorDetails::Configuration {
                            kind: crate::ConfigErrorKind::Nondeterminism,
                            resource: String::new(),
                            message: Some(msg),
                        });
                    }
                }
                must_poll = false;
            }

            match &event.kind {
                EventKind::OrchestrationStarted { .. } => {
                    // Skip - already validated
                }

                // Schedule events
                EventKind::ActivityScheduled { name, input: inp } => {
                    if let Err(result) = self.match_and_bind_schedule(
                        &ctx,
                        &mut emitted_actions,
                        &mut open_schedules,
                        &mut schedule_kinds,
                        event,
                        ActionKind::Activity {
                            name: name.clone(),
                            input: inp.clone(),
                        },
                    ) {
                        return result;
                    }
                }

                EventKind::TimerCreated { fire_at_ms } => {
                    if let Err(result) = self.match_and_bind_schedule(
                        &ctx,
                        &mut emitted_actions,
                        &mut open_schedules,
                        &mut schedule_kinds,
                        event,
                        ActionKind::Timer {
                            fire_at_ms: *fire_at_ms,
                        },
                    ) {
                        return result;
                    }
                }

                EventKind::ExternalSubscribed { name } => {
                    if let Err(result) = self.match_and_bind_schedule(
                        &ctx,
                        &mut emitted_actions,
                        &mut open_schedules,
                        &mut schedule_kinds,
                        event,
                        ActionKind::External { name: name.clone() },
                    ) {
                        return result;
                    }
                    // Bind external subscription index
                    ctx.inner
                        .lock()
                        .expect("Mutex should not be poisoned")
                        .bind_external_subscription(event.event_id(), name);
                }

                EventKind::SubOrchestrationScheduled {
                    name,
                    instance,
                    input: inp,
                    ..
                } => {
                    match self.match_and_bind_schedule(
                        &ctx,
                        &mut emitted_actions,
                        &mut open_schedules,
                        &mut schedule_kinds,
                        event,
                        ActionKind::SubOrch {
                            name: name.clone(),
                            instance: instance.clone(),
                            input: inp.clone(),
                        },
                    ) {
                        Ok(token) => {
                            // Bind the resolved instance ID to the token for cancellation lookup
                            ctx.bind_sub_orchestration_instance(token, instance.clone());
                        }
                        Err(result) => return result,
                    }
                }

                EventKind::SystemCall { op, value } => {
                    if let Err(result) = self.match_and_bind_schedule(
                        &ctx,
                        &mut emitted_actions,
                        &mut open_schedules,
                        &mut schedule_kinds,
                        event,
                        ActionKind::System { op: op.clone() },
                    ) {
                        return result;
                    }
                    // Deliver system call result immediately
                    ctx.deliver_result(event.event_id(), CompletionResult::SystemCallValue(value.clone()));
                    must_poll = true;
                }

                // Completion events
                EventKind::ActivityCompleted { result } => {
                    if let Some(source_id) = event.source_event_id {
                        if !open_schedules.contains(&source_id) {
                            return TurnResult::Failed(nondeterminism_error("completion without open schedule"));
                        }
                        if !matches!(schedule_kinds.get(&source_id), Some(ActionKind::Activity { .. })) {
                            return TurnResult::Failed(nondeterminism_error(
                                "completion kind mismatch: expected activity",
                            ));
                        }
                        ctx.deliver_result(source_id, CompletionResult::ActivityOk(result.clone()));
                        must_poll = true;
                    }
                }

                EventKind::ActivityFailed { details } => {
                    if let Some(source_id) = event.source_event_id {
                        if !open_schedules.contains(&source_id) {
                            return TurnResult::Failed(nondeterminism_error("completion without open schedule"));
                        }
                        ctx.deliver_result(source_id, CompletionResult::ActivityErr(details.display_message()));
                        must_poll = true;
                    }
                }

                EventKind::TimerFired { .. } => {
                    if let Some(source_id) = event.source_event_id {
                        if !open_schedules.contains(&source_id) {
                            return TurnResult::Failed(nondeterminism_error("completion without open schedule"));
                        }
                        ctx.deliver_result(source_id, CompletionResult::TimerFired);
                        must_poll = true;
                    }
                }

                EventKind::SubOrchestrationCompleted { result } => {
                    if let Some(source_id) = event.source_event_id {
                        if !open_schedules.contains(&source_id) {
                            return TurnResult::Failed(nondeterminism_error("completion without open schedule"));
                        }
                        ctx.deliver_result(source_id, CompletionResult::SubOrchOk(result.clone()));
                        must_poll = true;
                    }
                }

                EventKind::SubOrchestrationFailed { details } => {
                    if let Some(source_id) = event.source_event_id {
                        if !open_schedules.contains(&source_id) {
                            return TurnResult::Failed(nondeterminism_error("completion without open schedule"));
                        }
                        ctx.deliver_result(source_id, CompletionResult::SubOrchErr(details.display_message()));
                        must_poll = true;
                    }
                }

                EventKind::ExternalEvent { name, data } => {
                    ctx.inner
                        .lock()
                        .expect("Mutex should not be poisoned")
                        .deliver_external_event(name.clone(), data.clone());
                    must_poll = true;
                }

                EventKind::OrchestrationCancelRequested { .. } => {
                    // Cancel is handled at the end of the turn, after all history is processed.
                    // This allows the orchestration to run and produce output, but cancel
                    // takes precedence when returning the final result.
                    // Don't return early here - just continue processing.
                }

                // These should have been filtered out above
                EventKind::OrchestrationCompleted { .. }
                | EventKind::OrchestrationFailed { .. }
                | EventKind::OrchestrationContinuedAsNew { .. }
                | EventKind::OrchestrationChained { .. } => {
                    // Should not reach here due to terminal check above
                }
            }
        }

        // After processing all history, we're no longer replaying
        ctx.set_is_replaying(false);

        // Final poll after processing all history (including completions from prep_completions)
        // This may emit additional actions if completions resolved durable futures
        if must_poll && output_opt.is_none() {
            let poll_result = catch_unwind(AssertUnwindSafe(|| poll_once(fut.as_mut())));

            match poll_result {
                Ok(Poll::Ready(result)) => {
                    output_opt = Some(result);
                    emitted_actions.extend(ctx.drain_emitted_actions());
                }
                Ok(Poll::Pending) => {
                    emitted_actions.extend(ctx.drain_emitted_actions());
                }
                Err(panic_payload) => {
                    let msg = extract_panic_message(panic_payload);
                    return TurnResult::Failed(crate::ErrorDetails::Configuration {
                        kind: crate::ConfigErrorKind::Nondeterminism,
                        resource: String::new(),
                        message: Some(msg),
                    });
                }
            }
        }

        // Process emitted actions and handle system calls with re-polling.
        // System calls complete immediately, so we need to:
        // 1. Process emitted actions (including delivering system call results)
        // 2. Poll again if any system calls were delivered
        // 3. Repeat until no more system calls are emitted
        loop {
            if emitted_actions.is_empty() {
                break;
            }

            let mut delivered_system_call = false;

            // Convert emitted actions to pending_actions AND history_delta
            for (token, action) in emitted_actions.drain(..) {
                let event_id = self.next_event_id;
                self.next_event_id += 1;

                ctx.bind_token(token, event_id);

                let updated_action = update_action_event_id(action, event_id);

                if let crate::Action::StartSubOrchestration { instance, .. } = &updated_action {
                    ctx.bind_sub_orchestration_instance(token, instance.clone());
                }

                // For system calls, deliver the computed value immediately
                if let crate::Action::SystemCall { value, .. } = &updated_action {
                    ctx.deliver_result(event_id, CompletionResult::SystemCallValue(value.clone()));
                    delivered_system_call = true;
                }

                if let Some(event) = action_to_event(&updated_action, &self.instance, self.execution_id, event_id) {
                    self.history_delta.push(event);
                }

                self.pending_actions.push(updated_action);
            }

            // If we delivered system call results, poll again to let orchestration continue
            if delivered_system_call && output_opt.is_none() {
                let poll_result = catch_unwind(AssertUnwindSafe(|| poll_once(fut.as_mut())));

                match poll_result {
                    Ok(Poll::Ready(result)) => {
                        output_opt = Some(result);
                        emitted_actions.extend(ctx.drain_emitted_actions());
                        break;
                    }
                    Ok(Poll::Pending) => {
                        emitted_actions.extend(ctx.drain_emitted_actions());
                    }
                    Err(panic_payload) => {
                        let msg = extract_panic_message(panic_payload);
                        return TurnResult::Failed(crate::ErrorDetails::Configuration {
                            kind: crate::ConfigErrorKind::Nondeterminism,
                            resource: String::new(),
                            message: Some(msg),
                        });
                    }
                }
            } else {
                break;
            }
        }

        // Check for cancellation first - if cancelled, return immediately
        // This matches legacy mode behavior: cancel takes precedence over completion
        let cancel_event = self
            .baseline_history
            .iter()
            .chain(self.history_delta.iter())
            .find(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. }));

        if let Some(e) = cancel_event
            && let EventKind::OrchestrationCancelRequested { reason } = &e.kind
        {
            return TurnResult::Cancelled(reason.clone());
        }

        // Check for continue-as-new in pending_actions
        for decision in &self.pending_actions {
            if let crate::Action::ContinueAsNew { input, version } = decision {
                return TurnResult::ContinueAsNew {
                    input: input.clone(),
                    version: version.clone(),
                };
            }
        }

        // Return result
        if let Some(output) = output_opt {
            // Collect cancellation information from context before returning
            self.collect_cancelled_from_context(&ctx);

            return match output {
                Ok(result) => TurnResult::Completed(result),
                Err(error) => TurnResult::Failed(crate::ErrorDetails::Application {
                    kind: crate::AppErrorKind::OrchestrationFailed,
                    message: error,
                    retryable: false,
                }),
            };
        }

        // Collect cancellation information from context before returning Continue.
        //
        // SAFETY: Dehydration drops don't cause spurious cancellations because:
        // 1. We collect cancelled_tokens HERE, while the orchestration future is still alive
        // 2. TurnResult::Continue is returned, then `fut` goes out of scope
        // 3. DurableFuture::drop() calls mark_token_cancelled() on a ctx that's about to be dropped
        // 4. Next turn creates a fresh OrchestrationContext with empty cancelled_tokens
        // So dehydration drops write to a dying context that no one will read.
        self.collect_cancelled_from_context(&ctx);

        TurnResult::Continue
    }

    /// Collect cancelled activity/sub-orchestration IDs from OrchestrationContext
    fn collect_cancelled_from_context(&mut self, ctx: &OrchestrationContext) {
        // Collect cancelled activity IDs (already schedule_ids via token binding)
        self.cancelled_activity_ids.extend(ctx.get_cancelled_activity_ids());

        // Collect cancelled sub-orchestration instance IDs
        // The context now directly returns resolved instance IDs via the token→instance mapping
        self.cancelled_sub_orchestration_ids
            .extend(ctx.get_cancelled_sub_orchestration_ids());

        // Clear the context's cancelled tokens (avoid re-processing if called again)
        ctx.clear_cancelled_tokens();
    }

    /// Helper to match and bind schedule events.
    /// Returns `Ok(token)` on success, `Err(TurnResult)` on failure.
    fn match_and_bind_schedule(
        &self,
        ctx: &OrchestrationContext,
        emitted_actions: &mut VecDeque<(u64, Action)>,
        open_schedules: &mut HashSet<u64>,
        schedule_kinds: &mut std::collections::HashMap<u64, ActionKind>,
        event: &Event,
        expected_kind: ActionKind,
    ) -> Result<u64, TurnResult> {
        let (token, action) = match emitted_actions.pop_front() {
            Some(a) => a,
            None => {
                return Err(TurnResult::Failed(nondeterminism_error(
                    "history schedule but no emitted action",
                )));
            }
        };

        // Validate action matches event
        if !action_matches_event_kind(&action, &event.kind) {
            return Err(TurnResult::Failed(nondeterminism_error(&format!(
                "schedule mismatch: action={:?} vs event={:?}",
                action, event.kind
            ))));
        }

        // Bind token to schedule_id
        ctx.bind_token(token, event.event_id());
        open_schedules.insert(event.event_id());
        schedule_kinds.insert(event.event_id(), expected_kind);

        Ok(token)
    }
}

impl ReplayEngine {
    // Getter methods for atomic execution
    pub fn history_delta(&self) -> &[Event] {
        &self.history_delta
    }

    pub fn pending_actions(&self) -> &[crate::Action] {
        &self.pending_actions
    }

    pub fn cancelled_activity_ids(&self) -> &[u64] {
        &self.cancelled_activity_ids
    }

    pub fn cancelled_sub_orchestration_ids(&self) -> &[String] {
        &self.cancelled_sub_orchestration_ids
    }

    /// Check if this run made any progress (added history)
    pub fn made_progress(&self) -> bool {
        !self.history_delta.is_empty()
    }

    /// Get the final history after this run
    pub fn final_history(&self) -> Vec<Event> {
        let mut final_hist = self.baseline_history.clone();
        final_hist.extend_from_slice(&self.history_delta);
        final_hist
    }
}

// === Helper types and functions ===

/// Action kind for tracking schedule types
/// NOTE: Fields are intentionally unused - we only match on variant type, not field values
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum ActionKind {
    Activity {
        name: String,
        input: String,
    },
    Timer {
        fire_at_ms: u64,
    },
    External {
        name: String,
    },
    SubOrch {
        name: String,
        instance: String,
        input: String,
    },
    System {
        op: String,
    },
}

/// Create a nondeterminism error
fn nondeterminism_error(msg: &str) -> crate::ErrorDetails {
    crate::ErrorDetails::Configuration {
        kind: crate::ConfigErrorKind::Nondeterminism,
        resource: String::new(),
        message: Some(msg.to_string()),
    }
}

/// Extract panic message from payload
fn extract_panic_message(panic_payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic_payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "orchestration panicked".to_string()
    }
}

/// Convert an Action to an Event for history persistence
fn action_to_event(action: &Action, instance: &str, execution_id: u64, event_id: u64) -> Option<Event> {
    let kind = match action {
        Action::CallActivity { name, input, .. } => EventKind::ActivityScheduled {
            name: name.clone(),
            input: input.clone(),
        },
        Action::CreateTimer { fire_at_ms, .. } => EventKind::TimerCreated {
            fire_at_ms: *fire_at_ms,
        },
        Action::WaitExternal { name, .. } => EventKind::ExternalSubscribed { name: name.clone() },
        Action::StartSubOrchestration {
            name,
            instance: sub_instance,
            input,
            ..
        } => EventKind::SubOrchestrationScheduled {
            name: name.clone(),
            instance: sub_instance.clone(),
            input: input.clone(),
        },
        Action::SystemCall { op, value, .. } => EventKind::SystemCall {
            op: op.clone(),
            value: value.clone(),
        },
        // These don't become schedule events
        Action::ContinueAsNew { .. } | Action::StartOrchestrationDetached { .. } => {
            return None;
        }
    };

    Some(Event::with_event_id(event_id, instance, execution_id, None, kind))
}

/// Update an action's scheduling_event_id to the correct event_id.
/// Also generates the actual sub-orchestration instance ID from the event_id
/// (unless an explicit instance ID was provided, indicated by not starting with SUB_ORCH_PENDING_PREFIX).
fn update_action_event_id(action: Action, event_id: u64) -> Action {
    match action {
        Action::CallActivity { name, input, .. } => Action::CallActivity {
            scheduling_event_id: event_id,
            name,
            input,
        },
        Action::CreateTimer { fire_at_ms, .. } => Action::CreateTimer {
            scheduling_event_id: event_id,
            fire_at_ms,
        },
        Action::WaitExternal { name, .. } => Action::WaitExternal {
            scheduling_event_id: event_id,
            name,
        },
        Action::StartSubOrchestration {
            name,
            instance,
            input,
            version,
            ..
        } => {
            // If instance starts with the pending prefix, it's a placeholder that needs to be replaced.
            // Otherwise, it's an explicit instance ID provided by the user.
            let final_instance = if instance.starts_with(crate::SUB_ORCH_PENDING_PREFIX) {
                format!("{}{event_id}", crate::SUB_ORCH_AUTO_PREFIX)
            } else {
                instance
            };
            Action::StartSubOrchestration {
                scheduling_event_id: event_id,
                name,
                instance: final_instance,
                input,
                version,
            }
        }
        Action::SystemCall { op, value, .. } => Action::SystemCall {
            scheduling_event_id: event_id,
            op,
            value,
        },
        // These don't have scheduling_event_id
        Action::ContinueAsNew { .. } | Action::StartOrchestrationDetached { .. } => action,
    }
}

/// Poll a future once
fn poll_once<F: Future>(fut: Pin<&mut F>) -> Poll<F::Output> {
    // Create a no-op waker
    static VTABLE: RawWakerVTable =
        RawWakerVTable::new(|_| RawWaker::new(std::ptr::null(), &VTABLE), |_| {}, |_| {}, |_| {});
    let raw_waker = RawWaker::new(std::ptr::null(), &VTABLE);
    let waker = unsafe { Waker::from_raw(raw_waker) };
    let mut cx = Context::from_waker(&waker);
    fut.poll(&mut cx)
}

/// Check if an action matches an event kind
fn action_matches_event_kind(action: &Action, event_kind: &EventKind) -> bool {
    match (action, event_kind) {
        (Action::CallActivity { name, input, .. }, EventKind::ActivityScheduled { name: en, input: ei }) => {
            name == en && input == ei
        }

        (Action::CreateTimer { fire_at_ms, .. }, EventKind::TimerCreated { fire_at_ms: ef }) => {
            // Allow some tolerance for timer fire_at_ms since it's computed at different times
            // In practice, the replay should use the exact value from history
            let _ = fire_at_ms;
            let _ = ef;
            true // Timers match by position, not by exact fire_at_ms
        }

        (Action::WaitExternal { name, .. }, EventKind::ExternalSubscribed { name: en }) => name == en,

        (
            Action::StartSubOrchestration { name, input, .. },
            EventKind::SubOrchestrationScheduled {
                name: en, input: ei, ..
            },
        ) => name == en && input == ei,

        (Action::SystemCall { op, .. }, EventKind::SystemCall { op: eop, .. }) => op == eop,

        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, EventKind};

    #[test]
    fn test_engine_creation() {
        let engine = ReplayEngine::new(
            "test-instance".to_string(),
            1, // execution_id
            vec![Event::with_event_id(
                0,
                "test-instance",
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "test-orch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "test-input".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
        );

        assert_eq!(engine.instance, "test-instance");
        assert!(engine.history_delta.is_empty());
        assert!(!engine.made_progress());
    }
}
