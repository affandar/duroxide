#[cfg(test)]
mod tests {
    use crate::providers::WorkItem;
    use crate::runtime::replay_engine::*;
    use crate::{Event, OrchestrationContext, OrchestrationHandler};
    use async_trait::async_trait;
    use std::sync::Arc;

    // Mock orchestration handler for testing
    struct MockHandler {
        result: Result<String, String>,
    }

    #[async_trait]
    impl OrchestrationHandler for MockHandler {
        async fn invoke(&self, _ctx: OrchestrationContext, _input: String) -> Result<String, String> {
            self.result.clone()
        }
    }

    #[test]
    fn test_engine_creation() {
        let baseline_history = vec![Event::OrchestrationStarted {
            event_id: 0,
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let engine = ReplayEngine::new(
            "test-instance".to_string(),
            1, // execution_id
            baseline_history.clone(),
        );

        assert_eq!(engine.instance, "test-instance");
        assert_eq!(engine.baseline_history, baseline_history);
        // Ack tokens are no longer collected in the engine
        assert!(engine.history_delta.is_empty());
        assert!(engine.pending_actions.is_empty());
        assert!(!engine.made_progress());
    }

    #[test]
    fn test_prep_completions() {
        // Provide matching schedules for injected completions
        let baseline = vec![
            Event::ActivityScheduled {
                event_id: 1,
                name: "a1".to_string(),
                input: "i1".to_string(),
                execution_id: 1,
            },
            Event::ActivityScheduled {
                event_id: 2,
                name: "a2".to_string(),
                input: "i2".to_string(),
                execution_id: 1,
            },
        ];
        let mut engine = ReplayEngine::new("test-instance".to_string(), 1, baseline);

        let messages = vec![
            WorkItem::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1,
                result: "result1".to_string(),
            },
            WorkItem::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 2,
                result: "result2".to_string(),
            },
        ];

        engine.prep_completions(messages);

        // Should have events in history_delta
        assert_eq!(engine.history_delta.len(), 2);
        assert!(engine.made_progress());
    }

    #[test]
    fn test_prep_completions_with_external_events() {
        let baseline_history = vec![
            Event::OrchestrationStarted {
                event_id: 0,
                name: "test-orch".to_string(),
                version: "1.0.0".to_string(),
                input: "test-input".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ExternalSubscribed {
                event_id: 5,
                name: "test-event".to_string(),
            },
        ];

        let mut engine = ReplayEngine::new(
            "test-instance".to_string(),
            1, // execution_id
            baseline_history,
        );

        let messages = vec![
            WorkItem::ExternalRaised {
                instance: "test-instance".to_string(),
                name: "test-event".to_string(),
                data: "event-data".to_string(),
            },
        ];

        engine.prep_completions(messages);

        // Should have external event in history_delta
        assert!(!engine.history_delta.is_empty());
        assert!(engine.made_progress());
    }

    #[test]
    fn test_prep_completions_duplicate_handling() {
        let mut engine = ReplayEngine::new("test-instance".to_string(), 1, vec![]);

        let messages = vec![
            WorkItem::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1,
                result: "first-result".to_string(),
            },
            WorkItem::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1, // Same ID - should be duplicate
                result: "second-result".to_string(),
            },
        ];

        engine.baseline_history = vec![
            Event::ActivityScheduled {
                event_id: 1,
                name: "test".to_string(),
                input: "test".to_string(),
                execution_id: 1,
            },
            Event::ActivityCompleted {
                event_id: 2,
                source_event_id: 1,
                result: "first-result".to_string(),
            },
        ];

        engine.prep_completions(messages);

        // Should have zero events (both duplicates were filtered)
        assert_eq!(engine.history_delta.len(), 0);
    }

    #[test]
    fn test_execute_orchestration_completed() {
        let baseline_history = vec![Event::OrchestrationStarted {
            event_id: 0,
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let mut engine = ReplayEngine::new(
            "test-instance".to_string(),
            1, // execution_id
            baseline_history,
        );

        let handler = Arc::new(MockHandler {
            result: Ok("orchestration-result".to_string()),
        });

        let result = engine.execute_orchestration(handler, "test-input".to_string());

        match result {
            TurnResult::Completed(output) => {
                assert_eq!(output, "orchestration-result");
            }
            _ => panic!("Expected TurnResult::Completed"),
        }
    }

    #[test]
    fn test_execute_orchestration_failed() {
        let baseline_history = vec![Event::OrchestrationStarted {
            event_id: 0,
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let mut engine = ReplayEngine::new(
            "test-instance".to_string(),
            1, // execution_id
            baseline_history,
        );

        let handler = Arc::new(MockHandler {
            result: Err("orchestration-error".to_string()),
        });

        let result = engine.execute_orchestration(handler, "test-input".to_string());

        match result {
            TurnResult::Failed(error) => {
                assert_eq!(error, "orchestration-error");
            }
            _ => panic!("Expected TurnResult::Failed"),
        }
    }

    #[test]
    fn test_execute_orchestration_with_unconsumed_completions() {
        let baseline_history = vec![Event::OrchestrationStarted {
            event_id: 0,
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let mut engine = ReplayEngine::new(
            "test-instance".to_string(),
            1, // execution_id
            baseline_history,
        );

        // Add completion that won't be consumed by the handler but has a matching schedule
        let messages = vec![
            WorkItem::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 999, // Scheduled below, not consumed by the mock handler
                result: "test-result".to_string(),
            },
        ];
        // Provide matching schedule for id=999
        engine.baseline_history.push(Event::ActivityScheduled {
            event_id: 999,
            name: "test-activity".to_string(),
            input: "test-input".to_string(),
            execution_id: 1,
        });
        engine.prep_completions(messages);

        let handler = Arc::new(MockHandler {
            result: Ok("orchestration-result".to_string()),
        });

        let result = engine.execute_orchestration(handler, "test-input".to_string());

        // With the mock handler, the orchestration completes successfully
        // The cursor model handles non-determinism naturally
        match result {
            TurnResult::Completed(_) => {
                // Orchestration completed
            }
            _ => panic!("Expected TurnResult::Completed"),
        }
    }

    #[test]
    fn test_final_history() {
        let baseline_history = vec![Event::OrchestrationStarted {
            event_id: 0,
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let mut engine = ReplayEngine::new(
            "test-instance".to_string(),
            1, // execution_id
            baseline_history.clone(),
        );

        // Add some delta events (simulating orchestration execution)
        engine.history_delta = vec![
            Event::ActivityScheduled {
                event_id: 1,
                name: "test-activity".to_string(),
                input: "activity-input".to_string(),
                execution_id: 1,
            },
            Event::ActivityCompleted {
                event_id: 2,
                source_event_id: 1,
                result: "activity-result".to_string(),
            },
        ];

        let final_history = engine.final_history();

        assert_eq!(final_history.len(), 3); // baseline + 2 delta events
        assert_eq!(final_history[0], baseline_history[0]);
        assert!(matches!(final_history[1], Event::ActivityScheduled { .. }));
        assert!(matches!(final_history[2], Event::ActivityCompleted { .. }));
    }

    #[test]
    fn test_made_progress() {
        let mut engine = ReplayEngine::new(
            "test-instance".to_string(),
            1,
            vec![Event::ActivityScheduled {
                event_id: 1,
                name: "test".to_string(),
                input: "input".to_string(),
                execution_id: 1,
            }],
        );

        // Initially no progress
        assert!(!engine.made_progress());

        // Add completion - should show progress
        let messages = vec![
            WorkItem::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1,
                result: "result".to_string(),
            },
        ];

        engine.prep_completions(messages);
        assert!(engine.made_progress());

        // Add history delta - should still show progress
        engine.history_delta = vec![Event::ActivityScheduled {
            event_id: 1,
            name: "test".to_string(),
            input: "input".to_string(),
            execution_id: 1,
        }];
        assert!(engine.made_progress());

        // Clear both - no progress
        engine.history_delta.clear();
        assert!(!engine.made_progress());
    }
}


