use std::sync::Arc;
use tracing::{debug, warn};

use crate::Event;
use crate::providers::{QueueKind, WorkItem};

use super::Runtime;

pub async fn dispatch_call_activity(
    rt: &Arc<Runtime>,
    instance: &str,
    history: &[Event],
    id: u64,
    name: String,
    input: String,
) {
    let already_done = history.iter().rev().any(|e| match e {
        Event::ActivityCompleted { id: cid, .. } if *cid == id => true,
        Event::ActivityFailed { id: cid, .. } if *cid == id => true,
        _ => false,
    });
    if already_done {
        debug!(instance, id, name=%name, "skip dispatch: activity already completed/failed");
    } else {
        debug!(instance, id, name=%name, "dispatch activity");
        // Enqueue provider-backed execution; WorkDispatcher will execute and enqueue completion/failure
        let execution_id = rt.get_execution_id_for_instance(instance).await;
        let _ = rt
            .history_store
            .enqueue_work(
                QueueKind::Worker,
                WorkItem::ActivityExecute {
                    instance: instance.to_string(),
                    execution_id,
                    id,
                    name,
                    input,
                },
            )
            .await;
    }
}

pub async fn dispatch_create_timer(rt: &Arc<Runtime>, instance: &str, history: &[Event], id: u64, delay_ms: u64) {
    let already_fired = history
        .iter()
        .rev()
        .any(|e| matches!(e, Event::TimerFired { id: cid, .. } if *cid == id));
    if already_fired {
        debug!(instance, id, "skip dispatch: timer already fired");
        return;
    }
    let fire_at_ms = history
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::TimerCreated { id: cid, fire_at_ms } if *cid == id => Some(*fire_at_ms),
            _ => None,
        })
        .unwrap_or(0);
    debug!(instance, id, fire_at_ms, delay_ms, "dispatch timer");
    // Enqueue provider-backed schedule; a TimerDispatcher can later materialize TimerFired
    let execution_id = rt.get_execution_id_for_instance(instance).await;
    let _ = rt
        .history_store
        .enqueue_work(
            QueueKind::Timer,
            WorkItem::TimerSchedule {
                instance: instance.to_string(),
                execution_id,
                id,
                fire_at_ms,
            },
        )
        .await;
}

pub async fn dispatch_wait_external(_rt: &Arc<Runtime>, instance: &str, _history: &[Event], id: u64, name: String) {
    debug!(instance, id, name=%name, "subscribe external");
}

pub async fn dispatch_start_detached(
    rt: &Arc<Runtime>,
    instance: &str,
    id: u64,
    name: String,
    version: Option<String>,
    child_instance: String,
    input: String,
) {
    // Resolve version pin for the child instance (explicit or policy)
    if let Some(ver_str) = version.clone() {
        if let Ok(v) = semver::Version::parse(&ver_str) {
            rt.pinned_versions.lock().await.insert(child_instance.clone(), v);
        }
    } else if let Some((v, _h)) = rt.orchestration_registry.resolve_for_start(&name).await {
        rt.pinned_versions.lock().await.insert(child_instance.clone(), v);
    }
    let wi = WorkItem::StartOrchestration {
        instance: child_instance.clone(),
        orchestration: name.clone(),
        input: input.clone(),
    };
    if let Err(e) = rt.history_store.enqueue_work(QueueKind::Orchestrator, wi).await {
        warn!(instance, id, name=%name, child_instance=%child_instance, error=%e, "failed to enqueue detached start; will rely on bootstrap rehydration");
    } else {
        debug!(instance, id, name=%name, child_instance=%child_instance, "enqueued detached orchestration start");
    }
}

pub async fn dispatch_start_sub_orchestration(
    rt: &Arc<Runtime>,
    parent_instance: &str,
    history: &[Event],
    id: u64,
    name: String,
    version: Option<String>,
    child_suffix: String,
    input: String,
) {
    let already_done = history.iter().rev().any(|e| {
        matches!(e, Event::SubOrchestrationCompleted { id: cid, .. } if *cid == id)
            || matches!(e, Event::SubOrchestrationFailed { id: cid, .. } if *cid == id)
    });
    if already_done {
        debug!(parent_instance, id, name=%name, "skip dispatch: sub-orch already completed/failed");
        return;
    }
    let child_full = format!("{}::{}", parent_instance, child_suffix);
    let parent_inst = parent_instance.to_string();
    let name_clone = name.clone();
    let input_clone = input.clone();
    let rt_for_child = rt.clone();
    // Resolve version pin for the child (handled again inside start)
    if let Some(ver_str) = version.clone() {
        if let Ok(v) = semver::Version::parse(&ver_str) {
            rt_for_child.pinned_versions.lock().await.insert(child_full.clone(), v);
        }
    } else if let Some((v, _h)) = rt.orchestration_registry.resolve_for_start(&name).await {
        rt_for_child.pinned_versions.lock().await.insert(child_full.clone(), v);
    }
    debug!(parent_instance, id, name=%name, child_instance=%child_full, "start child orchestration");
    
    // Try to parse version for pinning (will be pinned in start_internal_wait as well)
    let version_pin = version.clone().and_then(|s| semver::Version::parse(&s).ok());
    
    // Just start the sub-orchestration - completion notification is automatic via handle_orchestration_completion!
    match rt_for_child
        .start_orchestration_with_parent(
            &child_full,
            &name_clone,
            input_clone,
            parent_inst.clone(),
            id,
            version_pin,
        )
        .await
    {
        Ok(()) => {
            debug!(parent_instance, id, child_instance=%child_full, 
                   "sub-orchestration started successfully - completion will be automatic");
            // Done! Child will auto-notify parent when it completes via handle_orchestration_completion
        }
        Err(e) => {
            // Only handle start failures - completion failures are handled by the child itself
            debug!(parent_instance, id, child_instance=%child_full, error=%e, 
                   "sub-orchestration start failed, enqueuing failure");
            let parent_execution_id = rt.get_execution_id_for_instance(parent_instance).await;
            let _ = rt.history_store.enqueue_work(
                QueueKind::Orchestrator,
                WorkItem::SubOrchFailed {
                    parent_instance: parent_inst,
                    parent_execution_id,
                    parent_id: id,
                    error: e,
                },
            ).await;
        }
    }
}
