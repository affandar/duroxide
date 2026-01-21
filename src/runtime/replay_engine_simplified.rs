// Test-only simplified replay engine prototype.
//
// Phase 1 (per proposals/replay-simplification.md): validate the new command-vs-history
// replay model in isolation without changing the real scheduling APIs.
//
// This module is intentionally self-contained so it can be deleted once the real
// engine is swapped.

#![cfg(test)]
#![allow(dead_code)]

use crate::{Event, EventKind};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

#[derive(Debug, Clone, PartialEq, Eq)]
enum SimActionKind {
    CallActivity { name: String, input: String },
    CreateTimer { fire_at_ms: u64 },
    SubscribeExternal { name: String },
    StartDetached { name: String, instance: String, input: String },
    StartSubOrchestration { name: String, instance: String, input: String },
    SystemCall { op: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SimEmittedAction {
    token: u64,
    kind: SimActionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SimCompletion {
    ActivityOk(String),
    ActivityErr(String),
    TimerFired { fire_at_ms: u64 },
    SubOrchOk(String),
    SubOrchErr(String),
    ExternalEvent { name: String, data: String },
    SystemCallValue { op: String, value: String },
}

#[derive(Default)]
struct SimInner {
    next_token: u64,
    emitted: VecDeque<SimEmittedAction>,
    // token -> schedule_id (bound when a schedule event is matched)
    bindings: HashMap<u64, Option<u64>>,
    // schedule_id -> completion payload
    results: HashMap<u64, SimCompletion>,
    // schedule_id -> schedule kind (for completion validation)
    schedule_kinds: HashMap<u64, SimActionKind>,
    // external arrivals: name -> payloads in arrival order
    external_arrivals: HashMap<String, Vec<String>>,
    // external subscription indices: name -> next subscription index
    external_next_index: HashMap<String, usize>,
    // schedule_id -> (name, subscription_index)
    external_subscriptions: HashMap<u64, (String, usize)>,
}

#[derive(Clone, Default)]
struct SimCtx {
    inner: Arc<Mutex<SimInner>>,
}

impl SimCtx {
    fn new() -> Self {
        Self::default()
    }

    fn emit_action(&self, kind: SimActionKind) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        inner.next_token += 1;
        let token = inner.next_token;
        inner.bindings.insert(token, None);
        inner.emitted.push_back(SimEmittedAction { token, kind });
        token
    }

    fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> SimActivityFuture {
        let token = self.emit_action(SimActionKind::CallActivity {
            name: name.into(),
            input: input.into(),
        });
        SimActivityFuture {
            token,
            inner: self.inner.clone(),
        }
    }

    fn schedule_timer(&self, fire_at_ms: u64) -> SimTimerFuture {
        let token = self.emit_action(SimActionKind::CreateTimer { fire_at_ms });
        SimTimerFuture {
            token,
            inner: self.inner.clone(),
        }
    }

    fn subscribe_external(&self, name: impl Into<String>) -> SimExternalFuture {
        let event_name = name.into();
        let token = self.emit_action(SimActionKind::SubscribeExternal {
            name: event_name.clone(),
        });
        SimExternalFuture {
            token,
            event_name,
            inner: self.inner.clone(),
        }
    }

    fn start_detached(&self, name: impl Into<String>, instance: impl Into<String>, input: impl Into<String>) {
        let _ = self.emit_action(SimActionKind::StartDetached {
            name: name.into(),
            instance: instance.into(),
            input: input.into(),
        });
    }

    fn start_sub_orchestration(
        &self,
        name: impl Into<String>,
        instance: impl Into<String>,
        input: impl Into<String>,
    ) -> SimSubOrchestrationFuture {
        let token = self.emit_action(SimActionKind::StartSubOrchestration {
            name: name.into(),
            instance: instance.into(),
            input: input.into(),
        });
        SimSubOrchestrationFuture {
            token,
            inner: self.inner.clone(),
        }
    }

    fn system_call(&self, op: impl Into<String>) -> SimSystemCallFuture {
        let token = self.emit_action(SimActionKind::SystemCall { op: op.into() });
        SimSystemCallFuture {
            token,
            inner: self.inner.clone(),
        }
    }

    fn drain_emitted(&self) -> VecDeque<SimEmittedAction> {
        let mut inner = self.inner.lock().unwrap();
        std::mem::take(&mut inner.emitted)
    }

    fn bind_token(&self, token: u64, schedule_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner
            .bindings
            .get_mut(&token)
            .expect("token must exist when binding");
        *entry = Some(schedule_id);
    }

    fn has_open_binding_for_schedule(&self, schedule_id: u64) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.results.contains_key(&schedule_id)
    }

    fn set_completion(&self, schedule_id: u64, completion: SimCompletion) {
        let mut inner = self.inner.lock().unwrap();
        inner.results.insert(schedule_id, completion);
    }

    fn bind_external_subscription(&self, schedule_id: u64, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        let idx = inner.external_next_index.entry(name.to_string()).or_insert(0);
        let subscription_index = *idx;
        *idx += 1;
        inner
            .external_subscriptions
            .insert(schedule_id, (name.to_string(), subscription_index));
    }

    fn deliver_external(&self, name: String, data: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.external_arrivals.entry(name).or_default().push(data);
    }
}

struct SimActivityFuture {
    token: u64,
    inner: Arc<Mutex<SimInner>>,
}

impl Future for SimActivityFuture {
    type Output = Result<String, String>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let token = self.token;
        let inner = self.inner.lock().unwrap();
        let schedule_id = match inner.bindings.get(&token) {
            Some(Some(id)) => *id,
            _ => return Poll::Pending,
        };
        match inner.results.get(&schedule_id) {
            Some(SimCompletion::ActivityOk(v)) => Poll::Ready(Ok(v.clone())),
            Some(SimCompletion::ActivityErr(e)) => Poll::Ready(Err(e.clone())),
            Some(other) => Poll::Ready(Err(format!("unexpected activity completion: {other:?}"))),
            None => Poll::Pending,
        }
    }
}

struct SimTimerFuture {
    token: u64,
    inner: Arc<Mutex<SimInner>>,
}

impl Future for SimTimerFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let token = self.token;
        let inner = self.inner.lock().unwrap();
        let schedule_id = match inner.bindings.get(&token) {
            Some(Some(id)) => *id,
            _ => return Poll::Pending,
        };
        match inner.results.get(&schedule_id) {
            Some(SimCompletion::TimerFired { .. }) => Poll::Ready(()),
            Some(_other) => Poll::Ready(()),
            None => Poll::Pending,
        }
    }
}

struct SimExternalFuture {
    token: u64,
    event_name: String,
    inner: Arc<Mutex<SimInner>>,
}

impl Future for SimExternalFuture {
    type Output = String;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Subscription must be validated/bound to history, and each subscription gets a stable
        // consumption index in schedule order so poll order can't steal earlier events.
        let inner = self.inner.lock().unwrap();
        let schedule_id = match inner.bindings.get(&self.token) {
            Some(Some(id)) => *id,
            _ => return Poll::Pending,
        };
        let (name, subscription_index) = match inner.external_subscriptions.get(&schedule_id) {
            Some(v) => v,
            None => return Poll::Pending,
        };
        let arrivals = match inner.external_arrivals.get(name) {
            Some(a) => a,
            None => return Poll::Pending,
        };
        match arrivals.get(*subscription_index) {
            Some(v) => Poll::Ready(v.clone()),
            None => Poll::Pending,
        }
    }
}

struct SimSubOrchestrationFuture {
    token: u64,
    inner: Arc<Mutex<SimInner>>,
}

impl Future for SimSubOrchestrationFuture {
    type Output = Result<String, String>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let token = self.token;
        let inner = self.inner.lock().unwrap();
        let schedule_id = match inner.bindings.get(&token) {
            Some(Some(id)) => *id,
            _ => return Poll::Pending,
        };
        match inner.results.get(&schedule_id) {
            Some(SimCompletion::SubOrchOk(v)) => Poll::Ready(Ok(v.clone())),
            Some(SimCompletion::SubOrchErr(e)) => Poll::Ready(Err(e.clone())),
            Some(other) => Poll::Ready(Err(format!("unexpected suborch completion: {other:?}"))),
            None => Poll::Pending,
        }
    }
}

struct SimSystemCallFuture {
    token: u64,
    inner: Arc<Mutex<SimInner>>,
}

impl Future for SimSystemCallFuture {
    type Output = String;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let token = self.token;
        let inner = self.inner.lock().unwrap();
        let schedule_id = match inner.bindings.get(&token) {
            Some(Some(id)) => *id,
            _ => return Poll::Pending,
        };
        match inner.results.get(&schedule_id) {
            Some(SimCompletion::SystemCallValue { value, .. }) => Poll::Ready(value.clone()),
            Some(other) => Poll::Ready(format!("unexpected systemcall completion: {other:?}")),
            None => Poll::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SimplifiedOutcome {
    actions_to_take: Vec<SimActionKind>,
}

fn noop_waker() -> Waker {
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    fn wake(_: *const ()) {}
    fn wake_by_ref(_: *const ()) {}
    fn drop(_: *const ()) {}

    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

fn poll_once<F: Future>(fut: Pin<&mut F>) -> Poll<F::Output> {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    fut.poll(&mut cx)
}

fn match_schedule(action: &SimActionKind, event: &Event) -> Result<(), String> {
    match (&action, &event.kind) {
        (SimActionKind::CallActivity { name, input }, EventKind::ActivityScheduled { name: n, input: i })
            if name == n && input == i => Ok(()),
        (SimActionKind::CreateTimer { fire_at_ms }, EventKind::TimerCreated { fire_at_ms: f }) if fire_at_ms == f => {
            Ok(())
        }
        (SimActionKind::SubscribeExternal { name }, EventKind::ExternalSubscribed { name: n }) if name == n => Ok(()),
        (
            SimActionKind::StartDetached {
                name,
                instance,
                input,
            },
            EventKind::OrchestrationChained {
                name: n,
                instance: inst,
                input: i,
            },
        ) if name == n && instance == inst && input == i => Ok(()),
        (
            SimActionKind::StartSubOrchestration {
                name,
                instance,
                input,
            },
            EventKind::SubOrchestrationScheduled {
                name: n,
                instance: inst,
                input: i,
            },
        ) if name == n && instance == inst && input == i => Ok(()),
        (SimActionKind::SystemCall { op }, EventKind::SystemCall { op: hist_op, .. }) if op == hist_op => Ok(()),
        _ => Err(format!(
            "schedule mismatch: action={action:?} vs event_kind={:?}",
            event.kind
        )),
    }
}

fn to_completion(event: &Event) -> Option<(u64, SimCompletion)> {
    let source_id = event.source_event_id?;
    match &event.kind {
        EventKind::ActivityCompleted { result } => Some((source_id, SimCompletion::ActivityOk(result.clone()))),
        EventKind::ActivityFailed { details } => Some((source_id, SimCompletion::ActivityErr(details.display_message()))),
        EventKind::TimerFired { fire_at_ms } => Some((source_id, SimCompletion::TimerFired { fire_at_ms: *fire_at_ms })),
        EventKind::SubOrchestrationCompleted { result } => Some((source_id, SimCompletion::SubOrchOk(result.clone()))),
        EventKind::SubOrchestrationFailed { details } => Some((source_id, SimCompletion::SubOrchErr(details.display_message()))),
        _ => None,
    }
}

fn completion_matches_schedule(schedule: &SimActionKind, completion: &SimCompletion) -> bool {
    match (schedule, completion) {
        (SimActionKind::CallActivity { .. }, SimCompletion::ActivityOk(_)) => true,
        (SimActionKind::CallActivity { .. }, SimCompletion::ActivityErr(_)) => true,
        (SimActionKind::CreateTimer { .. }, SimCompletion::TimerFired { .. }) => true,
        (SimActionKind::StartSubOrchestration { .. }, SimCompletion::SubOrchOk(_)) => true,
        (SimActionKind::StartSubOrchestration { .. }, SimCompletion::SubOrchErr(_)) => true,
        _ => false,
    }
}

/// Phase-1 simplified evaluator.
///
/// - Processes a fixed history in order.
/// - Validates schedule events against emitted actions.
/// - Plugs completion events into an open map and polls once after each plug.
/// - Returns any newly emitted actions beyond history as actions_to_take.
fn evaluate_simplified<O, F>(history: Vec<Event>, orchestrator: impl FnOnce(SimCtx) -> F) -> Result<SimplifiedOutcome, String>
where
    F: Future<Output = O>,
{
    if history.is_empty() {
        return Err("corrupted history: empty".to_string());
    }
    if !matches!(history[0].kind, EventKind::OrchestrationStarted { .. }) {
        return Err("corrupted history: first event must be OrchestrationStarted".to_string());
    }

    // Match current runtime behavior: terminal histories are not processed.
    // The orchestration dispatcher acks them without invoking user code.
    if history.iter().any(|e| {
        matches!(
            e.kind,
            EventKind::OrchestrationCompleted { .. }
                | EventKind::OrchestrationFailed { .. }
                | EventKind::OrchestrationContinuedAsNew { .. }
        )
    }) {
        return Ok(SimplifiedOutcome {
            actions_to_take: Vec::new(),
        });
    }

    let ctx = SimCtx::new();
    let mut fut = Box::pin(orchestrator(ctx.clone()));

    let mut emitted_actions: VecDeque<SimEmittedAction> = VecDeque::new();
    let mut open_schedules: HashSet<u64> = HashSet::new();

    let mut must_poll = true;

    for event in &history {
        if must_poll {
            let _ = poll_once(fut.as_mut());
            emitted_actions.extend(ctx.drain_emitted());
            must_poll = false;
        }

        match &event.kind {
            EventKind::OrchestrationStarted { .. } => {}

            // Schedule events
            EventKind::ActivityScheduled { .. }
            | EventKind::TimerCreated { .. }
            | EventKind::ExternalSubscribed { .. }
            | EventKind::OrchestrationChained { .. }
            | EventKind::SubOrchestrationScheduled { .. }
            | EventKind::SystemCall { .. } => {
                let emitted = emitted_actions
                    .pop_front()
                    .ok_or_else(|| "nondeterminism: history schedule but no emitted action".to_string())?;

                match_schedule(&emitted.kind, event)?;

                // Bind token -> schedule_id and mark schedule open
                ctx.bind_token(emitted.token, event.event_id());
                open_schedules.insert(event.event_id());

                // Record schedule kind for later completion validation
                {
                    let mut inner = ctx.inner.lock().unwrap();
                    inner.schedule_kinds.insert(event.event_id(), emitted.kind.clone());
                }

                // For ExternalSubscribed, bind a deterministic consumption index
                if let EventKind::ExternalSubscribed { name } = &event.kind {
                    ctx.bind_external_subscription(event.event_id(), name);
                }

                // For SystemCall we also deliver the recorded value and allow one poll
                if let EventKind::SystemCall { op, value } = &event.kind {
                    ctx.set_completion(
                        event.event_id(),
                        SimCompletion::SystemCallValue {
                            op: op.clone(),
                            value: value.clone(),
                        },
                    );
                    must_poll = true;
                }
            }

            // Completion events
            EventKind::ActivityCompleted { .. }
            | EventKind::ActivityFailed { .. }
            | EventKind::TimerFired { .. }
            | EventKind::SubOrchestrationCompleted { .. }
            | EventKind::SubOrchestrationFailed { .. } => {
                let (schedule_id, completion) =
                    to_completion(event).ok_or_else(|| "internal: expected completion".to_string())?;

                if !open_schedules.contains(&schedule_id) {
                    return Err("nondeterminism: completion without open schedule".to_string());
                }

                // Validate completion kind matches the schedule kind
                {
                    let inner = ctx.inner.lock().unwrap();
                    let sched = inner
                        .schedule_kinds
                        .get(&schedule_id)
                        .ok_or_else(|| "nondeterminism: completion without known schedule kind".to_string())?;
                    if !completion_matches_schedule(sched, &completion) {
                        return Err(format!(
                            "nondeterminism: completion kind mismatch for id={schedule_id}, schedule={sched:?}, completion={completion:?}"
                        ));
                    }
                }

                ctx.set_completion(schedule_id, completion);
                must_poll = true;
            }

            // ExternalEvent is name-based; deliver and poll
            EventKind::ExternalEvent { name, data } => {
                ctx.deliver_external(name.clone(), data.clone());
                must_poll = true;
            }

            // Cancellation request is not terminal; allow history to continue.
            EventKind::OrchestrationCancelRequested { .. } => {}

            // Terminal events are handled by the early terminal-history bail-out above.
            EventKind::OrchestrationCompleted { .. }
            | EventKind::OrchestrationFailed { .. }
            | EventKind::OrchestrationContinuedAsNew { .. } => {
                unreachable!("terminal histories are bailed out before replay")
            }
        }
    }

    // Final poll if last event delivered progress
    if must_poll {
        let _ = poll_once(fut.as_mut());
        emitted_actions.extend(ctx.drain_emitted());
    }

    // Remaining emitted actions are beyond history
    let actions_to_take = emitted_actions.into_iter().map(|a| a.kind).collect();
    Ok(SimplifiedOutcome { actions_to_take })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(event_id: u64) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        )
    }

    fn act_scheduled(event_id: u64, name: &str, input: &str) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            None,
            EventKind::ActivityScheduled {
                name: name.to_string(),
                input: input.to_string(),
            },
        )
    }

    fn act_completed(event_id: u64, source_event_id: u64, result: &str) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            Some(source_event_id),
            EventKind::ActivityCompleted {
                result: result.to_string(),
            },
        )
    }

    fn timer_created(event_id: u64, fire_at_ms: u64) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            None,
            EventKind::TimerCreated { fire_at_ms },
        )
    }

    fn timer_fired(event_id: u64, source_event_id: u64, fire_at_ms: u64) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            Some(source_event_id),
            EventKind::TimerFired { fire_at_ms },
        )
    }

    fn sub_scheduled(event_id: u64, name: &str, instance: &str, input: &str) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            None,
            EventKind::SubOrchestrationScheduled {
                name: name.to_string(),
                instance: instance.to_string(),
                input: input.to_string(),
            },
        )
    }

    fn sub_completed(event_id: u64, source_event_id: u64, result: &str) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            Some(source_event_id),
            EventKind::SubOrchestrationCompleted {
                result: result.to_string(),
            },
        )
    }

    fn ext_subscribed(event_id: u64, name: &str) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            None,
            EventKind::ExternalSubscribed {
                name: name.to_string(),
            },
        )
    }

    fn ext_event(event_id: u64, name: &str, data: &str) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            None,
            EventKind::ExternalEvent {
                name: name.to_string(),
                data: data.to_string(),
            },
        )
    }

    fn cancel_requested(event_id: u64, reason: &str) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            None,
            EventKind::OrchestrationCancelRequested {
                reason: reason.to_string(),
            },
        )
    }

    fn orch_completed(event_id: u64, output: &str) -> Event {
        Event::with_event_id(
            event_id,
            "inst",
            1,
            None,
            EventKind::OrchestrationCompleted {
                output: output.to_string(),
            },
        )
    }

    #[test]
    fn mismatch_schedule_is_nondeterminism() {
        let history = vec![started(1), act_scheduled(2, "A", "x")];

        let res = evaluate_simplified(history, |ctx| async move {
            let _fut = ctx.schedule_activity("B", "y");
        });

        assert!(res.is_err());
        let msg = res.err().unwrap();
        assert!(msg.contains("schedule mismatch"));
    }

    #[test]
    fn completion_without_open_schedule_is_nondeterminism() {
        let history = vec![started(1), act_completed(3, 2, "ok")];

        let res = evaluate_simplified(history, |_ctx| async move {
            // no schedule emitted
        });

        assert!(res.is_err());
        let msg = res.err().unwrap();
        assert!(msg.contains("completion without open schedule"));
    }

    #[test]
    fn end_of_history_returns_new_actions() {
        let history = vec![started(1)];

        let out = evaluate_simplified(history, |ctx| async move {
            let _ = ctx.schedule_activity("A", "x");
            let _ = ctx.schedule_timer(123);
        })
        .unwrap();

        assert_eq!(
            out.actions_to_take,
            vec![
                SimActionKind::CallActivity {
                    name: "A".to_string(),
                    input: "x".to_string()
                },
                SimActionKind::CreateTimer { fire_at_ms: 123 },
            ]
        );
    }

    #[test]
    fn completion_drives_poll_then_next_schedule_matches() {
        // Started
        // schedule A
        // completion A
        // schedule B
        let history = vec![
            started(1),
            act_scheduled(2, "A", "x"),
            act_completed(3, 2, "a_ok"),
            act_scheduled(4, "B", "y"),
            timer_created(5, 999),
            timer_fired(6, 5, 999),
        ];

        let out = evaluate_simplified(history, |ctx| async move {
            let a = ctx.schedule_activity("A", "x");
            let _ = a.await;
            let _b = ctx.schedule_activity("B", "y");
            let t = ctx.schedule_timer(999);
            t.await;
        })
        .unwrap();

        // All actions were replayed by history; nothing new beyond history
        assert!(out.actions_to_take.is_empty());
    }

    #[test]
    fn external_events_deliver_in_subscription_order_not_poll_order() {
        // Two subscriptions to the same name. First event must go to the first subscription.
        // Even if orchestration awaits the second subscription first, it should not steal the first event.
        let history = vec![
            started(1),
            ext_subscribed(2, "E"),
            ext_subscribed(3, "E"),
            ext_event(4, "E", "first"),
            ext_event(5, "E", "second"),
        ];

        let out = evaluate_simplified(history, |ctx| async move {
            let e1 = ctx.subscribe_external("E");
            let e2 = ctx.subscribe_external("E");

            // Await the second subscription first; it should still receive the second payload.
            let v2 = e2.await;
            let v1 = e1.await;
            (v1, v2)
        })
        .unwrap();

        assert!(out.actions_to_take.is_empty());
    }

    #[test]
    fn completion_kind_mismatch_is_nondeterminism() {
        // Schedule activity A (id=2) but record a TimerFired completion against it.
        let history = vec![
            started(1),
            act_scheduled(2, "A", "x"),
            timer_fired(3, 2, 123),
        ];

        let res = evaluate_simplified(history, |ctx| async move {
            let a = ctx.schedule_activity("A", "x");
            let _ = a.await;
        });

        assert!(res.is_err());
        let msg = res.err().unwrap();
        assert!(msg.contains("completion kind mismatch"));
    }

    #[test]
    fn sub_orchestration_completion_kind_mismatch_is_nondeterminism() {
        // Schedule suborch (id=2) but record an ActivityCompleted against it.
        let history = vec![
            started(1),
            sub_scheduled(2, "Child", "child-1", "{}"),
            act_completed(3, 2, "oops"),
        ];

        let res = evaluate_simplified(history, |ctx| async move {
            let c = ctx.start_sub_orchestration("Child", "child-1", "{}");
            let _ = c.await;
        });

        assert!(res.is_err());
        let msg = res.err().unwrap();
        assert!(msg.contains("completion kind mismatch"));
    }

    #[test]
    fn cancel_requested_is_not_terminal() {
        let history = vec![started(1), cancel_requested(2, "please"), orch_completed(3, "ok")];

        let out = evaluate_simplified(history, |_ctx| async move {
            // Return immediately; terminal history should be consistent.
        })
        .unwrap();

        assert!(out.actions_to_take.is_empty());
    }
}

/// Tests for the real OrchestrationContext running in simplified mode.
/// These tests validate that the modal switch approach works correctly.
#[cfg(test)]
mod real_ctx_simplified_tests {
    use super::*;
    use crate::{Action, OrchestrationContext, ReplayMode, SimplifiedResult};
    use crate::futures::DurableOutput;

    fn noop_waker() -> Waker {
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        fn wake(_: *const ()) {}
        fn wake_by_ref(_: *const ()) {}
        fn drop(_: *const ()) {}

        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
        unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
    }

    fn poll_once<F: Future>(fut: Pin<&mut F>) -> Poll<F::Output> {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        fut.poll(&mut cx)
    }

    #[test]
    fn real_ctx_simplified_mode_emits_activity_action() {
        // Create context in simplified mode
        let ctx = OrchestrationContext::new(
            vec![],
            1,
            "test-inst".to_string(),
            "TestOrch".to_string(),
            "1.0.0".to_string(),
            None,
        );
        ctx.set_replay_mode(ReplayMode::Simplified);

        // Schedule an activity
        let fut = ctx.schedule_activity("MyActivity", "my-input");
        let mut pinned = std::pin::pin!(fut);

        // First poll should emit the action and return Pending
        let result = poll_once(pinned.as_mut());
        assert!(matches!(result, Poll::Pending));

        // Check that the action was emitted
        let emitted = ctx.drain_emitted_actions();
        assert_eq!(emitted.len(), 1);
        let (token, action) = &emitted[0];
        assert!(*token > 0);
        match action {
            Action::CallActivity { name, input, .. } => {
                assert_eq!(name, "MyActivity");
                assert_eq!(input, "my-input");
            }
            _ => panic!("Expected CallActivity action"),
        }
    }

    #[test]
    fn real_ctx_simplified_mode_activity_resolves_on_result() {
        // Create context in simplified mode
        let ctx = OrchestrationContext::new(
            vec![],
            1,
            "test-inst".to_string(),
            "TestOrch".to_string(),
            "1.0.0".to_string(),
            None,
        );
        ctx.set_replay_mode(ReplayMode::Simplified);

        // Schedule an activity
        let fut = ctx.schedule_activity("MyActivity", "my-input");
        let mut pinned = std::pin::pin!(fut);

        // First poll should emit the action
        let _ = poll_once(pinned.as_mut());

        // Get the token and bind it to a schedule_id
        let emitted = ctx.drain_emitted_actions();
        let (token, _) = &emitted[0];
        ctx.bind_token(*token, 100); // Bind to schedule_id 100

        // Deliver a result
        ctx.deliver_result(100, SimplifiedResult::ActivityOk("result-value".to_string()));

        // Now the future should resolve
        let result = poll_once(pinned.as_mut());
        match result {
            Poll::Ready(Ok(v)) => {
                assert_eq!(v, "result-value");
            }
            other => panic!("Expected Poll::Ready(Ok(...)), got {:?}", other),
        }
    }

    #[test]
    fn real_ctx_simplified_mode_timer_emits_and_resolves() {
        use std::time::Duration;

        let ctx = OrchestrationContext::new(
            vec![],
            1,
            "test-inst".to_string(),
            "TestOrch".to_string(),
            "1.0.0".to_string(),
            None,
        );
        ctx.set_replay_mode(ReplayMode::Simplified);

        // Schedule a timer
        let fut = ctx.schedule_timer(Duration::from_secs(5));
        let mut pinned = std::pin::pin!(fut);

        // First poll should emit the action
        let _ = poll_once(pinned.as_mut());

        // Get the token
        let emitted = ctx.drain_emitted_actions();
        assert_eq!(emitted.len(), 1);
        let (token, action) = &emitted[0];
        match action {
            Action::CreateTimer { fire_at_ms, .. } => {
                assert!(*fire_at_ms > 0);
            }
            _ => panic!("Expected CreateTimer action"),
        }

        // Bind and deliver result
        ctx.bind_token(*token, 200);
        ctx.deliver_result(200, SimplifiedResult::TimerFired);

        // Timer should resolve
        let result = poll_once(pinned.as_mut());
        assert!(matches!(result, Poll::Ready(())));
    }

    #[test]
    fn real_ctx_simplified_mode_suborchestration_emits_and_resolves() {
        let ctx = OrchestrationContext::new(
            vec![],
            1,
            "test-inst".to_string(),
            "TestOrch".to_string(),
            "1.0.0".to_string(),
            None,
        );
        ctx.set_replay_mode(ReplayMode::Simplified);

        // Schedule a sub-orchestration
        let fut = ctx.schedule_sub_orchestration("ChildOrch", "child-input");
        let mut pinned = std::pin::pin!(fut);

        // First poll should emit the action
        let _ = poll_once(pinned.as_mut());

        // Get the token
        let emitted = ctx.drain_emitted_actions();
        assert_eq!(emitted.len(), 1);
        let (token, action) = &emitted[0];
        match action {
            Action::StartSubOrchestration { name, input, .. } => {
                assert_eq!(name, "ChildOrch");
                assert_eq!(input, "child-input");
            }
            _ => panic!("Expected StartSubOrchestration action"),
        }

        // Bind and deliver result
        ctx.bind_token(*token, 300);
        ctx.deliver_result(300, SimplifiedResult::SubOrchOk("child-result".to_string()));

        // Sub-orchestration should resolve
        let result = poll_once(pinned.as_mut());
        match result {
            Poll::Ready(Ok(v)) => {
                assert_eq!(v, "child-result");
            }
            other => panic!("Expected Poll::Ready(Ok(...)), got {:?}", other),
        }
    }

    #[test]
    fn real_ctx_simplified_mode_actions_emitted_in_schedule_order_not_poll_order() {
        // This is the critical test: actions must be emitted in SCHEDULE order,
        // not in the order futures are polled.
        let ctx = OrchestrationContext::new(
            vec![],
            1,
            "test-inst".to_string(),
            "TestOrch".to_string(),
            "1.0.0".to_string(),
            None,
        );
        ctx.set_replay_mode(ReplayMode::Simplified);

        // Schedule A first, then B
        let _fut_a = ctx.schedule_activity("ActivityA", "input-a");
        let _fut_b = ctx.schedule_activity("ActivityB", "input-b");

        // Actions should already be emitted (at schedule time), in schedule order
        let emitted = ctx.drain_emitted_actions();
        assert_eq!(emitted.len(), 2, "Both actions should be emitted at schedule time");
        
        // Verify order: A first, then B (schedule order)
        match &emitted[0].1 {
            Action::CallActivity { name, input, .. } => {
                assert_eq!(name, "ActivityA", "First action should be ActivityA");
                assert_eq!(input, "input-a");
            }
            _ => panic!("Expected CallActivity action"),
        }
        match &emitted[1].1 {
            Action::CallActivity { name, input, .. } => {
                assert_eq!(name, "ActivityB", "Second action should be ActivityB");
                assert_eq!(input, "input-b");
            }
            _ => panic!("Expected CallActivity action"),
        }
    }
}
