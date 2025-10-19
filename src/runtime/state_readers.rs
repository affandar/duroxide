use crate::{Event, providers::WorkItem};
use tracing::warn;

/// Reader for extracting metadata from orchestration history
/// 
/// This struct provides convenient access to key information derived from
/// the event history without needing to repeatedly scan through events.
#[derive(Debug, Clone)]
pub struct HistoryReader {
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
}

impl HistoryReader {
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
        history_reader: &HistoryReader,
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
                // No start item - extract from history reader
                let orchestration_name = history_reader.orchestration_name.clone().unwrap_or_else(|| {
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
        let metadata = HistoryReader::from_history(&[]);
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

        let metadata = HistoryReader::from_history(&history);
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

        let metadata = HistoryReader::from_history(&history);
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

        let metadata = HistoryReader::from_history(&history);
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

        let metadata = HistoryReader::from_history(&history);
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

        let metadata = HistoryReader::from_history(&history);
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
            },
            WorkItem::ActivityCompleted {
                instance: "test-inst".to_string(),
                execution_id: 1,
                id: 1,
                result: "result".to_string(),
            },
        ];

        let history_reader = HistoryReader::from_history(&[]);
        let reader = WorkItemReader::from_messages(&messages, &history_reader, "test-inst");

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

        let history_reader = HistoryReader::from_history(&[]);
        let reader = WorkItemReader::from_messages(&messages, &history_reader, "test-inst");

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

        let history_reader = HistoryReader::from_history(&history);
        let reader = WorkItemReader::from_messages(&messages, &history_reader, "test-inst");

        assert!(!reader.has_start_item());
        assert_eq!(reader.orchestration_name, "test-orch"); // From history
        assert_eq!(reader.input, ""); // No input for completion-only
        assert!(!reader.is_continue_as_new);
        assert_eq!(reader.completion_messages.len(), 2);
    }

    #[test]
    fn test_workitem_reader_empty_messages() {
        let messages = vec![];
        let history_reader = HistoryReader::from_history(&[]);
        let reader = WorkItemReader::from_messages(&messages, &history_reader, "test-inst");

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
            },
            WorkItem::StartOrchestration {
                instance: "test-inst".to_string(),
                orchestration: "second-orch".to_string(),
                input: "input2".to_string(),
                version: None,
                parent_instance: None,
                parent_id: None,
            },
        ];

        let history_reader = HistoryReader::from_history(&[]);
        let reader = WorkItemReader::from_messages(&messages, &history_reader, "test-inst");

        // Should only use the first one
        assert_eq!(reader.orchestration_name, "first-orch");
        assert_eq!(reader.input, "input1");
    }
}

