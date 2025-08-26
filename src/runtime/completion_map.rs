use std::collections::{HashMap, VecDeque};
use crate::{Event, runtime::router::OrchestratorMsg};
use tracing::debug;

/// The kind of completion for deterministic ordering
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    Activity,
    Timer,
    External,
    SubOrchestration,
    Cancel,
}

/// Data associated with a completion
#[derive(Debug, Clone)]
pub struct CompletionData {
    pub kind: CompletionKind,
    pub correlation_id: u64,
    pub data: CompletionValue,
    pub arrival_order: usize,
}

/// The actual completion value
#[derive(Debug, Clone)]
pub enum CompletionValue {
    ActivityResult(Result<String, String>),
    TimerFired { fire_at_ms: u64 },
    ExternalData { name: String, data: String },
    SubOrchResult(Result<String, String>),
    CancelReason(String),
}

/// Entry in the ordered completion list
#[derive(Debug, Clone)]
pub struct CompletionEntry {
    pub kind: CompletionKind,
    pub correlation_id: u64,
    pub arrival_order: usize,
    pub consumed: bool,
}

/// A deterministic completion map that ensures futures poll in arrival order
#[derive(Debug, Clone)]
pub struct CompletionMap {
    /// Fast lookup by correlation ID and kind
    pub by_id: HashMap<(CompletionKind, u64), CompletionData>,
    /// Ordered queue of completions by arrival time
    pub ordered: VecDeque<CompletionEntry>,
    /// Next arrival order counter
    pub next_order: usize,
}

impl CompletionMap {
    pub fn new() -> Self {
        Self {
            by_id: HashMap::new(),
            ordered: VecDeque::new(),
            next_order: 0,
        }
    }

    /// Add a completion from an orchestrator message
    pub fn add_completion(&mut self, msg: OrchestratorMsg) -> Option<String> {
        let (kind, correlation_id, value, ack_token) = match msg {
            OrchestratorMsg::ActivityCompleted { id, result, ack_token, .. } => {
                (CompletionKind::Activity, id, CompletionValue::ActivityResult(Ok(result)), ack_token)
            }
            OrchestratorMsg::ActivityFailed { id, error, ack_token, .. } => {
                (CompletionKind::Activity, id, CompletionValue::ActivityResult(Err(error)), ack_token)
            }
            OrchestratorMsg::TimerFired { id, fire_at_ms, ack_token, .. } => {
                (CompletionKind::Timer, id, CompletionValue::TimerFired { fire_at_ms }, ack_token)
            }
            OrchestratorMsg::ExternalByName { ack_token, .. } => {
                // For external events, we'll use a special correlation strategy
                // For now, we'll need to find the subscription ID from history
                // This will be handled by the caller
                return ack_token;
            }
            OrchestratorMsg::SubOrchCompleted { id, result, ack_token, .. } => {
                (CompletionKind::SubOrchestration, id, CompletionValue::SubOrchResult(Ok(result)), ack_token)
            }
            OrchestratorMsg::SubOrchFailed { id, error, ack_token, .. } => {
                (CompletionKind::SubOrchestration, id, CompletionValue::SubOrchResult(Err(error)), ack_token)
            }
            OrchestratorMsg::CancelRequested { reason, ack_token, .. } => {
                // Cancel uses a special correlation ID of 0
                (CompletionKind::Cancel, 0, CompletionValue::CancelReason(reason), ack_token)
            }
        };

        let key = (kind, correlation_id);
        
        // Check for duplicates
        if self.by_id.contains_key(&key) {
            debug!(kind=?kind, correlation_id, "ignoring duplicate completion");
            return ack_token;
        }

        let arrival_order = self.next_order;
        self.next_order += 1;

        let completion_data = CompletionData {
            kind,
            correlation_id,
            data: value,
            arrival_order,
        };

        self.by_id.insert(key, completion_data);
        // CR TODO : how is this ordered? push_back just puts it in the back of the list.
        self.ordered.push_back(CompletionEntry {
            kind,
            correlation_id,
            arrival_order,
            consumed: false,
        });

        debug!(kind=?kind, correlation_id, arrival_order, "added completion to map");
        ack_token
    }

    /// Check if a specific completion is the next one ready to be consumed
    /// This ensures deterministic polling order
    pub fn is_next_ready(&self, kind: CompletionKind, correlation_id: u64) -> bool {
        // Find the first unconsumed entry in the ordered queue
        if let Some(next_entry) = self.ordered.iter().find(|entry| !entry.consumed) {
            next_entry.kind == kind && next_entry.correlation_id == correlation_id
        } else {
            false
        }
    }

    /// Get completion data if it exists and is ready to be consumed
    pub fn get_ready_completion(&mut self, kind: CompletionKind, correlation_id: u64) -> Option<CompletionData> {
        let key = (kind, correlation_id);
        
        // Only return if this is the next completion in order
        if !self.is_next_ready(kind, correlation_id) {
            return None;
        }

        // Mark as consumed in the ordered queue
        if let Some(entry) = self.ordered.iter_mut().find(|entry| 
            !entry.consumed && entry.kind == kind && entry.correlation_id == correlation_id
        ) {
            entry.consumed = true;
        }

        self.by_id.remove(&key)
    }

    /// Check if there are any unconsumed completions
    pub fn has_unconsumed(&self) -> bool {
        self.ordered.iter().any(|entry| !entry.consumed)
    }

    /// Get all unconsumed completions (for error reporting)
    pub fn get_unconsumed(&self) -> Vec<(CompletionKind, u64)> {
        self.ordered.iter()
            .filter(|entry| !entry.consumed)
            .map(|entry| (entry.kind, entry.correlation_id))
            .collect()
    }

    /// Clear consumed entries to free memory
    pub fn cleanup_consumed(&mut self) {
        while let Some(front) = self.ordered.front() {
            if front.consumed {
                self.ordered.pop_front();
            } else {
                break;
            }
        }
    }

    /// Handle external events by finding the corresponding subscription
    pub fn add_external_completion(&mut self, name: String, data: String, history: &[Event], ack_token: Option<String>) -> Option<String> {
        // Find the most recent subscription for this event name
        let subscription_id = history.iter().rev().find_map(|e| match e {
            Event::ExternalSubscribed { id, name: sub_name } if sub_name == &name => Some(*id),
            _ => None,
        });

        if let Some(correlation_id) = subscription_id {
            let key = (CompletionKind::External, correlation_id);
            
            // Check for duplicates
            if self.by_id.contains_key(&key) {
                debug!(name=%name, correlation_id, "ignoring duplicate external completion");
                return ack_token;
            }

            let arrival_order = self.next_order;
            self.next_order += 1;

            let completion_data = CompletionData {
                kind: CompletionKind::External,
                correlation_id,
                data: CompletionValue::ExternalData { name: name.clone(), data },
                arrival_order,
            };

            self.by_id.insert(key, completion_data);
            self.ordered.push_back(CompletionEntry {
                kind: CompletionKind::External,
                correlation_id,
                arrival_order,
                consumed: false,
            });

            debug!(name=%name, correlation_id, arrival_order, "added external completion to map");
            ack_token
        } else {
            debug!(name=%name, "dropping external completion: no active subscription");
            ack_token
        }
    }
}

impl Default for CompletionMap {
    fn default() -> Self {
        Self::new()
    }
}

// Include comprehensive tests
#[path = "completion_map_tests.rs"]
mod completion_map_tests;


