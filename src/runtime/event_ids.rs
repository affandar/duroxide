use crate::Event;

/// Runtime-only wrapper to carry a unique, monotonically increasing event_id
/// and an optional scheduled_event_id reference for completion-type events.
#[derive(Debug, Clone)]
pub struct ReplayHistoryEvent {
    pub event_id: u64,
    pub scheduled_event_id: Option<u64>,
    pub event: Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ScheduleKind {
    Activity,
    Timer,
    External,
    SubOrch,
}

fn schedule_key_for_event(e: &Event) -> Option<(ScheduleKind, u64)> {
    match e {
        Event::ActivityScheduled { id, .. } => Some((ScheduleKind::Activity, *id)),
        Event::TimerCreated { id, .. } => Some((ScheduleKind::Timer, *id)),
        Event::ExternalSubscribed { id, .. } => Some((ScheduleKind::External, *id)),
        Event::SubOrchestrationScheduled { id, .. } => Some((ScheduleKind::SubOrch, *id)),
        _ => None,
    }
}

fn completion_key_for_event(e: &Event) -> Option<(ScheduleKind, u64)> {
    match e {
        Event::ActivityCompleted { id, .. } => Some((ScheduleKind::Activity, *id)),
        Event::ActivityFailed { id, .. } => Some((ScheduleKind::Activity, *id)),
        Event::TimerFired { id, .. } => Some((ScheduleKind::Timer, *id)),
        Event::ExternalEvent { id, .. } => Some((ScheduleKind::External, *id)),
        Event::SubOrchestrationCompleted { id, .. } => Some((ScheduleKind::SubOrch, *id)),
        Event::SubOrchestrationFailed { id, .. } => Some((ScheduleKind::SubOrch, *id)),
        _ => None,
    }
}

/// TODO : CR : IMPORTANT: When merging into mainline code, the "completion key for event" function should not be needed
/// the scheduled event id should be written by the entities that are emitting the completion events in the first place
///
/// Assign deterministic event_id values to the provided delta events based on the
/// baseline history length, and populate scheduled_event_id for completion events
/// by referencing the corresponding schedule event's event_id.
///
/// The ordering of `delta` is preserved exactly as provided.
pub fn assign_event_ids_for_delta(baseline: &[Event], delta: &[Event]) -> Vec<ReplayHistoryEvent> {
    use std::collections::HashMap;

    // Build mapping from scheduled correlation (kind, id) to event_id from baseline history
    let mut schedule_to_event_id: HashMap<(ScheduleKind, u64), u64> = HashMap::new();
    for (idx, e) in baseline.iter().enumerate() {
        let event_id = (idx as u64) + 1; // 1-based indexing per execution
        if let Some(key) = schedule_key_for_event(e) {
            schedule_to_event_id.insert(key, event_id);
        }
    }

    let mut next_event_id: u64 = (baseline.len() as u64) + 1;
    let mut out: Vec<ReplayHistoryEvent> = Vec::with_capacity(delta.len());

    for e in delta.iter().cloned() {
        // Determine scheduled_event_id if this is a completion
        let scheduled_event_id = if let Some(key) = completion_key_for_event(&e) {
            schedule_to_event_id.get(&key).copied()
        } else {
            None
        };

        // Assign an event_id to this delta event
        let wrapped = ReplayHistoryEvent {
            event_id: next_event_id,
            scheduled_event_id,
            event: e.clone(),
        };

        // If this event is itself a new schedule, record its event_id for later completions
        if let Some(key) = schedule_key_for_event(&wrapped.event) {
            schedule_to_event_id.insert(key, wrapped.event_id);
        }

        out.push(wrapped);
        next_event_id += 1;
    }

    out
}

/// Validate that wrapped delta has monotonically increasing event_ids starting at
/// baseline.len()+1 and that completion events reference an existing schedule's event_id
/// either in baseline or earlier in the wrapped delta. Returns Ok(()) if valid; Err(msg) otherwise.
pub fn validate_wrapped_delta(baseline: &[Event], wrapped: &[ReplayHistoryEvent]) -> Result<(), String> {
    use std::collections::HashMap;
    let mut expected_next = (baseline.len() as u64) + 1;

    // Build schedule mapping from baseline first
    let mut schedule_to_event_id: HashMap<(ScheduleKind, u64), u64> = HashMap::new();
    for (idx, e) in baseline.iter().enumerate() {
        let event_id = (idx as u64) + 1;
        if let Some(key) = schedule_key_for_event(e) {
            schedule_to_event_id.insert(key, event_id);
        }
    }

    for w in wrapped {
        if w.event_id != expected_next {
            return Err(format!(
                "non-monotonic event_id: expected {}, found {}",
                expected_next, w.event_id
            ));
        }
        expected_next += 1;

        // If completion, ensure we can resolve a schedule event_id
        if let Some((kind, cid)) = completion_key_for_event(&w.event) {
            // prefer scheduled_event_id if present, else resolve now
            let resolved = if let Some(seid) = w.scheduled_event_id {
                Some(seid)
            } else {
                schedule_to_event_id.get(&(kind, cid)).copied()
            };

            if resolved.is_none() {
                return Err(format!(
                    "completion missing schedule reference: kind={:?} id={}",
                    kind, cid
                ));
            }
        }

        // If this is a schedule, record mapping for subsequent completions in this delta
        if let Some(key) = schedule_key_for_event(&w.event) {
            schedule_to_event_id.insert(key, w.event_id);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monotonic_assignment_and_baseline_offset() {
        let baseline = vec![
            Event::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "A".to_string(),
                input: "x".to_string(),
                execution_id: 1,
            },
        ];

        let delta = vec![
            Event::ActivityCompleted {
                id: 1,
                result: "ok".to_string(),
            },
            Event::TimerCreated {
                id: 2,
                fire_at_ms: 123,
                execution_id: 1,
            },
        ];

        let wrapped = assign_event_ids_for_delta(&baseline, &delta);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].event_id, 3);
        assert_eq!(wrapped[1].event_id, 4);
        // completion should reference schedule id=1 which was at baseline event_id=2
        assert_eq!(wrapped[0].scheduled_event_id, Some(2));
        // timer created is a schedule event, not a completion
        assert_eq!(wrapped[1].scheduled_event_id, None);
    }

    #[test]
    fn test_same_tick_schedule_then_complete() {
        let baseline = vec![Event::OrchestrationStarted {
            name: "Test".to_string(),
            version: "1.0.0".to_string(),
            input: "{}".to_string(),
            parent_instance: None,
            parent_id: None,
        }];

        let delta = vec![
            Event::ActivityScheduled {
                id: 5,
                name: "A".to_string(),
                input: "x".to_string(),
                execution_id: 1,
            },
            Event::ActivityCompleted {
                id: 5,
                result: "ok".to_string(),
            },
        ];

        let wrapped = assign_event_ids_for_delta(&baseline, &delta);
        assert_eq!(wrapped.len(), 2);
        // schedule gets id 2
        assert_eq!(wrapped[0].event_id, 2);
        // completion references schedule's event_id
        assert_eq!(wrapped[1].scheduled_event_id, Some(2));
    }

    #[test]
    fn test_order_preserved() {
        let baseline: Vec<Event> = vec![];
        let delta = vec![
            Event::TimerCreated {
                id: 1,
                fire_at_ms: 10,
                execution_id: 1,
            },
            Event::ExternalSubscribed {
                id: 2,
                name: "E".to_string(),
            },
            Event::ExternalEvent {
                id: 2,
                name: "E".to_string(),
                data: "d".to_string(),
            },
        ];
        let wrapped = assign_event_ids_for_delta(&baseline, &delta);
        assert_eq!(
            wrapped
                .iter()
                .map(|w| std::mem::discriminant(&w.event))
                .collect::<Vec<_>>(),
            delta.iter().map(|e| std::mem::discriminant(e)).collect::<Vec<_>>()
        );
        assert_eq!(wrapped[2].scheduled_event_id, Some(2)); // ExternalSubscribed at event_id 2
        assert_eq!(wrapped[0].event_id, 1);
        assert_eq!(wrapped[1].event_id, 2);
        assert_eq!(wrapped[2].event_id, 3);
    }
}
