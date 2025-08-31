use std::collections::HashMap;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, warn};
use crate::providers::WorkItem;

/// Completion messages delivered to active orchestration instances.
/// These correspond directly to WorkItem completion variants but include ack tokens.
#[derive(Debug)]
pub enum OrchestratorMsg {
    ActivityCompleted {
        instance: String,
        execution_id: u64,
        id: u64,
        result: String,
        ack_token: Option<String>,
    },
    ActivityFailed {
        instance: String,
        execution_id: u64,
        id: u64,
        error: String,
        ack_token: Option<String>,
    },
    TimerFired {
        instance: String,
        execution_id: u64,
        id: u64,
        fire_at_ms: u64,
        ack_token: Option<String>,
    },
    ExternalByName {
        instance: String,
        name: String,
        data: String,
        ack_token: Option<String>,
    },
    SubOrchCompleted {
        instance: String,
        execution_id: u64,
        id: u64,
        result: String,
        ack_token: Option<String>,
    },
    SubOrchFailed {
        instance: String,
        execution_id: u64,
        id: u64,
        error: String,
        ack_token: Option<String>,
    },
    CancelRequested {
        instance: String,
        reason: String,
        ack_token: Option<String>,
    },
}

/// Convert a WorkItem completion to an OrchestratorMsg with ack token
impl OrchestratorMsg {
    pub fn from_work_item(work_item: WorkItem, ack_token: Option<String>) -> Option<Self> {
        match work_item {
            WorkItem::ActivityCompleted { instance, execution_id, id, result } => {
                Some(OrchestratorMsg::ActivityCompleted { instance, execution_id, id, result, ack_token })
            }
            WorkItem::ActivityFailed { instance, execution_id, id, error } => {
                Some(OrchestratorMsg::ActivityFailed { instance, execution_id, id, error, ack_token })
            }
            WorkItem::TimerFired { instance, execution_id, id, fire_at_ms } => {
                Some(OrchestratorMsg::TimerFired { instance, execution_id, id, fire_at_ms, ack_token })
            }
            WorkItem::ExternalRaised { instance, name, data } => {
                Some(OrchestratorMsg::ExternalByName { instance, name, data, ack_token })
            }
            WorkItem::SubOrchCompleted { parent_instance, parent_execution_id, parent_id, result } => {
                Some(OrchestratorMsg::SubOrchCompleted { 
                    instance: parent_instance, 
                    execution_id: parent_execution_id, 
                    id: parent_id, 
                    result, 
                    ack_token 
                })
            }
            WorkItem::SubOrchFailed { parent_instance, parent_execution_id, parent_id, error } => {
                Some(OrchestratorMsg::SubOrchFailed { 
                    instance: parent_instance, 
                    execution_id: parent_execution_id, 
                    id: parent_id, 
                    error, 
                    ack_token 
                })
            }
            WorkItem::CancelInstance { instance, reason } => {
                Some(OrchestratorMsg::CancelRequested { instance, reason, ack_token })
            }
            // Non-completion WorkItems don't convert to OrchestratorMsg
            WorkItem::StartOrchestration { .. } 
            | WorkItem::ActivityExecute { .. }
            | WorkItem::TimerSchedule { .. }
            | WorkItem::ContinueAsNew { .. } => None,
        }
    }
}

// StartRequest removed; instances are activated directly by the runtime

pub struct InstanceRouter {
    pub(crate) inboxes: Mutex<HashMap<String, mpsc::UnboundedSender<OrchestratorMsg>>>,
}

impl InstanceRouter {
    pub async fn register(&self, instance: &str) -> mpsc::UnboundedReceiver<OrchestratorMsg> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inboxes.lock().await.insert(instance.to_string(), tx);
        rx
    }
    pub async fn unregister(&self, instance: &str) {
        self.inboxes.lock().await.remove(instance);
    }
    pub async fn forward(&self, msg: OrchestratorMsg) {
        debug!("forward: {:#?}", msg);
        let key = match &msg {
            OrchestratorMsg::ActivityCompleted { instance, .. }
            | OrchestratorMsg::ActivityFailed { instance, .. }
            | OrchestratorMsg::TimerFired { instance, .. }
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

    pub async fn try_send(&self, msg: OrchestratorMsg) -> Result<(), ()> {
        let key = match &msg {
            OrchestratorMsg::ActivityCompleted { instance, .. }
            | OrchestratorMsg::ActivityFailed { instance, .. }
            | OrchestratorMsg::TimerFired { instance, .. }
            | OrchestratorMsg::ExternalByName { instance, .. }
            | OrchestratorMsg::SubOrchCompleted { instance, .. }
            | OrchestratorMsg::SubOrchFailed { instance, .. }
            | OrchestratorMsg::CancelRequested { instance, .. } => instance.clone(),
        };
        let kind = kind_of(&msg);
        let mut map = self.inboxes.lock().await;
        if let Some(tx) = map.get(&key) {
            if let Err(_e) = tx.send(msg) {
                // Receiver dropped; remove stale sender so dispatchers can rehydrate on redelivery
                map.remove(&key);
                warn!(instance=%key, kind=%kind, "router: receiver dropped, removing inbox");
                return Err(());
            }
            Ok(())
        } else {
            warn!(instance=%key, kind=%kind, "router: unknown instance, cannot send");
            Err(())
        }
    }
}

pub fn kind_of(msg: &OrchestratorMsg) -> &'static str {
    match msg {
        OrchestratorMsg::ActivityCompleted { .. } => "ActivityCompleted",
        OrchestratorMsg::ActivityFailed { .. } => "ActivityFailed",
        OrchestratorMsg::TimerFired { .. } => "TimerFired",
        OrchestratorMsg::ExternalByName { .. } => "ExternalByName",
        OrchestratorMsg::SubOrchCompleted { .. } => "SubOrchCompleted",
        OrchestratorMsg::SubOrchFailed { .. } => "SubOrchFailed",
        OrchestratorMsg::CancelRequested { .. } => "CancelRequested",
    }
}
