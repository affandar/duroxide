use tracing::{debug, error, warn};
use crate::Event;
use super::router::OrchestratorMsg;

pub fn append_completion(history: &mut Vec<Event>, msg: OrchestratorMsg) -> (Option<String>, bool) {
    match msg {
    OrchestratorMsg::ActivityCompleted { instance, id, result, ack_token } => {
            let is_system_trace = history.iter().rev().any(|e| matches!(e, Event::ActivityScheduled { id: cid, name, .. } if *cid == id && name == crate::SYSTEM_TRACE_ACTIVITY));
            if is_system_trace {
                if let Some((lvl, msg_text)) = result.split_once(':') {
                    match lvl { "ERROR" | "error" => error!(instance=%instance, id, "{}", msg_text),
                                  "WARN" | "warn" => warn!(instance=%instance, id, "{}", msg_text),
                                  "INFO" | "info" => debug!(instance=%instance, id, "{}", msg_text),
                                  _ => debug!(instance=%instance, id, "trace: {}", msg_text), }
                }
                return (ack_token, false);
            }
            let duplicate = history.iter().any(|e| matches!(e, Event::ActivityCompleted { id: cid, .. } if *cid == id));
            if duplicate { return (ack_token, false); }
            history.push(Event::ActivityCompleted { id, result });
            (ack_token, true)
        }
        OrchestratorMsg::ActivityFailed { id, error, ack_token, .. } => {
            if history.iter().any(|e| matches!(e, Event::ActivityFailed { id: cid, .. } if *cid == id)) { return (ack_token, false); }
            history.push(Event::ActivityFailed { id, error });
            (ack_token, true)
        }
        OrchestratorMsg::TimerFired { id, fire_at_ms, ack_token, .. } => {
            if history.iter().any(|e| matches!(e, Event::TimerFired { id: cid, .. } if *cid == id)) { return (ack_token, false); }
            history.push(Event::TimerFired { id, fire_at_ms });
            (ack_token, true)
        }
        OrchestratorMsg::ExternalEvent { id, name, data, ack_token, .. } => {
            if history.iter().any(|e| matches!(e, Event::ExternalSubscribed { id: cid, name: n } if *cid == id && *n == name)) {
                if history.iter().any(|e| matches!(e, Event::ExternalEvent { id: cid, name: n, .. } if *cid == id && *n == name)) { return (ack_token, false); }
                history.push(Event::ExternalEvent { id, name, data });
                return (ack_token, true);
            }
            warn!(id, name=%name, "dropping external: no active subscription");
            (ack_token, false)
        }
        OrchestratorMsg::ExternalByName { name, data, ack_token, .. } => {
            let accepted = history.iter().rev().find_map(|e| match e {
                Event::ExternalSubscribed { id, name: n } if *n == name => Some(*id), _ => None
            });
            match accepted {
                Some(id) => {
                    if history.iter().any(|e| matches!(e, Event::ExternalEvent { id: cid, name: n, .. } if *cid == id && *n == name)) { return (ack_token, false); }
                    history.push(Event::ExternalEvent { id, name, data });
                    (ack_token, true)
                }
                None => { warn!(name=%name, "dropping external by name: no active subscription"); (ack_token, false) }
            }
        }
        OrchestratorMsg::SubOrchCompleted { id, result, ack_token, .. } => {
            if history.iter().any(|e| matches!(e, Event::SubOrchestrationCompleted { id: cid, .. } if *cid == id)) { return (ack_token, false); }
            history.push(Event::SubOrchestrationCompleted { id, result });
            (ack_token, true)
        }
        OrchestratorMsg::SubOrchFailed { id, error, ack_token, .. } => {
            if history.iter().any(|e| matches!(e, Event::SubOrchestrationFailed { id: cid, .. } if *cid == id)) { return (ack_token, false); }
            history.push(Event::SubOrchestrationFailed { id, error });
            (ack_token, true)
        }
    }
}

pub async fn rehydrate_pending(instance: &str, history: &[Event], activity_tx: &tokio::sync::mpsc::Sender<super::ActivityWorkItem>, timer_tx: &tokio::sync::mpsc::Sender<super::TimerWorkItem>) {
    for e in history {
        match e {
            Event::ActivityScheduled { id, name, input } => {
                let _ = activity_tx.send(super::ActivityWorkItem { instance: instance.to_string(), id: *id, name: name.clone(), input: input.clone() }).await;
            }
            Event::TimerCreated { id, fire_at_ms } => {
                let now = super::Runtime::now_ms_static();
                if *fire_at_ms <= now {
                    let _ = timer_tx.send(super::TimerWorkItem { instance: instance.to_string(), id: *id, fire_at_ms: *fire_at_ms, delay_ms: 0 }).await;
                } else {
                    let delay = fire_at_ms.saturating_sub(now);
                    let _ = timer_tx.send(super::TimerWorkItem { instance: instance.to_string(), id: *id, fire_at_ms: *fire_at_ms, delay_ms: delay }).await;
                }
            }
            _ => {}
        }
    }
}


impl super::Runtime {
    pub(crate) async fn handle_continue_as_new(
        &self,
        instance: &str,
        orchestration_name: &str,
        history: &mut Vec<Event>,
        input: String,
        version: Option<String>,
    ) {
        let term = Event::OrchestrationContinuedAsNew { input: input.clone() };
        if let Err(e) = self.history_store.append(instance, vec![term.clone()]).await {
            tracing::error!(instance, error=%e, "failed to append continued-as-new");
            panic!("history append failed: {e}");
        }
        history.push(term);
        // Resolve target version: explicit arg wins; else use registry policy resolution
        let orch_name = self.history_store.get_instance_orchestration(instance).await
            .unwrap_or_else(|| orchestration_name.to_string());
        if let Some(ver_str) = version {
            if let Ok(v) = semver::Version::parse(&ver_str) {
                self.pinned_versions.lock().await.insert(instance.to_string(), v);
            }
        } else if let Some((v, _h)) = self.orchestration_registry.resolve_for_start(&orch_name).await {
            self.pinned_versions.lock().await.insert(instance.to_string(), v);
        }
        // Reset to a new execution and enqueue start
        let _new_exec = self.history_store.reset_for_continue_as_new(instance, &orch_name, &input).await
            .unwrap_or(1);
        let _ = self.start_tx.send(super::StartRequest { instance: instance.to_string(), orchestration_name: orch_name.clone() });
        // Notify any waiters so initial handle resolves (empty output for continued-as-new)
        if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
            for w in waiters { let _ = w.send((history.clone(), Ok(String::new()))); }
        }
    }
}


