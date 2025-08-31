
use std::sync::Arc;
mod common;
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;

async fn concurrent_orchestrations_different_activities_with(store: StdArc<dyn HistoryStore>) {
    let o1 = |ctx: OrchestrationContext, _input: String| async move {
        let f_a = ctx.schedule_activity("Add", "2,3");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let results = ctx.join(vec![f_a, f_e, f_t]).await;
        
        // Find activity and external results (order may vary due to history ordering)
        let mut a = None;
        let mut e = None;
        for result in &results {
            match result {
                rust_dtf::futures::DurableOutput::Activity(Ok(activity_result)) => {
                    a = Some(activity_result.clone());
                }
                rust_dtf::futures::DurableOutput::External(external_data) => {
                    e = Some(external_data.clone());
                }
                _ => {} // Ignore timer for now
            }
        }
        
        let a = a.expect("activity result not found");
        let e = e.expect("external result not found");
        
        Ok(format!("o1:sum={a};evt={e}"))
    };
    let o2 = |ctx: OrchestrationContext, _input: String| async move {
        let f_a = ctx.schedule_activity("Upper", "hi");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let results = ctx.join(vec![f_a, f_e, f_t]).await;
        
        // Find activity and external results (order may vary due to history ordering)
        let mut a = None;
        let mut e = None;
        for result in &results {
            match result {
                rust_dtf::futures::DurableOutput::Activity(Ok(activity_result)) => {
                    a = Some(activity_result.clone());
                }
                rust_dtf::futures::DurableOutput::External(external_data) => {
                    e = Some(external_data.clone());
                }
                _ => {} // Ignore timer for now
            }
        }
        
        let a = a.expect("activity result not found");
        let e = e.expect("external result not found");
        
        Ok(format!("o2:up={a};evt={e}"))
    };

    let activity_registry = ActivityRegistry::builder()
        .register("Add", |input: String| async move {
            let mut parts = input.split(',');
            let a = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
            let b = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
            Ok((a + b).to_string())
        })
        .register("Upper", |input: String| async move { Ok(input.to_uppercase()) })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("AddOrchestration", o1)
        .register("UpperOrchestration", o2)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let h1 = rt
        .clone()
        .start_orchestration("inst-multi-1", "AddOrchestration", "")
        .await;
    let h2 = rt
        .clone()
        .start_orchestration("inst-multi-2", "UpperOrchestration", "")
        .await;

    let store_for_wait1 = store.clone();
    let rt_c = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait1, "inst-multi-1", "Go", 1000).await;
        rt_c.raise_event("inst-multi-1", "Go", "E1").await;
    });
    let store_for_wait2 = store.clone();
    let rt_c2 = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait2, "inst-multi-2", "Go", 1000).await;
        rt_c2.raise_event("inst-multi-2", "Go", "E2").await;
    });

    let out1 = match rt
        .wait_for_orchestration("inst-multi-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => Ok(output),
        runtime::OrchestrationStatus::Failed { error } => Err(error),
        _ => panic!("unexpected orchestration status"),
    };
    
    let out2 = match rt
        .wait_for_orchestration("inst-multi-2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => Ok(output),
        runtime::OrchestrationStatus::Failed { error } => Err(error),
        _ => panic!("unexpected orchestration status"),
    };

    assert!(
        out1.as_ref().unwrap().contains("o1:sum=5;evt=E1"),
        "unexpected out1: {out1:?}"
    );
    assert!(
        out2.as_ref().unwrap().contains("o2:up=HI;evt=E2"),
        "unexpected out2: {out2:?}"
    );
    
    // Check histories
    let hist1 = rt.get_execution_history("inst-multi-1", 1).await;
    let hist2 = rt.get_execution_history("inst-multi-2", 1).await;

    assert!(
        hist1
            .iter()
            .any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "5"))
    );
    assert!(
        hist2
            .iter()
            .any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "HI"))
    );
    assert!(
        hist1
            .iter()
            .any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "E1"))
    );
    assert!(
        hist2
            .iter()
            .any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "E2"))
    );
    assert!(
        hist1
            .iter()
            .any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3))
    );
    assert!(
        hist2
            .iter()
            .any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3))
    );

    rt.shutdown().await;
}

#[tokio::test]
async fn concurrent_orchestrations_different_activities_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    concurrent_orchestrations_different_activities_with(store).await;
}

async fn concurrent_orchestrations_same_activities_with(store: StdArc<dyn HistoryStore>) {
    let o1 = |ctx: OrchestrationContext, _input: String| async move {
        let f_a = ctx.schedule_activity("Proc", "10");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let results = ctx.join(vec![f_a, f_e, f_t]).await;
        
        // Find activity and external results (order may vary due to history ordering)
        let mut a = None;
        let mut e = None;
        for result in &results {
            match result {
                rust_dtf::futures::DurableOutput::Activity(Ok(activity_result)) => {
                    a = Some(activity_result.clone());
                }
                rust_dtf::futures::DurableOutput::External(external_data) => {
                    e = Some(external_data.clone());
                }
                _ => {} // Ignore timer for now
            }
        }
        
        let a = a.expect("activity result not found");
        let e = e.expect("external result not found");
        
        Ok(format!("o1:a={a};evt={e}"))
    };
    let o2 = |ctx: OrchestrationContext, _input: String| async move {
        let _guid = ctx.system_new_guid().await;
        let f_a = ctx.schedule_activity("Proc", "20");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let results = ctx.join(vec![f_a, f_e, f_t]).await;
        
        // Find activity and external results (order may vary due to history ordering)
        let mut a = None;
        let mut e = None;
        for result in &results {
            match result {
                rust_dtf::futures::DurableOutput::Activity(Ok(activity_result)) => {
                    a = Some(activity_result.clone());
                }
                rust_dtf::futures::DurableOutput::External(external_data) => {
                    e = Some(external_data.clone());
                }
                _ => {} // Ignore timer for now
            }
        }
        
        let a = a.expect("activity result not found");
        let e = e.expect("external result not found");
        
        Ok(format!("o2:a={a};evt={e}"))
    };

    let activity_registry = ActivityRegistry::builder()
        .register("Proc", |input: String| async move {
            let n = input.parse::<i64>().unwrap_or(0);
            Ok((n + 1).to_string())
        })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ProcOrchestration1", o1)
        .register("ProcOrchestration2", o2)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let h1 = rt
        .clone()
        .start_orchestration("inst-same-acts-1", "ProcOrchestration1", "")
        .await;
    let h2 = rt
        .clone()
        .start_orchestration("inst-same-acts-2", "ProcOrchestration2", "")
        .await;

    let store_for_wait3 = store.clone();
    let rt_c = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait3, "inst-same-acts-1", "Go", 1000).await;
        rt_c.raise_event("inst-same-acts-1", "Go", "P1").await;
    });
    let store_for_wait4 = store.clone();
    let rt_c2 = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait4, "inst-same-acts-2", "Go", 1000).await;
        rt_c2.raise_event("inst-same-acts-2", "Go", "P2").await;
    });

    match rt
        .wait_for_orchestration("inst-same-acts-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "o1:a=11;evt=P1"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }
    
    match rt
        .wait_for_orchestration("inst-same-acts-2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "o2:a=21;evt=P2"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }
    
    // Check histories
    let hist1 = rt.get_execution_history("inst-same-acts-1", 1).await;
    let hist2 = rt.get_execution_history("inst-same-acts-2", 1).await;

    assert!(
        hist1
            .iter()
            .any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "11"))
    );
    assert!(
        hist2
            .iter()
            .any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 2 && result == "21"))
    );
    assert!(
        hist1
            .iter()
            .any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "P1"))
    );
    assert!(
        hist2
            .iter()
            .any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 3 && data == "P2"))
    );
    assert!(
        hist1
            .iter()
            .any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3))
    );
    assert!(
        hist2
            .iter()
            .any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 4))
    );

    rt.shutdown().await;
}

#[tokio::test]
async fn concurrent_orchestrations_same_activities_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    concurrent_orchestrations_same_activities_with(store).await;
}
