use std::collections::HashMap;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;

/// Messages delivered back to the orchestrator loop by workers and routers.
pub enum OrchestratorMsg {
    ActivityCompleted { instance: String, id: u64, result: String, ack_token: Option<String> },
    ActivityFailed { instance: String, id: u64, error: String, ack_token: Option<String> },
    TimerFired { instance: String, id: u64, fire_at_ms: u64, ack_token: Option<String> },
    ExternalEvent { instance: String, id: u64, name: String, data: String, ack_token: Option<String> },
    ExternalByName { instance: String, name: String, data: String, ack_token: Option<String> },
    SubOrchCompleted { instance: String, id: u64, result: String, ack_token: Option<String> },
    SubOrchFailed { instance: String, id: u64, error: String, ack_token: Option<String> },
    CancelRequested { instance: String, reason: String, ack_token: Option<String> },
}

/// Request to start a new orchestration instance.
#[derive(Clone, Debug)]
pub struct StartRequest { pub instance: String, pub orchestration_name: String }

pub struct CompletionRouter { pub(crate) inboxes: Mutex<HashMap<String, mpsc::UnboundedSender<OrchestratorMsg>>> }

impl CompletionRouter {
    pub async fn register(&self, instance: &str) -> mpsc::UnboundedReceiver<OrchestratorMsg> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inboxes.lock().await.insert(instance.to_string(), tx);
        rx
    }
    pub async fn unregister(&self, instance: &str) {
        self.inboxes.lock().await.remove(instance);
    }
    pub async fn forward(&self, msg: OrchestratorMsg) {
        let key = match &msg {
            OrchestratorMsg::ActivityCompleted { instance, .. }
            | OrchestratorMsg::ActivityFailed { instance, .. }
            | OrchestratorMsg::TimerFired { instance, .. }
            | OrchestratorMsg::ExternalEvent { instance, .. }
            | OrchestratorMsg::ExternalByName { instance, .. }
            | OrchestratorMsg::SubOrchCompleted { instance, .. }
            | OrchestratorMsg::SubOrchFailed { instance, .. }
            | OrchestratorMsg::CancelRequested { instance, .. } => instance.clone(),
        };
        let kind = kind_of(&msg);
        if let Some(tx) = self.inboxes.lock().await.get(&key) {
            if let Err(_e) = tx.send(msg) {
                warn!(instance=%key, kind=%kind, "router: receiver dropped, dropping message");
            }
        } else {
            warn!(instance=%key, kind=%kind, "router: unknown instance, dropping message");
        }
    }
}

pub fn kind_of(msg: &OrchestratorMsg) -> &'static str {
    match msg {
    OrchestratorMsg::ActivityCompleted { .. } => "ActivityCompleted",
    OrchestratorMsg::ActivityFailed { .. } => "ActivityFailed",
    OrchestratorMsg::TimerFired { .. } => "TimerFired",
    OrchestratorMsg::ExternalEvent { .. } => "ExternalEvent",
    OrchestratorMsg::ExternalByName { .. } => "ExternalByName",
    OrchestratorMsg::SubOrchCompleted { .. } => "SubOrchCompleted",
    OrchestratorMsg::SubOrchFailed { .. } => "SubOrchFailed",
    OrchestratorMsg::CancelRequested { .. } => "CancelRequested",
    }
}


