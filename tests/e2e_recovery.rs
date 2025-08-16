use std::sync::Arc;
use rust_dtf::{Event, OrchestrationContext};
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::in_memory::InMemoryHistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;

async fn recovery_across_restart_core<F1, F2>(make_store_stage1: F1, make_store_stage2: F2, instance: String)
where
    F1: Fn() -> StdArc<dyn HistoryStore>,
    F2: Fn() -> StdArc<dyn HistoryStore>,
{
    let orchestrator = |ctx: OrchestrationContext| async move {
        let s1 = ctx.schedule_activity("Step", "1").into_activity().await.unwrap();
        let s2 = ctx.schedule_activity("Step", "2").into_activity().await.unwrap();
        let _ = ctx.schedule_wait("Resume").into_event().await;
        let s3 = ctx.schedule_activity("Step", "3").into_activity().await.unwrap();
        let s4 = ctx.schedule_activity("Step", "4").into_activity().await.unwrap();
        format!("{s1}{s2}{s3}{s4}")
    };

    let count_scheduled = |hist: &Vec<Event>, input: &str| -> usize {
        hist.iter().filter(|e| matches!(e, Event::ActivityScheduled { name, input: inp, .. } if name == "Step" && inp == input)).count()
    };

    let store1 = make_store_stage1();
    let registry = ActivityRegistry::builder().register("Step", |input: String| async move { input }).build();

    let rt1 = runtime::Runtime::start_with_store(store1.clone(), Arc::new(registry.clone())).await;
    let handle1 = rt1.clone().spawn_instance_to_completion(&instance, orchestrator).await;

    tokio::time::sleep(std::time::Duration::from_millis(15)).await;

    let pre_crash_hist = store1.read(&instance).await;
    assert_eq!(count_scheduled(&pre_crash_hist, "1"), 1);
    assert_eq!(count_scheduled(&pre_crash_hist, "2"), 1);
    assert_eq!(count_scheduled(&pre_crash_hist, "3"), 0);

    rt1.shutdown().await;
    drop(handle1);

    let store2 = make_store_stage2();
    // Remove the instance before attempting restart; runtime now treats existing instances as an error
    let _ = store2.remove_instance(&instance).await;
    let rt2 = runtime::Runtime::start_with_store(store2.clone(), Arc::new(registry.clone())).await;
    let rt2_c = rt2.clone();
    let instance_for_spawn = instance.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        rt2_c.raise_event(&instance_for_spawn, "Resume", "go").await;
    });
    let handle2 = rt2.clone().spawn_instance_to_completion(&instance, orchestrator).await;
    let (_final_hist_runtime, output) = handle2.await.unwrap();
    assert_eq!(output, "1234");

    let final_hist2 = store2.read(&instance).await;
    assert_eq!(count_scheduled(&final_hist2, "3"), 1);
    assert_eq!(count_scheduled(&final_hist2, "4"), 1);

    rt2.shutdown().await;
}

#[tokio::test]
async fn recovery_across_restart_fs_provider() {
    let base = std::env::current_dir().unwrap().join(".testdata");
    std::fs::create_dir_all(&base).unwrap();
    let dir = base.join(format!("fs_recovery_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
    std::fs::create_dir_all(&dir).unwrap();

    let instance = String::from("inst-recover-fs-1");

    let make_store1 = || StdArc::new(FsHistoryStore::new(&dir, true)) as StdArc<dyn HistoryStore>;
    let make_store2 = || StdArc::new(FsHistoryStore::new(&dir, false)) as StdArc<dyn HistoryStore>;

    recovery_across_restart_core(make_store1, make_store2, instance.clone()).await;

    let store = StdArc::new(FsHistoryStore::new(&dir, false)) as StdArc<dyn HistoryStore>;
    let hist = store.read(&instance).await;
    let count = |inp: &str| hist.iter().filter(|e| matches!(e, Event::ActivityScheduled { name, input, .. } if name == "Step" && input == inp)).count();
    assert_eq!(count("1"), 1);
    assert_eq!(count("2"), 1);
    assert_eq!(count("3"), 1);
    assert_eq!(count("4"), 1);
}

#[tokio::test]
async fn recovery_across_restart_inmem_provider() {
    let instance = String::from("inst-recover-mem-1");
    let make_store1 = || StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    let make_store2 = || StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;

    recovery_across_restart_core(make_store1, make_store2, instance.clone()).await;

    let store_before = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    let store_after = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    let hist_before = store_before.read(&instance).await;
    let hist_after = store_after.read(&instance).await;

    let count = |hist: &Vec<Event>, inp: &str| hist.iter().filter(|e| matches!(e, Event::ActivityScheduled { name, input, .. } if name == "Step" && input == inp)).count();
    assert_eq!(count(&hist_before, "1"), 0);
    assert_eq!(count(&hist_before, "2"), 0);
    assert_eq!(count(&hist_after, "1"), 0);
    assert_eq!(count(&hist_after, "2"), 0);
}



