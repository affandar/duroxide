// Mutex poisoning is a critical error indicating a panic in another thread.
// In this module, all expect() calls are for mutex locks, which should panic on poison.
#![allow(clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::{Event, EventKind, OrchestrationContext, CompletionResult};

/// Helper function to check if a completion event can be consumed according to FIFO ordering.
///
/// Returns `true` if all completion events in history that occurred before the given
/// `completion_event_id` have already been consumed (or are cancelled select2 losers).
///
/// # Arguments
/// * `history` - The full event history
/// * `consumed_completions` - Set of already consumed completion event IDs
/// * `cancelled_source_ids` - Set of source_event_ids whose completions should be auto-skipped (select2 losers)
/// * `completion_event_id` - The event ID we want to consume
fn can_consume_completion(
    history: &[Event],
    consumed_completions: &std::collections::HashSet<u64>,
    cancelled_source_ids: &std::collections::HashSet<u64>,
    completion_event_id: u64,
) -> bool {
    history.iter().all(|e| {
        match &e.kind {
            // Completions with source_event_id - check if cancelled
            EventKind::ActivityCompleted { .. }
            | EventKind::ActivityFailed { .. }
            | EventKind::TimerFired { .. }
            | EventKind::SubOrchestrationCompleted { .. }
            | EventKind::SubOrchestrationFailed { .. } => {
                // source_event_id is now on the Event struct
                if let Some(source_event_id) = e.source_event_id {
                    // Cancelled completions (select2 losers) don't block - skip them
                    if cancelled_source_ids.contains(&source_event_id) {
                        return true;
                    }
                }
                // Otherwise: if before ours, must be consumed
                e.event_id >= completion_event_id || consumed_completions.contains(&e.event_id)
            }
            // External events don't have source_event_id - can't be cancelled via select2
            EventKind::ExternalEvent { .. } => {
                e.event_id >= completion_event_id || consumed_completions.contains(&e.event_id)
            }
            _ => true, // Non-completions don't affect ordering
        }
    })
}

#[derive(Debug, Clone)]
pub enum DurableOutput {
    Activity(Result<String, String>),
    Timer,
    External(String),
    SubOrchestration(Result<String, String>),
}

/// A durable future representing an asynchronous operation in an orchestration.
///
/// Common fields `claimed_event_id` and `ctx` are stored directly on the struct,
/// while operation-specific data is stored in the `kind` variant.
pub struct DurableFuture {
    /// The event ID claimed by this future during scheduling (set on first poll) - Legacy mode
    pub(crate) claimed_event_id: Cell<Option<u64>>,
    /// Token for result correlation (set when action is emitted)
    pub(crate) result_token: Cell<Option<u64>>,
    /// Reference to the orchestration context
    pub(crate) ctx: OrchestrationContext,
    /// Operation-specific data
    pub(crate) kind: Kind,
}

#[allow(dead_code)] // Legacy: Kind variants constructed by legacy schedule_* methods
pub(crate) enum Kind {
    Activity {
        name: String,
        input: String,
    },
    Timer {
        delay_ms: u64,
    },
    External {
        name: String,
        result: RefCell<Option<String>>, // Cache result once found
    },
    SubOrch {
        name: String,
        version: Option<String>,
        instance: RefCell<String>, // Updated once event_id is known
        input: String,
    },
    System {
        op: String,
        value: RefCell<Option<String>>,
    },
}

// KindTag removed - no longer needed with cursor model

impl Future for DurableFuture {
    type Output = DurableOutput;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We never move fields that are !Unpin; we only take &mut to mutate inner Cells and use ctx by reference.
        let this = unsafe { self.get_unchecked_mut() };

        // Common fields: this.claimed_event_id, this.result_token, this.ctx
        match &mut this.kind {
            Kind::Activity { name: _, input: _ } => {
                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // Token was set at schedule time - just check for result
                let token = this.result_token.get()
                    .expect("result_token should be set at schedule time");
                if let Some(result) = inner.get_result(token) {
                    return match result {
                        CompletionResult::ActivityOk(v) => {
                            Poll::Ready(DurableOutput::Activity(Ok(v.clone())))
                        }
                        CompletionResult::ActivityErr(e) => {
                            Poll::Ready(DurableOutput::Activity(Err(e.clone())))
                        }
                        _ => Poll::Pending, // Wrong completion type - should not happen
                    };
                }
                Poll::Pending
            }
            Kind::Timer { delay_ms: _ } => {
                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // Token was set at schedule time - just check for result
                let token = this.result_token.get()
                    .expect("result_token should be set at schedule time");
                if let Some(result) = inner.get_result(token) {
                    return match result {
                        CompletionResult::TimerFired => Poll::Ready(DurableOutput::Timer),
                        _ => Poll::Pending, // Wrong completion type - should not happen
                    };
                }
                Poll::Pending
            }
            Kind::External { name: _, result } => {
                // Check if we already have the result cached (legacy mode only)
                if let Some(cached) = result.borrow().clone() {
                    return Poll::Ready(DurableOutput::External(cached));
                }

                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // Token was set at schedule time - just check for result
                let token = this.result_token.get()
                    .expect("result_token should be set at schedule time");
                
                // Check for direct result delivery
                if let Some(res) = inner.get_result(token) {
                    return match res {
                        CompletionResult::ExternalData(data) => {
                            Poll::Ready(DurableOutput::External(data.clone()))
                        }
                        _ => Poll::Pending,
                    };
                }

                // Also check external event by schedule_id (for subscription-based delivery)
                if let Some(schedule_id) = inner.get_bound_schedule_id(token)
                    && let Some(data) = inner.get_external_event(schedule_id)
                {
                    return Poll::Ready(DurableOutput::External(data.clone()));
                }
                Poll::Pending
            }
            Kind::SubOrch {
                name: _,
                version: _,
                instance: _,
                input: _,
            } => {
                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // Token was set at schedule time - just check for result
                let token = this.result_token.get()
                    .expect("result_token should be set at schedule time");
                if let Some(res) = inner.get_result(token) {
                    return match res {
                        CompletionResult::SubOrchOk(v) => {
                            Poll::Ready(DurableOutput::SubOrchestration(Ok(v.clone())))
                        }
                        CompletionResult::SubOrchErr(e) => {
                            Poll::Ready(DurableOutput::SubOrchestration(Err(e.clone())))
                        }
                        _ => Poll::Pending,
                    };
                }
                Poll::Pending
            }
            Kind::System { op: _, value } => {
                // Check if we already computed the value (legacy mode cache)
                if let Some(v) = value.borrow().clone() {
                    return Poll::Ready(DurableOutput::Activity(Ok(v)));
                }

                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // Token was set at schedule time - check for result
                let token = this.result_token.get()
                    .expect("result_token should be set at schedule time");
                if let Some(res) = inner.get_result(token) {
                    return match res {
                        CompletionResult::SystemCallValue(v) => {
                            Poll::Ready(DurableOutput::Activity(Ok(v.clone())))
                        }
                        _ => Poll::Pending,
                    };
                }
                // System calls are synchronous in first execution, so value should be in cache
                // (set by schedule_system_call), but during replay we wait for result delivery
                Poll::Pending
            }
        }
    }
}

// Compile-time contract: DurableFuture must remain Unpin because poll() relies on freely
// projecting &mut self into its internal Kind. This assertion fails if future changes.
#[allow(dead_code)] // Used in const assertion below
const fn assert_unpin<T: Unpin>() {}
const _: () = {
    assert_unpin::<DurableFuture>();
};

// Helper function to generate deterministic GUIDs
pub(crate) fn generate_guid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    // Thread-local counter for uniqueness within the same timestamp
    thread_local! {
        static COUNTER: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let counter = COUNTER.with(|c| {
        let val = c.get();
        c.set(val.wrapping_add(1));
        val
    });

    // Format as UUID-like string
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (timestamp >> 96) as u32,
        ((timestamp >> 80) & 0xFFFF) as u16,
        (counter & 0xFFFF) as u16,
        ((timestamp >> 64) & 0xFFFF) as u16,
        (timestamp & 0xFFFFFFFFFFFF) as u64
    )
}

// Aggregate future machinery (legacy - not used in simplified mode)
#[allow(dead_code)]
enum AggregateMode {
    Select,
    Join,
}

pub enum AggregateOutput {
    Select { winner_index: usize, output: DurableOutput },
    Join { outputs: Vec<DurableOutput> },
}

#[allow(dead_code)] // Legacy: AggregateDurableFuture used by legacy ctx.select/join
pub struct AggregateDurableFuture {
    ctx: OrchestrationContext,
    children: Vec<DurableFuture>,
    mode: AggregateMode,
}

#[allow(dead_code)] // Legacy: new_select/new_join used by legacy ctx.select/join
impl AggregateDurableFuture {
    pub(crate) fn new_select(ctx: OrchestrationContext, children: Vec<DurableFuture>) -> Self {
        Self {
            ctx,
            children,
            mode: AggregateMode::Select,
        }
    }
    pub(crate) fn new_join(ctx: OrchestrationContext, children: Vec<DurableFuture>) -> Self {
        Self {
            ctx,
            children,
            mode: AggregateMode::Join,
        }
    }

    // Note: Unconsumed completion detection removed - the cursor model naturally
    // handles this via strict sequential consumption. Any unconsumed completions
    // will cause a panic when the next future tries to poll.
}

impl Future for AggregateDurableFuture {
    type Output = AggregateOutput;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };

        match this.mode {
            AggregateMode::Select => {
                // Poll all children, return first ready
                // Cancellation of losers is handled by the runtime
                let mut ready_results: Vec<Option<DurableOutput>> = vec![None; this.children.len()];
                for (i, child) in this.children.iter_mut().enumerate() {
                    if let Poll::Ready(output) = Pin::new(child).poll(cx) {
                        ready_results[i] = Some(output);
                    }
                }

                // Return first ready (biased toward lower indices)
                if let Some(winner_idx) = ready_results.iter().position(|r| r.is_some()) {
                    let output = ready_results[winner_idx].take().unwrap();
                    return Poll::Ready(AggregateOutput::Select {
                        winner_index: winner_idx,
                        output,
                    });
                }
                Poll::Pending
            }
            AggregateMode::Join => {
                // Poll all children and return when all ready
                // Order doesn't matter here - results are already delivered in history order
                let mut results: Vec<Option<DurableOutput>> = vec![None; this.children.len()];
                loop {
                    let mut made_progress = false;
                    for (i, child) in this.children.iter_mut().enumerate() {
                        if results[i].is_some() {
                            continue;
                        }
                        if let Poll::Ready(output) = Pin::new(child).poll(cx) {
                            results[i] = Some(output);
                            made_progress = true;
                        }
                    }

                    if results.iter().all(|r| r.is_some()) {
                        // All ready - return in original order
                        let outputs: Vec<DurableOutput> = results.into_iter().map(|r| r.unwrap()).collect();
                        return Poll::Ready(AggregateOutput::Join { outputs });
                    }

                    if !made_progress {
                        return Poll::Pending;
                    }
                }
            }
        }
    }
}

pub struct SelectFuture(pub(crate) AggregateDurableFuture);
impl Future for SelectFuture {
    type Output = (usize, DurableOutput);
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        match inner.poll(cx) {
            Poll::Ready(AggregateOutput::Select { winner_index, output }) => Poll::Ready((winner_index, output)),
            Poll::Ready(_) => unreachable!(),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub struct JoinFuture(pub(crate) AggregateDurableFuture);
impl Future for JoinFuture {
    type Output = Vec<DurableOutput>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        match inner.poll(cx) {
            Poll::Ready(AggregateOutput::Join { outputs }) => Poll::Ready(outputs),
            Poll::Ready(_) => unreachable!(),
            Poll::Pending => Poll::Pending,
        }
    }
}
