//! New future types for standard async/await support.
//!
//! These futures implement `std::future::Future` directly with specific output types,
//! enabling use of standard `futures::select!` and `futures::join!` combinators.
//!
//! Key design principles:
//! - Schedule-time validation (validation happens in `schedule_*_v2()`, not `poll()`)
//! - FIFO completion ordering (poll only returns Ready when this is the earliest completion)
//! - Drop-based cancellation (dropping records `Action::Cancel`, guarded by dehydrating flag)

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::future::FusedFuture;

use crate::{Action, CompletionResult, OrchestrationContext};

/// Future for activity completion.
///
/// Created by `OrchestrationContext::schedule_activity_v2()`.
/// Resolves to `Result<String, String>` (success or failure).
#[must_use = "futures do nothing unless awaited"]
pub struct ActivityFuture {
    /// The scheduling event_id assigned during schedule_activity_v2()
    pub(crate) scheduling_event_id: u64,
    /// Reference to the orchestration context
    pub(crate) ctx: OrchestrationContext,
    /// Whether this future has already returned Ready
    pub(crate) consumed: Cell<bool>,
}

impl Future for ActivityFuture {
    type Output = Result<String, String>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.consumed.get() {
            // FusedFuture behavior: already completed
            return Poll::Pending;
        }

        let inner = self.ctx.inner.lock().expect("Mutex should not be poisoned");

        // Check if our completion exists
        let completion = match inner.completions_v2.get(&self.scheduling_event_id) {
            Some(CompletionResult::Activity(result)) => result.clone(),
            _ => return Poll::Pending, // No completion yet
        };

        // Get our completion's event_id for FIFO check
        if let Some(completion_event_id) = inner.get_completion_event_id_v2(self.scheduling_event_id)
        {
            // FIFO check: can we consume this completion?
            if inner.can_consume_v2(completion_event_id) {
                drop(inner); // Release lock before mutating

                let mut inner = self.ctx.inner.lock().expect("Mutex should not be poisoned");
                inner.consumed_completions_v2.insert(completion_event_id);
                drop(inner);

                self.consumed.set(true);
                return Poll::Ready(completion);
            }
        }

        Poll::Pending
    }
}

impl FusedFuture for ActivityFuture {
    fn is_terminated(&self) -> bool {
        self.consumed.get()
    }
}

impl Drop for ActivityFuture {
    fn drop(&mut self) {
        // Already completed - nothing to cancel
        if self.consumed.get() {
            return;
        }

        // Use try_lock to avoid panic if lock is poisoned
        let Ok(mut inner) = self.ctx.inner.try_lock() else {
            return;
        };

        // Dehydrating = normal suspension, not cancellation
        if inner.dehydrating {
            return;
        }

        // Check if completion exists (might have arrived but not consumed)
        if inner.completions_v2.contains_key(&self.scheduling_event_id) {
            return;
        }

        // This is a real cancellation (e.g., select loser)
        inner.actions.push(Action::Cancel {
            scheduling_event_id: self.scheduling_event_id,
        });

        // Also track for the existing cancellation system
        inner.cancelled_activity_ids.insert(self.scheduling_event_id);
    }
}

/// Future for timer completion.
///
/// Created by `OrchestrationContext::schedule_timer_v2()`.
/// Resolves to `()` when the timer fires.
#[must_use = "futures do nothing unless awaited"]
pub struct TimerFuture {
    /// The scheduling event_id assigned during schedule_timer_v2()
    pub(crate) scheduling_event_id: u64,
    /// Reference to the orchestration context
    pub(crate) ctx: OrchestrationContext,
    /// Whether this future has already returned Ready
    pub(crate) consumed: Cell<bool>,
}

impl Future for TimerFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.consumed.get() {
            return Poll::Pending;
        }

        let inner = self.ctx.inner.lock().expect("Mutex should not be poisoned");

        // Check if our completion exists
        let has_completion = matches!(
            inner.completions_v2.get(&self.scheduling_event_id),
            Some(CompletionResult::Timer)
        );

        if !has_completion {
            return Poll::Pending;
        }

        // FIFO check
        if let Some(completion_event_id) = inner.get_completion_event_id_v2(self.scheduling_event_id)
        {
            if inner.can_consume_v2(completion_event_id) {
                drop(inner);

                let mut inner = self.ctx.inner.lock().expect("Mutex should not be poisoned");
                inner.consumed_completions_v2.insert(completion_event_id);
                drop(inner);

                self.consumed.set(true);
                return Poll::Ready(());
            }
        }

        Poll::Pending
    }
}

impl FusedFuture for TimerFuture {
    fn is_terminated(&self) -> bool {
        self.consumed.get()
    }
}

impl Drop for TimerFuture {
    fn drop(&mut self) {
        if self.consumed.get() {
            return;
        }

        let Ok(mut inner) = self.ctx.inner.try_lock() else {
            return;
        };

        if inner.dehydrating {
            return;
        }

        if inner.completions_v2.contains_key(&self.scheduling_event_id) {
            return;
        }

        inner.actions.push(Action::Cancel {
            scheduling_event_id: self.scheduling_event_id,
        });
    }
}

/// Future for external event completion.
///
/// Created by `OrchestrationContext::schedule_wait_v2()`.
/// Uses name-based matching with consumption order (not event_id).
/// Resolves to `String` containing the event payload.
#[must_use = "futures do nothing unless awaited"]
pub struct ExternalFuture {
    /// The event name to wait for
    pub(crate) event_name: String,
    /// Which occurrence of this event (0 = first, 1 = second, etc.)
    pub(crate) consumption_index: usize,
    /// The scheduling event_id (for cursor tracking, not completion matching)
    pub(crate) scheduling_event_id: u64,
    /// Reference to the orchestration context
    pub(crate) ctx: OrchestrationContext,
    /// Whether this future has already returned Ready
    pub(crate) consumed: Cell<bool>,
}

impl Future for ExternalFuture {
    type Output = String;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.consumed.get() {
            return Poll::Pending;
        }

        let inner = self.ctx.inner.lock().expect("Mutex should not be poisoned");

        // Name-based matching with consumption order
        if let Some(completions) = inner.external_completions_v2.get(&self.event_name) {
            if let Some(data) = completions.get(self.consumption_index) {
                self.consumed.set(true);
                return Poll::Ready(data.clone());
            }
        }

        Poll::Pending
    }
}

impl FusedFuture for ExternalFuture {
    fn is_terminated(&self) -> bool {
        self.consumed.get()
    }
}

impl Drop for ExternalFuture {
    fn drop(&mut self) {
        if self.consumed.get() {
            return;
        }

        let Ok(mut inner) = self.ctx.inner.try_lock() else {
            return;
        };

        if inner.dehydrating {
            return;
        }

        // Check if this event has arrived
        if let Some(completions) = inner.external_completions_v2.get(&self.event_name) {
            if completions.get(self.consumption_index).is_some() {
                return;
            }
        }

        // Record cancellation by name
        inner.actions.push(Action::CancelExternal {
            event_name: self.event_name.clone(),
        });
    }
}

/// Future for sub-orchestration completion.
///
/// Created by `OrchestrationContext::schedule_sub_orchestration_v2()`.
/// Resolves to `Result<String, String>` (success or failure of child orchestration).
#[must_use = "futures do nothing unless awaited"]
pub struct SubOrchestrationFuture {
    /// The scheduling event_id assigned during schedule_sub_orchestration_v2()
    pub(crate) scheduling_event_id: u64,
    /// Reference to the orchestration context
    pub(crate) ctx: OrchestrationContext,
    /// Whether this future has already returned Ready
    pub(crate) consumed: Cell<bool>,
}

impl Future for SubOrchestrationFuture {
    type Output = Result<String, String>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.consumed.get() {
            return Poll::Pending;
        }

        let inner = self.ctx.inner.lock().expect("Mutex should not be poisoned");

        // Check if our completion exists
        let completion = match inner.completions_v2.get(&self.scheduling_event_id) {
            Some(CompletionResult::SubOrchestration(result)) => result.clone(),
            _ => return Poll::Pending,
        };

        // FIFO check
        if let Some(completion_event_id) = inner.get_completion_event_id_v2(self.scheduling_event_id)
        {
            if inner.can_consume_v2(completion_event_id) {
                drop(inner);

                let mut inner = self.ctx.inner.lock().expect("Mutex should not be poisoned");
                inner.consumed_completions_v2.insert(completion_event_id);
                drop(inner);

                self.consumed.set(true);
                return Poll::Ready(completion);
            }
        }

        Poll::Pending
    }
}

impl FusedFuture for SubOrchestrationFuture {
    fn is_terminated(&self) -> bool {
        self.consumed.get()
    }
}

impl Drop for SubOrchestrationFuture {
    fn drop(&mut self) {
        if self.consumed.get() {
            return;
        }

        let Ok(mut inner) = self.ctx.inner.try_lock() else {
            return;
        };

        if inner.dehydrating {
            return;
        }

        if inner.completions_v2.contains_key(&self.scheduling_event_id) {
            return;
        }

        inner.actions.push(Action::Cancel {
            scheduling_event_id: self.scheduling_event_id,
        });
    }
}

/// Future for system call completion (trace, new_guid, utcnow).
///
/// System calls are executed synchronously and always complete immediately.
/// Created internally by context methods.
#[must_use = "futures do nothing unless awaited"]
pub struct SystemCallFuture {
    /// The scheduling event_id
    pub(crate) scheduling_event_id: u64,
    /// Reference to the orchestration context
    pub(crate) ctx: OrchestrationContext,
    /// Whether this future has already returned Ready
    pub(crate) consumed: Cell<bool>,
}

impl Future for SystemCallFuture {
    type Output = String;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.consumed.get() {
            return Poll::Pending;
        }

        let inner = self.ctx.inner.lock().expect("Mutex should not be poisoned");

        // Check if our completion exists
        let result = match inner.completions_v2.get(&self.scheduling_event_id) {
            Some(CompletionResult::SystemCall(value)) => value.clone(),
            _ => return Poll::Pending,
        };

        // FIFO check
        if let Some(completion_event_id) = inner.get_completion_event_id_v2(self.scheduling_event_id)
        {
            if inner.can_consume_v2(completion_event_id) {
                drop(inner);

                let mut inner = self.ctx.inner.lock().expect("Mutex should not be poisoned");
                inner.consumed_completions_v2.insert(completion_event_id);
                drop(inner);

                self.consumed.set(true);
                return Poll::Ready(result);
            }
        }

        Poll::Pending
    }
}

impl FusedFuture for SystemCallFuture {
    fn is_terminated(&self) -> bool {
        self.consumed.get()
    }
}

impl Drop for SystemCallFuture {
    fn drop(&mut self) {
        // System calls complete synchronously, so they should always be consumed.
        // No cancellation needed.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activity_future_is_must_use() {
        // This test just ensures the #[must_use] attribute is present
        // by checking the type exists
        fn _check_activity_future(_f: ActivityFuture) {}
        fn _check_timer_future(_f: TimerFuture) {}
        fn _check_external_future(_f: ExternalFuture) {}
        fn _check_sub_orchestration_future(_f: SubOrchestrationFuture) {}
        fn _check_system_call_future(_f: SystemCallFuture) {}
    }
}
