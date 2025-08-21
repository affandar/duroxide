use crate::Event;

pub fn detect_frontier_nondeterminism(prior: &[Event], deltas: &[Event]) -> Option<String> {
    // If prior has at least one schedule and no completions yet, and deltas include net-new schedules, flag
    let had_schedule = prior.iter().any(|e| matches!(e,
        Event::ActivityScheduled {..} | Event::TimerCreated {..} | Event::ExternalSubscribed {..} |
        Event::OrchestrationChained {..} | Event::SubOrchestrationScheduled {..}
    ));
    let had_completion = prior.iter().any(|e| matches!(e,
        Event::ActivityCompleted {..} | Event::ActivityFailed {..} | Event::TimerFired {..} |
        Event::ExternalEvent {..} | Event::SubOrchestrationCompleted {..} | Event::SubOrchestrationFailed {..}
    ));
    if had_schedule && !had_completion {
        let new_schedules = deltas.iter().filter(|e| matches!(e,
            Event::ActivityScheduled {..} | Event::TimerCreated {..} | Event::ExternalSubscribed {..} |
            Event::OrchestrationChained {..} | Event::SubOrchestrationScheduled {..}
        )).count();
        if new_schedules > 0 {
            return Some("nondeterministic: new schedules introduced at same decision frontier".to_string());
        }
    }
    None
}

pub fn detect_await_mismatch(last: &[(&'static str, u64)], claims: &crate::ClaimedIdsSnapshot) -> Option<String> {
    for (kind, id) in last {
        let ok = match *kind {
            "activity" => claims.activities.contains(id),
            "timer" => claims.timers.contains(id),
            "external" => claims.externals.contains(id),
            "sub" => claims.sub_orchestrations.contains(id),
            _ => true,
        };
        if !ok { return Some(format!("nondeterministic: completion id={} of kind '{}' was not awaited this turn", id, kind)); }
    }
    None
}

pub fn collect_last_appended(history: &[Event], start_idx: usize, out: &mut Vec<(&'static str, u64)>) {
    for e in &history[start_idx..] {
        match e {
            Event::ActivityCompleted { id, .. } => out.push(("activity", *id)),
            Event::ActivityFailed { id, .. } => out.push(("activity", *id)),
            Event::TimerFired { id, .. } => out.push(("timer", *id)),
            Event::ExternalEvent { id, .. } => out.push(("external", *id)),
            Event::SubOrchestrationCompleted { id, .. } => out.push(("sub", *id)),
            Event::SubOrchestrationFailed { id, .. } => out.push(("sub", *id)),
            _ => {}
        }
    }
}

pub fn detect_completion_kind_mismatch(prior: &[Event], last: &[(&'static str, u64)]) -> Option<String> {
    use std::collections::HashMap;
    let mut id_to_kind: HashMap<u64, &'static str> = HashMap::new();
    for e in prior {
        match e {
            Event::ActivityScheduled { id, .. } => { id_to_kind.insert(*id, "activity"); },
            Event::TimerCreated { id, .. } => { id_to_kind.insert(*id, "timer"); },
            Event::ExternalSubscribed { id, .. } => { id_to_kind.insert(*id, "external"); },
            Event::SubOrchestrationScheduled { id, .. } => { id_to_kind.insert(*id, "sub"); },
            _ => {}
        }
    }
    for (kind, id) in last {
        match id_to_kind.get(id).copied() {
            Some(expected) if expected != *kind => {
                return Some(format!(
                    "nondeterministic: completion kind mismatch for id={}, expected '{}', got '{}'",
                    id, expected, kind
                ));
            }
            Some(_) => {},
            None => {
                return Some(format!(
                    "nondeterministic: completion kind '{}' for id={} has no matching schedule in prior history",
                    kind, id
                ));
            }
        }
    }
    None
}


