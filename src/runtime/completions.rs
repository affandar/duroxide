use super::router::OrchestratorMsg;
use crate::Event;
use crate::providers::{HistoryStore, QueueKind, WorkItem};
use std::sync::Arc;
use tracing::{debug, error, warn};

pub fn append_completion(history: &mut Vec<Event>, msg: OrchestratorMsg) -> (Option<String>, bool) {
    match msg {
        OrchestratorMsg::ActivityCompleted {
            instance,
            execution_id: _,
            id,
            result,
            ack_token,
        } => {
            let is_system_trace = history.iter().rev().any(|e| matches!(e, Event::ActivityScheduled { id: cid, name, .. } if *cid == id && name == crate::SYSTEM_TRACE_ACTIVITY));
            if is_system_trace {
                if let Some((lvl, msg_text)) = result.split_once(':') {
                    match lvl {
                        "ERROR" | "error" => error!(instance=%instance, id, "{}", msg_text),
                        "WARN" | "warn" => warn!(instance=%instance, id, "{}", msg_text),
                        "INFO" | "info" => debug!(instance=%instance, id, "{}", msg_text),
                        _ => debug!(instance=%instance, id, "trace: {}", msg_text),
                    }
                }
                return (ack_token, false);
            }
            let duplicate = history
                .iter()
                .any(|e| matches!(e, Event::ActivityCompleted { id: cid, .. } if *cid == id));
            if duplicate {
                return (ack_token, false);
            }
            history.push(Event::ActivityCompleted { id, result });
            (ack_token, true)
        }
        OrchestratorMsg::ActivityFailed {
            execution_id: _,
            id, 
            error, 
            ack_token, 
            ..
        } => {
            if history
                .iter()
                .any(|e| matches!(e, Event::ActivityFailed { id: cid, .. } if *cid == id))
            {
                return (ack_token, false);
            }
            history.push(Event::ActivityFailed { id, error });
            (ack_token, true)
        }
        OrchestratorMsg::TimerFired {
            execution_id: _,
            id,
            fire_at_ms,
            ack_token,
            ..
        } => {
            if history
                .iter()
                .any(|e| matches!(e, Event::TimerFired { id: cid, .. } if *cid == id))
            {
                return (ack_token, false);
            }
            history.push(Event::TimerFired { id, fire_at_ms });
            (ack_token, true)
        }
        
        OrchestratorMsg::ExternalByName {
            name, data, ack_token, ..
        } => {
            let accepted = history.iter().rev().find_map(|e| match e {
                Event::ExternalSubscribed { id, name: n } if *n == name => Some(*id),
                _ => None,
            });
            match accepted {
                Some(id) => {
                    if history
                        .iter()
                        .any(|e| matches!(e, Event::ExternalEvent { id: cid, name: n, .. } if *cid == id && *n == name))
                    {
                        return (ack_token, false);
                    }
                    history.push(Event::ExternalEvent { id, name, data });
                    (ack_token, true)
                }
                None => {
                    warn!(name=%name, "dropping external by name: no active subscription");
                    (ack_token, false)
                }
            }
        }
        OrchestratorMsg::SubOrchCompleted {
            execution_id: _,
            id, 
            result, 
            ack_token, 
            ..
        } => {
            if history
                .iter()
                .any(|e| matches!(e, Event::SubOrchestrationCompleted { id: cid, .. } if *cid == id))
            {
                return (ack_token, false);
            }
            history.push(Event::SubOrchestrationCompleted { id, result });
            (ack_token, true)
        }
        OrchestratorMsg::SubOrchFailed {
            execution_id: _,
            id, 
            error, 
            ack_token, 
            ..
        } => {
            if history
                .iter()
                .any(|e| matches!(e, Event::SubOrchestrationFailed { id: cid, .. } if *cid == id))
            {
                return (ack_token, false);
            }
            history.push(Event::SubOrchestrationFailed { id, error });
            (ack_token, true)
        }
        OrchestratorMsg::CancelRequested { reason, ack_token, .. } => {
            // Idempotent append of cancel request
            if history
                .iter()
                .any(|e| matches!(e, Event::OrchestrationCancelRequested { .. }))
            {
                return (ack_token, false);
            }
            history.push(Event::OrchestrationCancelRequested { reason });
            (ack_token, true)
        }
    }
}

pub async fn rehydrate_pending(instance: &str, history: &[Event], history_store: &Arc<dyn HistoryStore>) {
    for e in history {
        match e {
            Event::ActivityScheduled { id, name, input } => {
                let _ = history_store
                    .enqueue_work(
                        QueueKind::Worker,
                        WorkItem::ActivityExecute {
                            instance: instance.to_string(),
                            id: *id,
                            name: name.clone(),
                            input: input.clone(),
                        },
                    )
                    .await;
            }
            Event::TimerCreated { id, fire_at_ms } => {
                // Enqueue a schedule request; TimerDispatcher will deliver TimerFired after delay
                let _ = history_store
                    .enqueue_work(
                        QueueKind::Timer,
                        WorkItem::TimerSchedule {
                            instance: instance.to_string(),
                            id: *id,
                            fire_at_ms: *fire_at_ms,
                        },
                    )
                    .await;
            }
            _ => {}
        }
    }
}

impl super::Runtime {
    pub(crate) async fn handle_continue_as_new(
        self: &std::sync::Arc<Self>,
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
        // Resolve orchestration name from history
        let orch_name = {
            let hist = self.history_store.read(instance).await;
            hist.iter()
                .find_map(|e| match e {
                    crate::Event::OrchestrationStarted { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| orchestration_name.to_string())
        };
        // Determine target version for the next execution and pin it
        let target_version_str: String = if let Some(ver_str) = version.clone() {
            if let Ok(v) = semver::Version::parse(&ver_str) {
                self.pinned_versions.lock().await.insert(instance.to_string(), v);
            }
            ver_str
        } else if let Some((v, _h)) = self.orchestration_registry.resolve_for_start(&orch_name).await {
            self.pinned_versions
                .lock()
                .await
                .insert(instance.to_string(), v.clone());
            v.to_string()
        } else if let Some(v) = self.pinned_versions.lock().await.get(instance).cloned() {
            v.to_string()
        } else {
            "0.0.0".to_string()
        };
        // Reset to a new execution and enqueue start
        // Preserve parent linkage and include version string in the new OrchestrationStarted
        let hist_for_link = self.history_store.read(instance).await;
        let mut parent_instance: Option<String> = None;
        let mut parent_id: Option<u64> = None;
        for e in hist_for_link.iter().rev() {
            if let crate::Event::OrchestrationStarted {
                parent_instance: pi,
                parent_id: pd,
                ..
            } = e
            {
                parent_instance = pi.clone();
                parent_id = *pd;
                break;
            }
        }
        let _new_exec = self
            .history_store
            .reset_for_continue_as_new(
                instance,
                &orch_name,
                &target_version_str,
                &input,
                parent_instance.as_deref(),
                parent_id,
            )
            .await
            .unwrap_or(1);
        // Defer restart slightly so the current execution can release the active guard
        let rt = self.clone();
        let inst = instance.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(super::Runtime::POLLER_GATE_DELAY_MS)).await;
            rt.ensure_instance_active(&inst, &orch_name).await;
        });
        // Notify any waiters so initial handle resolves (empty output for continued-as-new)
        if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
            for w in waiters {
                let _ = w.send((history.clone(), Ok(String::new())));
            }
        }
    }
}
