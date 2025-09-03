#[cfg(test)]
mod tests {
    use crate::runtime::completion_map::CompletionKind;
    use crate::runtime::orchestration_turn::*;
    use crate::runtime::router::OrchestratorMsg;
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
    fn test_turn_creation() {
        let baseline_history = vec![Event::OrchestrationStarted {
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let turn = OrchestrationTurn::new(
            "test-instance".to_string(),
            1,
            1, // execution_id
            baseline_history.clone(),
        );

        assert_eq!(turn.instance, "test-instance");
        assert_eq!(turn.turn_index, 1);
        assert_eq!(turn.baseline_history, baseline_history);
        assert!(turn.ack_tokens.is_empty());
        assert!(turn.history_delta.is_empty());
        assert!(turn.pending_actions.is_empty());
        assert!(!turn.made_progress());
    }

    #[test]
    fn test_prep_completions() {
        let mut turn = OrchestrationTurn::new("test-instance".to_string(), 1, 1, vec![]);

        let messages = vec![
            (
                OrchestratorMsg::ActivityCompleted {
                    instance: "test-instance".to_string(),
                    execution_id: 1,
                    id: 1,
                    result: "result1".to_string(),
                    ack_token: Some("token1".to_string()),
                },
                "token1".to_string(),
            ),
            (
                OrchestratorMsg::ActivityCompleted {
                    instance: "test-instance".to_string(),
                    execution_id: 1,
                    id: 2,
                    result: "result2".to_string(),
                    ack_token: Some("token2".to_string()),
                },
                "token2".to_string(),
            ),
        ];

        turn.prep_completions(messages);

        // Should have ack tokens
        assert_eq!(turn.ack_tokens.len(), 2);
        assert!(turn.ack_tokens.contains(&"token1".to_string()));
        assert!(turn.ack_tokens.contains(&"token2".to_string()));

        // Should have completions in map
        assert!(turn.completion_map.is_next_ready(CompletionKind::Activity, 1));
        assert!(!turn.completion_map.is_next_ready(CompletionKind::Activity, 2)); // Not ready yet

        // Should indicate progress
        assert!(turn.made_progress());
    }

    #[test]
    fn test_prep_completions_with_external_events() {
        let baseline_history = vec![
            Event::OrchestrationStarted {
                name: "test-orch".to_string(),
                version: "1.0.0".to_string(),
                input: "test-input".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ExternalSubscribed {
                id: 5,
                name: "test-event".to_string(),
            },
        ];

        let mut turn = OrchestrationTurn::new(
            "test-instance".to_string(),
            1,
            1, // execution_id
            baseline_history,
        );

        let messages = vec![(
            OrchestratorMsg::ExternalByName {
                instance: "test-instance".to_string(),
                name: "test-event".to_string(),
                data: "event-data".to_string(),
                ack_token: Some("external-token".to_string()),
            },
            "external-token".to_string(),
        )];

        turn.prep_completions(messages);

        // Should have external completion
        assert!(turn.completion_map.is_next_ready(CompletionKind::External, 5));
        assert_eq!(turn.ack_tokens.len(), 1);
        assert!(turn.made_progress());
    }

    #[test]
    fn test_prep_completions_duplicate_handling() {
        let mut turn = OrchestrationTurn::new("test-instance".to_string(), 1, 1, vec![]);

        let messages = vec![
            (
                OrchestratorMsg::ActivityCompleted {
                    instance: "test-instance".to_string(),
                    execution_id: 1,
                    id: 1,
                    result: "first-result".to_string(),
                    ack_token: Some("token1".to_string()),
                },
                "token1".to_string(),
            ),
            (
                OrchestratorMsg::ActivityCompleted {
                    instance: "test-instance".to_string(),
                    execution_id: 1,
                    id: 1, // Same ID - should be duplicate
                    result: "second-result".to_string(),
                    ack_token: Some("token2".to_string()),
                },
                "token2".to_string(),
            ),
        ];

        turn.prep_completions(messages);

        // Should have both ack tokens (even for duplicates)
        assert_eq!(turn.ack_tokens.len(), 2);

        // Should only have one completion (duplicate detected)
        assert!(turn.completion_map.is_next_ready(CompletionKind::Activity, 1));
        let comp = turn
            .completion_map_mut()
            .get_ready_completion(CompletionKind::Activity, 1);
        assert!(comp.is_some());

        // No more completions
        assert!(!turn.completion_map.has_unconsumed());
    }

    #[test]
    fn test_execute_orchestration_completed() {
        let baseline_history = vec![Event::OrchestrationStarted {
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let mut turn = OrchestrationTurn::new(
            "test-instance".to_string(),
            1,
            1, // execution_id
            baseline_history,
        );

        let handler = Arc::new(MockHandler {
            result: Ok("orchestration-result".to_string()),
        });

        let result = turn.execute_orchestration(handler, "test-input".to_string());

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
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let mut turn = OrchestrationTurn::new(
            "test-instance".to_string(),
            1,
            1, // execution_id
            baseline_history,
        );

        let handler = Arc::new(MockHandler {
            result: Err("orchestration-error".to_string()),
        });

        let result = turn.execute_orchestration(handler, "test-input".to_string());

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
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let mut turn = OrchestrationTurn::new(
            "test-instance".to_string(),
            1,
            1, // execution_id
            baseline_history,
        );

        // Add completion that won't be consumed
        let messages = vec![(
            OrchestratorMsg::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 999, // This won't be consumed by the mock handler
                result: "unused-result".to_string(),
                ack_token: Some("unused-token".to_string()),
            },
            "unused-token".to_string(),
        )];

        turn.prep_completions(messages);

        let handler = Arc::new(MockHandler {
            result: Ok("orchestration-result".to_string()),
        });

        let result = turn.execute_orchestration(handler, "test-input".to_string());

        // With the mock handler, the orchestration completes successfully
        // but leaves unconsumed completions - this validates that our completion map works
        match result {
            TurnResult::Completed(_) => {
                // The orchestration completed, but we should have unconsumed completions
                assert!(turn.completion_map.has_unconsumed());
                let unconsumed = turn.completion_map.get_unconsumed();
                assert_eq!(unconsumed.len(), 1);
                assert_eq!(unconsumed[0], (CompletionKind::Activity, 999));

                // This validates that:
                // 1. Completions are properly tracked in the map
                // 2. They persist through orchestration execution
                // 3. The system can detect unconsumed state
                //
                // NOTE: The actual non-determinism detection happens in the replay engine
                // when real orchestrations try to consume completions in the wrong order
            }
            _ => panic!("Expected TurnResult::Completed with unconsumed completions"),
        }
    }

    #[test]
    fn test_final_history() {
        let baseline_history = vec![Event::OrchestrationStarted {
            name: "test-orch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let mut turn = OrchestrationTurn::new(
            "test-instance".to_string(),
            1,
            1, // execution_id
            baseline_history.clone(),
        );

        // Add some delta events (simulating orchestration execution)
        turn.history_delta = vec![
            Event::ActivityScheduled {
                id: 1,
                name: "test-activity".to_string(),
                input: "activity-input".to_string(),
                execution_id: 1,
            },
            Event::ActivityCompleted {
                id: 1,
                result: "activity-result".to_string(),
            },
        ];

        let final_history = turn.final_history();

        assert_eq!(final_history.len(), 3); // baseline + 2 delta events
        assert_eq!(final_history[0], baseline_history[0]);
        assert!(matches!(final_history[1], Event::ActivityScheduled { .. }));
        assert!(matches!(final_history[2], Event::ActivityCompleted { .. }));
    }

    #[test]
    fn test_made_progress() {
        let mut turn = OrchestrationTurn::new("test-instance".to_string(), 1, 1, vec![]);

        // Initially no progress
        assert!(!turn.made_progress());

        // Add completion - should show progress
        let messages = vec![(
            OrchestratorMsg::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1,
                result: "result".to_string(),
                ack_token: Some("token".to_string()),
            },
            "token".to_string(),
        )];

        turn.prep_completions(messages);
        assert!(turn.made_progress());

        // Clear completions but add history delta - should still show progress
        turn.completion_map = crate::runtime::completion_map::CompletionMap::new();
        turn.history_delta = vec![Event::ActivityScheduled {
            id: 1,
            name: "test".to_string(),
            input: "input".to_string(),
            execution_id: 1,
        }];
        assert!(turn.made_progress());

        // Clear both - no progress
        turn.history_delta.clear();
        assert!(!turn.made_progress());
    }
}
