use std::time::{Duration, Instant};
use std::sync::Arc as StdArc;
use rust_dtf::Event;
use rust_dtf::providers::HistoryStore;

pub async fn wait_for_history<F>(store: StdArc<dyn HistoryStore>, instance: &str, predicate: F, timeout_ms: u64) -> bool
where
    F: Fn(&Vec<Event>) -> bool,
{
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let hist = store.read(instance).await;
        if predicate(&hist) { return true; }
        if Instant::now() > deadline { return false; }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

pub async fn wait_for_subscription(store: StdArc<dyn HistoryStore>, instance: &str, name: &str, timeout_ms: u64) -> bool {
    wait_for_history(store, instance, |hist| {
        hist.iter().any(|e| matches!(e, Event::ExternalSubscribed { name: n, .. } if n == name))
    }, timeout_ms).await
}


