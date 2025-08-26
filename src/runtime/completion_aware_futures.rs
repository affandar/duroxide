use std::sync::{Arc, Mutex};
use std::cell::RefCell;
use std::rc::Rc;
use crate::{OrchestrationContext, futures::{DurableFuture, DurableOutput, Kind}, Event};
use super::completion_map::{CompletionMap, CompletionKind, CompletionValue};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A completion map accessor that can be injected into the orchestration context
/// This allows durable futures to access completions deterministically
#[derive(Clone)]
pub struct CompletionMapAccessor {
    inner: Rc<RefCell<Option<Arc<Mutex<CompletionMap>>>>>,
}

impl CompletionMapAccessor {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(None)),
        }
    }

    pub fn set_completion_map(&self, completion_map: Arc<Mutex<CompletionMap>>) {
        *self.inner.borrow_mut() = Some(completion_map);
    }

    pub fn with_completion_map<T>(&self, f: impl FnOnce(&mut CompletionMap) -> T) -> Option<T> {
        if let Some(map_arc) = self.inner.borrow().as_ref() {
            if let Ok(mut map) = map_arc.try_lock() {
                Some(f(&mut *map))
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn is_completion_ready(&self, kind: CompletionKind, correlation_id: u64) -> bool {
        self.with_completion_map(|map| map.is_next_ready(kind, correlation_id))
            .unwrap_or(false)
    }

    pub fn get_completion(&self, kind: CompletionKind, correlation_id: u64) -> Option<CompletionValue> {
        self.with_completion_map(|map| {
            map.get_ready_completion(kind, correlation_id)
                .map(|comp| comp.data)
        })
        .flatten()
    }
}

/// Extension trait to add completion map support to OrchestrationContext
pub trait OrchestrationContextExt {
    fn set_completion_map_accessor(&self, accessor: CompletionMapAccessor);
    fn get_completion_map_accessor(&self) -> Option<CompletionMapAccessor>;
}

// We'll store the accessor in the context's inner state by extending CtxInner
// For now, we'll use a thread-local approach to avoid modifying the core library

thread_local! {
    static COMPLETION_MAP_ACCESSOR: RefCell<Option<CompletionMapAccessor>> = RefCell::new(None);
}

impl OrchestrationContextExt for OrchestrationContext {
    fn set_completion_map_accessor(&self, accessor: CompletionMapAccessor) {
        COMPLETION_MAP_ACCESSOR.with(|a| {
            *a.borrow_mut() = Some(accessor);
        });
    }

    fn get_completion_map_accessor(&self) -> Option<CompletionMapAccessor> {
        COMPLETION_MAP_ACCESSOR.with(|a| a.borrow().clone())
    }
}

/// Enhanced durable future that can use completion map for deterministic polling
pub struct CompletionAwareDurableFuture {
    inner: DurableFuture,
}

impl CompletionAwareDurableFuture {
    pub fn new(inner: DurableFuture) -> Self {
        Self { inner }
    }
}

impl Future for CompletionAwareDurableFuture {
    type Output = DurableOutput;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // CR TODO : this is a bit of a hack, we should be able to get the completion map from the context. 
        //  I guess this is to avoid breaking the existing API but its ok to break.

        // First try the completion map approach
        if let Some(accessor) = COMPLETION_MAP_ACCESSOR.with(|a| a.borrow().clone()) {
            // Try to get completion from completion map
            let (kind, correlation_id) = match &self.inner.0 {
                Kind::Activity { id, .. } => (CompletionKind::Activity, *id),
                Kind::Timer { id, .. } => (CompletionKind::Timer, *id),
                Kind::External { id, .. } => (CompletionKind::External, *id),
                Kind::SubOrch { id, .. } => (CompletionKind::SubOrchestration, *id),
            };

            // Check if this completion is ready in the ordered map
            if accessor.is_completion_ready(kind, correlation_id) {
                if let Some(completion_value) = accessor.get_completion(kind, correlation_id) {
                    let output = match completion_value {
                        CompletionValue::ActivityResult(result) => DurableOutput::Activity(result),
                        CompletionValue::TimerFired { .. } => DurableOutput::Timer,
                        CompletionValue::ExternalData { data, .. } => DurableOutput::External(data),
                        CompletionValue::SubOrchResult(result) => DurableOutput::SubOrchestration(result),
                        CompletionValue::CancelReason(_) => {
                            // Cancel is handled differently, fall back to normal polling
                            return Pin::new(&mut self.inner).poll(cx);
                        }
                    };
                    
                    return Poll::Ready(output);
                }
            }
            
            // If completion is not ready in the map, stay pending
            // but still call the original poll to handle scheduling
            let original_result = Pin::new(&mut self.inner).poll(cx);
            
            // If the original would return Ready but completion map says not ready, override to Pending
            match original_result {
                Poll::Ready(_) => {
                    // Check one more time if completion map allows this
                    if accessor.is_completion_ready(kind, correlation_id) {
                        original_result
                    } else {
                        Poll::Pending
                    }
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            // Fall back to original behavior if no completion map
            Pin::new(&mut self.inner).poll(cx)
        }
    }
}

/// Helper to create completion-aware versions of durable futures
pub trait DurableFutureExt {
    fn with_completion_awareness(self) -> CompletionAwareDurableFuture;
}

impl DurableFutureExt for DurableFuture {
    fn with_completion_awareness(self) -> CompletionAwareDurableFuture {
        CompletionAwareDurableFuture::new(self)
    }
}

/// Modified orchestration turn execution that injects completion map
pub fn run_turn_with_completion_map<O, F>(
    history: Vec<Event>,
    turn_index: u64,
    completion_map: Arc<Mutex<CompletionMap>>,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (
    Vec<crate::Event>,
    Vec<crate::Action>,
    Vec<(crate::LogLevel, String)>,
    Option<O>,
    crate::ClaimedIdsSnapshot,
)
where
    F: Future<Output = O>,
{
    // Set up completion map accessor
    let accessor = CompletionMapAccessor::new();
    accessor.set_completion_map(completion_map);
    
    // Store in thread-local for futures to access
    COMPLETION_MAP_ACCESSOR.with(|a| {
        *a.borrow_mut() = Some(accessor);
    });
    
    // Run the turn normally - futures will now use completion map
    let result = crate::run_turn_with_claims(history, turn_index, orchestrator);
    
    // Clean up thread-local
    COMPLETION_MAP_ACCESSOR.with(|a| {
        *a.borrow_mut() = None;
    });
    
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::completion_map::{CompletionMap, CompletionData, CompletionValue, CompletionKind};
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_completion_map_accessor() {
        let accessor = CompletionMapAccessor::new();
        let mut map = CompletionMap::new();
        
        // Add a completion
        map.by_id.insert(
            (CompletionKind::Activity, 1),
            CompletionData {
                kind: CompletionKind::Activity,
                correlation_id: 1,
                data: CompletionValue::ActivityResult(Ok("test".to_string())),
                arrival_order: 0,
            }
        );
        map.ordered.push_back(crate::runtime::completion_map::CompletionEntry {
            kind: CompletionKind::Activity,
            correlation_id: 1,
            arrival_order: 0,
            consumed: false,
        });

        let map_arc = Arc::new(Mutex::new(map));
        accessor.set_completion_map(map_arc);

        // Test completion readiness
        assert!(accessor.is_completion_ready(CompletionKind::Activity, 1));
        assert!(!accessor.is_completion_ready(CompletionKind::Activity, 2));

        // Test getting completion
        let completion = accessor.get_completion(CompletionKind::Activity, 1);
        assert!(completion.is_some());
        matches!(completion.unwrap(), CompletionValue::ActivityResult(Ok(ref s)) if s == "test");
    }
}
