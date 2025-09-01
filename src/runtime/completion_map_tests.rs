#[cfg(test)]
mod tests {
    use super::*;
    use crate::Event;
    use crate::runtime::completion_map::*;
    use crate::runtime::router::OrchestratorMsg;

    #[test]
    fn test_completion_map_deterministic_ordering() {
        let mut map = CompletionMap::new();

        // Add completions in reverse correlation ID order to test arrival ordering
        let msg1 = OrchestratorMsg::ActivityCompleted {
            instance: "test".to_string(),
            execution_id: 1,
            id: 5,
            result: "result5".to_string(),
            ack_token: Some("token5".to_string()),
        };
        let msg2 = OrchestratorMsg::ActivityCompleted {
            instance: "test".to_string(),
            execution_id: 1,
            id: 2,
            result: "result2".to_string(),
            ack_token: Some("token2".to_string()),
        };
        let msg3 = OrchestratorMsg::ActivityCompleted {
            instance: "test".to_string(),
            execution_id: 1,
            id: 10,
            result: "result10".to_string(),
            ack_token: Some("token10".to_string()),
        };

        // Add in order: 5, 2, 10
        let token1 = map.add_completion(msg1);
        let token2 = map.add_completion(msg2);
        let token3 = map.add_completion(msg3);

        assert_eq!(token1, Some("token5".to_string()));
        assert_eq!(token2, Some("token2".to_string()));
        assert_eq!(token3, Some("token10".to_string()));

        // Verify arrival order (not correlation ID order)
        assert!(map.is_next_ready(CompletionKind::Activity, 5)); // First arrival
        assert!(!map.is_next_ready(CompletionKind::Activity, 2)); // Second arrival, not ready yet
        assert!(!map.is_next_ready(CompletionKind::Activity, 10)); // Third arrival, not ready yet

        // Consume first completion (id=5)
        let comp1 = map.get_ready_completion(CompletionKind::Activity, 5);
        assert!(comp1.is_some());
        assert_eq!(comp1.unwrap().correlation_id, 5);

        // Now second arrival should be ready (id=2)
        assert!(map.is_next_ready(CompletionKind::Activity, 2));
        assert!(!map.is_next_ready(CompletionKind::Activity, 10));

        // Consume second completion (id=2)
        let comp2 = map.get_ready_completion(CompletionKind::Activity, 2);
        assert!(comp2.is_some());
        assert_eq!(comp2.unwrap().correlation_id, 2);

        // Now third arrival should be ready (id=10)
        assert!(map.is_next_ready(CompletionKind::Activity, 10));
        let comp3 = map.get_ready_completion(CompletionKind::Activity, 10);
        assert!(comp3.is_some());
        assert_eq!(comp3.unwrap().correlation_id, 10);

        // No more completions
        assert!(!map.has_unconsumed());
    }

    #[test]
    fn test_completion_map_duplicate_detection() {
        let mut map = CompletionMap::new();

        let msg1 = OrchestratorMsg::ActivityCompleted {
            instance: "test".to_string(),
            execution_id: 1,
            id: 1,
            result: "first".to_string(),
            ack_token: Some("token1".to_string()),
        };
        let msg2 = OrchestratorMsg::ActivityCompleted {
            instance: "test".to_string(),
            execution_id: 1,
            id: 1, // Same ID - should be duplicate
            result: "second".to_string(),
            ack_token: Some("token2".to_string()),
        };

        let token1 = map.add_completion(msg1);
        let token2 = map.add_completion(msg2); // Should be ignored

        assert_eq!(token1, Some("token1".to_string()));
        assert_eq!(token2, Some("token2".to_string())); // Returns token but doesn't add

        // Only one completion should be in the map
        assert!(map.is_next_ready(CompletionKind::Activity, 1));
        let comp = map.get_ready_completion(CompletionKind::Activity, 1);
        assert!(comp.is_some());

        // Should be the first completion (first wins)
        if let CompletionValue::ActivityResult(Ok(result)) = comp.unwrap().data {
            assert_eq!(result, "first");
        } else {
            panic!("Expected ActivityResult");
        }

        // No more completions
        assert!(!map.has_unconsumed());
    }

    #[test]
    fn test_completion_map_mixed_completion_types() {
        let mut map = CompletionMap::new();

        // Add different types of completions
        let activity_msg = OrchestratorMsg::ActivityCompleted {
            instance: "test".to_string(),
            execution_id: 1,
            id: 1,
            result: "activity_result".to_string(),
            ack_token: Some("activity_token".to_string()),
        };
        let timer_msg = OrchestratorMsg::TimerFired {
            instance: "test".to_string(),
            execution_id: 1,
            id: 2,
            fire_at_ms: 1000,
            ack_token: Some("timer_token".to_string()),
        };
        let suborh_msg = OrchestratorMsg::SubOrchCompleted {
            instance: "test".to_string(),
            execution_id: 1,
            id: 3,
            result: "suborh_result".to_string(),
            ack_token: Some("suborh_token".to_string()),
        };

        // Add in order: activity, timer, suborh
        map.add_completion(activity_msg);
        map.add_completion(timer_msg);
        map.add_completion(suborh_msg);

        // Should be able to consume in arrival order regardless of type
        assert!(map.is_next_ready(CompletionKind::Activity, 1));
        assert!(!map.is_next_ready(CompletionKind::Timer, 2));
        assert!(!map.is_next_ready(CompletionKind::SubOrchestration, 3));

        // Consume activity
        let comp1 = map.get_ready_completion(CompletionKind::Activity, 1);
        assert!(comp1.is_some());

        // Now timer should be ready
        assert!(map.is_next_ready(CompletionKind::Timer, 2));
        let comp2 = map.get_ready_completion(CompletionKind::Timer, 2);
        assert!(comp2.is_some());

        // Now suborh should be ready
        assert!(map.is_next_ready(CompletionKind::SubOrchestration, 3));
        let comp3 = map.get_ready_completion(CompletionKind::SubOrchestration, 3);
        assert!(comp3.is_some());
    }

    #[test]
    fn test_completion_map_external_events() {
        let mut map = CompletionMap::new();

        // Create history with external subscription
        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                input: "input".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ExternalSubscribed {
                id: 5,
                name: "test_event".to_string(),
            },
        ];

        // Add external completion
        let token = map.add_external_completion(
            "test_event".to_string(),
            "event_data".to_string(),
            &history,
            Some("external_token".to_string()),
        );

        assert_eq!(token, Some("external_token".to_string()));
        assert!(map.is_next_ready(CompletionKind::External, 5));

        let comp = map.get_ready_completion(CompletionKind::External, 5);
        assert!(comp.is_some());
        if let CompletionValue::ExternalData { name, data } = comp.unwrap().data {
            assert_eq!(name, "test_event");
            assert_eq!(data, "event_data");
        } else {
            panic!("Expected ExternalData");
        }
    }

    #[test]
    fn test_completion_map_external_events_no_subscription() {
        let mut map = CompletionMap::new();

        // Create history without external subscription
        let history = vec![Event::OrchestrationStarted {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            input: "input".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        // Add external completion for non-subscribed event
        let token = map.add_external_completion(
            "unknown_event".to_string(),
            "event_data".to_string(),
            &history,
            Some("external_token".to_string()),
        );

        // Should return token but not add completion
        assert_eq!(token, Some("external_token".to_string()));
        assert!(!map.has_unconsumed());
    }

    #[test]
    fn test_completion_map_cleanup_consumed() {
        let mut map = CompletionMap::new();

        // Add several completions
        for i in 1..=5 {
            let msg = OrchestratorMsg::ActivityCompleted {
                instance: "test".to_string(),
                execution_id: 1,
                id: i,
                result: format!("result{}", i),
                ack_token: Some(format!("token{}", i)),
            };
            map.add_completion(msg);
        }

        assert_eq!(map.ordered.len(), 5);

        // Consume first 3
        for i in 1..=3 {
            assert!(map.is_next_ready(CompletionKind::Activity, i));
            let comp = map.get_ready_completion(CompletionKind::Activity, i);
            assert!(comp.is_some());
        }

        // Before cleanup - should still have 5 entries (3 consumed, 2 pending)
        assert_eq!(map.ordered.len(), 5);
        assert_eq!(map.by_id.len(), 2); // Only unconsumed remain in by_id

        // Cleanup consumed entries
        map.cleanup_consumed();

        // After cleanup - should only have 2 entries
        assert_eq!(map.ordered.len(), 2);
        assert_eq!(map.by_id.len(), 2);

        // Should still be able to consume remaining
        assert!(map.is_next_ready(CompletionKind::Activity, 4));
        let comp4 = map.get_ready_completion(CompletionKind::Activity, 4);
        assert!(comp4.is_some());
    }

    #[test]
    fn test_completion_map_unconsumed_detection() {
        let mut map = CompletionMap::new();

        // Add completions
        for i in 1..=3 {
            let msg = OrchestratorMsg::ActivityCompleted {
                instance: "test".to_string(),
                execution_id: 1,
                id: i,
                result: format!("result{}", i),
                ack_token: Some(format!("token{}", i)),
            };
            map.add_completion(msg);
        }

        // Consume only the first one
        assert!(map.is_next_ready(CompletionKind::Activity, 1));
        let comp1 = map.get_ready_completion(CompletionKind::Activity, 1);
        assert!(comp1.is_some());

        // Should detect unconsumed completions
        assert!(map.has_unconsumed());
        let unconsumed = map.get_unconsumed();
        assert_eq!(unconsumed.len(), 2);
        assert_eq!(unconsumed[0], (CompletionKind::Activity, 2));
        assert_eq!(unconsumed[1], (CompletionKind::Activity, 3));
    }
}
