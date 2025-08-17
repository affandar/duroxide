use futures::future::{join3};
use std::sync::Arc;
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry};
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;

async fn concurrent_orchestrations_different_activities_with(store: StdArc<dyn HistoryStore>) {
    let o1 = |ctx: OrchestrationContext, _input: String| async move {
        let f_a = ctx.schedule_activity("Add", "2,3");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let (a, e, _) = join3(f_a.into_activity(), f_e.into_event(), f_t.into_timer()).await;
        let a = a.unwrap();
        Ok(format!("o1:sum={a};evt={e}"))
    };
    let o2 = |ctx: OrchestrationContext, _input: String| async move {
        let f_a = ctx.schedule_activity("Upper", "hi");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let (a, e, _) = join3(f_a.into_activity(), f_e.into_event(), f_t.into_timer()).await;
        let a = a.unwrap();
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

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let h1 = rt.clone().start_orchestration("inst-multi-1", "AddOrchestration", "").await;
    let h2 = rt.clone().start_orchestration("inst-multi-2", "UpperOrchestration", "").await;

    let rt_c = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        rt_c.raise_event("inst-multi-1", "Go", "E1").await;
    });
    let rt_c2 = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(4)).await;
        rt_c2.raise_event("inst-multi-2", "Go", "E2").await;
    });

    let (hist1, out1) = h1.unwrap().await.unwrap();
    let (hist2, out2) = h2.unwrap().await.unwrap();

    assert!(out1.as_ref().unwrap().contains("o1:sum=5;evt=E1"), "unexpected out1: {out1:?}");
    assert!(out2.as_ref().unwrap().contains("o2:up=HI;evt=E2"), "unexpected out2: {out2:?}");

    assert!(hist1.iter().any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "5")));
    assert!(hist2.iter().any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "HI")));
    assert!(hist1.iter().any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "E1")));
    assert!(hist2.iter().any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "E2")));
    assert!(hist1.iter().any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3)));
    assert!(hist2.iter().any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3)));

    rt.shutdown().await;
}

#[tokio::test]
async fn concurrent_orchestrations_different_activities_fs() {
    eprintln!("START: concurrent_orchestrations_different_activities_fs");
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    concurrent_orchestrations_different_activities_with(store).await;
}

async fn concurrent_orchestrations_same_activities_with(store: StdArc<dyn HistoryStore>) {
    let o1 = |ctx: OrchestrationContext, _input: String| async move {
        let f_a = ctx.schedule_activity("Proc", "10");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let (a, e, _) = join3(f_a.into_activity(), f_e.into_event(), f_t.into_timer()).await;
        let a = a.unwrap();
        Ok(format!("o1:a={a};evt={e}"))
    };
    let o2 = |ctx: OrchestrationContext, _input: String| async move {
        let _guid = ctx.new_guid();
        let f_a = ctx.schedule_activity("Proc", "20");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let (a, e, _) = join3(f_a.into_activity(), f_e.into_event(), f_t.into_timer()).await;
        let a = a.unwrap();
        Ok(format!("o2:a={a};evt={e}"))
    };

    let activity_registry = ActivityRegistry::builder()
        .register("Proc", |input: String| async move { let n = input.parse::<i64>().unwrap_or(0); Ok((n + 1).to_string()) })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ProcOrchestration1", o1)
        .register("ProcOrchestration2", o2)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let h1 = rt.clone().start_orchestration("inst-same-acts-1", "ProcOrchestration1", "").await;
    let h2 = rt.clone().start_orchestration("inst-same-acts-2", "ProcOrchestration2", "").await;

    let rt_c = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        rt_c.raise_event("inst-same-acts-1", "Go", "P1").await;
    });
    let rt_c2 = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        rt_c2.raise_event("inst-same-acts-2", "Go", "P2").await;
    });

    let (hist1, out1) = h1.unwrap().await.unwrap();
    let (hist2, out2) = h2.unwrap().await.unwrap();

    assert_eq!(out1.unwrap(), "o1:a=11;evt=P1");
    assert_eq!(out2.unwrap(), "o2:a=21;evt=P2");

    assert!(hist1.iter().any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "11")));
    assert!(hist2.iter().any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "21")));
    assert!(hist1.iter().any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "P1")));
    assert!(hist2.iter().any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "P2")));
    assert!(hist1.iter().any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3)));
    assert!(hist2.iter().any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3)));

    rt.shutdown().await;
}

#[tokio::test]
async fn concurrent_orchestrations_same_activities_fs() {
    eprintln!("START: concurrent_orchestrations_same_activities_fs");
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    concurrent_orchestrations_same_activities_with(store).await;
}


