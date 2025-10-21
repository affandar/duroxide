use crate::{Event, providers::WorkItem};
use tracing::warn;

/// Reader for extracting metadata from orchestration history
/// 
/// This struct provides convenient access to key information derived from
/// the event history without needing to repeatedly scan through events.
#[derive(Debug, Clone)]
pub struct HistoryManager {
    /// Orchestration name (from OrchestrationStarted)
    pub orchestration_name: Option<String>,
    
    /// Orchestration version (from OrchestrationStarted)
    pub orchestration_version: Option<String>,
    
    /// Original input (from OrchestrationStarted)
    pub orchestration_input: Option<String>,
    
    /// Parent instance if this is a sub-orchestration
    pub parent_instance: Option<String>,
    
    /// Parent event ID if this is a sub-orchestration
    pub parent_id: Option<u64>,
    
    /// Whether the orchestration has completed successfully
    pub is_completed: bool,
    
    /// Whether the orchestration has failed
    pub is_failed: bool,
    
    /// Whether the orchestration has continued as new
    pub is_continued_as_new: bool,
    
    /// The execution ID from the most recent OrchestrationStarted
    pub current_execution_id: Option<u64>,
    
    /// The complete history being managed
    history: Vec<Event>,
    
    /// New events to be appended (history delta)
    delta: Vec<Event>,
}

impl HistoryManager {
    /// Extract metadata from orchestration history
    /// 
    /// Scans through the history (in reverse for terminal states) to extract
    /// commonly needed information.
    pub fn from_history(history: &[Event]) -> Self {
        let mut metadata = Self {
            orchestration_name: None,
            orchestration_version: None,
            orchestration_input: None,
            parent_instance: None,
            parent_id: None,
            is_completed: false,
            is_failed: false,
            is_continued_as_new: false,
            current_execution_id: None,
            history: history.to_vec(),
            delta: Vec::new(),
        };

        // Scan forward for OrchestrationStarted (could be multiple due to CAN)
        // We want the most recent one for current execution
        // Note: execution_id is derived from counting OrchestrationStarted events, not stored in the event
        let mut execution_id_counter = 0u64;
        let mut last_started_index = None;
        for (idx, event) in history.iter().enumerate() {
            if let Event::OrchestrationStarted {
                name,
                version,
                input,
                parent_instance,
                parent_id,
                ..
            } = event
            {
                execution_id_counter += 1;
                metadata.orchestration_name = Some(name.clone());
                metadata.orchestration_version = Some(version.clone());
                metadata.orchestration_input = Some(input.clone());
                metadata.parent_instance = parent_instance.clone();
                metadata.parent_id = *parent_id;
                metadata.current_execution_id = Some(execution_id_counter);
                last_started_index = Some(idx);
                // Don't break - we want the LAST (most recent) OrchestrationStarted
            }
        }

        // Check for terminal states AFTER the most recent OrchestrationStarted
        if let Some(start_idx) = last_started_index {
            for event in history[(start_idx + 1)..].iter() {
                match event {
                    Event::OrchestrationCompleted { .. } => {
                        metadata.is_completed = true;
                        break;
                    }
                    Event::OrchestrationFailed { .. } => {
                        metadata.is_failed = true;
                        break;
                    }
                    Event::OrchestrationContinuedAsNew { .. } => {
                        metadata.is_continued_as_new = true;
                        break;
                    }
                    _ => {}
                }
            }
        }

        metadata
    }

    /// Check if the orchestration is in a terminal state
    pub fn is_terminal(&self) -> bool {
        self.is_completed || self.is_failed || self.is_continued_as_new
    }
    
    /// Check if the history is empty (new instance with no events yet)
    pub fn is_empty(&self) -> bool {
        self.history.is_empty() && self.delta.is_empty()
    }

    /// Get a human-readable status string
    pub fn status(&self) -> &'static str {
        if self.is_completed {
            "Completed"
        } else if self.is_failed {
            "Failed"
        } else if self.is_continued_as_new {
            "ContinuedAsNew"
        } else {
            "Running"
        }
    }
    
    // === Mutation methods for building history delta ===
    
    /// Calculate the next event ID based on existing history and delta
    pub fn next_event_id(&self) -> u64 {
        self.history
            .iter()
            .chain(self.delta.iter())
            .map(|e| e.event_id())
            .max()
            .unwrap_or(0)
            + 1
    }
    
    /// Append a single event to the delta
    pub fn append(&mut self, event: Event) {
        self.delta.push(event);
    }
    
    /// Extend delta with multiple events
    pub fn extend(&mut self, events: Vec<Event>) {
        self.delta.extend(events);
    }
    
    /// Get a reference to the history delta
    pub fn delta(&self) -> &[Event] {
        &self.delta
    }
    
    /// Consume the manager and return the history delta
    pub fn into_delta(self) -> Vec<Event> {
        self.delta
    }
    
    /// Get the complete history (original + delta)
    pub fn full_history(&self) -> Vec<Event> {
        [&self.history[..], &self.delta[..]].concat()
    }
    
    /// Get the version from the most recent OrchestrationStarted event
    /// Checks both existing history (cached) and delta (for newly created instances)
    /// Returns None for "0.0.0" (placeholder/unregistered version)
    pub fn version(&self) -> Option<String> {
        // First check cached metadata from initial history
        if let Some(ref v) = self.orchestration_version {
            if v == "0.0.0" {
                return None;
            }
            return Some(v.clone());
        }
        
        // If no cached version, check delta for newly appended OrchestrationStarted
        for e in self.delta.iter().rev() {
            if let Event::OrchestrationStarted { version, .. } = e {
                if version == "0.0.0" {
                    return None;
                }
                return Some(version.clone());
            }
        }
        
        None
    }
    
    /// Get the input from the most recent OrchestrationStarted event
    pub fn input(&self) -> Option<&str> {
        self.orchestration_input.as_deref()
    }
    
    /// Extract input and parent linkage from history for orchestration context
    /// This looks at the full history including any newly appended events in the delta
    pub fn extract_context(&self) -> (String, Option<(String, u64)>) {
        // First check if we have metadata from the initial scan
        if let Some(ref input) = self.orchestration_input {
            let parent_link = if let (Some(parent_inst), Some(parent_id)) = (&self.parent_instance, self.parent_id) {
                Some((parent_inst.clone(), parent_id))
            } else {
                None
            };
            return (input.clone(), parent_link);
        }
        
        // If no metadata yet (empty initial history), check the delta for OrchestrationStarted
        for e in self.delta.iter().rev() {
            if let Event::OrchestrationStarted {
                input,
                parent_instance,
                parent_id,
                ..
            } = e
            {
                let parent_link = if let (Some(pinst), Some(pid)) = (parent_instance.clone(), *parent_id) {
                    Some((pinst, pid))
                } else {
                    None
                };
                return (input.clone(), parent_link);
            }
        }
        
        // Fallback - no OrchestrationStarted found
        (String::new(), None)
    }
    
    /// Extract current execution history (filters out events from previous executions in CAN scenarios)
    pub fn current_execution_history(&self) -> Result<Vec<Event>, String> {
        let full_history = self.full_history();
        
        // Find the most recent OrchestrationStarted event to determine current execution boundary
        let current_execution_start = full_history
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, e)| {
                if matches!(e, Event::OrchestrationStarted { .. }) {
                    Some(idx)
                } else {
                    None
                }
            })
            .ok_or("corrupted history: no OrchestrationStarted event found")?;

        Ok(full_history[current_execution_start..].to_vec())
    }
}

/// Reader for extracting information from a batch of work items
/// 
/// Separates start/CAN items from completion messages and extracts
/// execution parameters in a single pass.
#[derive(Debug)]
pub struct WorkItemReader {
    /// The start or continue-as-new item, if present
    pub start_item: Option<WorkItem>,
    
    /// All completion messages (ActivityCompleted, TimerFired, etc.)
    pub completion_messages: Vec<WorkItem>,
    
    /// Orchestration name (from start item or fallback)
    pub orchestration_name: String,
    
    /// Input string (from start item or empty)
    pub input: String,
    
    /// Version (from start item or None)
    pub version: Option<String>,
    
    /// Parent instance (from start item or None)
    pub parent_instance: Option<String>,
    
    /// Parent event ID (from start item or None)
    pub parent_id: Option<u64>,
    
    /// Whether this is a ContinueAsNew
    pub is_continue_as_new: bool,
}

impl WorkItemReader {
    /// Parse a batch of work items
    /// 
    /// Separates start/CAN from completions and extracts parameters.
    /// Falls back to history_reader if no start item is present.
    pub fn from_messages(
        messages: &[WorkItem],
        history_mgr: &HistoryManager,
        instance: &str,
    ) -> Self {
        let mut start_item: Option<WorkItem> = None;
        let mut completion_messages: Vec<WorkItem> = Vec::new();

        // Separate start/CAN from completions
        for work_item in messages {
            match work_item {
                WorkItem::StartOrchestration { .. } | WorkItem::ContinueAsNew { .. } => {
                    if start_item.is_some() {
                        warn!(instance, "Duplicate Start/ContinueAsNew in batch - ignoring duplicate");
                        continue;
                    }
                    start_item = Some(work_item.clone());
                }
                // Non-start/CAN work items are completion messages
                WorkItem::ActivityCompleted { .. }
                | WorkItem::ActivityFailed { .. }
                | WorkItem::TimerFired { .. }
                | WorkItem::ExternalRaised { .. }
                | WorkItem::SubOrchCompleted { .. }
                | WorkItem::SubOrchFailed { .. }
                | WorkItem::CancelInstance { .. } => {
                    completion_messages.push(work_item.clone());
                }
                // ActivityExecute and TimerSchedule shouldn't appear in orchestrator queue
                WorkItem::ActivityExecute { .. } | WorkItem::TimerSchedule { .. } => {}
            }
        }

        // Extract parameters from start item or use defaults
        let (orchestration_name, input, version, parent_instance, parent_id, is_continue_as_new) =
            if let Some(ref item) = start_item {
                match item {
                    WorkItem::StartOrchestration {
                        orchestration,
                        input,
                        version,
                        parent_instance,
                        parent_id,
                        ..
                    } => (
                        orchestration.clone(),
                        input.clone(),
                        version.clone(),
                        parent_instance.clone(),
                        *parent_id,
                        false,
                    ),
                    WorkItem::ContinueAsNew {
                        orchestration,
                        input,
                        version,
                        ..
                    } => (
                        orchestration.clone(),
                        input.clone(),
                        version.clone(),
                        None,
                        None,
                        true,
                    ),
                    _ => unreachable!(),
                }
            } else {
                // No start item - extract from history manager
                let orchestration_name = history_mgr.orchestration_name.clone().unwrap_or_else(|| {
                    if !completion_messages.is_empty() {
                        warn!(instance, "completion messages for unstarted instance");
                    }
                    String::new()
                });
                (orchestration_name, String::new(), None, None, None, false)
            };

        Self {
            start_item,
            completion_messages,
            orchestration_name,
            input,
            version,
            parent_instance,
            parent_id,
            is_continue_as_new,
        }
    }

    /// Check if this batch has a start or continue-as-new item
    pub fn has_start_item(&self) -> bool {
        self.start_item.is_some()
    }

    /// Check if the orchestration name is empty (error condition)
    pub fn has_orchestration_name(&self) -> bool {
        !self.orchestration_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_history_reader_from_empty_history() {
        let metadata = HistoryManager::from_history(&[]);
        assert!(metadata.orchestration_name.is_none());
        assert!(!metadata.is_terminal());
        assert_eq!(metadata.status(), "Running");
    }

    #[test]
    fn test_history_reader_from_started_only() {
        let history = vec![Event::OrchestrationStarted {
            event_id: 1,
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let metadata = HistoryManager::from_history(&history);
        assert_eq!(metadata.orchestration_name, Some("test-orch".to_string()));
        assert_eq!(metadata.orchestration_version, Some("1.0.0".to_string()));
        assert_eq!(metadata.orchestration_input, Some("test-input".to_string()));
        assert!(!metadata.is_terminal());
        assert_eq!(metadata.status(), "Running");
        assert_eq!(metadata.current_execution_id, Some(1));
    }

    #[test]
    fn test_history_reader_completed() {
        let history = vec![
            Event::OrchestrationStarted {
                event_id: 1,
                name: "test-orch".to_string(),
                version: "1.0.0".to_string(),
                input: "test-input".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::OrchestrationCompleted {
                event_id: 2,
                output: "success".to_string(),
            },
        ];

        let metadata = HistoryManager::from_history(&history);
        assert!(metadata.is_completed);
        assert!(metadata.is_terminal());
        assert_eq!(metadata.status(), "Completed");
    }

    #[test]
    fn test_history_reader_failed() {
        let history = vec![
            Event::OrchestrationStarted {
                event_id: 1,
                name: "test-orch".to_string(),
                version: "1.0.0".to_string(),
                input: "test-input".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::OrchestrationFailed {
                event_id: 2,
                error: "boom".to_string(),
            },
        ];

        let metadata = HistoryManager::from_history(&history);
        assert!(metadata.is_failed);
        assert!(metadata.is_terminal());
        assert_eq!(metadata.status(), "Failed");
    }

    #[test]
    fn test_history_reader_continued_as_new() {
        let history = vec![
            Event::OrchestrationStarted {
                event_id: 1,
                name: "test-orch".to_string(),
                version: "1.0.0".to_string(),
                input: "input1".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::OrchestrationContinuedAsNew {
                event_id: 2,
                input: "input2".to_string(),
            },
            Event::OrchestrationStarted {
                event_id: 3,
                name: "test-orch".to_string(),
                version: "1.0.0".to_string(),
                input: "input2".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ];

        let metadata = HistoryManager::from_history(&history);
        // Most recent execution
        assert_eq!(metadata.orchestration_input, Some("input2".to_string()));
        assert_eq!(metadata.current_execution_id, Some(2));
        assert!(!metadata.is_terminal()); // Current execution is running
    }

    #[test]
    fn test_history_reader_with_parent() {
        let history = vec![Event::OrchestrationStarted {
            event_id: 1,
            name: "child-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test".to_string(),
            parent_instance: Some("parent-instance".to_string()),
            parent_id: Some(42),
        }];

        let metadata = HistoryManager::from_history(&history);
        assert_eq!(
            metadata.parent_instance,
            Some("parent-instance".to_string())
        );
        assert_eq!(metadata.parent_id, Some(42));
    }

    #[test]
    fn test_workitem_reader_with_start() {
        let messages = vec![
            WorkItem::StartOrchestration {
                instance: "test-inst".to_string(),
                orchestration: "test-orch".to_string(),
                input: "test-input".to_string(),
                version: Some("1.0.0".to_string()),
                parent_instance: Some("parent".to_string()),
                parent_id: Some(42),
                execution_id: crate::INITIAL_EXECUTION_ID,
            },
            WorkItem::ActivityCompleted {
                instance: "test-inst".to_string(),
                execution_id: 1,
                id: 1,
                result: "result".to_string(),
            },
        ];

        let history_mgr = HistoryManager::from_history(&[]);
        let reader = WorkItemReader::from_messages(&messages, &history_mgr, "test-inst");

        assert!(reader.has_start_item());
        assert_eq!(reader.orchestration_name, "test-orch");
        assert_eq!(reader.input, "test-input");
        assert_eq!(reader.version, Some("1.0.0".to_string()));
        assert_eq!(reader.parent_instance, Some("parent".to_string()));
        assert_eq!(reader.parent_id, Some(42));
        assert!(!reader.is_continue_as_new);
        assert_eq!(reader.completion_messages.len(), 1);
    }

    #[test]
    fn test_workitem_reader_with_can() {
        let messages = vec![
            WorkItem::ContinueAsNew {
                instance: "test-inst".to_string(),
                orchestration: "test-orch".to_string(),
                input: "new-input".to_string(),
                version: Some("2.0.0".to_string()),
            },
        ];

        let history_mgr = HistoryManager::from_history(&[]);
        let reader = WorkItemReader::from_messages(&messages, &history_mgr, "test-inst");

        assert!(reader.has_start_item());
        assert_eq!(reader.orchestration_name, "test-orch");
        assert_eq!(reader.input, "new-input");
        assert!(reader.is_continue_as_new);
        assert_eq!(reader.parent_instance, None);
        assert_eq!(reader.parent_id, None);
    }

    #[test]
    fn test_workitem_reader_completion_only() {
        let messages = vec![
            WorkItem::ActivityCompleted {
                instance: "test-inst".to_string(),
                execution_id: 1,
                id: 1,
                result: "result".to_string(),
            },
            WorkItem::TimerFired {
                instance: "test-inst".to_string(),
                execution_id: 1,
                id: 2,
                fire_at_ms: 1000,
            },
        ];

        let history = vec![Event::OrchestrationStarted {
            event_id: 1,
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let history_mgr = HistoryManager::from_history(&history);
        let reader = WorkItemReader::from_messages(&messages, &history_mgr, "test-inst");

        assert!(!reader.has_start_item());
        assert_eq!(reader.orchestration_name, "test-orch"); // From history
        assert_eq!(reader.input, ""); // No input for completion-only
        assert!(!reader.is_continue_as_new);
        assert_eq!(reader.completion_messages.len(), 2);
    }

    #[test]
    fn test_workitem_reader_empty_messages() {
        let messages = vec![];
        let history_mgr = HistoryManager::from_history(&[]);
        let reader = WorkItemReader::from_messages(&messages, &history_mgr, "test-inst");

        assert!(!reader.has_start_item());
        assert_eq!(reader.orchestration_name, ""); // Empty
        assert!(!reader.has_orchestration_name());
        assert_eq!(reader.completion_messages.len(), 0);
    }

    #[test]
    fn test_workitem_reader_duplicate_start() {
        let messages = vec![
            WorkItem::StartOrchestration {
                instance: "test-inst".to_string(),
                orchestration: "first-orch".to_string(),
                input: "input1".to_string(),
                version: None,
                parent_instance: None,
                parent_id: None,
                execution_id: crate::INITIAL_EXECUTION_ID,
            },
            WorkItem::StartOrchestration {
                instance: "test-inst".to_string(),
                orchestration: "second-orch".to_string(),
                input: "input2".to_string(),
                version: None,
                parent_instance: None,
                parent_id: None,
                execution_id: crate::INITIAL_EXECUTION_ID,
            },
        ];

        let history_mgr = HistoryManager::from_history(&[]);
        let reader = WorkItemReader::from_messages(&messages, &history_mgr, "test-inst");

        // Should only use the first one
        assert_eq!(reader.orchestration_name, "first-orch");
        assert_eq!(reader.input, "input1");
    }
}

